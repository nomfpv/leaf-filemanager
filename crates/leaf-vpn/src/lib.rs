//! leaf's VPN: only leaf's own connections (downloads, SFTP/FTP hosts that are set to use it) go through a WireGuard tunnel
//! that lives inside leaf. Nothing on the PC changes: no adapter, no driver, no admin rights, no other program's traffic.
//!
//! - [`Provider`] is the seam: it says which server to use and with which key ([`mullvad::Mullvad`] first; a plain WireGuard
//!   config file can be a second one later).
//! - [`connect`] / [`disconnect`] bring the one tunnel up and down; [`state`] (with [`changes`]) is what the title bar and
//!   the settings page show.
//! - [`dial`] (async) and [`dial_blocking`] are the only ways leaf's connections reach it: LAN addresses go direct, the
//!   rest through the tunnel when it is up; when it is down they go direct, unless the kill switch is on: then they are
//!   refused ([`is_blocked`] tells that error apart, so a transfer can wait instead of going out directly).
//! - The tunnel runs on leaf-remote's tokio runtime, given to [`init`]; nothing starts before the first use.

pub mod account;
mod http;
pub mod mullvad;
pub mod route;
pub mod tunnel;

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

pub use tunnel::{PrivateKey, TcpStream, Tunnel, TunnelConfig, TunnelError};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where the tunnel is.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Off,
    /// `place`: "Sweden, Gothenburg" once the server is chosen, empty before.
    Connecting { place: String },
    /// The handshake is done. `exit_ip` only when the provider's own check confirmed the traffic leaves through it; else
    /// `unverified` says why it could not be confirmed (the tunnel may still pass no traffic).
    On { country: String, city: String, server: String, exit_ip: Option<IpAddr>, unverified: Option<String> },
    /// leaf's sentence (the details after a line break).
    Failed(String),
}

/// What a provider hands over for one connection.
pub struct Prepared {
    pub config: TunnelConfig,
    pub country: String,
    pub city: String,
    /// The server's name (`se-got-wg-001`).
    pub server: String,
    /// How long the first handshake may take (longer right after a new key was registered: the servers learn it within a minute).
    pub handshake_wait: Duration,
}

/// Where the traffic left the tunnel, as the provider's own check saw it.
#[derive(Clone, Debug, Default)]
pub struct Exit {
    pub ip: Option<IpAddr>,
    pub country: Option<String>,
    pub city: Option<String>,
}

/// A VPN service (or a config file): which server, which key.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    /// The tunnel to open now (may ask the provider's API; runs on the runtime).
    fn prepare(&self) -> BoxFuture<'_, Result<Prepared, String>>;
    /// After the tunnel is up: where the traffic comes out, asked through the tunnel; `Err(why)` when that could not be
    /// confirmed.
    fn exit_check<'a>(&'a self, _tunnel: &'a Tunnel) -> BoxFuture<'a, Result<Exit, String>> {
        Box::pin(async { Err("this provider has no exit check".to_string()) })
    }
}

// ------------------------------------------------------------------------------------------------ the one tunnel

struct Manager {
    tunnel: Option<Arc<Tunnel>>,
    state: State,
    /// Each connect or disconnect: a connect that finishes after a newer one is thrown away.
    generation: u64,
}

static MANAGER: Mutex<Manager> = Mutex::new(Manager { tunnel: None, state: State::Off, generation: 0 });
static CHANGES: AtomicU64 = AtomicU64::new(0);
static KILL_SWITCH: AtomicBool = AtomicBool::new(false);
type RuntimeFn = Box<dyn Fn() -> tokio::runtime::Handle + Send + Sync>;
static RUNTIME: OnceLock<RuntimeFn> = OnceLock::new();

fn manager() -> std::sync::MutexGuard<'static, Manager> {
    MANAGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// The runtime the tunnel runs on (leaf-remote's): asked for on first use only.
pub fn init(runtime: impl Fn() -> tokio::runtime::Handle + Send + Sync + 'static) {
    let _ = RUNTIME.set(Box::new(runtime));
}

pub fn runtime() -> Option<tokio::runtime::Handle> {
    RUNTIME.get().map(|f| f())
}

/// Goes up on every change of [`state`].
pub fn changes() -> u64 {
    CHANGES.load(Ordering::Acquire)
}

pub fn state() -> State {
    manager().state.clone()
}

fn set_state(generation: u64, s: State) {
    let mut m = manager();
    if m.generation == generation && m.state != s {
        log::info!("vpn: {}", match &s {
            State::Off => "off".to_string(),
            State::Connecting { place } => format!("connecting {place}"),
            State::On { country, city, server, .. } => format!("on ({country}, {city}, {server})"),
            State::Failed(why) => format!("failed: {}", why.lines().next().unwrap_or_default()),
        });
        m.state = s;
        CHANGES.fetch_add(1, Ordering::AcqRel);
    }
}

/// The kill switch: while it is on and the tunnel is not up, connections set to use the VPN are refused.
pub fn set_kill_switch(on: bool) {
    KILL_SWITCH.store(on, Ordering::Release);
}

pub fn kill_switch() -> bool {
    KILL_SWITCH.load(Ordering::Acquire)
}

/// The tunnel, when it is up.
pub fn tunnel() -> Option<Arc<Tunnel>> {
    manager().tunnel.clone().filter(|t| t.is_alive())
}

pub fn is_on() -> bool {
    tunnel().is_some() && matches!(state(), State::On { .. })
}

/// Brings the tunnel up with `provider` (an open one is closed first). Returns at once; [`state`] tells how it goes.
pub fn connect(provider: Arc<dyn Provider>) {
    let Some(rt) = runtime() else {
        log::warn!("vpn: no runtime given to leaf_vpn::init");
        return;
    };
    let generation = {
        let mut m = manager();
        m.generation += 1;
        if let Some(t) = m.tunnel.take() {
            t.close();
        }
        m.generation
    };
    bump_route();
    set_state(generation, State::Connecting { place: String::new() });
    rt.spawn(run(provider, generation));
}

/// Waits between tries to bring a lost tunnel back.
const RECONNECT_BACKOFF: [u64; 6] = [2, 5, 10, 20, 30, 60];

/// One connection's life: up, then up again whenever it is lost (state `Connecting` meanwhile, so the kill switch holds what
/// is set to use it), until a newer connect or disconnect. `Failed` only when the provider refuses, or after the last try.
async fn run(provider: Arc<dyn Provider>, generation: u64) {
    let mut tries = 0usize;
    let mut was_up = false;
    loop {
        match up_once(&*provider, generation).await {
            Ok(Some(t)) => {
                was_up = true;
                tries = 0;
                // the tunnel ends by itself when the server stops answering its pings
                while t.is_alive() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if manager().generation != generation {
                        return;
                    }
                }
                {
                    let mut m = manager();
                    if m.generation != generation {
                        return;
                    }
                    m.tunnel = None;
                }
                bump_route();
                log::warn!("vpn: the connection was lost; connecting again");
                set_state(generation, State::Connecting { place: String::new() });
            }
            // a newer connect or disconnect took over
            Ok(None) => return,
            Err(Refusal::Provider(why)) => return set_state(generation, State::Failed(why)),
            Err(Refusal::Tunnel(why)) => {
                // (a first connect that never came up says so at once; a lost one is tried again for a while)
                if !was_up || tries >= RECONNECT_BACKOFF.len() {
                    let why = if was_up { format!("The VPN connection was lost and did not come back.\n{why}") } else { why };
                    return set_state(generation, State::Failed(why));
                }
                let wait = RECONNECT_BACKOFF[tries];
                tries += 1;
                log::info!("vpn: try {tries} failed, the next in {wait} s");
                set_state(generation, State::Connecting { place: String::new() });
                tokio::time::sleep(Duration::from_secs(wait)).await;
                if manager().generation != generation {
                    return;
                }
            }
        }
    }
}

enum Refusal {
    /// The provider said no (account, place): trying again does not help.
    Provider(String),
    /// The tunnel did not come up (no answer): worth another try.
    Tunnel(String),
}

/// Brings the tunnel up once; `Ok(None)` when a newer connect or disconnect came meanwhile.
async fn up_once(provider: &dyn Provider, generation: u64) -> Result<Option<Arc<Tunnel>>, Refusal> {
    let prepared = provider.prepare().await.map_err(Refusal::Provider)?;
    set_state(generation, State::Connecting { place: format!("{}, {}", prepared.country, prepared.city) });
    let Prepared { config, country, city, server, handshake_wait } = prepared;
    let t = Arc::new(Tunnel::start(config, handshake_wait).await.map_err(|e| Refusal::Tunnel(e.to_string()))?);
    {
        let mut m = manager();
        if m.generation != generation {
            t.close();
            return Ok(None);
        }
        m.tunnel = Some(t.clone());
    }
    bump_route();
    // (the address shown is the one the check confirmed, never the server's entry address standing in for it)
    let state = match provider.exit_check(&t).await {
        Ok(exit) => State::On { country: exit.country.unwrap_or(country), city: exit.city.unwrap_or(city), server, exit_ip: exit.ip, unverified: None },
        Err(why) => {
            log::warn!("vpn: the exit could not be confirmed: {why}");
            State::On { country, city, server, exit_ip: None, unverified: Some(why) }
        }
    };
    set_state(generation, state);
    Ok(Some(t))
}

static ROUTE_GEN: AtomicU64 = AtomicU64::new(0);

/// Goes up whenever the tunnel comes up or goes down: a connection opened before (direct or through it) no longer is on the
/// route leaf would take now.
pub fn generation() -> u64 {
    ROUTE_GEN.load(Ordering::Acquire)
}

fn bump_route() {
    ROUTE_GEN.fetch_add(1, Ordering::AcqRel);
}

pub fn disconnect() {
    let generation = {
        let mut m = manager();
        m.generation += 1;
        if let Some(t) = m.tunnel.take() {
            t.close();
        }
        m.generation
    };
    bump_route();
    set_state(generation, State::Off);
}

// ------------------------------------------------------------------------------------------------ leaf's connections

/// The error a connection gets when the kill switch holds it.
#[derive(Debug)]
pub struct Blocked;

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("leaf's VPN is not connected (yet, or the kill switch is on), so this connection waits instead of going out directly")
    }
}

impl std::error::Error for Blocked {}

fn blocked() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, Blocked)
}

/// Whether `e` (or an error it wraps) is the kill switch's refusal.
pub fn is_blocked(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if err.is::<Blocked>() {
            return true;
        }
        if let Some(io) = err.downcast_ref::<io::Error>()
            && io.get_ref().is_some_and(|inner| inner.is::<Blocked>())
        {
            return true;
        }
        cur = err.source();
    }
    false
}

/// How a connection to `host` that is set to use the VPN goes right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Tunnel,
    Direct,
    /// The kill switch holds it.
    Blocked,
}

pub fn route(host: &str) -> Route {
    let up = tunnel().is_some();
    route_with(host, up, kill_switch(), !up && matches!(state(), State::Connecting { .. }))
}

/// `connecting`: the user asked for the VPN and it is on its way (at start, a new place, a reconnect): what is set to use it
/// waits those seconds whatever the kill switch says; without the kill switch it goes direct only when the VPN is off or failed.
fn route_with(host: &str, up: bool, kill: bool, connecting: bool) -> Route {
    if route::is_lan_host(host) {
        Route::Direct
    } else if up {
        Route::Tunnel
    } else if kill || connecting {
        Route::Blocked
    } else {
        Route::Direct
    }
}

/// The VPN is about to be connected (at start, before the account is read): from now on [`route`] holds what is set to use it,
/// as while connecting. [`connect`] or [`disconnect`] follows.
pub fn hold() {
    let generation = manager().generation;
    set_state(generation, State::Connecting { place: String::new() });
}

/// A connection opened by [`dial`]: through the tunnel, or direct (LAN, or the VPN off without the kill switch).
#[derive(Debug)]
pub enum Stream {
    Tunnel(TcpStream),
    Direct(tokio::net::TcpStream),
}

impl Stream {
    pub fn through_tunnel(&self) -> bool {
        matches!(self, Stream::Tunnel(_))
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tunnel(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Direct(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Tunnel(s) => Pin::new(s).poll_write(cx, data),
            Stream::Direct(s) => Pin::new(s).poll_write(cx, data),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tunnel(s) => Pin::new(s).poll_flush(cx),
            Stream::Direct(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tunnel(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Direct(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

async fn direct(host: &str, port: u16, timeout: Duration) -> io::Result<tokio::net::TcpStream> {
    let work = async {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.trim_start_matches('[').trim_end_matches(']'), port)).await?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "the server's name has no address"));
        }
        tokio::net::TcpStream::connect(&addrs[..]).await
    };
    let s = tokio::time::timeout(timeout, work).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no answer"))??;
    let _ = s.set_nodelay(true);
    Ok(s)
}

/// A TCP connection to `host:port` for something set to use the VPN (see [`route`]). Inside the tunnel the name is looked
/// up through it (the PC's resolver never sees it); a name that turns out to be a LAN address is connected directly.
pub async fn dial(host: &str, port: u16, timeout: Duration) -> io::Result<Stream> {
    match route(host) {
        Route::Blocked => Err(blocked()),
        Route::Direct => direct(host, port, timeout).await.map(Stream::Direct),
        Route::Tunnel => {
            let Some(t) = tunnel() else {
                return if kill_switch() { Err(blocked()) } else { direct(host, port, timeout).await.map(Stream::Direct) };
            };
            let name = host.trim_start_matches('[').trim_end_matches(']');
            let ips = match route::inet_aton(name) {
                Some(ip) => vec![ip],
                None => t.resolve(name).await?,
            };
            // a public name whose answer is this PC or the local network (0.0.0.0 is this PC on Linux; DNS rebinding): refused,
            // never connected directly (a name that looks local went direct before it got here)
            if ips.iter().any(|ip| ip.is_unspecified() || route::is_lan_ip(IpAddr::V4(*ip))) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the name points to this PC or the local network; leaf does not connect to it through the VPN"));
            }
            let mut last = io::Error::new(io::ErrorKind::NotFound, "the server's name has no address");
            for ip in ips {
                match t.connect(ip, port, timeout).await {
                    Ok(s) => return Ok(Stream::Tunnel(s)),
                    Err(e) => last = e,
                }
            }
            Err(last)
        }
    }
}

/// [`dial`] for a thread that is not on the runtime (leaf-download's connections, FTP): reads and writes block, with the
/// timeouts set on it.
pub fn dial_blocking(host: &str, port: u16, timeout: Duration) -> io::Result<BlockingStream> {
    // the kill switch decides before anything starts (no runtime is needed to refuse)
    if route(host) == Route::Blocked {
        return Err(blocked());
    }
    let rt = runtime().ok_or_else(|| io::Error::other("leaf's VPN is not set up in this copy of leaf"))?;
    let stream = rt.block_on(dial(host, port, timeout))?;
    Ok(BlockingStream { stream, rt, read_timeout: None, write_timeout: None })
}

/// [`dial`] for a client that takes only a plain socket of its own (the FTP client): a listener on 127.0.0.1 that takes ONE
/// connection within 10 s (from this PC only) and joins it to `host:port` through [`dial`]. The kill switch refuses here, before
/// anything is opened. The address to connect to is returned.
pub fn loopback_bridge(host: &str, port: u16, timeout: Duration) -> io::Result<SocketAddr> {
    if route(host) == Route::Blocked {
        return Err(blocked());
    }
    let rt = runtime().ok_or_else(|| io::Error::other("leaf's VPN is not set up in this copy of leaf"))?;
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let host = host.to_string();
    rt.spawn(async move {
        let Ok(l) = tokio::net::TcpListener::from_std(listener) else { return };
        let Ok(Ok((mut local, peer))) = tokio::time::timeout(Duration::from_secs(10), l.accept()).await else { return };
        drop(l);
        if !peer.ip().is_loopback() {
            return;
        }
        match dial(&host, port, timeout).await {
            Ok(mut remote) => {
                let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
            }
            Err(e) => log::info!("vpn: the bridge to port {port} could not connect: {e}"),
        }
    });
    Ok(addr)
}

/// A [`Stream`] with blocking `Read` and `Write`.
#[derive(Debug)]
pub struct BlockingStream {
    stream: Stream,
    rt: tokio::runtime::Handle,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl BlockingStream {
    pub fn set_read_timeout(&mut self, t: Option<Duration>) {
        self.read_timeout = t.filter(|d| !d.is_zero());
    }
    pub fn set_write_timeout(&mut self, t: Option<Duration>) {
        self.write_timeout = t.filter(|d| !d.is_zero());
    }
    pub fn through_tunnel(&self) -> bool {
        self.stream.through_tunnel()
    }
}

impl io::Read for BlockingStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let t = self.read_timeout;
        let rt = self.rt.clone();
        let s = &mut self.stream;
        rt.block_on(async move {
            match t {
                Some(t) => tokio::time::timeout(t, s.read(buf)).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no data in time"))?,
                None => s.read(buf).await,
            }
        })
    }
}

impl io::Write for BlockingStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let t = self.write_timeout;
        let rt = self.rt.clone();
        let s = &mut self.stream;
        rt.block_on(async move {
            match t {
                Some(t) => tokio::time::timeout(t, s.write(buf)).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the data could not be sent in time"))?,
                None => s.write(buf).await,
            }
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        // (a write is handed to the stack whole; the driver sends it)
        Ok(())
    }
}

#[cfg(test)]
mod tests;
