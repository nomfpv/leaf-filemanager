//! One SSH connection per server (user@host:port), shared by every pane: connecting, the host-key check, the login, and
//! the SFTP channel on top. The async runtime lives here and starts with the first connection.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use leaf_vfs::{FsError, VPath};
use russh::client;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{MethodKind, MethodSet};
use russh_sftp::client::RawSftpSession;
use zeroize::Zeroizing;

use crate::known_hosts::{self, Verdict};
use crate::prompt::{self, HostKeyAnswer, HostKeyQuestion, Prompter, SecretQuestion};

/// Where leaf looks for keys and hosts, and how long it waits.
#[derive(Debug, Clone)]
pub struct Config {
    /// The user's known_hosts (read, and written by "Trust and remember").
    pub known_hosts: Option<PathBuf>,
    /// More files that are only read (the system-wide one).
    pub extra_known_hosts: Vec<PathBuf>,
    /// The folder of `hosts.toml` (a saved host's key file and user).
    pub config_dir: Option<PathBuf>,
    /// The folder of the default key files (`id_ed25519`, `id_ecdsa`, `id_rsa`); `~/.ssh`.
    pub ssh_dir: Option<PathBuf>,
    /// Ask the running SSH agent first.
    pub agent: bool,
    /// One request to the server (a listing batch, a read) may take this long, then the server counts as offline.
    pub op_timeout: Duration,
    pub connect_timeout: Duration,
    /// A connection nothing used for this long is closed.
    pub idle_after: Duration,
}

impl Config {
    /// The user's own files: `~/.ssh/known_hosts` (plus the system one), `~/.ssh/id_*`, the agent, hosts.toml in `config_dir`.
    pub fn for_user(config_dir: Option<PathBuf>) -> Config {
        let kh = known_hosts::user_file();
        Config {
            ssh_dir: kh.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)),
            known_hosts: kh,
            extra_known_hosts: known_hosts::global_file().into_iter().collect(),
            config_dir,
            agent: true,
            op_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
            idle_after: Duration::from_secs(10 * 60),
        }
    }
}

/// The runtime: one worker thread and at most one blocking thread (the name lookups run there, with a timeout). Started on
/// the first connection, never at app start.
pub(crate) fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        log::info!("remote: starting the SSH runtime");
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .thread_name("leaf-ssh")
            .enable_io()
            .enable_time()
            .build()
            .expect("the SSH runtime")
    })
}

/// Runs `fut` on the runtime from a blocking (worker) thread, polling `cancelled` every 50 ms.
pub(crate) fn block_on<T>(cancelled: &dyn Fn() -> bool, fut: impl std::future::Future<Output = Result<T, FsError>>) -> Result<T, FsError> {
    rt().block_on(async {
        let watch = async {
            loop {
                if cancelled() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::select! {
            r = fut => r,
            _ = watch => Err(FsError::Cancelled),
        }
    })
}

/// Who to log in as and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// `user@host:port`: the pool's key.
    pub key: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The saved host's key file (`~` expanded), tried before the default ones.
    pub key_file: Option<PathBuf>,
    pub jump: Option<String>,
}

fn local_user() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "root".into())
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
        && let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()).or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

/// The address with what `hosts.toml` says about that server (user, key file, jump host).
pub(crate) fn target(cfg: &Config, v: &VPath) -> Result<Target, FsError> {
    let host = v.host().to_string();
    if host.is_empty() {
        return Err(FsError::Other("The address has no server name".into()));
    }
    // the name is written into known_hosts later, where `,` `*` `!` or a space would make it a pattern for other hosts
    if !known_hosts::valid_host(&host) {
        return Err(FsError::Other("The server's name has characters a server name cannot have".into()));
    }
    let port = v.port().unwrap_or(22);
    let saved = cfg.config_dir.as_deref().map(rf_core::hosts::Hosts::load).unwrap_or_default();
    let h = saved.hosts.into_iter().find(|h| {
        h.scheme.eq_ignore_ascii_case("sftp")
            && h.host.eq_ignore_ascii_case(&host)
            && h.port.unwrap_or(22) == port
            && match (v.user(), h.user.as_deref()) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    });
    let user = v.user().map(str::to_string).or_else(|| h.as_ref().and_then(|h| h.user.clone())).unwrap_or_else(local_user);
    Ok(Target {
        key: format!("{user}@{}", known_hosts::host_name(&host, port)),
        key_file: h.as_ref().and_then(|h| h.key_file.as_deref()).map(expand_home),
        jump: h.and_then(|h| h.jump),
        host,
        port,
        user,
    })
}

/// What lives as long as leaf runs: the connections, keys trusted once, unlocked keys and passwords (memory only).
#[derive(Default)]
pub(crate) struct Shared {
    pool: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Arc<Conn>>>>>>,
    /// `(name, fingerprint)` of host keys the user trusted for this run (that key only).
    trusted_once: Mutex<HashSet<(String, String)>>,
    /// The fingerprint of the key each host name showed and was accepted with in this run: a later connection that shows
    /// another key that known_hosts does not have is refused as changed.
    seen: Mutex<HashMap<String, String>>,
    /// Key files unlocked with their passphrase (the decrypted key, not the passphrase).
    keys: Mutex<HashMap<PathBuf, Arc<PrivateKey>>>,
    /// `user@host:port` -> (the fingerprint of the host key the password was typed for, the password): used again only on a
    /// connection that shows that same key.
    passwords: Mutex<HashMap<String, (String, Zeroizing<String>)>>,
    idle_watch: OnceLock<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct Conn {
    pub sftp: Arc<RawSftpSession>,
    pub statvfs: bool,
    /// The server has `posix-rename@openssh.com` (a rename that replaces the target in one step).
    pub posix_rename: bool,
    /// The folder the server logs the user into (`/~` in an address stands for it).
    pub home: String,
    handle: client::Handle<Client>,
    last_used: AtomicU64,
    /// Open readers (an idle close waits for them).
    pub busy: AtomicUsize,
    born: Instant,
}

impl Conn {
    pub fn alive(&self) -> bool {
        !self.handle.is_closed()
    }
    pub fn touch(&self) {
        self.last_used.store(self.born.elapsed().as_secs(), Ordering::Relaxed);
    }
    fn idle_for(&self) -> Duration {
        Duration::from_secs(self.born.elapsed().as_secs().saturating_sub(self.last_used.load(Ordering::Relaxed)))
    }
}

impl Shared {
    /// The live connection to the target, connecting (and asking the user) when there is none.
    pub async fn conn(self: &Arc<Self>, cfg: &Config, prompter: &Arc<dyn Prompter>, t: &Target) -> Result<Arc<Conn>, FsError> {
        let slot = lock(&self.pool).entry(t.key.clone()).or_default().clone();
        // one connect per server at a time: a second pane waits for the first one's answer
        let mut g = slot.lock().await;
        if let Some(c) = g.as_ref()
            && c.alive()
        {
            c.touch();
            return Ok(c.clone());
        }
        *g = None;
        let c = Arc::new(connect(cfg, prompter, self, t).await?);
        c.touch();
        *g = Some(c.clone());
        drop(g);
        self.start_idle_watch(cfg.idle_after);
        Ok(c)
    }

    /// Whether a live connection to the target is open now; never connects.
    pub fn is_connected(&self, t: &Target) -> bool {
        let Some(slot) = lock(&self.pool).get(&t.key).cloned() else { return false };
        slot.try_lock().is_ok_and(|g| g.as_ref().is_some_and(|c| c.alive()))
    }

    /// Forgets every secret kept for the host `name` (`host` or `[host]:port`): its passwords, and the unlocked key files
    /// (asked again next time). Called when the host shows a key that is not the one accepted before.
    fn forget_secrets(&self, name: &str) {
        let suffix = format!("@{name}");
        lock(&self.passwords).retain(|k, _| !k.ends_with(&suffix));
        lock(&self.keys).clear();
    }

    /// Closes every connection (the tests' "the line dropped").
    #[cfg(test)]
    pub fn drop_all(&self) {
        let slots: Vec<_> = lock(&self.pool).drain().map(|(_, s)| s).collect();
        for slot in slots {
            if let Ok(mut g) = slot.try_lock()
                && let Some(c) = g.take()
            {
                close(c);
            }
        }
    }

    /// Forgets a connection that failed (the next call connects again).
    pub fn drop_conn(&self, t: &Target) {
        if let Some(slot) = lock(&self.pool).get(&t.key).cloned()
            && let Ok(mut g) = slot.try_lock()
            && let Some(c) = g.take()
        {
            close(c);
        }
    }

    fn start_idle_watch(self: &Arc<Self>, idle_after: Duration) {
        if self.idle_watch.set(()).is_err() {
            return;
        }
        let me = Arc::downgrade(self);
        rt().spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30).min(idle_after)).await;
                let Some(me) = me.upgrade() else { return };
                let slots: Vec<_> = lock(&me.pool).values().cloned().collect();
                for slot in slots {
                    let Ok(mut g) = slot.try_lock() else { continue };
                    let stale = g.as_ref().is_some_and(|c| !c.alive() || (c.busy.load(Ordering::Relaxed) == 0 && c.idle_for() >= idle_after));
                    if stale && let Some(c) = g.take() {
                        log::info!("remote: closing an idle connection");
                        close(c);
                    }
                }
            }
        });
    }
}

fn close(c: Arc<Conn>) {
    rt().spawn(async move {
        let _ = c.sftp.close_session();
        let _ = c.handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    });
}

/// The answer of the host-key check, kept for the error text (russh only says "unknown key").
type Refusal = Arc<Mutex<Option<FsError>>>;

pub(crate) struct Client {
    host: String,
    port: u16,
    files: Vec<PathBuf>,
    remember_to: Option<PathBuf>,
    prompter: Arc<dyn Prompter>,
    shared: Arc<Shared>,
    refusal: Refusal,
    /// The fingerprint of the key this connection was accepted with (the login binds a remembered password to it).
    accepted: Arc<Mutex<Option<String>>>,
    /// Set while the host-key dialog is open (the handshake's time limit waits).
    asking: Arc<AtomicBool>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, k: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        // a host certificate is checked by its plain key (no @cert-authority support yet)
        let key = k.public_key();
        let name = known_hosts::host_name(&self.host, self.port);
        let fp = known_hosts::fingerprint(&key);
        // a refused key also drops what was remembered for this host: nothing typed for it is sent again without asking
        let refuse = |e: FsError| {
            self.shared.forget_secrets(&name);
            *lock(&self.refusal) = Some(e);
            Ok(false)
        };
        let accept = || {
            lock(&self.shared.seen).insert(name.clone(), fp.clone());
            *lock(&self.accepted) = Some(fp.clone());
            Ok(true)
        };
        match known_hosts::check(&self.files, &self.host, self.port, &key) {
            // the file has this very key (it wins over what this run saw: the user may have fixed the file)
            Verdict::Known => accept(),
            Verdict::Changed { file, line } => {
                log::warn!("remote: the host key of {name} CHANGED (now {fp}); refused");
                refuse(FsError::Other(format!(
                    "WARNING: the key of {name} has changed. Someone may be intercepting the connection, or the server was \
                     set up again. leaf does not connect. The server now shows {} {fp}. If you know why it changed, remove \
                     line {line} from {} yourself and connect again.",
                    known_hosts::key_type(&key),
                    file.display()
                )))
            }
            Verdict::Revoked { file, line } => {
                log::warn!("remote: the host key of {name} is revoked; refused");
                refuse(FsError::Other(format!(
                    "WARNING: the key of {name} ({fp}) is marked as revoked (line {line} of {}). leaf does not connect.",
                    file.display()
                )))
            }
            Verdict::OtherType { known, file, line } => {
                log::warn!("remote: {name} showed a {} key, known_hosts has only {}; refused", known_hosts::key_type(&key), known.join(", "));
                refuse(FsError::Other(format!(
                    "WARNING: {name} showed a key of another type ({} {fp}) than the one known_hosts has for it ({}). Someone \
                     may be intercepting the connection. leaf does not connect. If the server's keys really changed, remove \
                     line {line} from {} yourself and connect again.",
                    known_hosts::key_type(&key),
                    known.join(", "),
                    file.display()
                )))
            }
            Verdict::Unknown => {
                // this run accepted another key for this name (trusted once, or remembered but not written): changed
                let before = lock(&self.shared.seen).get(&name).cloned();
                if let Some(before) = before.filter(|b| *b != fp) {
                    log::warn!("remote: {name} showed another key than earlier in this run (now {fp}); refused");
                    return refuse(FsError::Other(format!(
                        "WARNING: the key of {name} has changed since leaf connected to it earlier (then {before}, now {} {fp}). \
                         Someone may be intercepting the connection. leaf does not connect, and forgot the password it kept \
                         for this server. Restart leaf to be asked about the key again.",
                        known_hosts::key_type(&key)
                    )));
                }
                if lock(&self.shared.trusted_once).contains(&(name.clone(), fp.clone())) {
                    return accept();
                }
                let q = HostKeyQuestion { host: name.clone(), key_type: known_hosts::key_type(&key), fingerprint: fp.clone() };
                self.asking.store(true, Ordering::Relaxed);
                let answer = prompt::ask_host_key(&*self.prompter, q).await;
                self.asking.store(false, Ordering::Relaxed);
                match answer {
                    HostKeyAnswer::TrustAndRemember => {
                        match &self.remember_to {
                            Some(p) => {
                                if let Err(e) = known_hosts::remember(p, &self.host, self.port, &key) {
                                    log::warn!("remote: could not add the key to {}: {e} (trusted for this run only)", p.display());
                                }
                            }
                            None => log::warn!("remote: no known_hosts file to write to (trusted for this run only)"),
                        }
                        lock(&self.shared.trusted_once).insert((name.clone(), fp.clone()));
                        accept()
                    }
                    HostKeyAnswer::TrustOnce => {
                        lock(&self.shared.trusted_once).insert((name.clone(), fp.clone()));
                        accept()
                    }
                    HostKeyAnswer::Cancel => refuse(FsError::Cancelled),
                }
            }
        }
    }
}

fn offline(e: impl std::fmt::Display) -> FsError {
    FsError::Offline(e.to_string())
}

async fn connect(cfg: &Config, prompter: &Arc<dyn Prompter>, shared: &Arc<Shared>, t: &Target) -> Result<Conn, FsError> {
    if let Some(j) = &t.jump {
        return Err(FsError::Other(format!("This host is reached through a jump host ({j}); jump hosts are not supported yet")));
    }
    log::info!("remote: connecting to {}", known_hosts::host_name(&t.host, t.port));
    // the name is looked up on the runtime's blocking thread, with the connect timeout (a stalled lookup does not hold this
    // worker)
    let addrs: Vec<SocketAddr> = step(cfg.connect_timeout, tokio::net::lookup_host((t.host.as_str(), t.port))).await?.collect();
    if addrs.is_empty() {
        return Err(offline("the server's name has no address"));
    }
    let tcp = tokio::time::timeout(cfg.connect_timeout, tokio::net::TcpStream::connect(&addrs[..]))
        .await
        .map_err(|_| offline("no answer"))?
        .map_err(offline)?;
    let _ = tcp.set_nodelay(true);
    let refusal: Refusal = Arc::default();
    let accepted: Arc<Mutex<Option<String>>> = Arc::default();
    let asking: Arc<AtomicBool> = Arc::default();
    let mut files: Vec<PathBuf> = cfg.known_hosts.iter().cloned().collect();
    files.extend(cfg.extra_known_hosts.iter().cloned());
    let ssh_cfg = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        preferred: preferred_for(&known_hosts::known_types(&files, &t.host, t.port)),
        ..Default::default()
    });
    let handler = Client {
        host: t.host.clone(),
        port: t.port,
        files,
        remember_to: cfg.known_hosts.clone(),
        prompter: prompter.clone(),
        shared: shared.clone(),
        refusal: refusal.clone(),
        accepted: accepted.clone(),
        asking: asking.clone(),
    };
    let why = |e: russh::Error| lock(&refusal).take().unwrap_or_else(|| offline(e));
    // the version exchange and the key exchange get the request timeout, not counting the time the user looks at the
    // host-key dialog
    let handshake = paused_timeout(cfg.op_timeout, &asking, client::connect_stream(ssh_cfg, tcp, handler));
    let mut handle = handshake.await.ok_or_else(|| offline("the server did not finish the handshake in time"))?.map_err(why)?;
    // (connect_stream returns only after the key exchange, which called check_server_key)
    let host_fp = lock(&accepted).clone().ok_or_else(|| offline("the server's key was not checked"))?;
    login(&mut handle, cfg, prompter, shared, t, &host_fp).await.map_err(|e| lock(&refusal).take().unwrap_or(e))?;
    let ch = step(cfg.op_timeout, handle.channel_open_session()).await?;
    step(cfg.op_timeout, ch.request_subsystem(true, "sftp")).await?;
    let sftp = RawSftpSession::new(ch.into_stream());
    sftp.set_timeout(cfg.op_timeout.as_secs().max(1));
    let version = tokio::time::timeout(cfg.op_timeout, sftp.init())
        .await
        .map_err(|_| offline("the server did not start SFTP in time"))?
        .map_err(|e| FsError::Other(format!("The server has no SFTP: {e}")))?;
    let statvfs = version.extensions.get(russh_sftp::extensions::STATVFS).is_some_and(|v| v == "2");
    let posix_rename = version.extensions.get(POSIX_RENAME).is_some_and(|v| v == "1");
    let home = match tokio::time::timeout(cfg.op_timeout, sftp.realpath(".")).await {
        Ok(Ok(n)) => n.files.into_iter().next().map(|f| f.filename).filter(|h| h.starts_with('/')).unwrap_or_else(|| "/".into()),
        _ => "/".into(),
    };
    log::info!("remote: connected (SFTP version {}, statvfs {statvfs}, posix-rename {posix_rename})", version.version);
    Ok(Conn { sftp: Arc::new(sftp), statvfs, posix_rename, home, handle, last_used: AtomicU64::new(0), busy: AtomicUsize::new(0), born: Instant::now() })
}

/// The OpenSSH extension of a rename that replaces the target atomically.
pub(crate) const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// One step of the conversation with the server, with a time limit: a stalled server becomes "offline", never a hang.
async fn step<T, E: std::fmt::Display>(limit: Duration, fut: impl std::future::Future<Output = Result<T, E>>) -> Result<T, FsError> {
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r.map_err(offline),
        Err(_) => Err(offline("the server did not answer in time")),
    }
}

/// `fut` with a time limit that does not run while `paused` is set (while the user is asked something). `None`: too late.
async fn paused_timeout<T>(limit: Duration, paused: &AtomicBool, fut: impl std::future::Future<Output = T>) -> Option<T> {
    let tick = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    let mut fut = std::pin::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return Some(r),
            _ = tokio::time::sleep(tick) => {
                if !paused.load(Ordering::Relaxed) {
                    waited += tick;
                    if waited >= limit {
                        return None;
                    }
                }
            }
        }
    }
}

/// russh's algorithms with the host key types known_hosts has for this server first (as OpenSSH orders them): an honest
/// server that has the known key shows it, so a key of another type can be refused.
fn preferred_for(known: &[String]) -> russh::Preferred {
    let all = russh::Preferred::DEFAULT.key.to_vec();
    let (mut first, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|a| known.contains(&known_hosts::family(a)));
    first.extend(rest);
    russh::Preferred { key: std::borrow::Cow::Owned(first), ..russh::Preferred::DEFAULT }
}

fn allows(methods: &Option<MethodSet>, m: MethodKind) -> bool {
    methods.as_ref().is_none_or(|ms| ms.contains(&m))
}

/// Logs in: the agent, the key files, then a password (or the server's own questions). Errors are `Auth` (refused),
/// `Cancelled` (the user said no), `Offline` (the line broke).
async fn login(h: &mut client::Handle<Client>, cfg: &Config, prompter: &Arc<dyn Prompter>, shared: &Arc<Shared>, t: &Target, host_fp: &str) -> Result<(), FsError> {
    let user = t.user.as_str();
    let account = format!("{user}@{}", t.host);
    // "none" tells which methods the server takes
    // every request to the server (and to the agent) has the request timeout; the user's dialogs are outside it
    let limit = cfg.op_timeout;
    let mut methods = match step(limit, h.authenticate_none(user)).await? {
        russh::client::AuthResult::Success => return Ok(()),
        russh::client::AuthResult::Failure { remaining_methods, .. } => Some(remaining_methods),
    };
    let rsa_hash = tokio::time::timeout(limit, h.best_supported_rsa_hash()).await.ok().and_then(|r| r.ok()).flatten().flatten();

    // 1. the running agent
    if cfg.agent && allows(&methods, MethodKind::PublicKey) {
        if let Ok(Some(mut agent)) = tokio::time::timeout(limit, agent()).await {
            let ids = tokio::time::timeout(limit, agent.request_identities()).await.ok().and_then(|r| r.ok()).unwrap_or_default();
            for id in ids {
                let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = id else { continue };
                match tokio::time::timeout(limit, h.authenticate_publickey_with(user, key, rsa_hash, &mut agent)).await {
                    Ok(Ok(russh::client::AuthResult::Success)) => {
                        log::info!("remote: logged in with a key of the agent");
                        return Ok(());
                    }
                    Ok(Ok(russh::client::AuthResult::Failure { remaining_methods, .. })) => methods = Some(remaining_methods),
                    Ok(Err(e)) => log::debug!("remote: the agent did not sign: {e}"),
                    Err(_) => return Err(offline("the server or the agent did not answer in time")),
                }
            }
        }
    }

    // 2. the saved host's key file, then the default ones
    if allows(&methods, MethodKind::PublicKey) {
        let mut files: Vec<PathBuf> = t.key_file.iter().cloned().collect();
        if let Some(d) = &cfg.ssh_dir {
            files.extend(["id_ed25519", "id_ecdsa", "id_rsa"].iter().map(|n| d.join(n)));
        }
        let mut seen = HashSet::new();
        for f in files {
            if !seen.insert(f.clone()) || !f.is_file() {
                continue;
            }
            let Some(key) = load_key(&f, prompter, shared).await? else { continue };
            let k = PrivateKeyWithHashAlg::new(key, rsa_hash);
            match step(limit, h.authenticate_publickey(user, k)).await? {
                russh::client::AuthResult::Success => {
                    log::info!("remote: logged in with a key file");
                    return Ok(());
                }
                russh::client::AuthResult::Failure { remaining_methods, .. } => methods = Some(remaining_methods),
            }
        }
    }

    // 3. a password (asked; kept in memory for this run), or the server's questions
    if allows(&methods, MethodKind::Password) {
        let mut retry = false;
        for _ in 0..3 {
            // a remembered password only goes to the host key it was typed for
            let cached = lock(&shared.passwords).get(&t.key).filter(|(fp, _)| fp == host_fp).map(|(_, p)| p.clone());
            let pw = match cached {
                Some(p) if !retry => p,
                _ => prompt::ask_secret(&**prompter, SecretQuestion::Password { account: account.clone(), retry }).await.ok_or(FsError::Cancelled)?,
            };
            match step(limit, h.authenticate_password(user, pw.as_str())).await? {
                russh::client::AuthResult::Success => {
                    lock(&shared.passwords).insert(t.key.clone(), (host_fp.to_string(), pw));
                    log::info!("remote: logged in with a password");
                    return Ok(());
                }
                russh::client::AuthResult::Failure { .. } => {
                    lock(&shared.passwords).remove(&t.key);
                    retry = true;
                }
            }
        }
    } else if allows(&methods, MethodKind::KeyboardInteractive) {
        use russh::client::KeyboardInteractiveAuthResponse as R;
        let mut r = step(limit, h.authenticate_keyboard_interactive_start(user, None)).await?;
        // a server that asks without end (empty questions too) is given up on after a few rounds
        let mut rounds = 0;
        loop {
            rounds += 1;
            if rounds > 10 {
                return Err(FsError::Auth(format!("{account} kept asking questions; leaf stopped")));
            }
            match r {
                R::Success => {
                    log::info!("remote: logged in (keyboard-interactive)");
                    return Ok(());
                }
                R::Failure { .. } => break,
                R::InfoRequest { prompts, .. } => {
                    if prompts.len() > 16 {
                        return Err(FsError::Auth(format!("{account} asked too many questions at once")));
                    }
                    // (russh takes the answers as plain Strings: that copy is not wiped, a limit of its API)
                    let mut answers = Vec::with_capacity(prompts.len());
                    for p in prompts {
                        let q = SecretQuestion::Server { account: account.clone(), prompt: p.prompt, echo: p.echo };
                        let a = prompt::ask_secret(&**prompter, q).await.ok_or(FsError::Cancelled)?;
                        answers.push(a.as_str().to_string());
                    }
                    r = step(limit, h.authenticate_keyboard_interactive_respond(answers)).await?;
                }
            }
        }
    }
    Err(FsError::Auth(format!("{account} did not accept any key or password")))
}

/// A key file, unlocked with its passphrase when it has one (asked; the unlocked key is kept for this run).
/// `None`: not readable, or the user cancelled the passphrase.
async fn load_key(f: &Path, prompter: &Arc<dyn Prompter>, shared: &Arc<Shared>) -> Result<Option<Arc<PrivateKey>>, FsError> {
    if let Some(k) = lock(&shared.keys).get(f) {
        return Ok(Some(k.clone()));
    }
    let text = match std::fs::read_to_string(f) {
        Ok(t) => Zeroizing::new(t),
        Err(e) => {
            log::warn!("remote: {} is not readable: {e}", f.display());
            return Ok(None);
        }
    };
    match russh::keys::decode_secret_key(&text, None) {
        Ok(k) => return Ok(Some(Arc::new(k))),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(e) => {
            log::warn!("remote: {} is not a key leaf can read: {e}", f.display());
            return Ok(None);
        }
    }
    let mut retry = false;
    for _ in 0..3 {
        let q = SecretQuestion::Passphrase { key_file: f.display().to_string(), retry };
        let Some(pass) = prompt::ask_secret(&**prompter, q).await else { return Ok(None) };
        match russh::keys::decode_secret_key(&text, Some(pass.as_str())) {
            Ok(k) => {
                let k = Arc::new(k);
                lock(&shared.keys).insert(f.to_path_buf(), k.clone());
                return Ok(Some(k));
            }
            Err(_) => retry = true,
        }
    }
    Ok(None)
}

type Agent = russh::keys::agent::client::AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin + 'static>>;

/// The running agent: the Windows OpenSSH agent's pipe (or `SSH_AUTH_SOCK` when it names a pipe), `SSH_AUTH_SOCK` on Unix.
async fn agent() -> Option<Agent> {
    #[cfg(windows)]
    {
        let pipe = std::env::var("SSH_AUTH_SOCK").ok().filter(|s| s.starts_with(r"\\.\pipe\")).unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
        match russh::keys::agent::client::AgentClient::connect_named_pipe(&pipe).await {
            Ok(a) => Some(a.dynamic()),
            Err(e) => {
                log::debug!("remote: no agent at {pipe}: {e}");
                None
            }
        }
    }
    #[cfg(unix)]
    {
        match russh::keys::agent::client::AgentClient::connect_env().await {
            Ok(a) => Some(a.dynamic()),
            Err(e) => {
                log::debug!("remote: no agent: {e}");
                None
            }
        }
    }
}

/// What a command run on the server printed (standard output only, capped) and how it ended.
#[derive(Debug, Clone, Default)]
pub struct ExecOut {
    pub stdout: String,
    /// The command's exit code; none when the server did not tell.
    pub code: Option<u32>,
    /// More output came than `EXEC_CAP` and was dropped.
    pub truncated: bool,
}

/// The most output of a command that is kept.
const EXEC_CAP: usize = 4 * 1024 * 1024;

impl Conn {
    /// Runs `command` (the server's shell parses it) on a new channel of this connection and collects what it prints, waiting
    /// at most `limit` in all.
    pub async fn exec(&self, command: &str, limit: Duration) -> Result<ExecOut, FsError> {
        let mut ch = step(self.limit_open(), self.handle.channel_open_session()).await?;
        step(self.limit_open(), ch.exec(true, command.as_bytes().to_vec())).await?;
        let mut out = ExecOut::default();
        let mut bytes: Vec<u8> = Vec::new();
        let read = async {
            while let Some(msg) = ch.wait().await {
                match msg {
                    russh::ChannelMsg::Data { data } => {
                        let room = EXEC_CAP.saturating_sub(bytes.len());
                        if data.len() > room {
                            out.truncated = true;
                        }
                        bytes.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    russh::ChannelMsg::ExitStatus { exit_status } => out.code = Some(exit_status),
                    russh::ChannelMsg::Close => break,
                    _ => {}
                }
            }
        };
        if tokio::time::timeout(limit, read).await.is_err() {
            let _ = ch.close().await;
            return Err(offline("the server did not finish the command in time"));
        }
        self.touch();
        out.stdout = String::from_utf8_lossy(&bytes).into_owned();
        Ok(out)
    }

    fn limit_open(&self) -> Duration {
        Duration::from_secs(30)
    }
}
