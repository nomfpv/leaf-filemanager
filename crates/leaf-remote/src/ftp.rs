//! `FtpFs`: `ftp://user@host:port/path` and `ftps://...` (W1c) through `leaf-vfs`, on `suppaftp`'s blocking client. Every
//! call runs on the caller's worker thread (no runtime of its own); TLS is rustls with ring, certificates checked against the
//! system's store.
//!
//! - `ftps://` is explicit TLS (`AUTH TLS` on the plain port, 21 by default); port 990 is implicit TLS. The data connections
//!   are encrypted too (`PROT P`). A certificate the system does not trust is refused and asked about: "Trust this
//!   certificate" keeps its SHA-256 fingerprint for that server (`ftps-trusted.txt` beside hosts.toml), "Trust once" for this
//!   run. Nothing is ever accepted without the user's answer.
//! - Passive mode; the data connection always goes to the address of the control connection (a PASV reply that names
//!   another machine is not followed). IPv6 servers get EPSV.
//! - Connections: one control connection per server (`scheme://user@host:port`) serves the listings and commands; a transfer
//!   that is running holds a connection of its own (FTP has one transfer per control connection), at most `MAX_CONNS` per
//!   server. A connection that broke is dropped and the operation tried once more on a new one (never a write); connections
//!   unused for `IDLE_AFTER` are closed when the server is next used. Every read and write on a socket has the request timeout.
//! - Login: the user of the address or of the saved host, else `anonymous`. The password is asked in leaf's dialog and kept
//!   in memory (wiped on drop) for this run, keyed by scheme, user, server and port (a password typed for `ftps` is never
//!   sent over plain `ftp`); it is forgotten when the server refuses it. It is never in an address, a log line, a temporary
//!   name or hosts.toml. (suppaftp writes the PASS line at trace level: the app caps that crate's log at warn, see
//!   [`QUIET_LOG_TARGETS`].)
//! - Writes: `Replace` stores into a hidden temporary name beside the file (`.name.leaf-up-<n>`) and renames it over the real
//!   name only after the server confirmed the whole upload; a cut upload leaves the real file as it was. The generic copy's
//!   part file (`.name.<n>.leaf-part`) and `CreateNew` are stored in place after checking that nothing has the name (FTP has
//!   no exclusive create: a name that appears between the check and STOR is overwritten; a window of one round trip).
//!   `WriteMode::At` (resume) appends (`APPE`) to a regular file whose size is exactly the offset; anything else is refused.
//! - A replacing rename moves the old file aside, renames the new one in and removes the old one (moved back when the second
//!   step fails); no server-specific "RNTO replaces" behaviour is relied on.
//! - Links: MLSD hides them on many servers (it reports the target's type). Where a link matters (remove, resume) the parent
//!   folder is also listed with LIST, whose `l` marks links; a recursive remove unlinks (DELE) a link and never enters it.
//! - Free space: not offered by FTP (`None`).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use leaf_vfs::{Caps, Details, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, VPath, WriteMode, WriteOpts, WriteSink};
use suppaftp::types::FileType as TransferType;
use suppaftp::{FtpError, Mode, RustlsConnector, RustlsFtpStream, Status, TlsStream, TransferStream};

use crate::conn::Config;
use crate::prompt::{CertQuestion, HostKeyAnswer, Prompter, Secret, SecretQuestion};
use crate::secrets::SecretId;

/// Log targets that write secrets at their most detailed level (suppaftp logs the `PASS` line at trace; the HTTP client's
/// requests carry the `Authorization` header of a WebDAV sign-in). The app drops every record of these (and of their modules)
/// above `warn`, whatever `RUST_LOG` says: see [`is_quiet`]. (Nothing up to `debug` carries a secret either: the tests check that.)
pub const QUIET_LOG_TARGETS: &[&str] = &["suppaftp", "ureq", "ureq_proto"];

/// Whether a log record must be dropped, whatever the filter says: above `warn` from a target in [`QUIET_LOG_TARGETS`] or a
/// module of one (`suppaftp::sync_ftp::control`). A filter like `RUST_LOG=suppaftp::sync_ftp::control=trace` cannot turn it
/// back on: the app's logger asks this before its filter.
pub fn is_quiet(target: &str, level: log::Level) -> bool {
    level > log::Level::Warn && QUIET_LOG_TARGETS.iter().any(|t| target == *t || target.strip_prefix(t).is_some_and(|rest| rest.starts_with("::")))
}

/// Connections per server at most (one for browsing, the rest for transfers running at the same time).
const MAX_CONNS: usize = 3;
/// A connection nothing used for this long is closed the next time the server is used.
const IDLE_AFTER: Duration = Duration::from_secs(4 * 60);
/// The file the certificates the user trusted are kept in (beside hosts.toml): `host:port SHA256:...` per line. No secret.
pub const TRUSTED_CERTS_FILE: &str = "ftps-trusted.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tls {
    None,
    Explicit,
    Implicit,
}

/// Who to log in as and where.
#[derive(Debug, Clone)]
struct Target {
    /// `scheme://user@host:port`: the pool's and the password's key.
    key: String,
    host: String,
    port: u16,
    user: String,
    tls: Tls,
    /// The saved host says "Through VPN".
    vpn: bool,
}

impl Target {
    /// `host:port` as the certificate file names it.
    fn cert_name(&self) -> String {
        format!("{}:{}", self.host.to_ascii_lowercase(), self.port)
    }
    /// `user@host` as the password dialog shows it.
    fn account(&self) -> String {
        if self.port == 21 || (self.tls == Tls::Implicit && self.port == 990) {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

fn target(cfg: &Config, v: &VPath) -> Result<Target, FsError> {
    let host = v.host().to_string();
    if host.is_empty() {
        return Err(FsError::Other("The address has no server name".into()));
    }
    if !crate::known_hosts::valid_host(&host) {
        return Err(FsError::Other("The server's name has characters a server name cannot have".into()));
    }
    let scheme = v.scheme().to_ascii_lowercase();
    let secure = match scheme.as_str() {
        "ftp" => false,
        "ftps" => true,
        _ => return Err(FsError::Unsupported),
    };
    let port = v.port().unwrap_or(21);
    let tls = match (secure, port) {
        (false, _) => Tls::None,
        (true, 990) => Tls::Implicit,
        (true, _) => Tls::Explicit,
    };
    let saved = cfg.config_dir.as_deref().map(rf_core::hosts::Hosts::load).unwrap_or_default();
    let h = saved.hosts.into_iter().find(|h| {
        h.scheme.eq_ignore_ascii_case(&scheme) && h.host.eq_ignore_ascii_case(&host) && h.port.unwrap_or(21) == port && (v.user().is_none() || h.user.as_deref() == v.user())
    });
    let vpn = h.as_ref().is_some_and(|h| h.vpn);
    let user = v.user().map(str::to_string).or_else(|| h.and_then(|h| h.user)).unwrap_or_else(|| "anonymous".into());
    Ok(Target { key: format!("{scheme}://{user}@{}:{port}", host.to_ascii_lowercase()), host, port, user, tls, vpn })
}

// ------------------------------------------------------------------------------------------------ connections

struct Session {
    s: RustlsFtpStream,
    /// The server has MLSD/MLST (FEAT says MLST).
    mlsx: bool,
    /// The folder the server logs the user into (`/~` in an address stands for it).
    home: String,
    /// SYST says Windows (IIS and the like): names are compared ignoring case.
    windows: bool,
    /// The server lists `SITE CHMOD` in its HELP SITE (asked once, when first needed).
    site_chmod: Option<bool>,
    last_used: Instant,
    /// The route it was made on (see `leaf_vpn::RouteMark`): an idle one made on another route than leaf would take now is
    /// closed, not lent out again.
    route: leaf_vpn::RouteMark,
}

impl Session {
    fn quit(mut self) {
        // (best effort, short: the socket has the request timeout)
        let _ = self.s.get_ref().set_read_timeout(Some(Duration::from_secs(2)));
        let _ = self.s.quit();
    }
}

#[derive(Default)]
struct ServerState {
    idle: Vec<Session>,
    /// Connections open now (idle or lent out).
    open: usize,
    /// Of those, lent to a reader (`open_read`): at most `MAX_CONNS - 1`, so one is always left for a write, a listing or a
    /// command. (A copy from this server to itself holds a read while it writes: three of them at once would otherwise hold
    /// every connection and each wait for one more.)
    reading: usize,
    home: Option<String>,
}

#[derive(Default)]
struct Server {
    state: Mutex<ServerState>,
    freed: Condvar,
    /// One connect (and its questions) at a time per server: a second pane waits for the first one's answer.
    connecting: Mutex<()>,
}

/// A connection lent out: back to the server's idle list when dropped, or closed when it broke.
struct Lease {
    server: Arc<Server>,
    sess: Option<Session>,
    broken: bool,
    /// Counted in `ServerState::reading`.
    reading: bool,
}

impl Lease {
    fn s(&mut self) -> &mut RustlsFtpStream {
        &mut self.sess.as_mut().expect("leased session").s
    }
    fn home(&self) -> &str {
        &self.sess.as_ref().expect("leased session").home
    }
    fn mlsx(&self) -> bool {
        self.sess.as_ref().is_some_and(|s| s.mlsx)
    }
    fn windows(&self) -> bool {
        self.sess.as_ref().is_some_and(|s| s.windows)
    }
    /// Does the server do `SITE CHMOD`? Its FEAT does not say (SITE commands are not features), so HELP SITE is read once per connection.
    fn site_chmod(&mut self) -> bool {
        if let Some(k) = self.sess.as_ref().and_then(|s| s.site_chmod) {
            return k;
        }
        let known = self
            .s()
            .custom_command("HELP SITE", &[Status::Help, Status::Ready, Status::CommandOk])
            .is_ok_and(|r| String::from_utf8_lossy(&r.body).to_ascii_uppercase().contains("CHMOD"));
        if let Some(s) = self.sess.as_mut() {
            s.site_chmod = Some(known);
        }
        known
    }
    /// Makes the socket fail at once (a transfer stopped half-way: its connection is not used again).
    fn cut(&mut self) {
        self.broken = true;
        if let Some(s) = &self.sess {
            let _ = s.s.get_ref().shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(mut sess) = self.sess.take() else { return };
        let mut st = lock(&self.server.state);
        if self.reading {
            st.reading = st.reading.saturating_sub(1);
        }
        if self.broken {
            st.open = st.open.saturating_sub(1);
        } else {
            sess.last_used = Instant::now();
            st.idle.push(sess);
        }
        drop(st);
        self.server.freed.notify_all();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What lives as long as leaf runs: the connections, the passwords and the certificates trusted once (memory only).
#[derive(Default)]
struct Shared {
    servers: Mutex<HashMap<String, Arc<Server>>>,
    /// `scheme://user@host:port` -> the password.
    passwords: Mutex<HashMap<String, Secret>>,
    /// `(host:port, fingerprint)` of certificates trusted for this run.
    trusted_once: Mutex<HashSet<(String, String)>>,
}

pub struct FtpFs {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: Config,
    prompter: Arc<dyn Prompter>,
    shared: Shared,
}

impl FtpFs {
    pub fn new(cfg: Config, prompter: Arc<dyn Prompter>) -> FtpFs {
        FtpFs { inner: Arc::new(Inner { cfg, prompter, shared: Shared::default() }) }
    }

    /// The folder the server logs the user into, as an address on the same server.
    pub fn home(&self, v: &VPath) -> Result<VPath, FsError> {
        let home = self.inner.op(v, &|| false, |l| Ok(l.home().to_string()))?;
        Ok(v.with_path(&home))
    }

    /// Forgets the passwords this run keeps in memory for the server `host:port` (both schemes, any user).
    pub fn forget_passwords(&self, host: &str, port: u16) {
        let suffix = format!("@{}:{port}", host.to_ascii_lowercase());
        lock(&self.inner.shared.passwords).retain(|k, _| !k.ends_with(&suffix));
    }

    /// Whether a connection to the server of `v` is open now (never connects).
    pub fn target_connected(&self, v: &VPath) -> bool {
        let Ok(t) = target(&self.inner.cfg, v) else { return false };
        let server = lock(&self.inner.shared.servers).get(&t.key).cloned();
        server.is_some_and(|s| lock(&s.state).open > 0)
    }

    /// Closes every connection (the tests' "the line dropped").
    #[cfg(test)]
    pub(crate) fn drop_connections(&self) {
        let servers: Vec<_> = lock(&self.inner.shared.servers).values().cloned().collect();
        for s in servers {
            let mut st = lock(&s.state);
            let n = st.idle.len();
            st.idle.clear();
            st.open = st.open.saturating_sub(n);
        }
    }
}

/// What went wrong with one request, told apart for the retry.
enum Fail {
    /// The connection broke (or the server stopped answering): the operation may be tried again on a new one.
    Lost(String),
    Fs(FsError),
}

impl From<FsError> for Fail {
    fn from(e: FsError) -> Fail {
        Fail::Fs(e)
    }
}

impl Inner {
    fn server(&self, t: &Target) -> Arc<Server> {
        lock(&self.shared.servers).entry(t.key.clone()).or_default().clone()
    }

    /// A connection to the server of `t`: an idle one, or a new one while there are fewer than `MAX_CONNS`; otherwise it
    /// waits for one to come back (polling `cancelled`).
    fn lease(&self, t: &Target, cancelled: &dyn Fn() -> bool) -> Result<Lease, FsError> {
        self.lease_for(t, cancelled, false)
    }

    /// `lease` for a reader: never the last connection that is not reading (see `ServerState::reading`).
    fn lease_for(&self, t: &Target, cancelled: &dyn Fn() -> bool, reading: bool) -> Result<Lease, FsError> {
        let server = self.server(t);
        loop {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            let mut st = lock(&server.state);
            if reading && st.reading >= MAX_CONNS - 1 {
                let _ = server.freed.wait_timeout(st, Duration::from_millis(50));
                continue;
            }
            // connections unused for long are closed (the server has likely dropped them anyway)
            // (and the ones made on another route than leaf would take now: leaf's VPN came up or went down, or the host's tick changed)
            let route = leaf_vpn::RouteMark::now(&t.host, t.vpn);
            let (fresh, stale): (Vec<Session>, Vec<Session>) = std::mem::take(&mut st.idle).into_iter().partition(|s| s.last_used.elapsed() < IDLE_AFTER && s.route.reusable(&route));
            st.idle = fresh;
            st.open = st.open.saturating_sub(stale.len());
            if let Some(sess) = st.idle.pop() {
                st.reading += reading as usize;
                drop(st);
                for s in stale {
                    s.quit();
                }
                return Ok(Lease { server, sess: Some(sess), broken: false, reading });
            }
            if st.open < MAX_CONNS {
                st.open += 1;
                st.reading += reading as usize;
                drop(st);
                for s in stale {
                    s.quit();
                }
                let made = {
                    let _gate = lock(&server.connecting);
                    self.connect(t)
                };
                return match made {
                    Ok(sess) => {
                        lock(&server.state).home = Some(sess.home.clone());
                        Ok(Lease { server, sess: Some(sess), broken: false, reading })
                    }
                    Err(e) => {
                        let mut st = lock(&server.state);
                        st.open -= 1;
                        st.reading -= reading as usize;
                        drop(st);
                        server.freed.notify_all();
                        Err(e)
                    }
                };
            }
            let _ = server.freed.wait_timeout(st, Duration::from_millis(50));
        }
    }

    /// Runs `f` on a connection of `v`'s server. A connection that broke is dropped and `f` tried once more on a new one:
    /// only for requests that change nothing or can be sent twice.
    fn op<T>(&self, v: &VPath, cancelled: &dyn Fn() -> bool, mut f: impl FnMut(&mut Lease) -> Result<T, Fail>) -> Result<T, FsError> {
        let t = target(&self.cfg, v)?;
        let mut tries = 0;
        loop {
            tries += 1;
            let mut l = self.lease(&t, cancelled)?;
            match f(&mut l) {
                Ok(x) => return Ok(x),
                Err(Fail::Fs(e)) => return Err(e),
                Err(Fail::Lost(why)) => {
                    l.cut();
                    if tries < 2 && !cancelled() {
                        log::info!("ftp: the connection to {} broke ({why}); connecting again", t.host);
                        continue;
                    }
                    return Err(FsError::Offline(why));
                }
            }
        }
    }

    /// Runs `f` once (a write: never sent twice).
    fn once<T>(&self, v: &VPath, f: impl FnOnce(&mut Lease) -> Result<T, Fail>) -> Result<T, FsError> {
        let t = target(&self.cfg, v)?;
        let mut l = self.lease(&t, &|| false)?;
        match f(&mut l) {
            Ok(x) => Ok(x),
            Err(Fail::Fs(e)) => Err(e),
            Err(Fail::Lost(why)) => {
                l.cut();
                Err(FsError::Offline(why))
            }
        }
    }

    fn connect(&self, t: &Target) -> Result<Session, FsError> {
        log::info!("ftp: connecting to {}:{} ({})", t.host, t.port, if t.tls == Tls::None { "not encrypted" } else { "TLS" });
        // a host set to use leaf's VPN: through it (the name is looked up through it too), held by its kill switch while it is
        // down, direct on the local network
        let through = t.vpn
            && match leaf_vpn::route(&t.host) {
                leaf_vpn::Route::Blocked => return Err(FsError::Offline(crate::conn::VPN_HELD.into())),
                leaf_vpn::Route::Tunnel => true,
                leaf_vpn::Route::Direct => false,
            };
        let addrs: Vec<SocketAddr> = if through {
            Vec::new()
        } else {
            (t.host.as_str(), t.port).to_socket_addrs().map_err(|_| FsError::Offline("the server's name has no address".into()))?.collect()
        };
        if addrs.is_empty() && !through {
            return Err(FsError::Offline("the server's name has no address".into()));
        }
        let mut tries = 0;
        loop {
            tries += 1;
            let check = self.cert_check(t)?;
            // (through the VPN: each try gets its own bridge, which takes one connection)
            let via = if through { Some(bridge(t, t.port, self.cfg.connect_timeout).map_err(|e| FsError::Offline(e.to_string()))?) } else { None };
            match self.open(t, &addrs, via, check.clone()) {
                Ok(s) => return self.login(t, s),
                Err(e) => {
                    // a certificate the system does not trust: asked (once per connect)
                    let refused = lock(&check.refusal).take();
                    match refused {
                        Some((fp, why)) if tries < 2 => self.ask_certificate(t, &check, fp, why)?,
                        Some((fp, why)) => {
                            return Err(FsError::Other(format!("The server's certificate is not trusted ({why}); leaf does not connect. It shows {fp}.")));
                        }
                        None => return Err(e),
                    }
                }
            }
        }
    }

    /// The control connection, TLS as the scheme says, with the timeouts on every socket.
    fn open(&self, t: &Target, addrs: &[SocketAddr], via: Option<leaf_vpn::Bridge>, check: Arc<CertCheck>) -> Result<RustlsFtpStream, FsError> {
        let (op, ct) = (self.cfg.op_timeout, self.cfg.connect_timeout);
        let through = via.is_some();
        let timeouts = move |s: &TcpStream| {
            let _ = s.set_read_timeout(Some(op));
            let _ = s.set_write_timeout(Some(op));
            let _ = s.set_nodelay(true);
        };
        let connector = || -> Result<RustlsConnector, FsError> { Ok(RustlsConnector::from(Arc::new(tls_config(check.clone())?))) };
        let s = match (t.tls, via) {
            (Tls::Implicit, Some(b)) => {
                // through the VPN: suppaftp connects to the bridge itself (no stream can be handed in), so no test connection
                // first: the bridge takes the first connection from this PC
                let a = b.any_local();
                let s = RustlsFtpStream::connect_secure_implicit(a, Timed { inner: connector()?, op }, &t.host).map_err(|e| connect_error(e, "connecting"))?;
                timeouts(&s.get_ref());
                s
            }
            (Tls::Implicit, None) => {
                // suppaftp connects itself here (no stream can be handed in): each address is first tried with the connect
                // timeout and the first one that answers is used; the socket gets its read and write timeouts in the TLS
                // connector, before the handshake and the greeting
                let mut last = FsError::Offline("no address".into());
                let mut made = None;
                for a in addrs {
                    if let Err(e) = connect_any(std::slice::from_ref(a), ct) {
                        last = e;
                        continue;
                    }
                    match RustlsFtpStream::connect_secure_implicit(*a, Timed { inner: connector()?, op }, &t.host) {
                        Ok(s) => {
                            made = Some(s);
                            break;
                        }
                        Err(e) => last = connect_error(e, "connecting"),
                    }
                }
                let s = made.ok_or(last)?;
                timeouts(&s.get_ref());
                s
            }
            (Tls::None | Tls::Explicit, via) => {
                let tcp = match via {
                    Some(b) => b.connect(ct).map_err(|e| FsError::Offline(io_words(&e)))?,
                    None => connect_any(addrs, ct)?,
                };
                timeouts(&tcp);
                let s = RustlsFtpStream::connect_with_stream(tcp).map_err(|e| connect_error(e, "greeting"))?;
                if t.tls == Tls::Explicit {
                    s.into_secure(connector()?, &t.host).map_err(|e| connect_error(e, "starting TLS"))?
                } else {
                    s
                }
            }
        };
        let ipv6 = s.get_ref().peer_addr().is_ok_and(|a| a.is_ipv6());
        let vt = t.clone();
        let mut s = s.passive_stream_builder(move |addr| {
            // through the VPN the control connection's peer is the bridge on this PC: the data connection goes through a bridge
            // of its own to the server's name, at the port the server named; only our own socket is let through it
            let d = if through {
                bridge(&vt, addr.port(), ct).and_then(|b| b.connect(ct)).map_err(FtpError::ConnectionError)?
            } else {
                TcpStream::connect_timeout(&addr, ct).map_err(FtpError::ConnectionError)?
            };
            timeouts(&d);
            Ok(d)
        });
        if ipv6 {
            s.set_mode(Mode::ExtendedPassive);
        } else {
            s.set_mode(Mode::Passive);
            // the data connection goes to the control connection's address, never to the one a PASV reply names
            s.set_passive_nat_workaround(true);
        }
        Ok(s)
    }

    fn login(&self, t: &Target, mut s: RustlsFtpStream) -> Result<Session, FsError> {
        let anonymous = t.user.eq_ignore_ascii_case("anonymous") || t.user.eq_ignore_ascii_case("ftp");
        // a password saved for ftps is never sent over plain ftp: the scheme is part of the keyring's name for it
        let id = SecretId::password(if t.tls == Tls::None { "ftp" } else { "ftps" }, &t.user, &t.host, t.port);
        let vault = &self.cfg.vault;
        let mut retry = false;
        let mut tries = 0;
        loop {
            tries += 1;
            let known = if anonymous || retry { None } else { lock(&self.shared.passwords).get(&t.key).cloned().or_else(|| vault.get(&id)) };
            let pw: Secret = if anonymous {
                Secret::new("anonymous@".into())
            } else if let Some(p) = known {
                p
            } else {
                let q = SecretQuestion::Password { account: t.account(), retry, unencrypted: t.tls == Tls::None, keyring: vault.state() };
                crate::prompt::ask_secret_blocking(&*self.prompter, q).ok_or(FsError::Cancelled)?
            };
            match s.login(t.user.as_str(), pw.as_str()) {
                Ok(()) => {
                    if !anonymous {
                        vault.keep_if_asked(&id, &pw);
                        lock(&self.shared.passwords).insert(t.key.clone(), pw.remembered(false));
                    }
                    break;
                }
                Err(FtpError::UnexpectedResponse(r)) if matches!(r.status.code(), 530 | 331 | 332) => {
                    lock(&self.shared.passwords).remove(&t.key);
                    if !anonymous {
                        // a saved password the server refuses is deleted from the keyring (then asked)
                        vault.refused(&id, &pw);
                    }
                    log::info!("ftp: {} refused the login of {}", t.host, t.user);
                    if anonymous {
                        return Err(FsError::Auth("the server does not take anonymous logins; put the user name in the address (ftp://user@host)".into()));
                    }
                    if tries >= 3 {
                        return Err(FsError::Auth("the user name or password is wrong".into()));
                    }
                    retry = true;
                }
                Err(e) => return Err(connect_error(e, "logging in")),
            }
        }
        // binary transfers (the default, ASCII, would change line ends); names in UTF-8 where the server can
        s.transfer_type(TransferType::Binary).map_err(|e| connect_error(e, "setting binary mode"))?;
        let feat = s.feat().unwrap_or_default();
        let mlsx = feat.keys().any(|k| k.eq_ignore_ascii_case("MLST"));
        if feat.keys().any(|k| k.eq_ignore_ascii_case("UTF8")) {
            let _ = s.opts("UTF8", Some("ON"));
        }
        let home = s.pwd().ok().filter(|h| h.starts_with('/')).unwrap_or_else(|| "/".into());
        let windows = s.custom_command("SYST", &[Status::Name]).is_ok_and(|r| String::from_utf8_lossy(&r.body).to_ascii_lowercase().contains("windows"));
        log::info!("ftp: connected to {} (MLSD {mlsx}, Windows {windows})", t.host);
        Ok(Session { s, mlsx, home, windows, site_chmod: None, last_used: Instant::now(), route: leaf_vpn::RouteMark::now(&t.host, t.vpn) })
    }

    // -------------------------------------------------------------------------------------------- certificates

    fn trusted(&self, t: &Target) -> Vec<String> {
        trusted_certs(&self.cfg, &self.shared.trusted_once, &t.cert_name())
    }

    fn cert_check(&self, t: &Target) -> Result<Arc<CertCheck>, FsError> {
        new_cert_check(self.trusted(t))
    }

    fn ask_certificate(&self, t: &Target, check: &CertCheck, fp: String, why: String) -> Result<(), FsError> {
        ask_cert(&self.cfg, &*self.prompter, &self.shared.trusted_once, &t.cert_name(), check, fp, why, "ftp")
    }
}

/// The fingerprints trusted for the server `name` (`host:port`): remembered in the file (beside hosts.toml), and trusted once in
/// this run (`once`). Shared by FTPS and WebDAV over https.
pub(crate) fn trusted_certs(cfg: &Config, once: &Mutex<HashSet<(String, String)>>, name: &str) -> Vec<String> {
    let mut v: Vec<String> = cfg
        .config_dir
        .as_ref()
        .map(|d| d.join(TRUSTED_CERTS_FILE))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            text.lines()
                .filter_map(|l| l.split_once(' '))
                .filter(|(h, _)| h.eq_ignore_ascii_case(name))
                .map(|(_, fp)| fp.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.extend(lock(once).iter().filter(|(h, _)| h == name).map(|(_, fp)| fp.clone()));
    v
}

/// The system's certificate check with the fingerprints in `trusted` as the only way past a refusal.
pub(crate) fn new_cert_check(trusted: Vec<String>) -> Result<Arc<CertCheck>, FsError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let system = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| FsError::Other(format!("The system's certificates cannot be read ({e})")))?;
    Ok(Arc::new(CertCheck { system: Arc::new(system), provider, trusted, refusal: Mutex::new(None) }))
}

/// Asks the user about a certificate the system does not trust (`check` is the refused connection's, `fp` and `why` what it
/// recorded). `Ok`: the answer was "trust"; the caller connects again with a new check. Never accepts without an answer.
pub(crate) fn ask_cert(
    cfg: &Config,
    prompter: &dyn Prompter,
    once: &Mutex<HashSet<(String, String)>>,
    name: &str,
    check: &CertCheck,
    fp: String,
    why: String,
    tag: &str,
) -> Result<(), FsError> {
    let changed = !check.trusted.is_empty();
    let q = CertQuestion { host: name.to_string(), fingerprint: fp.clone(), reason: why, trusted_before: check.trusted.clone() };
    log::info!("{tag}: the certificate of {} is not trusted by the system{}; asking", q.host, if changed { " and not the one trusted before" } else { "" });
    let file = cfg.config_dir.as_ref().map(|d| d.join(TRUSTED_CERTS_FILE));
    match crate::prompt::ask_certificate_blocking(prompter, q) {
        HostKeyAnswer::TrustAndRemember => {
            // a new certificate replaces the ones trusted for this server before (in the file and in this run)
            if let Some(p) = file {
                let kept: String = std::fs::read_to_string(&p)
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.split_once(' ').is_some_and(|(h, _)| h.eq_ignore_ascii_case(name)))
                    .map(|l| format!("{l}\n"))
                    .collect();
                if let Err(e) = std::fs::write(&p, format!("{kept}{name} {fp}\n")) {
                    log::warn!("{tag}: could not write {}: {e} (trusted for this run only)", p.display());
                }
            }
            let mut once = lock(once);
            once.retain(|(h, _)| h != name);
            once.insert((name.to_string(), fp));
            Ok(())
        }
        // (never for a certificate that replaces a trusted one: the dialog does not offer it)
        HostKeyAnswer::TrustOnce if changed => Err(FsError::Cancelled),
        HostKeyAnswer::TrustOnce => {
            lock(once).insert((name.to_string(), fp));
            Ok(())
        }
        HostKeyAnswer::Cancel => Err(FsError::Cancelled),
    }
}

/// The TLS connector with the request timeouts set on the socket before the handshake (implicit TLS, where suppaftp opens the
/// socket itself).
#[derive(Debug)]
struct Timed {
    inner: RustlsConnector,
    op: Duration,
}

impl suppaftp::TlsConnector for Timed {
    type Stream = <RustlsConnector as suppaftp::TlsConnector>::Stream;
    fn connect(&self, domain: &str, stream: TcpStream) -> suppaftp::FtpResult<Self::Stream> {
        let _ = stream.set_read_timeout(Some(self.op));
        let _ = stream.set_write_timeout(Some(self.op));
        let _ = stream.set_nodelay(true);
        self.inner.connect(domain, stream)
    }
}

/// A connection through leaf's VPN as a socket on this PC (the FTP client takes only a plain socket): see
/// `leaf_vpn::loopback_bridge`.
fn bridge(t: &Target, port: u16, limit: Duration) -> std::io::Result<leaf_vpn::Bridge> {
    let r = leaf_vpn::loopback_bridge(&t.host, port, limit);
    if let Err(e) = &r
        && leaf_vpn::is_blocked(e)
    {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, crate::conn::VPN_HELD));
    }
    r
}

fn connect_any(addrs: &[SocketAddr], limit: Duration) -> Result<TcpStream, FsError> {
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(a, limit) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(FsError::Offline(match last {
        Some(e) if e.kind() == std::io::ErrorKind::TimedOut => "no answer".into(),
        Some(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => "the server does not take FTP connections on this port".into(),
        Some(e) => format!("the connection failed ({e})"),
        None => "no address".into(),
    }))
}

fn connect_error(e: FtpError, step: &str) -> FsError {
    match e {
        FtpError::ConnectionError(io) => FsError::Offline(io_words(&io)),
        FtpError::SecureError(m) => FsError::Other(format!("The secure connection could not be made while {step}. Details: {m}")),
        FtpError::UnexpectedResponse(r) if r.status.code() == 421 => FsError::Offline("the server is busy or closed the connection".into()),
        FtpError::UnexpectedResponse(r) if step == "starting TLS" => {
            FsError::Other(format!("The server does not offer TLS (ftps://); connect with ftp:// only if you accept an unencrypted connection. Details: {}", reply_text(&r)))
        }
        e => FsError::Other(format!("The server answered wrongly while {step}. Details: {e}")),
    }
}

fn io_words(io: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match io.kind() {
        K::TimedOut | K::WouldBlock => "the server did not answer in time".into(),
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::UnexpectedEof | K::NotConnected => "the connection was closed".into(),
        _ => format!("the connection failed ({io})"),
    }
}

fn reply_text(r: &suppaftp::types::Response) -> String {
    let t = String::from_utf8_lossy(&r.body);
    let t = t.trim();
    let mut s: String = t.chars().take(200).collect();
    if s.len() < t.len() {
        s.push('…');
    }
    s
}

/// A server's answer as leaf says it; the server's own text only as the details.
fn fail_of(e: FtpError) -> Fail {
    match e {
        FtpError::ConnectionError(io) => Fail::Lost(io_words(&io)),
        FtpError::UnexpectedResponse(r) => {
            let code = r.status.code();
            let body = reply_text(&r);
            let lower = body.to_lowercase();
            Fail::Fs(match code {
                421 => return Fail::Lost("the server closed the connection".into()),
                425 | 426 => FsError::Other(format!("The transfer broke off. Details: {body}")),
                530 | 532 => FsError::Auth(body),
                550 if lower.contains("exist") && !lower.contains("not exist") && !lower.contains("no such") => FsError::Exists,
                550 if lower.contains("denied") || lower.contains("permission") || lower.contains("not allowed") => FsError::Denied,
                550 | 450 => FsError::NotFound,
                553 => FsError::Other(format!("The server does not take this name. Details: {body}")),
                452 | 552 => FsError::Other(format!("The server has no room for this. Details: {body}")),
                500 | 501 | 502 | 504 => FsError::Unsupported,
                _ => FsError::Other(format!("The server refused this ({code}). Details: {body}")),
            })
        }
        FtpError::DataConnectionAlreadyOpen => Fail::Fs(FsError::Other("Another transfer is still running on this connection".into())),
        FtpError::SecureError(m) => Fail::Lost(format!("the secure connection failed ({m})")),
        e => Fail::Fs(FsError::Other(format!("The server answered wrongly. Details: {e}"))),
    }
}

trait FtpResultExt<T> {
    fn ftp(self) -> Result<T, Fail>;
}
impl<T> FtpResultExt<T> for Result<T, FtpError> {
    fn ftp(self) -> Result<T, Fail> {
        self.map_err(fail_of)
    }
}

/// The system's certificate check, with the fingerprints the user trusted for this server as the only way past a refusal.
#[derive(Debug)]
pub(crate) struct CertCheck {
    system: Arc<rustls_platform_verifier::Verifier>,
    provider: Arc<rustls::crypto::CryptoProvider>,
    pub(crate) trusted: Vec<String>,
    /// The fingerprint and the reason of the last refusal (asked about after the connect failed).
    pub(crate) refusal: Mutex<Option<(String, String)>>,
}

pub(crate) fn tls_config(check: Arc<CertCheck>) -> Result<rustls::ClientConfig, FsError> {
    let provider = check.provider.clone();
    let c = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| FsError::Other(format!("TLS could not be set up ({e})")))?
        .dangerous()
        .with_custom_certificate_verifier(check)
        .with_no_client_auth();
    Ok(c)
}

/// `SHA256:<base64>` of the certificate (as the dialog and the file write it).
pub(crate) fn cert_fingerprint(der: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha2::Sha256::digest(der)))
}

fn cert_words(e: &rustls::Error) -> String {
    use rustls::CertificateError as C;
    match e {
        rustls::Error::InvalidCertificate(c) => match c {
            C::UnknownIssuer => "it is not signed by an authority this PC trusts; it may be self-signed".into(),
            C::NotValidForName | C::NotValidForNameContext { .. } => "it is made out for another server name".into(),
            C::Expired | C::ExpiredContext { .. } => "it has expired".into(),
            C::NotValidYet | C::NotValidYetContext { .. } => "it is not valid yet".into(),
            C::Revoked => "it is revoked".into(),
            C::BadSignature => "its signature is wrong".into(),
            _ => "it could not be checked".into(),
        },
        _ => "it could not be checked".into(),
    }
}

impl rustls::client::danger::ServerCertVerifier for CertCheck {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self.system.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(v) => Ok(v),
            Err(e) => {
                let fp = cert_fingerprint(end_entity.as_ref());
                if self.trusted.contains(&fp) {
                    return Ok(rustls::client::danger::ServerCertVerified::assertion());
                }
                *lock(&self.refusal) = Some((fp, cert_words(&e)));
                Err(e)
            }
        }
    }

    // the signatures are always checked (a trusted fingerprint skips the chain, never the proof of the key)
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

// ------------------------------------------------------------------------------------------------ listings

/// The path as the server takes it: `/~` and `/~/...` stand for the login folder.
fn spath(home: &str, v: &VPath) -> String {
    let p = v.path();
    match p.strip_prefix("/~") {
        Some("") => home.to_string(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.trim_end_matches('/')),
        _ => p.to_string(),
    }
}

/// One line of a listing, read.
#[derive(Debug, Clone)]
struct Item {
    name: String,
    kind: Kind,
    size: u64,
    modified_ms: i64,
    mode: Option<u32>,
}

impl Item {
    fn entry(&self) -> Entry {
        let m = self.modified_ms;
        let mut e = match self.kind {
            Kind::Dir => Entry::new(&self.name, EntryKind::Dir, 0, m, m, rf_core::attrs::DIRECTORY),
            Kind::File => Entry::new(&self.name, EntryKind::File, self.size, m, m, 0),
            Kind::Link => {
                let mut e = Entry::new(&self.name, EntryKind::File, 0, m, m, rf_core::attrs::REPARSE_POINT);
                e.link = true;
                e
            }
        };
        e.mode = self.mode.map_or(0, |p| (p & 0o7777) as u16);
        e
    }
    fn meta(&self) -> Meta {
        Meta { kind: self.kind, size: self.size, modified_ms: self.modified_ms, created_ms: 0, mode: self.mode }
    }
}

/// An MLSD/MLST line (RFC 3659): `fact=value;fact=value; name`. `None` for the folder itself and its parent (`cdir`, `pdir`)
/// and for lines that cannot be read.
fn parse_mlsx(line: &str) -> Option<Item> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (facts, name) = match line.strip_prefix(' ') {
        Some(n) => ("", n),
        None => {
            let i = line.find("; ")?;
            (&line[..=i], &line[i + 2..])
        }
    };
    let mut it = Item { name: name.to_string(), kind: Kind::File, size: 0, modified_ms: 0, mode: None };
    for f in facts.split(';') {
        let Some((k, v)) = f.split_once('=') else { continue };
        match k.to_ascii_lowercase().as_str() {
            "type" => {
                let v = v.to_ascii_lowercase();
                it.kind = match v.as_str() {
                    "cdir" | "pdir" => return None,
                    "dir" => Kind::Dir,
                    "file" => Kind::File,
                    // `OS.unix=symlink`, `OS.unix=slink:<target>`, `link`
                    v if v.contains("link") => Kind::Link,
                    _ => Kind::File,
                };
            }
            "size" => it.size = v.parse().unwrap_or(0),
            "modify" => it.modified_ms = mlsx_time(v).unwrap_or(0),
            "unix.mode" => it.mode = u32::from_str_radix(v.trim_start_matches('0'), 8).ok().or(Some(0)),
            _ => {}
        }
    }
    Some(it)
}

/// `YYYYMMDDHHMMSS[.sss]` (UTC) as Unix milliseconds.
fn mlsx_time(v: &str) -> Option<i64> {
    let (main, frac) = v.split_once('.').unwrap_or((v, ""));
    let t = chrono::NaiveDateTime::parse_from_str(main, "%Y%m%d%H%M%S").ok()?;
    let ms: i64 = frac.chars().chain("000".chars()).take(3).collect::<String>().parse().unwrap_or(0);
    Some(t.and_utc().timestamp_millis() + ms)
}

/// A LIST line (`ls -l` style, or the DOS style of IIS).
fn parse_list(line: &str) -> Option<Item> {
    let f = suppaftp::list::ListParser::parse_posix(line).or_else(|_| suppaftp::list::ListParser::parse_dos(line)).ok()?;
    let kind = if f.is_symlink() {
        Kind::Link
    } else if f.is_directory() {
        Kind::Dir
    } else {
        Kind::File
    };
    let mut name = f.name().to_string();
    if kind == Kind::Link
        && let Some((n, _)) = name.split_once(" -> ")
    {
        name = n.to_string();
    }
    let modified_ms = f.modified().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    Some(Item { name, kind, size: if kind == Kind::File { f.size() as u64 } else { 0 }, modified_ms, mode: None })
}

/// A listing's lines may be this long, and the whole listing this big (a server that sends without end does not fill the
/// memory).
const LINE_MAX: usize = 16 << 10;
const LISTING_MAX: usize = 64 << 20;

/// `MLSD path` or `LIST path`, read line by line within `LINE_MAX` and `LISTING_MAX`. Past a limit the connection is cut and
/// the error says so in leaf's words.
fn read_listing(l: &mut Lease, verb: &str, path: &str) -> Result<Vec<String>, Fail> {
    use std::io::BufRead as _;
    let (_, stream) = l.s().custom_data_command(format!("{verb} {path}"), &[Status::AboutToSend, Status::AlreadyOpen]).ftp()?;
    let mut rd = std::io::BufReader::new(stream);
    let mut lines = Vec::new();
    let mut total = 0usize;
    let mut buf = Vec::new();
    // (the transfer is not at its end: the connection is closed, not used again)
    let too_big = |l: &mut Lease, what: &str| {
        l.cut();
        Fail::Fs(FsError::Other(format!("The server's listing of this folder has {what} than leaf reads ({} MB at most)", LISTING_MAX >> 20)))
    };
    loop {
        buf.clear();
        let n = (&mut rd).take(LINE_MAX as u64 + 1).read_until(b'\n', &mut buf).map_err(|e| Fail::Lost(io_words(&e)))?;
        if n == 0 {
            break;
        }
        if buf.len() > LINE_MAX && !buf.ends_with(b"\n") {
            return Err(too_big(l, "a line longer"));
        }
        total += n;
        if total > LISTING_MAX {
            return Err(too_big(l, "more"));
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if !line.is_empty() {
            lines.push(String::from_utf8_lossy(line).into_owned());
        }
    }
    rd.into_inner().finish().ftp()?;
    Ok(lines)
}

/// The items of folder `path` (a server path): MLSD where the server has it, else LIST. Names that cannot be one part of an
/// address are left out (counted in the log).
fn list_items(l: &mut Lease, path: &str, use_list: bool) -> Result<Vec<Item>, Fail> {
    let mlsd = l.mlsx() && !use_list;
    let lines = if mlsd {
        match read_listing(l, "MLSD", path) {
            Ok(lines) => lines,
            // (500, 501, 502, 504: the server has no MLSD after all)
            Err(Fail::Fs(FsError::Unsupported)) => {
                if let Some(s) = l.sess.as_mut() {
                    s.mlsx = false;
                }
                return list_items(l, path, true);
            }
            Err(e) => return Err(e),
        }
    } else {
        read_listing(l, "LIST", path)?
    };
    let mut out = Vec::with_capacity(lines.len());
    let mut left_out = 0usize;
    for line in &lines {
        let item = if mlsd { parse_mlsx(line) } else { parse_list(line) };
        let Some(item) = item else { continue };
        // (a server that sends a path instead of a name: its last part)
        let name = if mlsd && item.name.contains('/') { item.name.rsplit('/').next().unwrap_or("").to_string() } else { item.name.clone() };
        match leaf_vfs::names::address_problem(&name) {
            None => out.push(Item { name, ..item }),
            Some(_) if name == "." || name == ".." => {}
            Some(why) => {
                left_out += 1;
                log::debug!("ftp: a name in the listing left out: {why}");
            }
        }
    }
    if left_out > 0 {
        log::warn!("ftp: {left_out} names in a listing cannot be used (empty, . or .., / or \\, NUL) and are left out");
    }
    Ok(out)
}

/// The names in folder `path` that LIST marks as links (`l`): MLSD shows many links as what they point to.
fn links_in(l: &mut Lease, path: &str) -> Result<HashSet<String>, Fail> {
    Ok(list_items(l, path, true)?.into_iter().filter(|i| i.kind == Kind::Link).map(|i| i.name).collect())
}

/// What is at `path` (a server path), `None` when nothing is. MLST where the server has it, else the parent's listing.
fn stat_on(l: &mut Lease, path: &str) -> Result<Option<Item>, Fail> {
    if path == "/" {
        return Ok(Some(Item { name: String::new(), kind: Kind::Dir, size: 0, modified_ms: 0, mode: None }));
    }
    if l.mlsx() {
        match l.s().mlst(Some(path)) {
            Ok(line) => {
                // (MLST answers with the path; `cdir` when asked about a folder on some servers)
                let line = line.trim_start();
                let fixed = line.replacen("type=cdir", "type=dir", 1).replacen("type=pdir", "type=dir", 1);
                return Ok(parse_mlsx(&fixed).map(|i| Item { name: base_name(path).to_string(), ..i }));
            }
            Err(FtpError::UnexpectedResponse(r)) if matches!(r.status.code(), 550 | 450) => return Ok(None),
            Err(FtpError::UnexpectedResponse(r)) if matches!(r.status.code(), 500 | 501 | 502 | 504) => {}
            Err(e) => return Err(fail_of(e)),
        }
    }
    let (dir, name) = split(path);
    match list_items(l, dir, true) {
        Ok(items) => Ok(items.into_iter().find(|i| i.name == name)),
        Err(Fail::Fs(FsError::NotFound)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Like `stat_on`, but a link that MLST shows as its target is a link here (the parent's LIST says so), and a link whose
/// target is gone (MLST: 550) is a link too, not "nothing there". Without MLST, `stat_on` read the LIST already.
fn lstat_on(l: &mut Lease, path: &str) -> Result<Option<Item>, Fail> {
    if path == "/" || !l.mlsx() {
        return stat_on(l, path);
    }
    let it = stat_on(l, path)?;
    if it.as_ref().is_some_and(|i| i.kind == Kind::Link) {
        return Ok(it);
    }
    let (dir, name) = split(path);
    let listed = match list_items(l, dir, true) {
        Ok(items) => items.into_iter().find(|i| i.name == name),
        // (a parent the user cannot list: what MLST said)
        Err(Fail::Fs(FsError::NotFound | FsError::Denied)) => return Ok(it),
        Err(e) => return Err(e),
    };
    Ok(match (it, listed) {
        (Some(mut it), Some(li)) if li.kind == Kind::Link => {
            it.kind = Kind::Link;
            Some(it)
        }
        (None, Some(li)) if li.kind == Kind::Link => Some(li),
        (it, _) => it,
    })
}

/// `("/a/b", "c")` of `/a/b/c`; `("/", "c")` of `/c`.
fn split(path: &str) -> (&str, &str) {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", n)) => ("/", n),
        Some((d, n)) => (d, n),
        None => ("/", path),
    }
}

fn base_name(path: &str) -> &str {
    split(path).1
}

fn child(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

/// A hidden name beside `name` that nothing else uses: `.name.<tag>-<n>` (no secret in it: the process id, a counter, the time).
pub(crate) fn side_name(name: &str, tag: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut short = name;
    while short.len() > 200 {
        let mut cut = short.len() - 1;
        while !short.is_char_boundary(cut) {
            cut -= 1;
        }
        short = &short[..cut];
    }
    format!(".{short}.{tag}-{:x}{n:x}{nanos:x}", std::process::id())
}

/// The generic copy's temporary name (`.name.<n>.leaf-part`): such a file is written in place.
pub(crate) fn is_part_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".leaf-part")
}

/// How a move into place failed.
enum PlaceFail {
    /// Nothing changed: the name has its old file (or nothing), `from` is where it was.
    Clean(Fail),
    /// The old file may have been moved aside: the error names where both versions may be now. Nothing may be removed.
    Left(FsError),
}

impl From<Fail> for PlaceFail {
    fn from(f: Fail) -> PlaceFail {
        PlaceFail::Clean(f)
    }
}

fn fail_text(f: &Fail) -> String {
    match f {
        Fail::Lost(m) => m.clone(),
        Fail::Fs(e) => e.to_string(),
    }
}

/// RNFR then RNTO. The error says whether RNTO was sent: after that the server may have moved it even when the answer
/// did not arrive.
fn rnfr_rnto(l: &mut Lease, from: &str, to: &str) -> Result<(), (Fail, bool)> {
    l.s().custom_command(format!("RNFR {from}"), &[Status::RequestFilePending]).map_err(|e| (fail_of(e), false))?;
    l.s().custom_command(format!("RNTO {to}"), &[Status::RequestedFileActionOk]).map(|_| ()).map_err(|e| (fail_of(e), true))
}

/// Moves `from` to `to` (server paths) on this connection. Nothing at `to`: a plain rename. A file at `to`: `Exists` unless
/// `replace`; then the old one is moved aside, the new one renamed in and the old one removed (moved back when the second
/// step fails). A folder at `to` is never replaced. A failure once the old one may have left its name is `Left`, naming the
/// aside name and `from`; a connection that broke is cut.
fn put_in_place(l: &mut Lease, from: &str, to: &str, replace: bool) -> Result<(), PlaceFail> {
    let Some(old) = stat_on(l, to)? else {
        return rnfr_rnto(l, from, to).map_err(|(f, _)| PlaceFail::Clean(f));
    };
    if !replace || old.kind == Kind::Dir {
        return Err(PlaceFail::Clean(Fail::Fs(FsError::Exists)));
    }
    let (dir, name) = split(to);
    let aside = child(dir, &side_name(name, "leaf-old"));
    let left = |why: String| {
        log::warn!("ftp: a replace stopped half-way ({why}); the old version may be kept as an aside name");
        PlaceFail::Left(FsError::Other(format!(
            "The new version could not take the name {name} ({why}). The old version may now be kept on the server as {aside}, and the new one as {from}"
        )))
    };
    // 1. the old one aside
    match rnfr_rnto(l, to, &aside) {
        Ok(()) => {}
        Err((f, false)) => return Err(PlaceFail::Clean(f)),
        Err((Fail::Fs(e), true)) => {
            // refused: the old one is still under its name when the server says so
            let still = matches!(stat_on(l, to), Ok(Some(_))) && matches!(stat_on(l, &aside), Ok(None));
            if still {
                return Err(PlaceFail::Clean(Fail::Fs(e)));
            }
            return Err(left(e.to_string()));
        }
        Err((lost, true)) => {
            l.cut();
            return Err(left(fail_text(&lost)));
        }
    }
    // 2. the new one in
    match rnfr_rnto(l, from, to) {
        Ok(()) => {
            if let Err(Fail::Fs(e)) = l.s().rm(aside.as_str()).ftp() {
                log::warn!("ftp: the old version could not be removed after a replace ({e}); it stays beside the new one");
            }
            Ok(())
        }
        Err((f @ Fail::Fs(_), _)) => {
            // the old one back under its name
            match rnfr_rnto(l, &aside, to) {
                Ok(()) => Err(PlaceFail::Clean(f)),
                Err((back, _)) => {
                    if let Fail::Lost(_) = back {
                        l.cut();
                    }
                    Err(left(format!("{}; putting the old version back failed too: {}", fail_text(&f), fail_text(&back))))
                }
            }
        }
        Err((lost, _)) => {
            l.cut();
            Err(left(fail_text(&lost)))
        }
    }
}

impl FtpFs {
    /// A folder and everything in it. A name that LIST marks as a link is unlinked (DELE), never entered; a folder is
    /// entered only after the server shows it under the path asked for (CWD + PWD), so a link that both listings show as a
    /// folder is not followed either on servers that report the real path.
    fn remove_tree(&self, l: &mut Lease, dir: &str, depth: usize) -> Result<(), Fail> {
        if depth >= leaf_vfs::copy::MAX_DEPTH {
            return Err(Fail::Fs(leaf_vfs::copy::too_deep()));
        }
        let items = list_items(l, dir, false)?;
        let links = if l.mlsx() { links_in(l, dir)? } else { items.iter().filter(|i| i.kind == Kind::Link).map(|i| i.name.clone()).collect() };
        for it in items {
            let path = child(dir, &it.name);
            if it.kind == Kind::Dir && !links.contains(&it.name) && really_folder(l, &path)? {
                self.remove_tree(l, &path, depth + 1)?;
            } else {
                // a file, or a link (DELE unlinks it; its target stays)
                l.s().rm(path.as_str()).ftp()?;
            }
        }
        rmdir(l, dir)
    }
}

/// Whether `path` is a folder the server shows under that very path (CWD there, then PWD). A server that resolves links in
/// PWD shows a link's target path instead: then it is not entered.
fn really_folder(l: &mut Lease, path: &str) -> Result<bool, Fail> {
    match l.s().cwd(path) {
        Ok(()) => {}
        Err(FtpError::UnexpectedResponse(r)) if r.status.code() == 550 => return Ok(false),
        Err(e) => return Err(fail_of(e)),
    }
    let pwd = l.s().pwd().ftp()?;
    let _ = l.s().cwd("/");
    Ok(pwd.trim_end_matches('/') == path.trim_end_matches('/'))
}

fn rmdir(l: &mut Lease, dir: &str) -> Result<(), Fail> {
    match l.s().rmdir(dir).ftp() {
        Ok(()) => Ok(()),
        Err(Fail::Fs(e @ (FsError::NotFound | FsError::Other(_) | FsError::Exists | FsError::Denied))) => {
            let any = list_items(l, dir, false).is_ok_and(|v| !v.is_empty());
            Err(Fail::Fs(if any { FsError::NotEmpty } else { e }))
        }
        Err(e) => Err(e),
    }
}

impl Fs for FtpFs {
    fn caps(&self) -> Caps {
        Caps { write: true, links: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let items = self.inner.op(dir, cancelled, |l| {
            let p = spath(l.home(), dir);
            match list_items(l, &p, false) {
                // an empty folder answers 550 on some servers; a missing one too: told apart
                Err(Fail::Fs(FsError::NotFound)) => match stat_on(l, &p)? {
                    Some(i) if i.kind != Kind::File => Ok(Vec::new()),
                    _ => Err(Fail::Fs(FsError::NotFound)),
                },
                r => r,
            }
        })?;
        let mut sent = 0;
        for part in items.chunks(batch.max(1)) {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            sent += part.len();
            on_batch(part.iter().map(Item::entry).collect());
        }
        Ok(sent)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        self.inner.op(p, &|| false, |l| {
            let sp = spath(l.home(), p);
            // (not following a link, as `Fs::stat` promises: MLST alone shows a link as what it points to)
            lstat_on(l, &sp)?.map(|i| i.meta()).ok_or(Fail::Fs(FsError::NotFound))
        })
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let t = target(&self.inner.cfg, p)?;
        let mut tries = 0;
        loop {
            tries += 1;
            let mut l = self.inner.lease_for(&t, &|| false, true)?;
            let sp = spath(l.home(), p);
            let r = (|| {
                if offset > 0 {
                    l.s().resume_transfer(offset as usize).ftp()?;
                }
                l.s().retr_as_stream(sp.as_str()).ftp()
            })();
            match r {
                Ok(stream) => return Ok(Box::new(Reader { stream: Some(Box::new(stream)), lease: l })),
                Err(Fail::Lost(why)) => {
                    l.cut();
                    if tries < 2 {
                        continue;
                    }
                    return Err(FsError::Offline(why));
                }
                Err(Fail::Fs(e)) => return Err(e),
            }
        }
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let t = target(&self.inner.cfg, p)?;
        let mut l = self.inner.lease(&t, &|| false)?;
        let r = (|| {
            let sp = spath(l.home(), p);
            let existing = lstat_on(&mut l, &sp)?;
            if existing.as_ref().is_some_and(|i| i.kind == Kind::Dir) {
                return Err(Fail::Fs(FsError::Exists));
            }
            let (store_to, place, append) = match o.mode {
                WriteMode::At(off) => match &existing {
                    None => return Err(Fail::Fs(FsError::NotFound)),
                    // never through a link (its target would be written)
                    Some(i) if i.kind == Kind::Link => return Err(Fail::Fs(FsError::Other("This is a link on the server; leaf does not write through it".into()))),
                    Some(i) if i.size != off => return Err(Fail::Fs(FsError::Other("The file on the server is not as long as the part sent; it cannot be resumed here".into()))),
                    Some(_) => (sp.clone(), None, true),
                },
                WriteMode::CreateNew => {
                    if existing.is_some() {
                        return Err(Fail::Fs(FsError::Exists));
                    }
                    (sp.clone(), None, false)
                }
                WriteMode::Replace if is_part_name(p.name()) => {
                    if existing.is_some() {
                        return Err(Fail::Fs(FsError::Exists));
                    }
                    (sp.clone(), None, false)
                }
                WriteMode::Replace => {
                    let (dir, name) = split(&sp);
                    let temp = child(dir, &side_name(name, "leaf-up"));
                    (temp.clone(), Some(Place { temp, target: sp.clone() }), false)
                }
            };
            let stream = if append { l.s().append_with_stream(store_to.as_str()) } else { l.s().put_with_stream(store_to.as_str()) }.ftp()?;
            Ok((stream, place))
        })();
        match r {
            Ok((stream, place)) => Ok(Box::new(Writer { inner: self.inner.clone(), v: p.clone(), stream: Some(Box::new(stream)), lease: Some(l), place, done: false })),
            Err(Fail::Lost(why)) => {
                l.cut();
                Err(FsError::Offline(why))
            }
            Err(Fail::Fs(e)) => Err(e),
        }
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        if p.is_root() {
            return Err(FsError::Exists);
        }
        let made = self.inner.once(p, |l| {
            let sp = spath(l.home(), p);
            match l.s().mkdir(sp.as_str()).ftp() {
                Ok(()) => Ok(Ok(())),
                // most servers answer a plain 550 for a name that is taken
                Err(Fail::Fs(e)) => Ok(Err(match stat_on(l, &sp) {
                    Ok(Some(_)) => FsError::Exists,
                    _ => e,
                })),
                Err(lost) => Err(lost),
            }
        });
        match made {
            Ok(r) => r,
            // the line broke: the folder may have been made and the answer lost; tried again on a new connection, and a
            // folder that is there then counts as made
            Err(FsError::Offline(_)) => self.inner.op(p, &|| false, |l| {
                let sp = spath(l.home(), p);
                match l.s().mkdir(sp.as_str()).ftp() {
                    Ok(()) => Ok(()),
                    Err(Fail::Fs(e)) => match stat_on(l, &sp)? {
                        Some(i) if i.kind == Kind::Dir => Ok(()),
                        Some(_) => Err(Fail::Fs(FsError::Exists)),
                        None => Err(Fail::Fs(e)),
                    },
                    Err(lost) => Err(lost),
                }
            }),
            Err(e) => Err(e),
        }
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        if from.is_root() || to.is_root() {
            return Err(FsError::Denied);
        }
        self.inner.once(from, |l| {
            let (a, b) = (spath(l.home(), from), spath(l.home(), to));
            let src = stat_on(l, &a)?.ok_or(Fail::Fs(FsError::NotFound))?;
            // a folder never replaces anything
            if src.kind == Kind::Dir && stat_on(l, &b)?.is_some() {
                return Err(Fail::Fs(FsError::Exists));
            }
            put_in_place(l, &a, &b, replace).map_err(|e| match e {
                PlaceFail::Clean(f) => f,
                PlaceFail::Left(e) => Fail::Fs(e),
            })
        })
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() || p.path() == "/~" {
            return Err(FsError::Denied);
        }
        self.inner.once(p, |l| {
            // never the root, the login folder (by any address of it) or a folder the login folder is in
            let sp = spath(l.home(), p);
            let (mut norm, mut home) = (sp.trim_end_matches('/').to_string(), l.home().trim_end_matches('/').to_string());
            // (a Windows server takes `/Home` for `/home`)
            if l.windows() {
                norm = norm.to_lowercase();
                home = home.to_lowercase();
            }
            if norm.is_empty() || norm == home || home.starts_with(&format!("{norm}/")) {
                return Err(Fail::Fs(FsError::Denied));
            }
            let it = lstat_on(l, &sp)?.ok_or(Fail::Fs(FsError::NotFound))?;
            match it.kind {
                // DELE unlinks a link (its target stays) and removes a file
                Kind::Link | Kind::File => l.s().rm(sp.as_str()).ftp(),
                Kind::Dir if recursive => {
                    if !really_folder(l, &sp)? {
                        return l.s().rm(sp.as_str()).ftp();
                    }
                    self.remove_tree(l, &sp, 0)
                }
                Kind::Dir => rmdir(l, &sp),
            }
        })
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.modified_ms.is_none() && m.mode.is_none() {
            return Ok(());
        }
        // (times are not set over FTP; the mode only through `SITE CHMOD`, on a server that lists it, and never on a link)
        let Some(mode) = m.mode.filter(|_| m.modified_ms.is_none()) else { return Err(FsError::Unsupported) };
        self.inner.op(p, &|| false, |l| {
            let sp = spath(l.home(), p);
            if !l.site_chmod() || lstat_on(l, &sp)?.is_none_or(|i| i.kind == Kind::Link) {
                return Err(Fail::Fs(FsError::Unsupported));
            }
            l.s().custom_command(format!("SITE CHMOD {:o} {sp}", mode & 0o7777), &[Status::CommandOk]).ftp().map(|_| ())
        })
    }

    fn details(&self, p: &VPath) -> Result<Details, FsError> {
        self.inner.op(p, &|| false, |l| {
            let sp = spath(l.home(), p);
            let it = lstat_on(l, &sp)?.ok_or(Fail::Fs(FsError::NotFound))?;
            // (the owner is in the MLSD/LIST facts of some servers only; none is read here)
            let can_chmod = it.kind != Kind::Link && l.site_chmod();
            Ok(Details { mode: it.mode.map(|m| m & 0o7777), can_chmod, ..Details::default() })
        })
    }
}

// ------------------------------------------------------------------------------------------------ transfers

/// A running transfer, whatever the TLS type inside (suppaftp does not name it).
trait Transfer: Read + Write + Send {
    fn finish_box(self: Box<Self>) -> Result<(), FtpError>;
}

impl<T: TlsStream + Send> Transfer for TransferStream<T>
where
    TransferStream<T>: Send,
{
    fn finish_box(self: Box<Self>) -> Result<(), FtpError> {
        (*self).finish()
    }
}

/// A file being read; its connection is lent to it until the end.
struct Reader {
    stream: Option<Box<dyn Transfer>>,
    lease: Lease,
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let Some(s) = self.stream.as_mut() else { return Ok(0) };
        let n = s.read(out).map_err(|e| {
            if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "the server did not answer in time")
            } else {
                e
            }
        })?;
        if n == 0 && !out.is_empty() {
            // the end: the server's "transfer complete" read, the connection free again
            let s = self.stream.take().expect("stream");
            if let Err(e) = s.finish_box() {
                self.lease.broken = true;
                return Err(std::io::Error::other(match fail_of(e) {
                    Fail::Lost(m) => m,
                    Fail::Fs(e) => e.to_string(),
                }));
            }
        }
        Ok(n)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(s) = self.stream.take() {
            // stopped before the end: the connection is closed, not used again (no waiting for the server's answer)
            self.lease.cut();
            drop(s);
        }
    }
}

/// Where a written temporary file goes in `finish`.
struct Place {
    temp: String,
    target: String,
}

/// A file being written to the server.
struct Writer {
    inner: Arc<Inner>,
    v: VPath,
    stream: Option<Box<dyn Transfer>>,
    lease: Option<Lease>,
    place: Option<Place>,
    done: bool,
}

impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let s = self.stream.as_mut().ok_or_else(|| std::io::Error::other("the upload broke off"))?;
        s.write(data).map_err(|e| std::io::Error::new(e.kind(), io_words(&e)))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.stream.as_mut() {
            Some(s) => s.flush(),
            None => Err(std::io::Error::other("the upload broke off")),
        }
    }
}

impl WriteSink for Writer {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        let stream = self.stream.take().ok_or_else(|| FsError::Other("the upload broke off".into()))?;
        let mut l = self.lease.take().expect("lease");
        // whatever comes of it, nothing is left for Drop to do: what happens to the temporary file is decided here
        let place = self.place.take();
        self.done = true;
        let r = (|| {
            // the server confirms the whole file (226) before anything takes the real name
            stream.finish_box().ftp()?;
            match &place {
                Some(p) => put_in_place(&mut l, &p.temp, &p.target, true),
                None => Ok(()),
            }
        })();
        match r {
            Ok(()) => Ok(()),
            // the old version may be aside and the new one under its temporary name: both stay, the error names them
            Err(PlaceFail::Left(e)) => Err(e),
            Err(PlaceFail::Clean(Fail::Lost(why))) => {
                // (the real name was not touched) the temporary file goes over another connection, off this thread
                l.cut();
                drop(l);
                if let Some(p) = place {
                    let (inner, v) = (self.inner.clone(), self.v.clone());
                    std::thread::spawn(move || {
                        let _ = inner.once(&v, |l| l.s().rm(p.temp.as_str()).ftp());
                    });
                }
                Err(FsError::Offline(why))
            }
            Err(PlaceFail::Clean(Fail::Fs(e))) => {
                // the temporary file goes now, on this connection (the real name was not touched)
                if let Some(p) = place {
                    let _ = l.s().rm(p.temp.as_str());
                }
                Err(e)
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // not finished: the rest happens off this thread. The data connection is closed and the server's answer read (so the
        // server is done with the temporary file), then the temporary file is removed: on this connection when it still
        // works, else on another one (best effort; on a cut line it stays beside the real file, which is as it was)
        let (stream, lease, place) = (self.stream.take(), self.lease.take(), self.place.take());
        if stream.is_none() && place.is_none() {
            return;
        }
        let (inner, v) = (self.inner.clone(), self.v.clone());
        std::thread::spawn(move || {
            let mut lease = lease;
            // the server's answer to the transfer is read; when it is not the expected one the replies on this connection
            // may be one behind, so it is not used again
            let answered = stream.is_none_or(|s| s.finish_box().is_ok());
            if !answered && let Some(l) = lease.as_mut() {
                l.cut();
            }
            let Some(p) = place else { return };
            let removed = match lease.as_mut() {
                Some(l) if !l.broken => match l.s().rm(p.temp.as_str()).ftp() {
                    Ok(()) => true,
                    Err(Fail::Lost(_)) => {
                        l.cut();
                        false
                    }
                    // (not there: the server kept nothing)
                    Err(Fail::Fs(_)) => true,
                },
                _ => false,
            };
            drop(lease);
            if !removed {
                let _ = inner.once(&v, |l| l.s().rm(p.temp.as_str()).ftp());
            }
        });
    }
}
