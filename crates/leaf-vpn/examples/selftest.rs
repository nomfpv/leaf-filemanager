//! A live test of leaf's VPN with a real Mullvad account, outside the app:
//!
//!     cargo run --profile fast -p leaf-vpn --example selftest -- <account-file> [country]
//!
//! `<account-file>` holds the account number (spaces allowed). It is read into memory only: no keyring, no settings, no file
//! is written. A fresh device (key) is registered for the run and removed from the account again at the end, also after a
//! failure. Each step prints one line `step: ok/FAILED reason (ms)`. Nothing secret is printed: not the account number, the
//! key, a token, nor an API answer (status codes and short reasons only).

use std::sync::Arc;
use std::time::{Duration, Instant};

use leaf_vpn::account::{self, AccountNumber};
use leaf_vpn::mullvad::{self, DnsBlocking, Mullvad, Place};

fn step<T>(name: &str, started: Instant, r: Result<T, String>, ok: impl FnOnce(&T) -> String) -> Option<T> {
    let ms = started.elapsed().as_millis();
    match r {
        Ok(v) => {
            let more = ok(&v);
            println!("{name}: ok{}{more} ({ms} ms)", if more.is_empty() { "" } else { " " });
            Some(v)
        }
        Err(why) => {
            println!("{name}: FAILED {} ({ms} ms)", why.lines().next().unwrap_or_default());
            None
        }
    }
}

/// Removes the run's device from the account when dropped: at the end, and after any failure on the way.
struct Device {
    rt: tokio::runtime::Handle,
    account: AccountNumber,
    id: String,
}

impl Drop for Device {
    fn drop(&mut self) {
        leaf_vpn::disconnect();
        let t = Instant::now();
        let r = self.rt.block_on(account::sign_out(&self.account, &self.id)).map_err(|e| e.to_string());
        step("remove device", t, r, |_| String::new());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(file) = args.next() else {
        eprintln!("usage: selftest <account-file> [country code, e.g. se]");
        std::process::exit(2);
    };
    let country = args.next().map(|c| c.trim().to_ascii_lowercase()).filter(|c| !c.is_empty());
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(&file).unwrap_or_default());
    let Some(acc) = AccountNumber::parse(text.trim()) else {
        println!("account file: FAILED it does not hold a 16-digit account number");
        std::process::exit(2);
    };
    drop(text);
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
    let h = rt.handle().clone();
    leaf_vpn::init(move || h.clone());
    leaf_vpn::set_kill_switch(false);

    let t = Instant::now();
    let relays = step("relay list", t, rt.block_on(mullvad::relays()).map_err(|e| e.to_string()), |r| format!("{} servers", r.len()));
    if relays.is_none() {
        return;
    }

    let t = Instant::now();
    let Some(signed) = step("register key", t, rt.block_on(account::sign_in(&acc)).map_err(|e| e.to_string()), |s| format!("device \"{}\"", s.device.name)) else {
        return;
    };
    let guard = Device { rt: rt.handle().clone(), account: acc.clone(), id: signed.device.id.clone() };
    if let Some(days) = signed.expiry.as_deref().and_then(|e| account::days_left(e, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64))) {
        println!("days left: {days}");
    }

    // pick + handshake: the app's own path (fresh key: 60 s for the first handshake; port fallback; exit check)
    let t = Instant::now();
    let fastest = country.is_none();
    let place = Place { country: country.clone(), city: None, fastest };
    leaf_vpn::connect(Arc::new(Mullvad { device: signed.device.clone(), place, dns: DnsBlocking::default(), fresh: true }));
    let deadline = Instant::now() + Duration::from_secs(240);
    let state = loop {
        let s = leaf_vpn::state();
        if matches!(s, leaf_vpn::State::On { .. } | leaf_vpn::State::Failed(_)) || Instant::now() > deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let up = match &state {
        leaf_vpn::State::On { country, city, server, .. } => {
            println!("pick relay: ok {server} in {city}, {country}{}", if fastest { " (fastest or weighted)" } else { "" });
            step("handshake", t, Ok(()), |_| String::new())
        }
        leaf_vpn::State::Failed(why) => step::<()>("handshake", t, Err(why.clone()), |_| String::new()),
        other => step::<()>("handshake", t, Err(format!("still {other:?} after 240 s")), |_| String::new()),
    };
    if up.is_none() {
        drop(guard);
        return;
    }
    let Some(tunnel) = leaf_vpn::tunnel() else {
        println!("tunnel: FAILED it is gone");
        drop(guard);
        return;
    };

    let t = Instant::now();
    step("dns through tunnel", t, rt.block_on(tunnel.resolve("example.com")).map_err(|e| e.to_string()), |v| format!("{} address(es)", v.len()));

    let t = Instant::now();
    match &state {
        leaf_vpn::State::On { country, exit_ip, unverified: None, .. } => {
            step("exit check", t, Ok(()), |_| format!("exit {} in {country}, Mullvad says it is a Mullvad exit", exit_ip.map(|i| i.to_string()).unwrap_or_else(|| "?".into())));
        }
        leaf_vpn::State::On { unverified: Some(why), .. } => {
            step::<()>("exit check", t, Err(why.clone()), |_| String::new());
        }
        _ => {}
    }

    let get = |name: &str| {
        let t = Instant::now();
        let r = rt.block_on(leaf_vpn::https_get(&tunnel, "www.example.com", "/"));
        let r = r.and_then(|(s, n)| if s == 200 { Ok(n) } else { Err(format!("HTTP {s}")) });
        step(name, t, r, |n| format!("{n} bytes"));
    };
    get("https get");
    println!("idle: 40 s");
    std::thread::sleep(Duration::from_secs(40));
    get("https get after idle");
    drop(tunnel);

    // the SOCKS proxy for other programs: the exit check through it, with the name looked up by the proxy (remote DNS)
    let t = Instant::now();
    match leaf_vpn::socks::start(0, None) {
        Ok(port) => {
            let r = rt.block_on(exit_through_socks(port));
            match r {
                Ok((ip, mullvad)) => println!("socks: ok exit {ip} Mullvad exit {} ({} ms)", if mullvad { "yes" } else { "no" }, t.elapsed().as_millis()),
                Err(why) => println!("socks: FAILED {why} ({} ms)", t.elapsed().as_millis()),
            }
            // the tunnel goes down: a new connection through the proxy must be refused, never made directly
            leaf_vpn::disconnect();
            let t = Instant::now();
            let r = rt.block_on(socks_connect(port, "am.i.mullvad.net", 443));
            match r {
                Err(SocksError::Refused(code)) => println!("socks after tunnel down: ok refused (reply {code}) ({} ms)", t.elapsed().as_millis()),
                Err(SocksError::Other(why)) => println!("socks after tunnel down: FAILED {why} ({} ms)", t.elapsed().as_millis()),
                Ok(_) => println!("socks after tunnel down: FAILED the connection was made ({} ms)", t.elapsed().as_millis()),
            }
            leaf_vpn::socks::stop();
        }
        Err(e) => println!("socks: FAILED the proxy did not start ({e}) ({} ms)", t.elapsed().as_millis()),
    }
    drop(guard);
}

enum SocksError {
    /// The proxy answered with this reply code.
    Refused(u8),
    Other(String),
}

/// A plain SOCKS5 client: no sign-in, CONNECT to `name:port` with the name given to the proxy (it looks it up).
async fn socks_connect(proxy: u16, name: &str, port: u16) -> Result<tokio::net::TcpStream, SocksError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let io = |e: std::io::Error| SocksError::Other(e.to_string());
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy)).await.map_err(io)?;
    s.write_all(&[5, 1, 0]).await.map_err(io)?;
    let mut b = [0u8; 2];
    s.read_exact(&mut b).await.map_err(io)?;
    if b != [5, 0] {
        return Err(SocksError::Other(format!("the proxy did not take the greeting ({b:?})")));
    }
    let mut req = vec![5, 1, 0, 3, name.len() as u8];
    req.extend_from_slice(name.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.map_err(io)?;
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.map_err(io)?;
    if rep[1] != 0 {
        return Err(SocksError::Refused(rep[1]));
    }
    Ok(s)
}

/// `https://am.i.mullvad.net/json` through the proxy: the exit address and Mullvad's word whether it is one of its exits.
async fn exit_through_socks(proxy: u16) -> Result<(String, bool), String> {
    let s = tokio::time::timeout(Duration::from_secs(30), socks_connect(proxy, "am.i.mullvad.net", 443)).await.map_err(|_| "no answer in time".to_string())?.map_err(|e| match e {
        SocksError::Refused(c) => format!("the proxy refused (reply {c})"),
        SocksError::Other(why) => why,
    })?;
    let (status, body) = leaf_vpn::https_get_over(s, "am.i.mullvad.net", "/json").await?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| "the answer could not be read".to_string())?;
    let ip = v.get("ip").and_then(|i| i.as_str()).unwrap_or("?").to_string();
    Ok((ip, v.get("mullvad_exit_ip").and_then(|m| m.as_bool()) == Some(true)))
}
