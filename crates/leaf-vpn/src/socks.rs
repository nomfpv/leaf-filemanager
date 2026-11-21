//! "Let other programs use the VPN": a SOCKS5 proxy on 127.0.0.1 (never another address) whose every connection goes through
//! leaf's tunnel. The browser (the leaf extension's "Browse through Mullvad") and any program with a SOCKS5 setting can use it.
//!
//! - CONNECT only, IPv4 addresses and names. A name is looked up THROUGH the tunnel (Mullvad's resolver); the PC's resolver never
//!   sees it. IPv6 addresses, BIND and UDP ASSOCIATE are refused.
//! - While the tunnel is down, connecting or reconnecting, every request is refused: nothing is ever connected directly, whatever
//!   the kill switch says (a program set to this proxy expects the VPN or nothing).
//! - Addresses of this PC or the local network are refused through it (the browser's bypass list keeps those direct itself).
//! - Optional user name and password (RFC 1929) so other local users or processes cannot use it.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::BoxFuture;

/// The port asked for by default (the usual SOCKS port); a free one is taken when it is in use.
pub const DEFAULT_PORT: u16 = 1080;
/// The user name when a password is asked (the password is what keeps others out).
pub const USER: &str = "leaf";
/// How long a client may take to say what it wants.
const HANDSHAKE: Duration = Duration::from_secs(15);
/// How long a connection through the tunnel may take to open.
const CONNECT: Duration = Duration::from_secs(20);

/// The proxy's user name and password. `Debug` shows the user only.
#[derive(Clone)]
pub struct Credentials {
    pub user: String,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Credentials({}, password hidden)", self.user)
    }
}

impl Credentials {
    /// [`USER`] and a new random password (24 letters and digits from the OS's random source).
    pub fn generate() -> Credentials {
        const SET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
        let mut raw = Zeroizing::new([0u8; 24]);
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut *raw);
        let password: String = raw.iter().map(|b| SET[*b as usize % SET.len()] as char).collect();
        Credentials { user: USER.to_string(), password: Zeroizing::new(password) }
    }

    fn matches(&self, user: &[u8], password: &[u8]) -> bool {
        // (every byte is compared, so the time taken does not tell how much of a guess was right)
        let eq = |a: &[u8], b: &[u8]| a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
        let u = eq(user, self.user.as_bytes());
        let p = eq(password, self.password.as_bytes());
        u & p
    }
}

/// Whether the proxy asks for a password unless the user says otherwise: on a PC where several people sign in (Linux with more
/// than one user account), so the others cannot use this user's VPN; elsewhere not.
pub fn password_by_default() -> bool {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
        let people = text
            .lines()
            .filter(|l| {
                let f: Vec<&str> = l.split(':').collect();
                let uid = f.get(2).and_then(|u| u.parse::<u32>().ok()).unwrap_or(0);
                let shell = f.get(6).copied().unwrap_or("");
                (1000..60000).contains(&uid) && !shell.ends_with("nologin") && !shell.ends_with("false")
            })
            .count();
        people > 1
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

// ------------------------------------------------------------------------------------------------ where connections go

/// A connection out of the proxy.
pub(crate) trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// Where the proxy sends its connections: the tunnel ([`Tunneled`]); the tests give their own.
pub(crate) trait Upstream: Send + Sync + 'static {
    /// The tunnel is up (else every request is refused before anything is looked up or opened).
    fn ready(&self) -> bool;
    /// A name's IPv4 addresses, asked through the tunnel.
    fn resolve<'a>(&'a self, name: &'a str) -> BoxFuture<'a, io::Result<Vec<Ipv4Addr>>>;
    /// A connection to `ip:port` through the tunnel.
    fn connect(&self, ip: Ipv4Addr, port: u16) -> BoxFuture<'_, io::Result<Box<dyn Duplex>>>;
}

/// leaf's tunnel, as it is at each request.
pub(crate) struct Tunneled;

impl Upstream for Tunneled {
    fn ready(&self) -> bool {
        crate::tunnel().is_some()
    }
    fn resolve<'a>(&'a self, name: &'a str) -> BoxFuture<'a, io::Result<Vec<Ipv4Addr>>> {
        Box::pin(async move {
            let t = crate::tunnel().ok_or_else(crate::blocked)?;
            t.resolve(name).await
        })
    }
    fn connect(&self, ip: Ipv4Addr, port: u16) -> BoxFuture<'_, io::Result<Box<dyn Duplex>>> {
        Box::pin(async move {
            // (this PC or the local network through the VPN: refused, never connected directly)
            if ip.is_unspecified() || crate::route::is_lan_ip(std::net::IpAddr::V4(ip)) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "a local address"));
            }
            let t = crate::tunnel().ok_or_else(crate::blocked)?;
            let s = t.connect(ip, port, CONNECT).await?;
            Ok(Box::new(s) as Box<dyn Duplex>)
        })
    }
}

// ------------------------------------------------------------------------------------------------ the one proxy

struct Running {
    port: u16,
    stop: watch::Sender<bool>,
}

static PROXY: Mutex<Option<Running>> = Mutex::new(None);

fn proxy() -> std::sync::MutexGuard<'static, Option<Running>> {
    PROXY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the proxy on 127.0.0.1:`port` (a free port when that one cannot be had), with `credentials` asked when given. A proxy
/// already running is stopped first. Returns the port it listens on.
pub fn start(port: u16, credentials: Option<Credentials>) -> io::Result<u16> {
    stop();
    let (port, stop_tx) = launch(port, credentials, Arc::new(Tunneled))?;
    *proxy() = Some(Running { port, stop: stop_tx });
    log::info!("vpn: the SOCKS proxy listens on 127.0.0.1:{port}");
    Ok(port)
}

/// Stops the proxy and ends the connections through it.
pub fn stop() {
    if let Some(r) = proxy().take() {
        let _ = r.stop.send(true);
        log::info!("vpn: the SOCKS proxy stopped");
    }
}

/// The port the proxy listens on, while it runs.
pub fn port() -> Option<u16> {
    proxy().as_ref().map(|r| r.port)
}

/// Binds 127.0.0.1:`port` (or a free port) and serves it on the runtime with `up`. The sender stops it.
pub(crate) fn launch<U: Upstream>(port: u16, credentials: Option<Credentials>, up: Arc<U>) -> io::Result<(u16, watch::Sender<bool>)> {
    let rt = crate::runtime().ok_or_else(|| io::Error::other("leaf's VPN is not set up in this copy of leaf"))?;
    let listener = match std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => l,
        // (in use, or a port Windows keeps for itself: any free one; the page shows which)
        Err(e) if port != 0 => {
            log::info!("vpn: port {port} for the SOCKS proxy cannot be had ({e}); a free one is used");
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        }
        Err(e) => return Err(e),
    };
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = watch::channel(false);
    let credentials = credentials.map(Arc::new);
    rt.spawn(async move {
        let l = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => return log::warn!("vpn: the SOCKS proxy could not start: {e}"),
        };
        serve(l, credentials, up, rx).await;
    });
    Ok((port, tx))
}

async fn serve<U: Upstream>(l: tokio::net::TcpListener, credentials: Option<Arc<Credentials>>, up: Arc<U>, mut stop: watch::Receiver<bool>) {
    loop {
        let (s, peer) = tokio::select! {
            r = l.accept() => match r {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("vpn: socks accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = stop.changed() => return,
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let _ = s.set_nodelay(true);
        let (credentials, up, mut stop) = (credentials.clone(), up.clone(), stop.clone());
        tokio::spawn(async move {
            let opened = tokio::time::timeout(HANDSHAKE + CONNECT, negotiate(s, credentials.as_deref(), &*up)).await;
            let Ok(Some((mut client, mut remote))) = opened else { return };
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut client, &mut remote) => {}
                _ = stop.changed() => {}
            }
        });
    }
}

/// SOCKS5's reply codes used here.
const REP_OK: u8 = 0x00;
const REP_FAILURE: u8 = 0x01;
const REP_NOT_ALLOWED: u8 = 0x02;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_REFUSED: u8 = 0x05;
const REP_COMMAND: u8 = 0x07;
const REP_ADDRESS_TYPE: u8 = 0x08;

async fn reply(s: &mut tokio::net::TcpStream, rep: u8) {
    let _ = s.write_all(&[5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await;
    let _ = s.flush().await;
}

fn rep_of(e: &io::Error) -> u8 {
    if crate::is_blocked(e) || e.kind() == io::ErrorKind::PermissionDenied {
        REP_NOT_ALLOWED
    } else if e.kind() == io::ErrorKind::ConnectionRefused {
        REP_REFUSED
    } else if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::NotFound) {
        REP_HOST_UNREACHABLE
    } else {
        REP_FAILURE
    }
}

/// One client's greeting, sign-in and request; the open connection when it all went well (the client has its success reply).
async fn negotiate(mut s: tokio::net::TcpStream, credentials: Option<&Credentials>, up: &dyn Upstream) -> Option<(tokio::net::TcpStream, Box<dyn Duplex>)> {
    let shake = async {
        let mut head = [0u8; 2];
        s.read_exact(&mut head).await.ok()?;
        if head[0] != 5 {
            return None;
        }
        let mut methods = vec![0u8; head[1] as usize];
        s.read_exact(&mut methods).await.ok()?;
        let want = if credentials.is_some() { 0x02 } else { 0x00 };
        if !methods.contains(&want) {
            let _ = s.write_all(&[5, 0xff]).await;
            return None;
        }
        s.write_all(&[5, want]).await.ok()?;
        if let Some(c) = credentials {
            // RFC 1929: version 1, the user name, the password
            let mut v = [0u8; 2];
            s.read_exact(&mut v).await.ok()?;
            let mut user = Zeroizing::new(vec![0u8; v[1] as usize]);
            s.read_exact(&mut user).await.ok()?;
            let mut n = [0u8; 1];
            s.read_exact(&mut n).await.ok()?;
            let mut password = Zeroizing::new(vec![0u8; n[0] as usize]);
            s.read_exact(&mut password).await.ok()?;
            if v[0] != 1 || !c.matches(&user, &password) {
                let _ = s.write_all(&[1, 1]).await;
                log::info!("vpn: a SOCKS client gave a wrong user name or password");
                return None;
            }
            s.write_all(&[1, 0]).await.ok()?;
        }
        let mut req = [0u8; 4];
        s.read_exact(&mut req).await.ok()?;
        if req[0] != 5 {
            return None;
        }
        if req[1] != 1 {
            // BIND, UDP ASSOCIATE
            reply(&mut s, REP_COMMAND).await;
            return None;
        }
        let target = match req[3] {
            1 => {
                let mut a = [0u8; 4];
                s.read_exact(&mut a).await.ok()?;
                Target::Ip(Ipv4Addr::from(a))
            }
            3 => {
                let mut n = [0u8; 1];
                s.read_exact(&mut n).await.ok()?;
                let mut name = vec![0u8; n[0] as usize];
                s.read_exact(&mut name).await.ok()?;
                let name = String::from_utf8(name).ok().filter(|n| !n.is_empty())?;
                match name.parse::<Ipv4Addr>() {
                    Ok(ip) => Target::Ip(ip),
                    Err(_) => Target::Name(name),
                }
            }
            _ => {
                reply(&mut s, REP_ADDRESS_TYPE).await;
                return None;
            }
        };
        let mut p = [0u8; 2];
        s.read_exact(&mut p).await.ok()?;
        Some((target, u16::from_be_bytes(p)))
    };
    let (target, port) = tokio::time::timeout(HANDSHAKE, shake).await.ok()??;
    // the tunnel is not up: refused before anything is looked up or opened (never direct)
    if !up.ready() {
        reply(&mut s, REP_NOT_ALLOWED).await;
        return None;
    }
    let ips = match target {
        Target::Ip(ip) => vec![ip],
        Target::Name(name) => match up.resolve(&name).await {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => {
                reply(&mut s, REP_HOST_UNREACHABLE).await;
                return None;
            }
            Err(e) => {
                reply(&mut s, rep_of(&e)).await;
                return None;
            }
        },
    };
    let mut last = REP_HOST_UNREACHABLE;
    for ip in ips {
        match up.connect(ip, port).await {
            Ok(remote) => {
                reply(&mut s, REP_OK).await;
                return Some((s, remote));
            }
            Err(e) => last = rep_of(&e),
        }
    }
    reply(&mut s, last).await;
    None
}

enum Target {
    Ip(Ipv4Addr),
    Name(String),
}

/// The address to give other programs: `127.0.0.1:port`.
pub fn address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}
