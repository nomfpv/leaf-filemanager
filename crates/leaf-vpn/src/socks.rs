//! "Let other programs use the VPN": a SOCKS5 proxy on 127.0.0.1 (never another address) whose every connection goes through
//! leaf's tunnel. The browser (the leaf extension's "Browse through Mullvad") and any program with a SOCKS5 setting can use it.
//!
//! - CONNECT only, IPv4 addresses and names. A name is looked up THROUGH the tunnel (Mullvad's resolver); the PC's resolver never
//!   sees it. IPv6 addresses, BIND and UDP ASSOCIATE are refused.
//! - While the tunnel is down, connecting or reconnecting, every request is refused: nothing is ever connected directly, whatever
//!   the kill switch says (a program set to this proxy expects the VPN or nothing).
//! - Addresses of this PC or the local network are refused through it (the browser's bypass list keeps those direct itself).
//! - Optional user name and password (RFC 1929) so other local users or processes cannot use it.
//! - At most 256 connections at once (more are closed at once); one with no byte either way for 10 minutes is closed.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
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
/// Connections kept at once (a browser opens a few dozen; each one holds a socket and its buffers in the tunnel's stack, so a
/// local program must not be able to open thousands and starve leaf's own downloads).
const MAX_CONNECTIONS: usize = 256;
/// A connection with no byte either way for this long is closed (a browser's idle kept connections are shorter).
const IDLE: Duration = Duration::from_secs(600);

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

/// Whether the proxy asks for a password unless the user says otherwise: on a PC where several people sign in (127.0.0.1 is shared
/// by every signed-in session: fast user switching, remote desktop), so the others cannot use this user's VPN; elsewhere not.
/// Linux: more than one user account with a login shell. Windows: more than one user profile (a profile folder with its
/// `NTUSER.DAT`; every user who has signed in has one, so this also covers several users signed in now).
pub fn password_by_default() -> bool {
    #[cfg(windows)]
    {
        // (the folder that holds the profiles: the parent of this user's)
        let Some(root) = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from).and_then(|p| p.parent().map(|p| p.to_path_buf())) else {
            return false;
        };
        let people = std::fs::read_dir(&root)
            .map(|d| {
                d.flatten()
                    .filter(|e| {
                        let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                        let system = matches!(name.as_str(), "default" | "default user" | "public" | "all users") || name.starts_with("defaultuser");
                        !system && e.path().join("NTUSER.DAT").is_file()
                    })
                    .count()
            })
            .unwrap_or(0);
        people > 1
    }
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
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
}

// ------------------------------------------------------------------------------------------------ where connections go

/// A connection out of the proxy.
pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
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

// ------------------------------------------------------------------------------------------------ a proxy of leaf's own

/// The address a [`Private`] proxy's own name stands for (reserved, never routed: no server can have it).
const OWN_IP: Ipv4Addr = Ipv4Addr::new(240, 0, 0, 1);

/// What answers a [`Private`] proxy's own name: a connection that ends inside leaf (given the port asked for).
pub type Own = Arc<dyn Fn(u16) -> Box<dyn Duplex> + Send + Sync>;

/// The tunnel, and one name that is answered inside leaf instead (only while the tunnel is up, as everything else).
struct TunneledWithOwn {
    name: String,
    own: Own,
}

impl Upstream for TunneledWithOwn {
    fn ready(&self) -> bool {
        Tunneled.ready()
    }
    fn resolve<'a>(&'a self, name: &'a str) -> BoxFuture<'a, io::Result<Vec<Ipv4Addr>>> {
        if name.eq_ignore_ascii_case(&self.name) {
            return Box::pin(async { Ok(vec![OWN_IP]) });
        }
        Tunneled.resolve(name)
    }
    fn connect(&self, ip: Ipv4Addr, port: u16) -> BoxFuture<'_, io::Result<Box<dyn Duplex>>> {
        if ip == OWN_IP {
            return Box::pin(async move { Ok((self.own)(port)) });
        }
        Box::pin(async move { Tunneled.connect(ip, port).await })
    }
}

/// A SOCKS5 proxy of its own for one part of leaf (the torrent engine, whose library takes a proxy and nothing else): on
/// 127.0.0.1 with a free port and a random password, so no other program can use it. The same rule as the user's proxy:
/// the tunnel or nothing, never direct, never this PC or the local network. Dropping it stops it and its connections.
pub struct Private {
    pub port: u16,
    pub credentials: Credentials,
    stop: watch::Sender<bool>,
}

impl std::fmt::Debug for Private {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "socks::Private(127.0.0.1:{})", self.port)
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Starts a [`Private`] proxy on `rt` keeping at most `connections` at once. `own`: a name the proxy answers inside leaf
/// instead of looking it up (the torrent engine's bridge to UDP trackers).
pub fn start_private(rt: &tokio::runtime::Handle, connections: usize, own: Option<(String, Own)>) -> io::Result<Private> {
    let credentials = Credentials::generate();
    let limits = Limits { connections, idle: IDLE };
    let (port, stop) = match own {
        Some((name, own)) => launch_on(rt, 0, None, Some(credentials.clone()), Arc::new(TunneledWithOwn { name, own }), limits)?,
        None => launch_on(rt, 0, None, Some(credentials.clone()), Arc::new(Tunneled), limits)?,
    };
    Ok(Private { port, credentials, stop })
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
    // (a restart, for a new password: the port it had is wanted again, and it is free once the old listener has gone, a moment
    // after it was told to stop; so programs set to it need no new address)
    let before = proxy().as_ref().map(|r| r.port);
    stop();
    let again = before.filter(|b| *b != 0);
    let (port, stop_tx) = launch_with(port, again, credentials, Arc::new(Tunneled), Limits::default())?;
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

/// Binds 127.0.0.1:`port` (or a free port) and serves it on the runtime with `up` (the tests' stand-in for the tunnel). The
/// sender stops it.
#[cfg(test)]
pub(crate) fn launch<U: Upstream>(port: u16, credentials: Option<Credentials>, up: Arc<U>) -> io::Result<(u16, watch::Sender<bool>)> {
    launch_with(port, None, credentials, up, Limits::default())
}

/// [`launch`] with other limits (the tests' small ones).
#[cfg(test)]
pub(crate) fn launch_limited<U: Upstream>(credentials: Option<Credentials>, up: Arc<U>, limits: Limits) -> io::Result<(u16, watch::Sender<bool>)> {
    launch_with(0, None, credentials, up, limits)
}

/// How many connections the proxy keeps at once, and after how long without a byte either way one is closed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub connections: usize,
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { connections: MAX_CONNECTIONS, idle: IDLE }
    }
}

/// Binds `port`; else `again` (the port the proxy just stopped had: tried for up to a second while the old listener goes); else
/// any free port.
fn bind(port: u16, again: Option<u16>) -> io::Result<std::net::TcpListener> {
    let try_bind = |p: u16| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, p));
    let mut first = None;
    for (i, p) in std::iter::once(port).chain(again).enumerate() {
        if p == 0 {
            continue;
        }
        let deadline = std::time::Instant::now() + if Some(p) == again { Duration::from_secs(1) } else { Duration::ZERO };
        loop {
            match try_bind(p) {
                Ok(l) => return Ok(l),
                Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => {
                    if i == 0 {
                        first = Some(e);
                    }
                    break;
                }
            }
        }
    }
    // (in use, or a port Windows keeps for itself: any free one; the page shows which)
    if let Some(e) = first {
        log::info!("vpn: port {port} for the SOCKS proxy cannot be had ({e}); another one is used");
    }
    try_bind(0)
}

fn launch_with<U: Upstream>(port: u16, again: Option<u16>, credentials: Option<Credentials>, up: Arc<U>, limits: Limits) -> io::Result<(u16, watch::Sender<bool>)> {
    let rt = crate::runtime().ok_or_else(|| io::Error::other("leaf's VPN is not set up in this copy of leaf"))?;
    launch_on(&rt, port, again, credentials, up, limits)
}

fn launch_on<U: Upstream>(rt: &tokio::runtime::Handle, port: u16, again: Option<u16>, credentials: Option<Credentials>, up: Arc<U>, limits: Limits) -> io::Result<(u16, watch::Sender<bool>)> {
    let listener = bind(port, again)?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = watch::channel(false);
    let credentials = credentials.map(Arc::new);
    rt.spawn(async move {
        let l = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => return log::warn!("vpn: the SOCKS proxy could not start: {e}"),
        };
        serve(l, credentials, up, limits, rx).await;
    });
    Ok((port, tx))
}

async fn serve<U: Upstream>(l: tokio::net::TcpListener, credentials: Option<Arc<Credentials>>, up: Arc<U>, limits: Limits, mut stop: watch::Receiver<bool>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(limits.connections));
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
        // (beyond the limit a connection is closed at once: each one holds a socket and buffers in the tunnel's stack)
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            log::debug!("vpn: a SOCKS connection over the limit of {} was closed", limits.connections);
            drop(s);
            continue;
        };
        let _ = s.set_nodelay(true);
        let (credentials, up, mut stop, idle) = (credentials.clone(), up.clone(), stop.clone(), limits.idle);
        tokio::spawn(async move {
            let _permit = permit;
            let opened = tokio::time::timeout(HANDSHAKE + CONNECT, negotiate(s, credentials.as_deref(), &*up)).await;
            let Ok(Some((client, mut remote))) = opened else { return };
            let mut client = Active::new(client);
            let last = client.last.clone();
            let started = client.started;
            // nothing in either direction for `idle`: closed
            let watch = async {
                loop {
                    let quiet = started.elapsed().saturating_sub(Duration::from_millis(last.load(Ordering::Relaxed)));
                    if quiet >= idle {
                        return;
                    }
                    tokio::time::sleep(idle - quiet).await;
                }
            };
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut client, &mut remote) => {}
                _ = watch => log::debug!("vpn: an idle SOCKS connection was closed"),
                _ = stop.changed() => {}
            }
        });
    }
}

/// The client's side of a connection, noting when bytes last went either way (what comes from the far end is written to it).
struct Active<S> {
    inner: S,
    started: std::time::Instant,
    /// Milliseconds after `started`.
    last: Arc<AtomicU64>,
}

impl<S> Active<S> {
    fn new(inner: S) -> Active<S> {
        Active { inner, started: std::time::Instant::now(), last: Arc::new(AtomicU64::new(0)) }
    }
    fn touch(&self) {
        self.last.store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Active<S> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let r = Pin::new(&mut this.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            this.touch();
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Active<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_write(cx, data);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            this.touch();
        }
        r
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
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
