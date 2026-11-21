//! LocalSend, the open protocol (v2) that the LocalSend apps on phones and computers speak: leaf sends files to those devices and, once
//! the user allows it, takes theirs. Written on what leaf already has (tokio, rustls with ring, httparse) rather than with the
//! official Rust core, which wants a second async stack: the protocol is small.
//!
//! Discovery is UDP multicast on 224.0.0.167:53317 (a JSON announcement; the answer is an HTTPS `register` call to the announcer, or
//! a multicast announcement when that fails). Transfers are HTTPS with a self-signed certificate whose SHA-256 is the device's
//! fingerprint: `prepare-upload` (the receiver's user says yes or no there), `upload` per file with a token, `cancel`.
//!
//! Nothing here touches the network on its own. A search ([`LocalSend::scan`]) opens the multicast socket for a few seconds; receiving
//! ([`LocalSend::set_receiving`]) is off until the app turns it on and is the only thing that binds the HTTPS port.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail, ensure};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::oneshot;

use crate::identity::{self, Identity};
use crate::proto::Item;

/// LocalSend's port: the multicast group's, and the HTTPS server's unless told otherwise.
pub const PORT: u16 = 53317;
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const API: &str = "/api/localsend/v2";
/// How long a search listens.
const SCAN: Duration = Duration::from_secs(6);
/// How long a sender waits for the other user to answer a request, and how long a request of ours waits for our user.
const DECISION: Duration = Duration::from_secs(120);
const CONNECT: Duration = Duration::from_secs(6);
/// A transfer nobody has touched for this long no longer blocks the next one.
const STALE: Duration = Duration::from_secs(60);
const MAX_JSON: usize = 8 << 20;
const MAX_FILES: usize = 20_000;
const BUFFER: usize = 256 << 10;
/// Transfer and request ids start here (the app's other Nearby engine counts from 1).
pub const ID_BASE: u64 = 1 << 40;
/// A search is not repeated sooner than this unless asked for again by name.
const SCAN_EVERY: Duration = Duration::from_secs(30);

// ------------------------------------------------------------------------------------------------------------------------ public

#[derive(Clone, Debug)]
pub struct LsConfig {
    /// The name the other devices show for this one.
    pub alias: String,
    /// "Windows", "Linux", ...: shown by some apps under the name.
    pub model: String,
    /// Where the certificate is kept (shared with the rest of Nearby: one identity per install).
    pub state_dir: String,
    /// The HTTPS port; 0 lets the system choose (tests). LocalSend's own apps expect 53317.
    pub port: u16,
    /// Use the multicast group. Off for tests: nothing leaves 127.0.0.1 then.
    pub multicast: bool,
    /// Listen on 127.0.0.1 only (tests).
    pub local_only: bool,
    /// Asked of senders when set (the protocol's PIN).
    pub pin: Option<String>,
}

/// A device found on the network (or added by address).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsDevice {
    pub fingerprint: String,
    pub alias: String,
    pub model: Option<String>,
    /// "mobile", "desktop", "web", "headless", "server".
    pub device_type: String,
    pub ip: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsFile {
    pub name: String,
    pub size: u64,
}

/// Files another device wants to send: waits for [`LocalSend::accept`] or [`LocalSend::decline`] (no answer is a decline after two
/// minutes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LsRequest {
    pub id: u64,
    /// The transfer it becomes when accepted: progress and the end are reported under this id.
    pub transfer: u64,
    pub from: String,
    pub from_model: Option<String>,
    pub files: Vec<LsFile>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LsResult {
    pub ok: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    /// Receiving: the top-level files and folders that arrived, at their final paths.
    pub paths: Vec<String>,
}

/// What the crate tells the app, from threads of its own.
pub trait LsListener: Send + Sync {
    fn devices_changed(&self, devices: Vec<LsDevice>);
    fn request(&self, request: LsRequest);
    /// At most about ten times a second per transfer, and once more at the end.
    fn progress(&self, transfer: u64, done: u64, total: u64);
    fn finished(&self, transfer: u64, result: LsResult);
}

#[derive(Debug, thiserror::Error)]
pub enum LsError {
    #[error("the network could not be used: {0}")]
    Network(String),
    #[error("no device {0} is known")]
    UnknownDevice(String),
    #[error("{0}")]
    Io(String),
}

pub struct LocalSend {
    inner: Arc<Inner>,
    rt: Mutex<Option<tokio::runtime::Runtime>>,
}

impl LocalSend {
    /// Nothing is bound or sent: see the module's note.
    pub fn new(cfg: LsConfig, listener: Arc<dyn LsListener>) -> Result<Arc<LocalSend>, LsError> {
        let me = Identity::load_or_create(Path::new(&cfg.state_dir)).map_err(|e| LsError::Io(format!("{e:#}")))?;
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("rf-localsend").enable_all().build().map_err(|e| LsError::Io(e.to_string()))?;
        let tls = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_no_client_auth().with_single_cert(vec![me.cert.clone()], me.key()))
            .map_err(|e| LsError::Network(e.to_string()))?;
        let inner = Arc::new(Inner {
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(tls)),
            handle: rt.handle().clone(),
            devices: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
            // (far from the other engine's ids: the app keeps one list of transfers for both)
            next: AtomicU64::new(ID_BASE),
            alias: Mutex::new(cfg.alias.clone()),
            last_scan: Mutex::new(None),
            cancelled: Mutex::new(HashSet::new()),
            receiving: AtomicBool::new(false),
            tcp_port: AtomicU16::new(0),
            udp: Mutex::new(None),
            scanning: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            cfg,
            me,
            listener,
        });
        Ok(Arc::new(LocalSend { inner, rt: Mutex::new(Some(rt)) }))
    }

    /// The SHA-256 of this device's certificate.
    pub fn fingerprint(&self) -> String {
        self.inner.me.fingerprint.clone()
    }

    pub fn devices(&self) -> Vec<LsDevice> {
        self.inner.device_list()
    }

    pub fn is_receiving(&self) -> bool {
        self.inner.receiving.load(Ordering::SeqCst)
    }

    /// Looks for devices: announces on the multicast group and listens for a few seconds (devices found come with `devices_changed`).
    pub fn scan(&self) {
        self.scan_inner(true);
    }

    /// [`LocalSend::scan`], but not when one ran in the last 30 seconds (for a menu that opens often).
    pub fn scan_if_stale(&self) {
        self.scan_inner(false);
    }

    /// The name the other devices see from now on.
    pub fn set_alias(&self, alias: &str) {
        *self.inner.alias.lock().unwrap_or_else(|e| e.into_inner()) = alias.to_string();
    }

    fn scan_inner(&self, force: bool) {
        if !self.inner.cfg.multicast {
            return;
        }
        {
            let mut last = self.inner.last_scan.lock().unwrap_or_else(|e| e.into_inner());
            if !force && last.is_some_and(|t| t.elapsed() < SCAN_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let inner = self.inner.clone();
        inner.prune();
        if inner.receiving.load(Ordering::SeqCst) {
            let udp = inner.udp.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(udp) = udp {
                let inner2 = inner.clone();
                inner.handle.spawn(async move { announce(&udp, &inner2, true).await });
            }
            return;
        }
        if inner.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let handle = inner.handle.clone();
        handle.spawn(async move {
            match open_udp(PORT) {
                Ok(udp) => {
                    let udp = Arc::new(udp);
                    announce(&udp, &inner, true).await;
                    let listen = tokio::spawn(listen(inner.clone(), udp.clone(), Some(Instant::now() + SCAN), false));
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    announce(&udp, &inner, true).await;
                    let _ = listen.await;
                }
                Err(e) => log::info!("localsend: no multicast socket: {e}"),
            }
            inner.scanning.store(false, Ordering::SeqCst);
        });
    }

    /// Asks the device at `addr` for who it is and lists it (a device that multicast does not reach: another subnet, a saved address).
    pub fn add_device(&self, addr: SocketAddr) {
        let inner = self.inner.clone();
        self.inner.handle.spawn(async move {
            match call(&inner, addr.ip(), addr.port(), None, "GET", &format!("{API}/info"), None, CONNECT * 2).await {
                Ok(r) if r.status == 200 => match serde_json::from_slice::<Info>(&r.body) {
                    Ok(info) if info.fingerprint != inner.me.fingerprint => inner.upsert(info, addr.ip(), Some(addr.port())),
                    _ => {}
                },
                Ok(r) => log::info!("localsend: {addr} answered {}", r.status),
                Err(e) => log::info!("localsend: {addr}: {e:#}"),
            }
        });
    }

    /// Turns receiving on (binds the HTTPS port, and with multicast on, joins the group and announces) or off. Returns the port.
    pub fn set_receiving(&self, on: bool) -> Result<u16, LsError> {
        let inner = &self.inner;
        if on == inner.receiving.load(Ordering::SeqCst) {
            return Ok(inner.tcp_port.load(Ordering::SeqCst));
        }
        if !on {
            inner.receiving.store(false, Ordering::SeqCst);
            for t in inner.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
                t.abort();
            }
            *inner.udp.lock().unwrap_or_else(|e| e.into_inner()) = None;
            inner.tcp_port.store(0, Ordering::SeqCst);
            // (the questions still open are declined, and a transfer half done is dropped)
            inner.pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
            inner.end_session(true, Some("receiving was turned off"));
            return Ok(0);
        }
        let ip = if inner.cfg.local_only { Ipv4Addr::LOCALHOST } else { Ipv4Addr::UNSPECIFIED };
        let std_listener = std::net::TcpListener::bind((ip, inner.cfg.port)).map_err(|e| LsError::Network(format!("port {} is not free: {e}", inner.cfg.port)))?;
        std_listener.set_nonblocking(true).map_err(|e| LsError::Network(e.to_string()))?;
        let port = std_listener.local_addr().map_err(|e| LsError::Network(e.to_string()))?.port();
        let _guard = inner.handle.enter();
        let listener = TcpListener::from_std(std_listener).map_err(|e| LsError::Network(e.to_string()))?;
        inner.tcp_port.store(port, Ordering::SeqCst);
        inner.receiving.store(true, Ordering::SeqCst);
        let mut tasks = inner.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.push(tokio::spawn(accept_loop(inner.clone(), listener)));
        if inner.cfg.multicast {
            match open_udp(PORT) {
                Ok(udp) => {
                    let udp = Arc::new(udp);
                    *inner.udp.lock().unwrap_or_else(|e| e.into_inner()) = Some(udp.clone());
                    tasks.push(tokio::spawn(listen(inner.clone(), udp.clone(), None, true)));
                    let inner2 = inner.clone();
                    tasks.push(tokio::spawn(async move { announce(&udp, &inner2, true).await }));
                }
                Err(e) => log::info!("localsend: no multicast socket: {e}"),
            }
        }
        Ok(port)
    }

    /// Says yes to a request: its files go into `dest` (made when missing), never over a file that is there.
    pub fn accept(&self, id: u64, dest: PathBuf) {
        if let Some(tx) = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            let _ = tx.send(Some(dest));
        }
    }

    pub fn decline(&self, id: u64) {
        if let Some(tx) = self.inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            let _ = tx.send(None);
        }
    }

    /// Sends `paths` (files, and folders as their files) to the device with this fingerprint; returns the transfer's id. Progress and
    /// the end come with the listener.
    pub fn send(&self, fingerprint: &str, paths: Vec<PathBuf>) -> Result<u64, LsError> {
        let device = self.inner.devices.lock().unwrap_or_else(|e| e.into_inner()).get(fingerprint).map(|(d, _)| d.clone()).ok_or_else(|| LsError::UnknownDevice(fingerprint.to_string()))?;
        let tid = self.inner.next.fetch_add(1, Ordering::SeqCst);
        let inner = self.inner.clone();
        self.inner.handle.spawn(async move {
            let result = match send_task(&inner, tid, &device, paths).await {
                Ok(()) => LsResult { ok: true, ..Default::default() },
                Err(e) if inner.take_cancelled(tid) => LsResult { cancelled: true, error: Some(format!("{e:#}")), ..Default::default() },
                Err(e) => LsResult { error: Some(format!("{e:#}")), ..Default::default() },
            };
            inner.listener.finished(tid, result);
        });
        Ok(tid)
    }

    /// Stops a transfer, sending or receiving.
    pub fn cancel(&self, transfer: u64) {
        self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner()).insert(transfer);
        let mine = self.inner.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.transfer == transfer);
        if mine {
            self.inner.end_session(true, None);
        }
    }

    /// Stops everything (closes the sockets); waits for the runtime, so not on a UI thread.
    pub fn stop(&self) {
        let _ = self.set_receiving(false);
        if let Some(rt) = self.rt.lock().unwrap_or_else(|e| e.into_inner()).take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for LocalSend {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.lock().unwrap_or_else(|e| e.into_inner()).take() {
            rt.shutdown_background();
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------------ state

struct Inner {
    cfg: LsConfig,
    me: Identity,
    listener: Arc<dyn LsListener>,
    acceptor: tokio_rustls::TlsAcceptor,
    handle: tokio::runtime::Handle,
    devices: Mutex<HashMap<String, (LsDevice, Instant)>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Option<PathBuf>>>>,
    session: Mutex<Option<Session>>,
    next: AtomicU64,
    cancelled: Mutex<HashSet<u64>>,
    receiving: AtomicBool,
    tcp_port: AtomicU16,
    udp: Mutex<Option<Arc<Udp>>>,
    scanning: AtomicBool,
    alias: Mutex<String>,
    last_scan: Mutex<Option<Instant>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// An accepted request's transfer.
struct Session {
    id: String,
    transfer: u64,
    dest: PathBuf,
    files: HashMap<String, SFile>,
    total: u64,
    received: Arc<AtomicU64>,
    dead: Arc<AtomicBool>,
    touched: Instant,
}

struct SFile {
    target: PathBuf,
    token: String,
    busy: bool,
    done: Option<PathBuf>,
}

/// A device's description, in the shape LocalSend's apps use (register, announcements, `info`, and the `info` of a prepare-upload).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    alias: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    device_model: Option<String>,
    #[serde(default)]
    device_type: Option<String>,
    fingerprint: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_protocol")]
    protocol: String,
    #[serde(default)]
    download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    announce: Option<bool>,
}

fn default_port() -> u16 {
    PORT
}

fn default_protocol() -> String {
    "https".into()
}

impl Inner {
    fn info(&self, announce: Option<bool>) -> Info {
        let port = match self.tcp_port.load(Ordering::SeqCst) {
            0 if self.cfg.port != 0 => self.cfg.port,
            0 => PORT,
            p => p,
        };
        Info {
            alias: self.alias.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            version: "2.0".into(),
            device_model: Some(self.cfg.model.clone()),
            device_type: Some("desktop".into()),
            fingerprint: self.me.fingerprint.clone(),
            port,
            protocol: "https".into(),
            download: false,
            announce,
        }
    }

    fn device_list(&self) -> Vec<LsDevice> {
        let mut v: Vec<LsDevice> = self.devices.lock().unwrap_or_else(|e| e.into_inner()).values().map(|(d, _)| d.clone()).collect();
        v.sort_by(|a, b| a.alias.to_lowercase().cmp(&b.alias.to_lowercase()).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
        v
    }

    /// Lists a device (or refreshes it); the listener hears when something about it changed.
    fn upsert(&self, info: Info, ip: IpAddr, port: Option<u16>) {
        if info.fingerprint.is_empty() || info.fingerprint == self.me.fingerprint || info.protocol != "https" {
            return;
        }
        let dev = LsDevice {
            fingerprint: info.fingerprint,
            alias: info.alias,
            model: info.device_model,
            device_type: info.device_type.unwrap_or_else(|| "desktop".into()),
            ip,
            port: port.unwrap_or(info.port),
        };
        let changed = {
            let mut g = self.devices.lock().unwrap_or_else(|e| e.into_inner());
            let changed = g.get(&dev.fingerprint).map(|(d, _)| d) != Some(&dev);
            g.insert(dev.fingerprint.clone(), (dev, Instant::now()));
            changed
        };
        if changed {
            self.listener.devices_changed(self.device_list());
        }
    }

    /// Devices not heard from for five minutes are forgotten at the next search.
    fn prune(&self) {
        let gone = {
            let mut g = self.devices.lock().unwrap_or_else(|e| e.into_inner());
            let before = g.len();
            g.retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(300));
            g.len() != before
        };
        if gone {
            self.listener.devices_changed(self.device_list());
        }
    }

    fn take_cancelled(&self, tid: u64) -> bool {
        self.cancelled.lock().unwrap_or_else(|e| e.into_inner()).remove(&tid)
    }

    /// Drops the transfer being received (its half-written files go); tells the app unless it ended well.
    fn end_session(&self, cancelled: bool, why: Option<&str>) {
        let Some(s) = self.session.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        s.dead.store(true, Ordering::SeqCst);
        for f in s.files.values() {
            let _ = std::fs::remove_file(part_of(&f.target));
        }
        let _ = self.take_cancelled(s.transfer);
        self.listener.finished(s.transfer, LsResult { cancelled, error: why.map(str::to_string), ..Default::default() });
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn random_hex(bytes: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut b = vec![0u8; bytes];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn part_of(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(crate::transfer::PART);
    PathBuf::from(s)
}

// -------------------------------------------------------------------------------------------------------------------- discovery

struct Udp {
    sock: UdpSocket,
    ifaces: Vec<Ipv4Addr>,
}

/// The multicast socket: bound to LocalSend's port with address reuse (other LocalSend apps on this PC share it) and joined to the
/// group on every IPv4 interface that is up. Made inside the runtime.
fn open_udp(port: u16) -> io::Result<Udp> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    let _ = sock.set_reuse_port(true);
    let bind = |p: u16| sock.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, p)).into());
    if let Err(e) = bind(port) {
        log::info!("localsend: port {port} is taken ({e}); the answers of other devices may not reach us");
        bind(0)?;
    }
    let mut ifaces = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(a) = &i.addr
            && !i.is_loopback()
            && sock.join_multicast_v4(&GROUP, &a.ip).is_ok()
        {
            ifaces.push(a.ip);
        }
    }
    if ifaces.is_empty() {
        let _ = sock.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED);
    }
    sock.set_nonblocking(true)?;
    Ok(Udp { sock: UdpSocket::from_std(sock.into())?, ifaces })
}

async fn announce(udp: &Udp, inner: &Inner, announce: bool) {
    let Ok(body) = serde_json::to_vec(&inner.info(Some(announce))) else { return };
    let to = SocketAddr::from((GROUP, PORT));
    let ifaces = if udp.ifaces.is_empty() { vec![Ipv4Addr::UNSPECIFIED] } else { udp.ifaces.clone() };
    for ip in ifaces {
        let _ = socket2::SockRef::from(&udp.sock).set_multicast_if_v4(&ip);
        let _ = udp.sock.send_to(&body, to).await;
    }
}

/// Reads announcements until `until` (for ever when None). `reply`: answer announcements (only while receiving: a search alone makes
/// no promise of being reachable).
async fn listen(inner: Arc<Inner>, udp: Arc<Udp>, until: Option<Instant>, reply: bool) {
    let mut buf = vec![0u8; 8192];
    loop {
        let recv = udp.sock.recv_from(&mut buf);
        let got = match until {
            Some(t) => match tokio::time::timeout_at(t.into(), recv).await {
                Ok(r) => r,
                Err(_) => return,
            },
            None => recv.await,
        };
        let Ok((n, from)) = got else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(info) = serde_json::from_slice::<Info>(&buf[..n]) else { continue };
        if info.fingerprint == inner.me.fingerprint {
            continue;
        }
        let asked = info.announce == Some(true);
        let (ip, port) = (from.ip(), info.port);
        inner.upsert(info.clone(), ip, None);
        if asked && reply {
            let (inner, udp) = (inner.clone(), udp.clone());
            tokio::spawn(async move {
                let me = serde_json::to_vec(&inner.info(None)).unwrap_or_default();
                let fp = Some(info.fingerprint.as_str());
                let ok = matches!(call(&inner, ip, port, fp, "POST", &format!("{API}/register"), Some(me), CONNECT).await, Ok(r) if r.status == 200);
                if !ok {
                    announce(&udp, &inner, false).await;
                }
            });
        }
    }
}

// -------------------------------------------------------------------------------------------------------------------- HTTP client

/// Accepts the certificate of the device we meant to reach (by fingerprint, when we know it) and proves the TLS handshake as usual.
#[derive(Debug)]
struct Pinned {
    expected: Option<String>,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match &self.expected {
            Some(e) if !identity::fingerprint(end).eq_ignore_ascii_case(e) => Err(rustls::Error::General("the certificate is not the one the device announced".into())),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

type Client = tokio_rustls::client::TlsStream<TcpStream>;

async fn open(ip: IpAddr, port: u16, expected: Option<&str>) -> anyhow::Result<Client> {
    let p = provider();
    let cfg = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { expected: expected.map(str::to_string), algs: p.signature_verification_algorithms }))
        .with_no_client_auth();
    let tcp = tokio::time::timeout(CONNECT, TcpStream::connect((ip, port))).await.map_err(|_| anyhow!("timed out connecting to {ip}"))?.with_context(|| format!("cannot connect to {ip}:{port}"))?;
    let _ = tcp.set_nodelay(true);
    let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(ServerName::from(ip), tcp).await.context("TLS handshake failed")?;
    Ok(tls)
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

/// One request with a small body and its answer (the connection is not kept).
#[allow(clippy::too_many_arguments)]
async fn call(_inner: &Inner, ip: IpAddr, port: u16, expected: Option<&str>, method: &str, path: &str, body: Option<Vec<u8>>, limit: Duration) -> anyhow::Result<Resp> {
    let work = async {
        let mut s = open(ip, port, expected).await?;
        write_request(&mut s, method, path, ip, port, body.as_deref().map(|b| b.len() as u64)).await?;
        if let Some(b) = &body {
            s.write_all(b).await?;
        }
        s.flush().await?;
        read_response(&mut BufReader::with_capacity(BUFFER, s)).await
    };
    tokio::time::timeout(limit, work).await.map_err(|_| anyhow!("no answer in time"))?
}

async fn write_request<W: AsyncWrite + Unpin>(w: &mut W, method: &str, path: &str, ip: IpAddr, port: u16, len: Option<u64>) -> io::Result<()> {
    let host = match ip {
        IpAddr::V4(a) => format!("{a}:{port}"),
        IpAddr::V6(a) => format!("[{a}]:{port}"),
    };
    let ctype = if path.contains("/upload") { "application/octet-stream" } else { "application/json" };
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: leaf\r\nConnection: close\r\n");
    if let Some(len) = len {
        head.push_str(&format!("Content-Type: {ctype}\r\nContent-Length: {len}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes()).await
}

async fn read_response<R: AsyncBufRead + Unpin>(r: &mut R) -> anyhow::Result<Resp> {
    let head = read_head(r).await?.ok_or_else(|| anyhow!("the device closed the connection"))?;
    let mut headers = [httparse::EMPTY_HEADER; 48];
    let mut resp = httparse::Response::new(&mut headers);
    resp.parse(&head).map_err(|e| anyhow!("a bad answer: {e}"))?;
    let status = resp.code.unwrap_or(0);
    let (len, chunked) = framing(resp.headers);
    let mut body = Vec::new();
    let eof = len.is_none() && !chunked;
    read_body(r, len, chunked, eof, &mut |b: &[u8]| {
        if body.len() + b.len() > MAX_JSON {
            return Err(io::Error::other("an answer is too large"));
        }
        body.extend_from_slice(b);
        Ok(())
    })
    .await?;
    Ok(Resp { status, body })
}

// -------------------------------------------------------------------------------------------------------------------- HTTP codec

/// The head of a message (request line or status line, headers, the empty line); None at a clean end of the stream.
async fn read_head<R: AsyncBufRead + Unpin>(r: &mut R) -> anyhow::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    loop {
        let n = r.read_until(b'\n', &mut buf).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            bail!("the message was cut off");
        }
        if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
            return Ok(Some(buf));
        }
        ensure!(buf.len() < 64 << 10, "the message head is too large");
    }
}

fn framing(headers: &[httparse::Header<'_>]) -> (Option<u64>, bool) {
    let mut len = None;
    let mut chunked = false;
    for h in headers {
        let v = String::from_utf8_lossy(h.value);
        if h.name.eq_ignore_ascii_case("content-length") {
            len = v.trim().parse().ok();
        } else if h.name.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    (len, chunked)
}

/// The body of a message, handed to `sink` in pieces: by Content-Length, chunked, or (`eof`) until the stream ends.
async fn read_body<R: AsyncBufRead + Unpin>(r: &mut R, len: Option<u64>, chunked: bool, eof: bool, sink: &mut (dyn FnMut(&[u8]) -> io::Result<()> + Send)) -> io::Result<u64> {
    async fn exact<R: AsyncBufRead + Unpin>(r: &mut R, mut left: u64, sink: &mut (dyn FnMut(&[u8]) -> io::Result<()> + Send)) -> io::Result<()> {
        while left > 0 {
            let buf = r.fill_buf().await?;
            if buf.is_empty() {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let take = (buf.len() as u64).min(left) as usize;
            sink(&buf[..take])?;
            r.consume(take);
            left -= take as u64;
        }
        Ok(())
    }
    if chunked {
        let mut total = 0u64;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| io::Error::other("a bad chunk size"))?;
            if size == 0 {
                // (trailers, then the empty line)
                loop {
                    let mut t = String::new();
                    if r.read_line(&mut t).await? == 0 || t.trim().is_empty() {
                        return Ok(total);
                    }
                }
            }
            exact(r, size, sink).await?;
            total += size;
            let mut crlf = String::new();
            r.read_line(&mut crlf).await?;
        }
    } else if let Some(n) = len {
        exact(r, n, sink).await?;
        Ok(n)
    } else if eof {
        let mut total = 0u64;
        loop {
            let buf = r.fill_buf().await?;
            if buf.is_empty() {
                return Ok(total);
            }
            let n = buf.len();
            sink(buf)?;
            r.consume(n);
            total += n as u64;
        }
    } else {
        Ok(0)
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

async fn respond<W: AsyncWrite + Unpin>(w: &mut W, status: u16, body: &[u8], close: bool) -> io::Result<()> {
    let head = format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n", reason(status), body.len(), if close { "close" } else { "keep-alive" });
    w.write_all(head.as_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await
}

fn message(text: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "message": text })).unwrap_or_default()
}

fn query_of(target: &str) -> (&str, HashMap<String, String>) {
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let map = q.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), leaf_download::percent_decode(v))).collect();
    (path, map)
}

// -------------------------------------------------------------------------------------------------------------------- receiving

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    // (a crowd of connections is not served: a few at once are plenty for one phone)
    let limit = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        let Ok((tcp, addr)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
        let inner = inner.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve(inner, tcp, addr.ip()).await {
                log::debug!("localsend: a connection from {addr} ended: {e:#}");
            }
        });
    }
}

async fn serve(inner: Arc<Inner>, tcp: TcpStream, ip: IpAddr) -> anyhow::Result<()> {
    let _ = tcp.set_nodelay(true);
    let tls = tokio::time::timeout(Duration::from_secs(10), inner.acceptor.accept(tcp)).await.map_err(|_| anyhow!("the handshake took too long"))??;
    let mut s = BufReader::with_capacity(BUFFER, tls);
    while inner.receiving.load(Ordering::SeqCst) {
        let Some(head) = tokio::time::timeout(Duration::from_secs(30), read_head(&mut s)).await.map_err(|_| anyhow!("idle"))?? else { return Ok(()) };
        let mut headers = [httparse::EMPTY_HEADER; 48];
        let mut req = httparse::Request::new(&mut headers);
        req.parse(&head).map_err(|e| anyhow!("a bad request: {e}"))?;
        let (method, target) = (req.method.unwrap_or("").to_string(), req.path.unwrap_or("").to_string());
        let (len, chunked) = framing(req.headers);
        let (path, q) = query_of(&target);
        let route = path.strip_prefix(API).unwrap_or("");
        let (status, body, keep) = match (method.as_str(), route) {
            ("GET", "/info") => (200, serde_json::to_vec(&inner.info(None))?, true),
            ("POST", "/register") | ("POST", "/prepare-upload") => {
                let mut json = Vec::new();
                read_body(&mut s, len, chunked, false, &mut |b| {
                    if json.len() + b.len() > MAX_JSON {
                        return Err(io::Error::other("a body is too large"));
                    }
                    json.extend_from_slice(b);
                    Ok(())
                })
                .await?;
                if route == "/register" {
                    match serde_json::from_slice::<Info>(&json) {
                        Ok(info) => {
                            inner.upsert(info, ip, None);
                            (200, serde_json::to_vec(&inner.info(None))?, true)
                        }
                        Err(_) => (400, message("Invalid body"), true),
                    }
                } else {
                    let (status, body) = prepare(&inner, &q, &json).await;
                    (status, body, true)
                }
            }
            ("POST", "/upload") => upload(&inner, &mut s, &q, len, chunked).await,
            ("POST", "/cancel") => {
                let id = q.get("sessionId").cloned().unwrap_or_default();
                let mine = inner.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|x| x.id == id);
                if mine {
                    inner.end_session(true, None);
                }
                (200, Vec::new(), true)
            }
            _ => (404, message("Not found"), len.is_none() && !chunked),
        };
        respond(s.get_mut(), status, &body, !keep).await?;
        if !keep {
            return Ok(());
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct Prepare {
    files: HashMap<String, FileDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDto {
    #[serde(default)]
    id: String,
    file_name: String,
    #[serde(default)]
    size: u64,
}

/// A name from a sender as a relative path of harmless parts: every part goes through the download manager's sanitiser (`..`, device
/// names, control and direction characters, long names), so nothing leaves the folder it is put in.
fn clean_name(raw: &str) -> String {
    let parts: Vec<String> = raw.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").map(leaf_download::sanitize).collect();
    if parts.is_empty() { "file".to_string() } else { parts.join("/") }
}

/// Where each named file goes under `dest`: a name already taken there (or taken by an earlier file of the same request) gets
/// " (2)", " (3)"... so that nothing is overwritten.
fn plan_targets(dest: &Path, names: &[String]) -> Vec<PathBuf> {
    let items: Vec<Item> = names.iter().map(|n| Item { path: clean_name(n), ..Default::default() }).collect();
    crate::transfer::targets(dest, &items)
}

/// The sender's request: asked of our user; the answer is the session (its tokens) or a refusal.
async fn prepare(inner: &Arc<Inner>, q: &HashMap<String, String>, json: &[u8]) -> (u16, Vec<u8>) {
    if let Some(pin) = &inner.cfg.pin
        && q.get("pin") != Some(pin)
    {
        return (401, message("Pin required"));
    }
    let Ok(req) = serde_json::from_slice::<Prepare>(json) else { return (400, message("Invalid body")) };
    if req.files.is_empty() || req.files.len() > MAX_FILES {
        return (400, message("No files"));
    }
    let from = serde_json::from_slice::<serde_json::Value>(json).ok().and_then(|v| v.get("info").cloned()).and_then(|v| serde_json::from_value::<Info>(v).ok());
    let mut entries: Vec<(String, FileDto)> = req.files.into_iter().collect();
    entries.sort_by(|a, b| a.1.file_name.cmp(&b.1.file_name));
    let rid = inner.next.fetch_add(1, Ordering::SeqCst);
    let tid = inner.next.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    {
        let session = inner.session.lock().unwrap_or_else(|e| e.into_inner());
        if session.as_ref().is_some_and(|s| s.touched.elapsed() > STALE) {
            drop(session);
            inner.end_session(true, Some("the sender stopped"));
        } else if session.is_some() {
            return (409, message("Blocked by another session"));
        }
        let mut pending = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.is_empty() {
            return (409, message("Blocked by another session"));
        }
        pending.insert(rid, tx);
    }
    let total: u64 = entries.iter().map(|(_, f)| f.size).sum();
    inner.listener.request(LsRequest {
        id: rid,
        transfer: tid,
        from: from.as_ref().map(|i| i.alias.clone()).unwrap_or_else(|| "?".into()),
        from_model: from.and_then(|i| i.device_model),
        files: entries.iter().map(|(_, f)| LsFile { name: clean_name(&f.file_name), size: f.size }).collect(),
        total_bytes: total,
    });
    let answer = tokio::time::timeout(DECISION, rx).await;
    inner.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&rid);
    let Ok(Ok(Some(dest))) = answer else { return (403, message("File request declined by recipient")) };
    if let Err(e) = std::fs::create_dir_all(&dest) {
        log::warn!("localsend: cannot make {}: {e}", dest.display());
        return (500, message("The folder cannot be made"));
    }
    let names: Vec<String> = entries.iter().map(|(_, f)| f.file_name.clone()).collect();
    let targets = plan_targets(&dest, &names);
    let mut files = HashMap::new();
    let mut tokens = serde_json::Map::new();
    for ((key, dto), target) in entries.iter().zip(targets) {
        let id = if dto.id.is_empty() { key.clone() } else { dto.id.clone() };
        let token = random_hex(16);
        tokens.insert(id.clone(), serde_json::Value::String(token.clone()));
        files.insert(id, SFile { target, token, busy: false, done: None });
    }
    let sid = random_hex(16);
    let session = Session { id: sid.clone(), transfer: tid, dest, files, total, received: Arc::new(AtomicU64::new(0)), dead: Arc::new(AtomicBool::new(false)), touched: Instant::now() };
    *inner.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
    (200, serde_json::to_vec(&serde_json::json!({ "sessionId": sid, "files": tokens })).unwrap_or_default())
}

/// One file's bytes: written beside its place under a temporary name, then moved into place without replacing anything.
async fn upload<R: AsyncBufRead + Unpin>(inner: &Arc<Inner>, s: &mut R, q: &HashMap<String, String>, len: Option<u64>, chunked: bool) -> (u16, Vec<u8>, bool) {
    let refuse = |status: u16, text: &str| (status, message(text), false);
    let (Some(sid), Some(fid), Some(token)) = (q.get("sessionId"), q.get("fileId"), q.get("token")) else { return refuse(400, "Missing parameters") };
    if len.is_none() && !chunked {
        return refuse(411, "Length required");
    }
    let (target, transfer, total, received, dead) = {
        let mut g = inner.session.lock().unwrap_or_else(|e| e.into_inner());
        let Some(sess) = g.as_mut().filter(|x| &x.id == sid) else { return refuse(403, "Invalid token or IP address") };
        let Some(f) = sess.files.get_mut(fid) else { return refuse(404, "File not found") };
        if &f.token != token {
            return refuse(403, "Invalid token or IP address");
        }
        if f.busy || f.done.is_some() {
            return refuse(409, "Already uploaded");
        }
        f.busy = true;
        sess.touched = Instant::now();
        (f.target.clone(), sess.transfer, sess.total, sess.received.clone(), sess.dead.clone())
    };
    let part = part_of(&target);
    let written = async {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::File::create(&part)?;
        let mut last = Instant::now();
        let r = read_body(s, len, chunked, false, &mut |b: &[u8]| {
            if dead.load(Ordering::SeqCst) {
                return Err(io::Error::other("cancelled"));
            }
            io::Write::write_all(&mut file, b)?;
            let done = received.fetch_add(b.len() as u64, Ordering::SeqCst) + b.len() as u64;
            if last.elapsed() >= Duration::from_millis(100) {
                last = Instant::now();
                inner.listener.progress(transfer, done, total);
            }
            Ok(())
        })
        .await?;
        io::Write::flush(&mut file)?;
        drop(file);
        // (never over a file that is there now: another one with the name arrived meanwhile)
        let mut final_path = target.clone();
        if final_path.exists() {
            let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
            let name = final_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            final_path = (2..)
                .map(|n| {
                    let (stem, ext) = match name.rfind('.') {
                        Some(i) if i > 0 => (&name[..i], &name[i..]),
                        _ => (name.as_str(), ""),
                    };
                    dir.join(format!("{stem} ({n}){ext}"))
                })
                .find(|p| !p.exists())
                .unwrap_or(final_path);
        }
        std::fs::rename(&part, &final_path)?;
        io::Result::Ok((r, final_path))
    }
    .await;
    match written {
        Ok((_, final_path)) => {
            let finished = {
                let mut g = inner.session.lock().unwrap_or_else(|e| e.into_inner());
                match g.as_mut().filter(|x| &x.id == sid) {
                    Some(sess) => {
                        if let Some(f) = sess.files.get_mut(fid) {
                            f.busy = false;
                            f.done = Some(final_path);
                        }
                        sess.touched = Instant::now();
                        if sess.files.values().all(|f| f.done.is_some()) { g.take() } else { None }
                    }
                    None => None,
                }
            };
            if let Some(sess) = finished {
                let mut tops: Vec<String> = Vec::new();
                for f in sess.files.values() {
                    let done = f.done.as_deref().unwrap_or(&f.target);
                    let top = done.strip_prefix(&sess.dest).ok().and_then(|r| r.components().next()).map(|c| sess.dest.join(c.as_os_str())).unwrap_or_else(|| done.to_path_buf());
                    let top = top.to_string_lossy().into_owned();
                    if !tops.contains(&top) {
                        tops.push(top);
                    }
                }
                tops.sort();
                inner.listener.progress(sess.transfer, sess.total, sess.total);
                inner.listener.finished(sess.transfer, LsResult { ok: true, paths: tops, ..Default::default() });
            }
            (200, Vec::new(), true)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            if let Some(f) = inner.session.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|s| s.files.get_mut(fid)) {
                f.busy = false;
            }
            log::info!("localsend: an upload failed: {e}");
            (500, message("The file could not be written"), false)
        }
    }
}

// -------------------------------------------------------------------------------------------------------------------- sending

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

async fn send_task(inner: &Arc<Inner>, tid: u64, dev: &LsDevice, paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let (items, sources) = tokio::task::spawn_blocking(move || crate::transfer::collect(&paths)).await??;
    // (folders go as their files; an empty folder or a link has nothing to send)
    let files: Vec<(String, String, u64, PathBuf)> = items
        .iter()
        .zip(sources)
        .filter_map(|(i, src)| src.filter(|_| i.is_file()).map(|p| (i.path.clone(), i.size, p)))
        .enumerate()
        .map(|(n, (name, size, p))| (n.to_string(), name, size, p))
        .collect();
    ensure!(!files.is_empty(), "there is nothing to send");
    let total: u64 = files.iter().map(|f| f.2).sum();
    let mut dto = serde_json::Map::new();
    for (id, name, size, _) in &files {
        dto.insert(id.clone(), serde_json::json!({ "id": id, "fileName": name, "size": size, "fileType": mime_of(name) }));
    }
    let body = serde_json::to_vec(&serde_json::json!({ "info": inner.info(None), "files": dto }))?;
    let path = format!("{API}/prepare-upload");
    let fp = Some(dev.fingerprint.as_str());
    let r = call(inner, dev.ip, dev.port, fp, "POST", &path, Some(body), DECISION + Duration::from_secs(20)).await.map_err(|e| anyhow!("{e:#}"))?;
    match r.status {
        200 => {}
        403 => bail!("the other device declined"),
        401 => bail!("the other device asks for a PIN"),
        409 => bail!("the other device is busy with another transfer"),
        s => bail!("the other device answered {s}"),
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Accepted {
        session_id: String,
        files: HashMap<String, String>,
    }
    let acc: Accepted = serde_json::from_slice(&r.body).context("the other device's answer was not understood")?;
    let done = Arc::new(AtomicU64::new(0));
    let mut last = Instant::now();
    for (id, _name, size, src) in &files {
        let Some(token) = acc.files.get(id) else { continue };
        let up = format!("{API}/upload?sessionId={}&fileId={id}&token={token}", acc.session_id);
        if let Err(e) = send_file(inner, tid, dev, &up, src, *size, &done, total, &mut last).await {
            let cancel = format!("{API}/cancel?sessionId={}", acc.session_id);
            let _ = call(inner, dev.ip, dev.port, fp, "POST", &cancel, None, CONNECT).await;
            return Err(e);
        }
    }
    inner.listener.progress(tid, total, total);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_file(inner: &Arc<Inner>, tid: u64, dev: &LsDevice, path: &str, src: &Path, size: u64, done: &Arc<AtomicU64>, total: u64, last: &mut Instant) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::open(src).await.with_context(|| format!("cannot read {}", src.display()))?;
    let mut s = open(dev.ip, dev.port, Some(&dev.fingerprint)).await?;
    write_request(&mut s, "POST", path, dev.ip, dev.port, Some(size)).await?;
    let mut buf = vec![0u8; BUFFER];
    let mut sent = 0u64;
    while sent < size {
        if inner.cancelled.lock().unwrap_or_else(|e| e.into_inner()).contains(&tid) {
            bail!("cancelled");
        }
        let want = ((size - sent) as usize).min(buf.len());
        let n = file.read(&mut buf[..want]).await?;
        ensure!(n > 0, "{} changed while it was sent", src.display());
        s.write_all(&buf[..n]).await?;
        sent += n as u64;
        let all = done.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            *last = Instant::now();
            inner.listener.progress(tid, all, total);
        }
    }
    s.flush().await?;
    let r = tokio::time::timeout(Duration::from_secs(60), read_response(&mut BufReader::new(s))).await.map_err(|_| anyhow!("the other device did not answer"))??;
    ensure!(r.status == 200, "the other device refused a file ({})", r.status);
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, Sender, channel};

    enum Ev {
        Devices(Vec<LsDevice>),
        Request(LsRequest),
        Done(LsResult),
    }

    struct L(Mutex<Sender<Ev>>);

    impl LsListener for L {
        fn devices_changed(&self, d: Vec<LsDevice>) {
            let _ = self.0.lock().unwrap().send(Ev::Devices(d));
        }
        fn request(&self, r: LsRequest) {
            let _ = self.0.lock().unwrap().send(Ev::Request(r));
        }
        fn progress(&self, _: u64, _: u64, _: u64) {}
        fn finished(&self, t: u64, r: LsResult) {
            let _ = (t, self.0.lock().unwrap().send(Ev::Done(r)));
        }
    }

    fn peer(dir: &Path, name: &str) -> (Arc<LocalSend>, Receiver<Ev>) {
        let (tx, rx) = channel();
        let cfg = LsConfig { alias: name.into(), model: "test".into(), state_dir: dir.join(name).to_string_lossy().into_owned(), port: 0, multicast: false, local_only: true, pin: None };
        (LocalSend::new(cfg, Arc::new(L(Mutex::new(tx)))).unwrap(), rx)
    }

    fn next(rx: &Receiver<Ev>) -> Ev {
        rx.recv_timeout(Duration::from_secs(20)).expect("an event")
    }

    /// Two peers on 127.0.0.1: B receives, A finds B by address and sends it `names` (each a file made under `src`); the request is
    /// answered with `dest` (or declined with None). Returns what B's end reported.
    fn exchange(names: &[(&str, &[u8])], dest: Option<&Path>, preexisting: &[(&str, &[u8])]) -> (Vec<LsRequest>, LsResult, LsResult, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.keep();
        let (a, arx) = peer(&tmp, "a");
        let (b, brx) = peer(&tmp, "b");
        let port = b.set_receiving(true).unwrap();
        a.add_device(SocketAddr::from(([127, 0, 0, 1], port)));
        let dev = loop {
            if let Ev::Devices(d) = next(&arx) {
                if let Some(d) = d.into_iter().next() {
                    break d;
                }
            }
        };
        assert_eq!(dev.fingerprint, b.fingerprint());
        let src = tmp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut paths = Vec::new();
        for (n, bytes) in names {
            std::fs::write(src.join(n), bytes).unwrap();
            paths.push(src.join(n));
        }
        let into = dest.map(Path::to_path_buf).unwrap_or_else(|| tmp.join("in"));
        std::fs::create_dir_all(&into).unwrap();
        for (n, bytes) in preexisting {
            std::fs::write(into.join(n), bytes).unwrap();
        }
        a.send(&dev.fingerprint, paths).unwrap();
        let mut requests = Vec::new();
        let recv = loop {
            match next(&brx) {
                Ev::Request(r) => {
                    match dest {
                        Some(_) => b.accept(r.id, into.clone()),
                        None => b.decline(r.id),
                    }
                    requests.push(r);
                    if dest.is_none() {
                        break LsResult::default();
                    }
                }
                Ev::Done(r) => break r,
                Ev::Devices(_) => {}
            }
        };
        let sent = loop {
            if let Ev::Done(r) = next(&arx) {
                break r;
            }
        };
        a.stop();
        b.stop();
        (requests, sent, recv, into)
    }

    #[test]
    fn a_file_sent_arrives_byte_identical_and_an_existing_one_is_kept() {
        let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("in");
        let (reqs, sent, recv, into) = exchange(&[("photo.bin", &bytes)], Some(&dest), &[("photo.bin", b"mine")]);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].total_bytes, bytes.len() as u64);
        assert!(sent.ok, "{sent:?}");
        assert!(recv.ok, "{recv:?}");
        assert_eq!(std::fs::read(into.join("photo.bin")).unwrap(), b"mine");
        assert_eq!(std::fs::read(into.join("photo (2).bin")).unwrap(), bytes);
        assert_eq!(recv.paths.len(), 1);
    }

    #[test]
    fn a_declined_request_writes_nothing() {
        let (reqs, sent, _recv, into) = exchange(&[("secret.txt", b"x")], None, &[]);
        assert_eq!(reqs.len(), 1);
        assert!(!sent.ok);
        assert!(sent.error.unwrap_or_default().contains("declined"));
        assert_eq!(std::fs::read_dir(&into).unwrap().count(), 0);
    }

    #[test]
    fn a_hostile_file_name_lands_inside_the_chosen_folder() {
        let dest = Path::new("/tmp/leaf-in");
        let names: Vec<String> = ["../../evil.txt", "..\\..\\evil2.txt", "/abs/olute.txt", "a/../../b.txt", "CON", "nul.txt", "x\u{202e}y.txt", ""].iter().map(|s| s.to_string()).collect();
        for t in plan_targets(dest, &names) {
            assert!(t.starts_with(dest), "{t:?}");
            assert!(t.components().all(|c| !matches!(c, std::path::Component::ParentDir)), "{t:?}");
        }
    }
}
