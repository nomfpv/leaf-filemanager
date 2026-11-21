//! The tunnel: WireGuard (`boringtun`'s sans-IO `Tunn`) over one UDP socket, and a TCP/IP stack (`smoltcp`) on the inside
//! that gives leaf TCP streams and DNS lookups through it. Nothing on the PC changes: no adapter, no route, no admin; only the
//! connections leaf opens through [`Tunnel::connect`] go inside.
//!
//! The design (UDP <-> Tunn <-> a virtual IP device <-> smoltcp sockets, one driver task) follows `onetun`
//! (<https://github.com/aramperes/onetun>, MIT License, Copyright (c) 2021 Aram Peres); this is leaf's own code for it.
//!
//! One driver task per tunnel runs on the runtime it was started on (leaf-remote's). The streams share the stack with it
//! behind one short lock (never held across an await); a stream that writes wakes the driver, the driver wakes a stream
//! through the socket's waker.

use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use boringtun::noise::{Tunn, TunnResult, errors::WireGuardError};
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dns, tcp};
use smoltcp::wire::{DnsQueryType, HardwareAddress, IpAddress, IpCidr};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UdpSocket;
use tokio::sync::Notify;
use zeroize::Zeroizing;

/// The inner MTU: Mullvad's relays take 1420; a little less leaves room for PPPoE and mobile links on the way.
const MTU: usize = 1380;
/// Per TCP stream (a download segment fills a window of this).
const RX_BUF: usize = 512 << 10;
const TX_BUF: usize = 128 << 10;
/// How long the first handshake may take.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);
/// No handshake for this long: the server is gone (WireGuard renews every 2 minutes while traffic flows; keepalives every 25 s).
const DEAD_AFTER: Duration = Duration::from_secs(190);

/// A WireGuard private key. Its `Debug` shows nothing of it; it is wiped when dropped.
#[derive(Clone)]
pub struct PrivateKey(Zeroizing<[u8; 32]>);

impl PrivateKey {
    /// A new random key (the OS's random source).
    pub fn generate() -> PrivateKey {
        let secret = StaticSecret::random_from_rng(rand_core::OsRng);
        PrivateKey(Zeroizing::new(secret.to_bytes()))
    }
    pub fn from_bytes(b: [u8; 32]) -> PrivateKey {
        PrivateKey(Zeroizing::new(b))
    }
    /// From the base64 text WireGuard uses (`wg genkey`); `None` for anything else.
    pub fn from_base64(s: &str) -> Option<PrivateKey> {
        let raw = Zeroizing::new(base64_decode(s.trim())?);
        let b: [u8; 32] = raw.as_slice().try_into().ok()?;
        Some(PrivateKey::from_bytes(b))
    }
    /// The base64 text (for the keyring only).
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(base64_encode(&*self.0))
    }
    /// The public half, base64 (what a server is told).
    pub fn public_base64(&self) -> String {
        base64_encode(PublicKey::from(&StaticSecret::from(*self.0)).as_bytes())
    }
    fn secret(&self) -> StaticSecret {
        StaticSecret::from(*self.0)
    }
    pub(crate) fn secret_for_probe(&self) -> StaticSecret {
        self.secret()
    }
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateKey(hidden)")
    }
}

pub(crate) fn base64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// What a tunnel needs: our key and address inside, the server's key and address outside, the resolver inside.
#[derive(Clone)]
pub struct TunnelConfig {
    pub private_key: PrivateKey,
    pub peer_public: [u8; 32],
    pub endpoint: SocketAddr,
    /// Our address inside the tunnel (Mullvad gives one /32 per device).
    pub address: Ipv4Addr,
    /// The resolver asked through the tunnel.
    pub dns: Ipv4Addr,
}

impl std::fmt::Debug for TunnelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelConfig").field("endpoint", &self.endpoint).field("address", &self.address).field("dns", &self.dns).finish_non_exhaustive()
    }
}

/// Why a tunnel did not come up. The text is leaf's sentence; the system's own words come after it.
#[derive(Debug)]
pub enum TunnelError {
    /// The UDP socket could not be opened or pointed at the server.
    Socket(io::Error),
    /// No handshake answer in time (server down, UDP blocked, or the key is not registered there).
    NoAnswer,
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TunnelError::Socket(e) => write!(f, "The connection to the VPN server could not be opened ({e})"),
            TunnelError::NoAnswer => f.write_str("The VPN server did not answer. UDP may be blocked on this network, or this device's key is not known to the server."),
        }
    }
}

// ------------------------------------------------------------------------------------------------ the virtual device

/// IP packets between the stack and the tunnel: `rx` came out of the tunnel, `tx` goes into it.
struct VirtDev {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut VecDeque<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut p = vec![0u8; len];
        let r = f(&mut p);
        self.0.push_back(p);
        r
    }
}

impl Device for VirtDev {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;
    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(Rx, Tx<'_>)> {
        let p = self.rx.pop_front()?;
        Some((Rx(p), Tx(&mut self.tx)))
    }
    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ip;
        c.max_transmission_unit = MTU;
        c
    }
}

// ------------------------------------------------------------------------------------------------ shared state

struct Net {
    iface: Interface,
    dev: VirtDev,
    sockets: SocketSet<'static>,
    dns: SocketHandle,
    /// Streams that were dropped: removed once closed (or after a while).
    orphans: Vec<(SocketHandle, Instant)>,
    /// The tasks waiting on each socket, one reading and one writing: both are woken when the tunnel closes (the stack's own
    /// wakers never fire again then).
    waiting: HashMap<SocketHandle, (Option<Waker>, Option<Waker>)>,
}

impl Net {
    fn park(&mut self, h: SocketHandle, w: &Waker, read: bool) {
        let slot = self.waiting.entry(h).or_default();
        let s = if read { &mut slot.0 } else { &mut slot.1 };
        if !s.as_ref().is_some_and(|o| o.will_wake(w)) {
            *s = Some(w.clone());
        }
    }

    fn wake_all(&mut self) {
        for (_, (r, w)) in self.waiting.drain() {
            r.into_iter().chain(w).for_each(Waker::wake);
        }
    }
}

struct Shared {
    net: Mutex<Net>,
    wake: Notify,
    /// A handshake completed since the start.
    up: AtomicBool,
    closed: AtomicBool,
    next_port: AtomicU16,
    started: Instant,
}

fn now_smol(started: Instant) -> smoltcp::time::Instant {
    smoltcp::time::Instant::from_micros(started.elapsed().as_micros() as i64)
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Net> {
        self.net.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn port(&self) -> u16 {
        // 49152..=65535, the ephemeral range
        49152 + self.next_port.fetch_add(1, Ordering::Relaxed) % 16384
    }
    fn closed_err() -> io::Error {
        io::Error::new(io::ErrorKind::ConnectionAborted, "the VPN tunnel closed")
    }
}

/// A running tunnel. Dropping it (or [`Tunnel::close`]) ends the driver and every stream through it.
pub struct Tunnel {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tunnel")
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.close();
    }
}

impl Tunnel {
    /// Opens the tunnel and waits for the first handshake. Must run on a tokio runtime (the driver is spawned on it).
    pub async fn start(cfg: TunnelConfig) -> Result<Tunnel, TunnelError> {
        let bind: SocketAddr = if cfg.endpoint.is_ipv4() { "0.0.0.0:0".parse().unwrap() } else { "[::]:0".parse().unwrap() };
        let udp = UdpSocket::bind(bind).await.map_err(TunnelError::Socket)?;
        udp.connect(cfg.endpoint).await.map_err(TunnelError::Socket)?;
        let mut seed = [0u8; 8];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut seed);
        let seed = u64::from_le_bytes(seed);
        let tunn = Tunn::new(cfg.private_key.secret(), PublicKey::from(cfg.peer_public), None, Some(25), (seed >> 40) as u32, None);
        let mut dev = VirtDev { rx: VecDeque::new(), tx: VecDeque::new() };
        let started = Instant::now();
        let mut icfg = Config::new(HardwareAddress::Ip);
        icfg.random_seed = seed;
        let mut iface = Interface::new(icfg, &mut dev, now_smol(started));
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv4(cfg.address), 32));
        });
        // (with one /32 nothing is on-link: everything goes "via" the server's side; the medium is IP, so no neighbour is asked)
        let _ = iface.routes_mut().add_default_ipv4_route(Ipv4Addr::new(10, 64, 0, 1));
        let mut sockets = SocketSet::new(Vec::new());
        let dns = sockets.add(dns::Socket::new(&[IpAddress::Ipv4(cfg.dns)], Vec::new()));
        let shared = Arc::new(Shared {
            net: Mutex::new(Net { iface, dev, sockets, dns, orphans: Vec::new(), waiting: HashMap::new() }),
            wake: Notify::new(),
            up: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            next_port: AtomicU16::new((seed & 0x3fff) as u16),
            started,
        });
        log::info!("vpn: connecting to {}", cfg.endpoint);
        tokio::spawn(drive(shared.clone(), udp, tunn));
        let tunnel = Tunnel { shared };
        let deadline = Instant::now() + HANDSHAKE_WAIT;
        while !tunnel.shared.up.load(Ordering::Acquire) {
            if Instant::now() >= deadline || tunnel.shared.closed.load(Ordering::Acquire) {
                return Err(TunnelError::NoAnswer);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        log::info!("vpn: handshake done");
        Ok(tunnel)
    }

    /// Whether the tunnel still runs (the driver stops when the server has been silent too long).
    pub fn is_alive(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }

    pub fn close(&self) {
        if self.shared.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut n = self.shared.lock();
        let handles: Vec<SocketHandle> = n.sockets.iter().map(|(h, _)| h).filter(|h| *h != n.dns).collect();
        for h in handles {
            n.sockets.get_mut::<tcp::Socket>(h).abort();
        }
        n.wake_all();
        drop(n);
        self.shared.wake.notify_one();
    }

    /// Points the inside resolver somewhere else (a DNS blocking choice changed).
    pub fn set_dns(&self, dns: Ipv4Addr) {
        let mut n = self.shared.lock();
        let h = n.dns;
        n.sockets.get_mut::<dns::Socket>(h).update_servers(&[IpAddress::Ipv4(dns)]);
    }

    /// The IPv4 addresses of `name`, asked through the tunnel (an address literal is returned as it is).
    pub async fn resolve(&self, name: &str) -> io::Result<Vec<Ipv4Addr>> {
        if let Ok(ip) = name.parse::<Ipv4Addr>() {
            return Ok(vec![ip]);
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(Shared::closed_err());
        }
        let q = {
            let mut g = self.shared.lock();
            let n = &mut *g;
            let h = n.dns;
            n.sockets
                .get_mut::<dns::Socket>(h)
                .start_query(n.iface.context(), name, DnsQueryType::A)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("the name cannot be looked up ({e:?})")))?
        };
        self.shared.wake.notify_one();
        let shared = self.shared.clone();
        let wait = poll_fn(|cx| {
            if shared.closed.load(Ordering::Acquire) {
                return Poll::Ready(Err(Shared::closed_err()));
            }
            let mut g = shared.lock();
            let n = &mut *g;
            let h = n.dns;
            let s = n.sockets.get_mut::<dns::Socket>(h);
            match s.get_query_result(q) {
                Ok(v) => Poll::Ready(Ok(v.iter().filter_map(|a| match a {
                    IpAddress::Ipv4(v4) => Some(*v4),
                    #[allow(unreachable_patterns)]
                    _ => None,
                }).collect::<Vec<_>>())),
                Err(dns::GetQueryResultError::Pending) => {
                    s.register_query_waker(q, cx.waker());
                    n.park(h, cx.waker(), true);
                    Poll::Pending
                }
                Err(dns::GetQueryResultError::Failed) => Poll::Ready(Err(io::Error::new(io::ErrorKind::NotFound, "the name has no address"))),
            }
        });
        match tokio::time::timeout(Duration::from_secs(8), wait).await {
            Ok(Ok(v)) if !v.is_empty() => Ok(v),
            Ok(Ok(_)) => Err(io::Error::new(io::ErrorKind::NotFound, "the name has no IPv4 address")),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let mut g = self.shared.lock();
                let h = g.dns;
                g.sockets.get_mut::<dns::Socket>(h).cancel_query(q);
                Err(io::Error::new(io::ErrorKind::TimedOut, "the name lookup through the VPN did not answer"))
            }
        }
    }

    /// A TCP connection to `ip:port` through the tunnel.
    pub async fn connect(&self, ip: Ipv4Addr, port: u16, timeout: Duration) -> io::Result<TcpStream> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(Shared::closed_err());
        }
        let handle = {
            let mut g = self.shared.lock();
            let n = &mut *g;
            let mut s = tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; RX_BUF]), tcp::SocketBuffer::new(vec![0u8; TX_BUF]));
            s.set_nagle_enabled(false);
            s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(25)));
            s.set_timeout(Some(smoltcp::time::Duration::from_secs(120)));
            let local = self.shared.port();
            s.connect(n.iface.context(), (IpAddress::Ipv4(ip), port), local)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("the connection cannot be made ({e:?})")))?;
            n.sockets.add(s)
        };
        self.shared.wake.notify_one();
        let stream = TcpStream { shared: self.shared.clone(), handle, fin_seen: false };
        let shared = self.shared.clone();
        let wait = poll_fn(|cx| {
            if shared.closed.load(Ordering::Acquire) {
                return Poll::Ready(Err(Shared::closed_err()));
            }
            let mut n = shared.lock();
            let s = n.sockets.get_mut::<tcp::Socket>(handle);
            match s.state() {
                tcp::State::Established => Poll::Ready(Ok(())),
                tcp::State::SynSent | tcp::State::SynReceived => {
                    s.register_send_waker(cx.waker());
                    n.park(handle, cx.waker(), false);
                    Poll::Pending
                }
                _ => Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionRefused, "the server refused the connection"))),
            }
        });
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(())) => Ok(stream),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "no answer through the VPN")),
        }
    }
}

// ------------------------------------------------------------------------------------------------ the driver

async fn send(udp: &UdpSocket, b: &[u8]) {
    if let Err(e) = udp.send(b).await {
        log::debug!("vpn: a packet was not sent: {e}");
    }
}

/// Moves packets: stack -> Tunn -> UDP, UDP -> Tunn -> stack, and runs WireGuard's timers.
async fn drive(shared: Arc<Shared>, udp: UdpSocket, mut tunn: Tunn) {
    let mut out = vec![0u8; 65536];
    let mut inb = vec![0u8; 65536];
    if let TunnResult::WriteToNetwork(b) = tunn.format_handshake_initiation(&mut out, false) {
        send(&udp, b).await;
    }
    let mut last_timers = Instant::now();
    let mut last_alive = Instant::now();
    while !shared.closed.load(Ordering::Acquire) {
        let (packets, delay) = {
            let mut g = shared.lock();
            let n = &mut *g;
            let now = now_smol(shared.started);
            n.iface.poll(now, &mut n.dev, &mut n.sockets);
            reap(n);
            let packets: Vec<Vec<u8>> = n.dev.tx.drain(..).collect();
            (packets, n.iface.poll_delay(now, &n.sockets))
        };
        for p in &packets {
            match tunn.encapsulate(p, &mut out) {
                TunnResult::WriteToNetwork(b) => send(&udp, b).await,
                TunnResult::Err(e) => log::debug!("vpn: a packet could not be sealed: {e:?}"),
                _ => {}
            }
        }
        let wait = delay.map_or(Duration::from_millis(100), |d| Duration::from_micros(d.total_micros())).min(Duration::from_millis(100));
        tokio::select! {
            r = udp.recv(&mut inb) => match r {
                Ok(len) => {
                    let mut to_stack = Vec::new();
                    match tunn.decapsulate(None, &inb[..len], &mut out) {
                        TunnResult::WriteToNetwork(b) => {
                            send(&udp, b).await;
                            // (packets that waited for the handshake go now)
                            while let TunnResult::WriteToNetwork(b) = tunn.decapsulate(None, &[], &mut out) {
                                send(&udp, b).await;
                            }
                        }
                        TunnResult::WriteToTunnelV4(p, _) => to_stack.push(p.to_vec()),
                        TunnResult::WriteToTunnelV6(..) | TunnResult::Done => {}
                        TunnResult::Err(e) => log::debug!("vpn: a packet was dropped: {e:?}"),
                    }
                    if !to_stack.is_empty() {
                        shared.lock().dev.rx.extend(to_stack);
                    }
                    if tunn.time_since_last_handshake().is_some() {
                        shared.up.store(true, Ordering::Release);
                        last_alive = Instant::now();
                    }
                }
                // (Windows reports an ICMP "port unreachable" on a connected UDP socket as an error: nothing to do)
                Err(e) => {
                    log::debug!("vpn: udp: {e}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
            _ = shared.wake.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
        if last_timers.elapsed() >= Duration::from_millis(250) {
            last_timers = Instant::now();
            match tunn.update_timers(&mut out) {
                TunnResult::WriteToNetwork(b) => send(&udp, b).await,
                TunnResult::Err(WireGuardError::ConnectionExpired) => log::debug!("vpn: the session expired, a new handshake follows"),
                TunnResult::Err(e) => log::debug!("vpn: timers: {e:?}"),
                _ => {}
            }
            if let Some(since) = tunn.time_since_last_handshake()
                && since < DEAD_AFTER
            {
                last_alive = Instant::now();
            }
            if shared.up.load(Ordering::Acquire) && last_alive.elapsed() > DEAD_AFTER {
                log::warn!("vpn: the server has not answered for {} s; the tunnel is closed", DEAD_AFTER.as_secs());
                break;
            }
        }
    }
    // (closed from here, or the server went silent: every stream gets an error now)
    shared.closed.store(true, Ordering::Release);
    let mut n = shared.lock();
    n.wake_all();
    log::info!("vpn: tunnel closed");
}

/// Removes the sockets of dropped streams that are closed, or that waited too long for the other side.
fn reap(n: &mut Net) {
    let now = Instant::now();
    let mut keep = Vec::new();
    for (h, since) in std::mem::take(&mut n.orphans) {
        let s = n.sockets.get_mut::<tcp::Socket>(h);
        let gone = matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait);
        if gone || now.duration_since(since) > Duration::from_secs(30) {
            if !gone {
                s.abort();
            }
            n.sockets.remove(h);
            n.waiting.remove(&h);
        } else {
            keep.push((h, since));
        }
    }
    n.orphans = keep;
}

// ------------------------------------------------------------------------------------------------ a stream

/// The other side sent its FIN (in these states; `Closed` can also be a reset).
fn peer_finished(state: tcp::State) -> bool {
    matches!(state, tcp::State::CloseWait | tcp::State::LastAck | tcp::State::TimeWait | tcp::State::Closing)
}

/// What a read gets once nothing more can come: the end of the stream after the other side's FIN (seen now, or before the
/// socket went on to `Closed`), else a reset.
pub(crate) fn read_end(state: tcp::State, fin_seen: bool) -> io::Result<()> {
    if fin_seen || peer_finished(state) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "the connection through the VPN was reset"))
    }
}

/// A TCP connection through the tunnel (tokio's `AsyncRead` + `AsyncWrite`).
pub struct TcpStream {
    shared: Arc<Shared>,
    handle: SocketHandle,
    /// The other side's FIN was seen (so a later `Closed` is the normal end, not a reset).
    fin_seen: bool,
}

impl std::fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("vpn::TcpStream")
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let mut n = self.shared.lock();
        n.sockets.get_mut::<tcp::Socket>(self.handle).close();
        n.orphans.push((self.handle, Instant::now()));
        drop(n);
        self.shared.wake.notify_one();
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.shared.closed.load(Ordering::Acquire) {
            return Poll::Ready(Err(Shared::closed_err()));
        }
        let shared = this.shared.clone();
        let mut n = shared.lock();
        let s = n.sockets.get_mut::<tcp::Socket>(this.handle);
        if peer_finished(s.state()) {
            this.fin_seen = true;
        }
        if s.can_recv() {
            let k = s.recv_slice(buf.initialize_unfilled()).map_err(|e| io::Error::other(format!("{e:?}")))?;
            buf.advance(k);
            drop(n);
            // (the window opened: the driver tells the other side)
            shared.wake.notify_one();
            return Poll::Ready(Ok(()));
        }
        if !s.may_recv() {
            // the end of the stream only after the other side's FIN; a reset (or the stack's timeout) is an error, as with
            // plain TCP, so a cut answer is never taken for a whole one
            return Poll::Ready(read_end(s.state(), this.fin_seen));
        }
        s.register_recv_waker(cx.waker());
        n.park(this.handle, cx.waker(), true);
        Poll::Pending
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Poll::Ready(Err(Shared::closed_err()));
        }
        let mut n = self.shared.lock();
        let s = n.sockets.get_mut::<tcp::Socket>(self.handle);
        if !s.may_send() {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "the connection through the VPN is closed")));
        }
        if s.can_send() {
            let k = s.send_slice(data).map_err(|e| io::Error::other(format!("{e:?}")))?;
            drop(n);
            self.shared.wake.notify_one();
            return Poll::Ready(Ok(k));
        }
        s.register_send_waker(cx.waker());
        n.park(self.handle, cx.waker(), false);
        Poll::Pending
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared.wake.notify_one();
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared.lock().sockets.get_mut::<tcp::Socket>(self.handle).close();
        self.shared.wake.notify_one();
        Poll::Ready(Ok(()))
    }
}
