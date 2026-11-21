//! The kill switch and the secrets (the brief's tests); no live tunnel (no Mullvad account here).

use super::*;

#[test]
fn kill_switch_refuses_a_direct_connection_while_the_tunnel_is_down() {
    // the routing rule: down + kill switch = refused; LAN always direct; down without it = direct
    assert_eq!(route_with("example.com", false, true, false), Route::Blocked);
    assert_eq!(route_with("example.com", false, false, false), Route::Direct);
    assert_eq!(route_with("example.com", true, true, false), Route::Tunnel);
    assert_eq!(route_with("192.168.1.5", false, true, false), Route::Direct);
    assert_eq!(route_with("nas.local", true, true, false), Route::Direct);
    // connecting: held even without the kill switch (the user asked for the VPN)
    assert_eq!(route_with("example.com", false, false, true), Route::Blocked);

    // and the real call, with no runtime given at all: the refusal comes before any socket is opened (the address is a
    // documentation one the test never reaches)
    set_kill_switch(true);
    let e = dial_blocking("203.0.113.7", 443, Duration::from_secs(1)).unwrap_err();
    assert!(is_blocked(&e), "{e}");
    let wrapped: Box<dyn std::error::Error> = Box::new(e);
    assert!(is_blocked(wrapped.as_ref()));
    set_kill_switch(false);
}

#[test]
fn a_reset_through_the_tunnel_is_an_error_not_the_end_of_the_stream() {
    use smoltcp::socket::tcp::State as S;
    // closed without the other side's FIN: a reset or the stack's timeout
    let e = tunnel::read_end(S::Closed, false).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
    // the other side's FIN, seen now or before the socket went on to Closed: the normal end
    for s in [S::CloseWait, S::LastAck, S::TimeWait, S::Closing] {
        assert!(tunnel::read_end(s, false).is_ok(), "{s:?}");
    }
    assert!(tunnel::read_end(S::Closed, true).is_ok());
}

#[test]
fn an_idle_tunnel_is_not_declared_lost() {
    use std::time::Instant;
    use tunnel::{Liveness, Tick};
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    // ten idle minutes: a ping every 30 s, each one answered: never lost
    let mut live = Liveness::new(t0);
    let mut probes = 0;
    for s in (1..=600).step_by(1) {
        match live.tick(at(s)) {
            Tick::Probe => {
                probes += 1;
                live.on_rx(at(s) + Duration::from_millis(80));
            }
            Tick::Dead => panic!("an answered idle tunnel was declared lost at {s} s"),
            Tick::Fine => {}
        }
    }
    assert!((15..=21).contains(&probes), "{probes} pings in ten minutes");
    // nothing answers any more: lost after three unanswered pings, not before
    let mut live = Liveness::new(t0);
    let mut dead_at = None;
    for s in 1..=300 {
        if live.tick(at(s)) == Tick::Dead {
            dead_at = Some(s);
            break;
        }
    }
    let d = dead_at.expect("a silent server is noticed");
    assert!((100..=130).contains(&d), "lost after {d} s");
}

/// A runtime for the tests, driven on a thread of its own (as leaf-remote's is in the app).
fn test_runtime() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let h = rt.handle().clone();
        std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
        init(move || h.clone());
    });
}

/// An echo server on 127.0.0.1 (a LAN address: the bridge joins it directly, no tunnel needed).
fn echo() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = std::io::copy(&mut r, &mut w);
            });
        }
    });
    port
}

fn round_trip(mut s: std::net::TcpStream) -> Vec<u8> {
    use std::io::{Read, Write};
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"leaf").unwrap();
    let mut b = [0u8; 4];
    s.read_exact(&mut b).unwrap();
    b.to_vec()
}

#[test]
fn the_bridge_serves_implicit_ftps_connection_and_only_its_own_caller() {
    test_runtime();
    let port = echo();
    // implicit FTPS: the client connects by itself to the address (no test connection takes the bridge's one accept first)
    let b = loopback_bridge("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let addr = b.any_local();
    let s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    assert_eq!(round_trip(s), b"leaf");
    // the caller's own socket: another program's connection that comes first is dropped, the caller's is served
    let b = loopback_bridge("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let addr = b.addr;
    let mut stray = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    let mine = b.connect(Duration::from_secs(5)).unwrap();
    assert_eq!(round_trip(mine), b"leaf");
    use std::io::Read;
    stray.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(stray.read(&mut buf), Ok(0) | Err(_)), "the stray connection was not served");
}

/// A SOCKS5 client: greeting (with a user name and password when given), CONNECT to `name:port`. The open stream, or the
/// refusal: the reply code, `0xF1` for a refused sign-in, `0xFF` for no acceptable method.
fn socks_ask(proxy: u16, login: Option<(&str, &str)>, name: &str, port: u16) -> Result<std::net::TcpStream, u8> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", proxy)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let method = if login.is_some() { 2 } else { 0 };
    s.write_all(&[5, 1, method]).unwrap();
    let mut b = [0u8; 2];
    s.read_exact(&mut b).map_err(|_| 0xFFu8)?;
    if b[1] != method {
        return Err(0xFF);
    }
    if let Some((u, p)) = login {
        let mut m = vec![1, u.len() as u8];
        m.extend_from_slice(u.as_bytes());
        m.push(p.len() as u8);
        m.extend_from_slice(p.as_bytes());
        s.write_all(&m).unwrap();
        s.read_exact(&mut b).map_err(|_| 0xF1u8)?;
        if b[1] != 0 {
            // (and the proxy closes the connection)
            let mut rest = [0u8; 1];
            assert!(matches!(s.read(&mut rest), Ok(0) | Err(_)), "the connection stays open after a refused sign-in");
            return Err(0xF1);
        }
    }
    let mut req = vec![5, 1, 0];
    match name.parse::<Ipv4Addr>() {
        Ok(ip) => {
            req.push(1);
            req.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            req.extend_from_slice(&[3, name.len() as u8]);
            req.extend_from_slice(name.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).map_err(|_| 0xFEu8)?;
    if rep[1] != 0 { Err(rep[1]) } else { Ok(s) }
}

/// An echo server that counts the connections it got.
fn counted_echo() -> (u16, Arc<AtomicU64>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let n = Arc::new(AtomicU64::new(0));
    let seen = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = std::io::copy(&mut r, &mut w);
            });
        }
    });
    (port, n)
}

/// A stand-in for the tunnel: its resolver knows only the names given (and notes each one asked), its connections are plain
/// local ones (the echo server), counted.
struct FakeTunnel {
    up: bool,
    names: Names,
    asked: Mutex<Vec<String>>,
    dialed: AtomicU64,
}

type Names = Vec<(&'static str, Ipv4Addr)>;

impl socks::Upstream for FakeTunnel {
    fn ready(&self) -> bool {
        self.up
    }
    fn resolve<'a>(&'a self, name: &'a str) -> BoxFuture<'a, io::Result<Vec<Ipv4Addr>>> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(name.to_string());
            Ok(self.names.iter().filter(|(n, _)| *n == name).map(|(_, ip)| *ip).collect())
        })
    }
    fn connect(&self, ip: Ipv4Addr, port: u16) -> BoxFuture<'_, io::Result<Box<dyn socks::Duplex>>> {
        Box::pin(async move {
            self.dialed.fetch_add(1, Ordering::SeqCst);
            let s = tokio::net::TcpStream::connect((ip, port)).await?;
            Ok(Box::new(s) as Box<dyn socks::Duplex>)
        })
    }
}

fn fake(up: bool, names: Names) -> Arc<FakeTunnel> {
    Arc::new(FakeTunnel { up, names, asked: Mutex::new(Vec::new()), dialed: AtomicU64::new(0) })
}

#[test]
fn socks_connect_while_the_tunnel_is_down_is_refused_and_nothing_is_dialed_directly() {
    test_runtime();
    let (echo_port, accepted) = counted_echo();
    // the real proxy (leaf's tunnel): no tunnel in this test, the kill switch off (its default): still refused, by address and by name
    let port = socks::start(0, None).unwrap();
    assert_eq!(socks::port(), Some(port));
    assert_eq!(socks_ask(port, None, "127.0.0.1", echo_port).unwrap_err(), 0x02);
    assert_eq!(socks_ask(port, None, "localhost", echo_port).unwrap_err(), 0x02);
    // connecting (the state while a tunnel comes up or comes back): the same
    hold();
    assert_eq!(socks_ask(port, None, "127.0.0.1", echo_port).unwrap_err(), 0x02);
    disconnect();
    socks::stop();
    assert_eq!(socks::port(), None);
    // a tunnel that is down: nothing looked up, nothing opened
    let up = fake(false, vec![("files.leaf-test.invalid", Ipv4Addr::LOCALHOST)]);
    let (port, _stop) = socks::launch(0, None, up.clone()).unwrap();
    assert_eq!(socks_ask(port, None, "files.leaf-test.invalid", echo_port).unwrap_err(), 0x02);
    assert!(up.asked.lock().unwrap().is_empty());
    assert_eq!(up.dialed.load(Ordering::SeqCst), 0);
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "a connection reached the server directly");
}

#[test]
fn socks_names_are_looked_up_through_the_tunnel_never_by_the_system() {
    test_runtime();
    let (echo_port, accepted) = counted_echo();
    // `.invalid` is never answered by any real resolver (RFC 6761): the connection can only work when the name went to the
    // tunnel's resolver, which knows it
    let up = fake(true, vec![("files.leaf-test.invalid", Ipv4Addr::LOCALHOST)]);
    let (port, _stop) = socks::launch(0, None, up.clone()).unwrap();
    let s = socks_ask(port, None, "files.leaf-test.invalid", echo_port).unwrap();
    assert_eq!(round_trip(s), b"leaf");
    assert_eq!(*up.asked.lock().unwrap(), vec!["files.leaf-test.invalid".to_string()]);
    assert_eq!(up.dialed.load(Ordering::SeqCst), 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    // a name the tunnel's resolver does not know: unreachable, not asked anywhere else
    assert_eq!(socks_ask(port, None, "other.leaf-test.invalid", echo_port).unwrap_err(), 0x04);
    assert_eq!(up.dialed.load(Ordering::SeqCst), 1);
    // UDP ASSOCIATE is refused
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(&[5, 1, 0, 5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
    let mut b = [0u8; 12];
    s.read_exact(&mut b).unwrap();
    assert_eq!(b[3], 0x07);
}

#[test]
fn socks_wrong_proxy_password_is_refused() {
    test_runtime();
    let (echo_port, accepted) = counted_echo();
    let login = socks::Credentials::generate();
    assert_eq!(login.password.len(), 24);
    assert!(!format!("{login:?}").contains(login.password.as_str()));
    let up = fake(true, vec![]);
    let (port, _stop) = socks::launch(0, Some(login.clone()), up.clone()).unwrap();
    // a wrong password, a wrong user, no password at all: refused, nothing opened
    assert_eq!(socks_ask(port, Some((socks::USER, "not-the-password")), "127.0.0.1", echo_port).unwrap_err(), 0xF1);
    assert_eq!(socks_ask(port, Some(("someone", login.password.as_str())), "127.0.0.1", echo_port).unwrap_err(), 0xF1);
    assert_eq!(socks_ask(port, None, "127.0.0.1", echo_port).unwrap_err(), 0xFF);
    assert_eq!(up.dialed.load(Ordering::SeqCst), 0);
    assert_eq!(accepted.load(Ordering::SeqCst), 0);
    // the right one works
    let s = socks_ask(port, Some((socks::USER, login.password.as_str())), "127.0.0.1", echo_port).unwrap();
    assert_eq!(round_trip(s), b"leaf");
}

#[test]
fn socks_connections_over_the_limit_are_closed() {
    test_runtime();
    let (echo_port, _) = counted_echo();
    let up = fake(true, vec![]);
    let (port, _stop) = socks::launch_limited(None, up.clone(), socks::Limits { connections: 2, idle: Duration::from_secs(60) }).unwrap();
    let a = socks_ask(port, None, "127.0.0.1", echo_port).unwrap();
    let b = socks_ask(port, None, "127.0.0.1", echo_port).unwrap();
    // the third is closed before its greeting is answered; nothing is opened for it
    assert_eq!(socks_ask(port, None, "127.0.0.1", echo_port).unwrap_err(), 0xFF);
    assert_eq!(up.dialed.load(Ordering::SeqCst), 2);
    // one ends: there is room again
    drop(a);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let c = loop {
        match socks_ask(port, None, "127.0.0.1", echo_port) {
            Ok(c) => break c,
            Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("no room after a connection ended ({e:#x})"),
        }
    };
    assert_eq!(round_trip(c), b"leaf");
    assert_eq!(round_trip(b), b"leaf");
}

#[test]
fn socks_idle_connections_are_closed() {
    use std::io::Read;
    test_runtime();
    let (echo_port, _) = counted_echo();
    let up = fake(true, vec![]);
    let (port, _stop) = socks::launch_limited(None, up, socks::Limits { connections: 8, idle: Duration::from_millis(300) }).unwrap();
    let s = socks_ask(port, None, "127.0.0.1", echo_port).unwrap();
    // busy: kept (each round trip is well within the idle time)
    let mut s = {
        let s = s;
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(round_trip(s.try_clone().unwrap()), b"leaf");
        }
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    };
    // quiet: closed by the proxy
    std::thread::sleep(Duration::from_millis(800));
    let mut b = [0u8; 1];
    assert!(matches!(s.read(&mut b), Ok(0) | Err(_)), "an idle connection was kept");
}

#[test]
fn a_pooled_direct_connection_is_not_reused_once_the_host_is_set_to_vpn() {
    // made while the host was not ticked: direct, and reusable while it stays so
    let kept = RouteMark::now("files.example.com", false);
    assert!(kept.reusable(&RouteMark::now("files.example.com", false)));
    // the tick is set: the kept direct connection is not used again
    let now = RouteMark::now("files.example.com", true);
    assert!(!kept.reusable(&now));
    // made for a VPN host on one route, then the VPN came up or went down: not used again either
    let before = RouteMark { vpn: true, tunnel: false, generation: 3 };
    assert!(!before.reusable(&RouteMark { vpn: true, tunnel: true, generation: 4 }));
    assert!(!before.reusable(&RouteMark { vpn: true, tunnel: false, generation: 4 }));
    assert!(before.reusable(&before));
}
