//! The running side of shares: the listener (only while a share exists), the connections, the friend's dialing with a growing
//! wait, the folder watches, and the syncs on the sharing PC.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rf_nearby::kit::{self, Identity};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Notify, oneshot};

use crate::folder::{Folder, err_kind};
use crate::store::{INVITE_LIFE_MS, Invite, InviteRec, PeerRec, Role, ShareRec, Store, random_hex, sha256_hex};
use crate::transport::{Link, Quic, RecvHalf, SendHalf, Transport};
use crate::wire::{self, Notice, Op, Open, OpenReply, PROTO, PairConfirm, Reply, Request};

/// How long a user has to confirm the code.
const PAIR_TIME: Duration = Duration::from_secs(180);
/// The first frame of a connection must come this soon.
const HELLO_TIME: Duration = Duration::from_secs(10);
/// The friend's dialing waits this long after a failure at first, doubling up to the second.
const BACKOFF_FIRST: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// A sync also runs this often by itself (a change the watch missed).
const SAFETY_SYNC: Duration = Duration::from_secs(15 * 60);

type TrashFn = dyn Fn(&[PathBuf]) -> Result<(), String> + Send + Sync;
type BinFn = dyn Fn(&Path) -> bool + Send + Sync;

/// What the app gives the service once.
#[derive(Clone)]
pub struct Config {
    /// This PC's name as the friend sees it.
    pub name: String,
    /// Where the shares and the sync states are kept.
    pub state_dir: PathBuf,
    /// Nearby's state folder: the identity (made there when missing) and Nearby's paired devices.
    pub identity_dir: PathBuf,
    /// The UDP port to listen on (0: the system picks one, for tests).
    pub port: u16,
    /// Listen on 127.0.0.1 only (tests).
    pub local_only: bool,
    /// How long a folder must be quiet after a change before the sync (5 s).
    pub debounce: Duration,
    /// Moves local files to the Trash (never a permanent delete); and whether the Trash takes a file by a move. When it does not,
    /// the file goes to the share's own `.leaf-sync-trash`.
    pub trash_local: Arc<TrashFn>,
    pub bin_takes: Arc<BinFn>,
    /// The runtime to run on (the app passes the one its remote connections use); `None`: the service makes its own.
    pub runtime: Option<tokio::runtime::Handle>,
}

impl Config {
    pub fn new(name: &str, state_dir: PathBuf, identity_dir: PathBuf, port: u16) -> Config {
        Config {
            name: name.to_string(),
            state_dir,
            identity_dir,
            port,
            local_only: false,
            debounce: Duration::from_secs(5),
            trash_local: Arc::new(|_| Err("no Trash here".into())),
            bin_takes: Arc::new(|_| false),
            runtime: None,
        }
    }
}

/// Everything the service tells the app; called from the service's own threads.
pub trait ShareListener: Send + Sync {
    /// Something in [`ShareService::list`] changed (a state, a sync's end).
    fn changed(&self);
    /// The two PCs meet for the first time: show `code` and answer with [`ShareService::confirm_pairing`].
    fn pairing_code(&self, share: &str, peer: &str, code: &str);
    /// A join ended: in, or why not (leaf's words).
    fn joined(&self, share: &str, result: Result<(), String>);
    /// A share stopped by itself (a refusal of the sync, the friend left): for a notice.
    fn paused(&self, share: &str, name: &str, reason: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    InSync,
    Syncing { files: u64 },
    /// Not connected; the address being dialed, or where the friend was last seen (may be empty).
    Waiting { address: String },
    /// A shared folder nobody joined yet.
    NoFriend,
    Joining,
    Paused { reason: String },
}

#[derive(Debug, Clone)]
pub struct ShareInfo {
    pub id: String,
    pub name: String,
    pub folder: PathBuf,
    pub role: Role,
    pub peer_name: String,
    pub address: String,
    pub friend_can_write: bool,
    pub state: State,
    pub last_sync_ms: i64,
    pub last_result: String,
}

pub(crate) struct Session {
    pub(crate) link: Arc<dyn Link>,
    ctl: tokio::sync::Mutex<SendHalf>,
}

impl Session {
    async fn notice(&self, n: &Notice) {
        let mut s = self.ctl.lock().await;
        if let Err(e) = wire::write_frame(&mut *s, n).await {
            log::debug!("share: a notice did not go out: {e:#}");
        }
    }
}

/// The running bits of one share.
pub(crate) struct Live {
    pub(crate) wake: Arc<Notify>,
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) syncing: Option<u64>,
    joining: bool,
    _watch: Option<leaf_sys::tree_watch::TreeWatch>,
}

pub(crate) struct Inner {
    pub(crate) cfg: Config,
    pub(crate) listener: Arc<dyn ShareListener>,
    me: Identity,
    pub(crate) store: Mutex<Store>,
    pub(crate) rt: tokio::runtime::Handle,
    transport: Mutex<Option<Arc<dyn Transport>>>,
    problem: Mutex<Option<String>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    pairings: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    pub(crate) live: Mutex<HashMap<String, Live>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct ShareService {
    inner: Arc<Inner>,
    rt: Mutex<Option<tokio::runtime::Runtime>>,
}
/// A folder as the system resolves it (the part that exists; the rest added as it is), lower-cased on Windows, whose names are
/// not case-sensitive: two spellings of one folder give the same key.
fn path_key(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = p;
    let found = loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            break Some(c);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => break None,
        }
    };
    let mut k = match found {
        Some(c) => c,
        None => {
            rest.clear();
            p.to_path_buf()
        }
    };
    for r in rest.iter().rev() {
        k.push(r);
    }
    if cfg!(windows) { PathBuf::from(k.to_string_lossy().to_lowercase()) } else { k }
}

/// One folder lies in the other (or they are the same).
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}


impl ShareService {
    /// Reads the shares and starts what they need (the listener only when there is one).
    pub fn start(cfg: Config, listener: Arc<dyn ShareListener>) -> Result<Arc<ShareService>, String> {
        let me = Identity::load_or_create(&cfg.identity_dir).map_err(|e| format!("This PC's identity could not be made: {e:#}"))?;
        let own = match &cfg.runtime {
            Some(_) => None,
            None => Some(tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("leaf-share").enable_all().build().map_err(|e| e.to_string())?),
        };
        let handle = cfg.runtime.clone().or_else(|| own.as_ref().map(|r| r.handle().clone())).ok_or("no runtime")?;
        let store = Store::load(&cfg.state_dir);
        let ids: Vec<String> = store.shares.iter().map(|s| s.id.clone()).collect();
        let inner = Arc::new(Inner {
            cfg,
            listener,
            me,
            store: Mutex::new(store),
            rt: handle,
            transport: Mutex::new(None),
            problem: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            pairings: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        });
        if !ids.is_empty() {
            let _ = inner.ensure_transport();
        }
        for id in ids {
            inner.activate(&id);
        }
        Ok(Arc::new(ShareService { inner, rt: Mutex::new(own) }))
    }

    /// This PC's device id.
    pub fn device_id(&self) -> String {
        self.inner.me.id.clone()
    }

    /// The port shares listen on now (0: not listening).
    pub fn port(&self) -> u16 {
        lock(&self.inner.transport).as_ref().map_or(0, |t| t.local_port())
    }

    /// Why the listener could not start (the port taken...).
    pub fn problem(&self) -> Option<String> {
        lock(&self.inner.problem).clone()
    }

    pub fn list(&self) -> Vec<ShareInfo> {
        let store = lock(&self.inner.store);
        store.shares.iter().map(|s| self.inner.info(s)).collect()
    }

    /// The local folders that are shared (for the badge).
    pub fn shared_folders(&self) -> Vec<PathBuf> {
        lock(&self.inner.store).shares.iter().map(|s| s.folder.clone()).collect()
    }

    /// Shares `folder` (or finds its share when it is shared already). Returns the share's id.
    pub fn create(&self, folder: &Path) -> Result<String, String> {
        let meta = std::fs::symlink_metadata(folder).map_err(|e| format!("The folder cannot be read ({e})"))?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Only a folder can be shared (not a file or a link)".into());
        }
        let id = {
            let mut store = lock(&self.inner.store);
            let key = path_key(folder);
            if let Some(s) = store.shares.iter().find(|s| s.role == Role::Host && path_key(&s.folder) == key) {
                return Ok(s.id.clone());
            }
            if let Some(s) = store.shares.iter().find(|s| overlap(&key, &path_key(&s.folder))) {
                return Err(format!("This folder is inside a shared folder, or holds one ({})", s.folder.display()));
            }
            let name = folder.file_name().map_or_else(|| folder.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
            let id = random_hex(8);
            store.shares.push(ShareRec {
                id: id.clone(),
                name,
                folder: folder.to_path_buf(),
                role: Role::Host,
                peer: None,
                friend_can_write: true,
                paused: None,
                last_sync_ms: 0,
                last_result: String::new(),
                exclude: Vec::new(),
                invites: Vec::new(),
            });
            store.save();
            id
        };
        self.inner.ensure_transport()?;
        self.inner.activate(&id);
        self.inner.listener.changed();
        Ok(id)
    }

    /// A new invite for a share of this PC, with `address` as the friend reaches this PC (the port is added when missing).
    pub fn invite(&self, share: &str, address: &str) -> Result<String, String> {
        self.invite_until(share, address, crate::now_ms() + INVITE_LIFE_MS)
    }

    #[doc(hidden)]
    pub fn invite_until(&self, share: &str, address: &str, expires_ms: i64) -> Result<String, String> {
        let port = self.inner.ensure_transport()?.local_port();
        let address = address.trim();
        if address.is_empty() || address.contains(char::is_whitespace) {
            return Err("Type the address your friend reaches this PC at".into());
        }
        let has_port = address.rsplit_once(':').is_some_and(|(h, p)| !h.ends_with(':') && p.parse::<u16>().is_ok()) && !address.ends_with(']');
        let address = if has_port { address.to_string() } else { format!("{address}:{port}") };
        let secret = random_hex(16);
        let mut store = lock(&self.inner.store);
        let s = store.get_mut(share).ok_or("The shared folder is not there any more")?;
        if s.role != Role::Host {
            return Err("Only the PC that shared the folder can invite".into());
        }
        if let Some(p) = &s.peer {
            return Err(format!("This folder is shared with {} already", p.name));
        }
        let now = crate::now_ms();
        s.invites.retain(|i| !i.used && i.expires_ms > now);
        s.invites.push(InviteRec { secret_sha256: sha256_hex(&secret), expires_ms, used: false });
        let name = s.name.clone();
        store.save();
        Ok(Invite { device: self.inner.me.id.clone(), share: share.to_string(), secret, expires_ms, address, name }.to_text())
    }

    /// Joins a friend's shared folder into `folder` (made when missing). The end comes to [`ShareListener::joined`].
    pub fn join(&self, invite: &str, folder: &Path) -> Result<String, String> {
        let inv = Invite::parse(invite)?;
        if inv.expires_ms <= crate::now_ms() {
            return Err("The invite ran out (an invite is good for 24 hours). Ask your friend for a new one".into());
        }
        if inv.device == self.inner.me.id {
            return Err("This invite is from this PC".into());
        }
        {
            let store = lock(&self.inner.store);
            if store.get(&inv.share).is_some() {
                return Err("This PC takes part in that shared folder already".into());
            }
            let key = path_key(folder);
            if let Some(s) = store.shares.iter().find(|s| overlap(&key, &path_key(&s.folder))) {
                return Err(format!("That place is inside a shared folder, or holds one ({})", s.folder.display()));
            }
        }
        std::fs::create_dir_all(folder).map_err(|e| format!("The folder could not be made ({e})"))?;
        let t = self.inner.ensure_transport()?;
        let share = inv.share.clone();
        lock(&self.inner.live).insert(share.clone(), Live { wake: Arc::new(Notify::new()), stop: Arc::new(AtomicBool::new(false)), syncing: None, joining: true, _watch: None });
        self.inner.listener.changed();
        let inner = self.inner.clone();
        let folder = folder.to_path_buf();
        let id = share.clone();
        self.inner.rt.spawn(async move {
            let r = inner.join_inner(t, inv, folder).await;
            if r.is_err() {
                lock(&inner.live).remove(&share);
                inner.maybe_close_transport();
            }
            inner.listener.joined(&share, r);
            inner.listener.changed();
        });
        Ok(id)
    }

    /// This side's answer to the code shown for a share's first meeting.
    pub fn confirm_pairing(&self, share: &str, ok: bool) {
        if let Some(tx) = lock(&self.inner.pairings).remove(share) {
            let _ = tx.send(ok);
        }
    }

    pub fn set_paused(&self, share: &str, paused: bool) {
        {
            let mut store = lock(&self.inner.store);
            let Some(s) = store.get_mut(share) else { return };
            s.paused = paused.then(String::new);
            store.save();
        }
        if paused {
            if let Some(sess) = lock(&self.inner.sessions).remove(share) {
                sess.link.close();
            }
        } else {
            self.inner.wake(share);
        }
        self.inner.listener.changed();
    }

    /// Where the other PC is reached (`host:port`).
    pub fn set_address(&self, share: &str, address: &str) {
        {
            let mut store = lock(&self.inner.store);
            let Some(s) = store.get_mut(share) else { return };
            if let Some(p) = s.peer.as_mut() {
                p.address = address.trim().to_string();
            }
            store.save();
        }
        self.inner.wake(share);
        self.inner.listener.changed();
    }

    /// On a share of this PC: whether the friend's changes come back.
    pub fn set_friend_can_write(&self, share: &str, can: bool) {
        {
            let mut store = lock(&self.inner.store);
            let Some(s) = store.get_mut(share) else { return };
            if s.role != Role::Host || s.friend_can_write == can {
                return;
            }
            s.friend_can_write = can;
            store.save();
        }
        if let Some(sess) = self.inner.session(share) {
            self.inner.rt.spawn(async move { sess.notice(&Notice::CanWrite { can_write: can }).await });
        }
        self.inner.wake(share);
        self.inner.listener.changed();
    }

    /// A sync soon (after the quiet time).
    pub fn sync_now(&self, share: &str) {
        self.inner.wake(share);
    }

    /// Stops sharing: the files stay, the other PC is told, this PC forgets the share.
    pub fn leave(&self, share: &str) {
        let rec = {
            let mut store = lock(&self.inner.store);
            let rec = store.get(share).cloned();
            store.shares.retain(|s| s.id != share);
            store.save();
            rec
        };
        if let Some(l) = lock(&self.inner.live).remove(share) {
            l.stop.store(true, Ordering::Relaxed);
            l.wake.notify_waiters();
            l.wake.notify_one();
        }
        if let Some(sess) = lock(&self.inner.sessions).remove(share) {
            self.inner.rt.spawn(async move {
                sess.notice(&Notice::Left).await;
                tokio::time::sleep(Duration::from_millis(300)).await;
                sess.link.close();
            });
        }
        if rec.is_some() {
            let _ = std::fs::remove_file(self.inner.cfg.state_dir.join("state").join(format!("{share}.state")));
        }
        self.inner.maybe_close_transport();
        self.inner.listener.changed();
    }

    /// Stops everything (the shares stay saved).
    pub fn stop(&self) {
        for (_, l) in lock(&self.inner.live).drain() {
            l.stop.store(true, Ordering::Relaxed);
            l.wake.notify_one();
        }
        for (_, s) in lock(&self.inner.sessions).drain() {
            s.link.close();
        }
        if let Some(t) = lock(&self.inner.transport).take() {
            t.close();
        }
        if let Some(rt) = lock(&self.rt).take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for ShareService {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Inner {
    fn info(&self, s: &ShareRec) -> ShareInfo {
        let live = lock(&self.live);
        let l = live.get(&s.id);
        let connected = self.session(&s.id).is_some();
        let address = s.peer.as_ref().map(|p| p.address.clone()).unwrap_or_default();
        let state = if let Some(r) = &s.paused {
            State::Paused { reason: r.clone() }
        } else if let Some(n) = l.and_then(|l| l.syncing) {
            State::Syncing { files: n }
        } else if l.is_some_and(|l| l.joining) {
            State::Joining
        } else if s.peer.is_none() {
            State::NoFriend
        } else if !connected {
            State::Waiting { address: address.clone() }
        } else {
            State::InSync
        };
        ShareInfo {
            id: s.id.clone(),
            name: s.name.clone(),
            folder: s.folder.clone(),
            role: s.role,
            peer_name: s.peer.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
            address,
            friend_can_write: s.friend_can_write,
            state,
            last_sync_ms: s.last_sync_ms,
            last_result: s.last_result.clone(),
        }
    }

    pub(crate) fn session(&self, share: &str) -> Option<Arc<Session>> {
        lock(&self.sessions).get(share).filter(|s| !s.link.is_closed()).cloned()
    }

    fn wake(&self, share: &str) {
        if let Some(l) = lock(&self.live).get(share) {
            l.wake.notify_one();
        }
    }

    pub(crate) fn pause(&self, share: &str, reason: &str) {
        let name = {
            let mut store = lock(&self.store);
            let Some(s) = store.get_mut(share) else { return };
            s.paused = Some(reason.to_string());
            s.last_result = reason.to_string();
            let name = s.name.clone();
            store.save();
            name
        };
        self.listener.paused(share, &name, reason);
        self.listener.changed();
    }

    fn ensure_transport(self: &Arc<Self>) -> Result<Arc<dyn Transport>, String> {
        let mut t = lock(&self.transport);
        if let Some(t) = t.as_ref() {
            return Ok(t.clone());
        }
        let bound = {
            let _g = self.rt.enter();
            Quic::bind(&self.me, self.cfg.port, self.cfg.local_only)
        };
        match bound {
            Ok(q) => {
                let q: Arc<dyn Transport> = Arc::new(q);
                *t = Some(q.clone());
                *lock(&self.problem) = None;
                let inner = self.clone();
                let acc = q.clone();
                self.rt.spawn(async move {
                    while let Some(link) = acc.accept().await {
                        match link {
                            Ok(link) => {
                                let inner = inner.clone();
                                tokio::spawn(async move { inner.incoming(link).await });
                            }
                            Err(e) => log::debug!("share: an incoming connection failed: {e:#}"),
                        }
                    }
                });
                log::info!("share: listening on port {}", q.local_port());
                Ok(q)
            }
            Err(e) => {
                let msg = format!("Shared folders cannot listen on port {} ({e:#}). Pick another port in Settings", self.cfg.port);
                *lock(&self.problem) = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Stops listening once no share and no join is left.
    fn maybe_close_transport(&self) {
        if !lock(&self.store).shares.is_empty() || !lock(&self.live).is_empty() {
            return;
        }
        if let Some(t) = lock(&self.transport).take() {
            log::info!("share: no shared folder left; not listening any more");
            t.close();
        }
    }

    /// Starts what a share needs: its folder watch, and its sync loop (the sharing PC) or its dialing (the friend's).
    fn activate(self: &Arc<Self>, id: &str) {
        let Some(rec) = lock(&self.store).get(id).cloned() else { return };
        let wake = Arc::new(Notify::new());
        let stop = Arc::new(AtomicBool::new(false));
        let w = wake.clone();
        let watch = match leaf_sys::tree_watch::watch_tree(&rec.folder, Duration::from_millis(500), move |_| w.notify_one()) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("share: {} cannot be watched ({e:#}); it syncs on connect and every 15 minutes", rec.folder.display());
                None
            }
        };
        lock(&self.live).insert(id.to_string(), Live { wake: wake.clone(), stop: stop.clone(), syncing: None, joining: false, _watch: watch });
        let inner = self.clone();
        let id = id.to_string();
        match rec.role {
            Role::Host => {
                self.rt.spawn(async move { inner.host_loop(id, wake, stop).await });
            }
            Role::Guest => {
                let (inner2, id2, stop2) = (inner.clone(), id.clone(), stop.clone());
                let dial_wake = Arc::new(Notify::new());
                let dw = dial_wake.clone();
                self.rt.spawn(async move { inner2.dial_loop(id2, dw, stop2).await });
                self.rt.spawn(async move { inner.guest_notify_loop(id, wake, dial_wake, stop).await });
            }
        }
    }

    /// Waits for a wake and then for `debounce` of quiet (every further wake starts the quiet again); false when stopped.
    async fn quiet(&self, wake: &Notify, stop: &AtomicBool, first: Duration) -> bool {
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(first) => {}
        }
        loop {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            tokio::select! {
                _ = wake.notified() => continue,
                _ = tokio::time::sleep(self.cfg.debounce) => return !stop.load(Ordering::Relaxed),
            }
        }
    }

    /// The sharing PC: a sync after every quiet change (and on connect, and every 15 minutes).
    async fn host_loop(self: Arc<Self>, id: String, wake: Arc<Notify>, stop: Arc<AtomicBool>) {
        loop {
            if !self.quiet(&wake, &stop, SAFETY_SYNC).await {
                return;
            }
            let paused = lock(&self.store).get(&id).is_none_or(|s| s.paused.is_some());
            let Some(sess) = (!paused).then(|| self.session(&id)).flatten() else { continue };
            let (inner, share) = (self.clone(), id.clone());
            let (tx, rx) = oneshot::channel();
            // (the sync blocks: its own thread, which waits on the runtime for the friend's answers)
            std::thread::Builder::new()
                .name("leaf-share-sync".into())
                .spawn(move || {
                    crate::sync::sync_share(&inner, &share, sess.link.clone());
                    let _ = tx.send(());
                })
                .ok();
            let _ = rx.await;
        }
    }

    /// The friend's PC: tells the sharing PC about quiet changes here.
    async fn guest_notify_loop(self: Arc<Self>, id: String, wake: Arc<Notify>, dial_wake: Arc<Notify>, stop: Arc<AtomicBool>) {
        loop {
            if !self.quiet(&wake, &stop, SAFETY_SYNC).await {
                // (the dialing sees the stop)
                dial_wake.notify_one();
                return;
            }
            // (a resume, a new address: dial now)
            dial_wake.notify_one();
            if let Some(sess) = self.session(&id) {
                sess.notice(&Notice::Changed).await;
            }
        }
    }

    /// The friend's PC: keeps a connection to the sharing PC, dialing again with a growing wait.
    async fn dial_loop(self: Arc<Self>, id: String, wake: Arc<Notify>, stop: Arc<AtomicBool>) {
        let mut backoff = BACKOFF_FIRST;
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            if let Some(sess) = self.session(&id) {
                tokio::select! {
                    _ = sess.link.closed() => {}
                    _ = wake.notified() => continue,
                }
                self.drop_session(&id, &sess);
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            let Some(rec) = lock(&self.store).get(&id).cloned() else { return };
            if rec.paused.is_some() {
                wake.notified().await;
                continue;
            }
            let Some(t) = lock(&self.transport).clone() else {
                tokio::time::sleep(backoff).await;
                continue;
            };
            match self.resume(t, &rec).await {
                Ok(()) => backoff = BACKOFF_FIRST,
                Err(e) => {
                    log::info!("share: {} is not reachable at {}: {e:#}", rec.name, rec.peer.as_ref().map_or("", |p| p.address.as_str()));
                    self.listener.changed();
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = wake.notified() => {}
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }

    fn drop_session(&self, id: &str, sess: &Arc<Session>) {
        let mut s = lock(&self.sessions);
        if s.get(id).is_some_and(|x| Arc::ptr_eq(x, sess)) {
            s.remove(id);
        }
        drop(s);
        self.listener.changed();
    }

    fn put_session(&self, id: &str, sess: Arc<Session>) {
        if let Some(old) = lock(&self.sessions).insert(id.to_string(), sess) {
            old.link.close();
        }
        self.listener.changed();
    }

    /// The friend's PC: one connection to the sharing PC, for a share it is in.
    async fn resume(self: &Arc<Self>, t: Arc<dyn Transport>, rec: &ShareRec) -> anyhow::Result<()> {
        let peer = rec.peer.clone().ok_or_else(|| anyhow::anyhow!("no peer"))?;
        let link = t.connect(&peer.address).await?;
        if link.peer_fingerprint() != peer.fingerprint {
            link.close();
            anyhow::bail!("another device answered at {}", peer.address);
        }
        let (mut s, mut r) = link.open().await?;
        wire::write_frame(&mut s, &Open::Resume { proto: PROTO, share: rec.id.clone(), name: self.cfg.name.clone() }).await?;
        let reply: OpenReply = match tokio::time::timeout(HELLO_TIME, wire::read_frame(&mut r)).await {
            Ok(Ok(r)) => r,
            _ => {
                link.close();
                anyhow::bail!("the other PC did not take the connection (it may have paused or stopped sharing the folder)");
            }
        };
        let OpenReply::Welcome { name, can_write, .. } = reply else {
            link.close();
            anyhow::bail!("an unexpected answer");
        };
        {
            let mut store = lock(&self.store);
            if let Some(s) = store.get_mut(&rec.id) {
                s.friend_can_write = can_write;
                if let Some(p) = s.peer.as_mut() {
                    p.name = name;
                }
                store.save();
            }
        }
        self.start_guest_session(&rec.id, rec.folder.clone(), link, s, r);
        Ok(())
    }

    /// The friend's side of a connection: answers the sharing PC's file requests and hears its notices.
    fn start_guest_session(self: &Arc<Self>, id: &str, folder: PathBuf, link: Arc<dyn Link>, ctl: SendHalf, mut ctl_recv: RecvHalf) {
        let sess = Arc::new(Session { link: link.clone(), ctl: tokio::sync::Mutex::new(ctl) });
        self.put_session(id, sess.clone());
        let (inner, share) = (self.clone(), id.to_string());
        self.rt.spawn(async move {
            while let Ok(n) = wire::read_frame::<_, Notice>(&mut ctl_recv).await {
                match n {
                    Notice::Left => {
                        let who = lock(&inner.store).get(&share).and_then(|s| s.peer.as_ref().map(|p| p.name.clone())).unwrap_or_default();
                        inner.pause(&share, &format!("{who} stopped sharing this folder; the files here stay"));
                        sess.link.close();
                        break;
                    }
                    Notice::CanWrite { can_write } => {
                        let mut store = lock(&inner.store);
                        if let Some(s) = store.get_mut(&share) {
                            s.friend_can_write = can_write;
                            store.save();
                        }
                        drop(store);
                        inner.listener.changed();
                    }
                    Notice::Changed => {}
                }
            }
        });
        let share = id.to_string();
        self.rt.spawn(async move {
            let folder = match tokio::task::spawn_blocking(move || Folder::new(&folder)).await {
                Ok(Ok(f)) => Some(Arc::new(f)),
                Ok(Err(e)) => {
                    log::warn!("share: the shared folder cannot be used: {e}");
                    None
                }
                Err(_) => None,
            };
            while let Ok((s, r)) = link.accept().await {
                let (folder, share) = (folder.clone(), share.clone());
                tokio::spawn(async move {
                    if let Err(e) = serve(folder, &share, s, r).await {
                        log::debug!("share: a request ended: {e:#}");
                    }
                });
            }
        });
    }

    /// The friend's PC: the first connection, with the invite.
    async fn join_inner(self: &Arc<Self>, t: Arc<dyn Transport>, inv: Invite, folder: PathBuf) -> Result<(), String> {
        let link = t.connect(&inv.address).await.map_err(|e| format!("The other PC could not be reached at {} ({e:#})", inv.address))?;
        let fp = link.peer_fingerprint();
        if kit::id_of(&fp) != inv.device {
            link.close();
            return Err(format!("Another device answered at {}; the invite is for a different PC", inv.address));
        }
        let refused = || "The other PC did not take the invite: it may have run out or been used already. Ask your friend for a new one".to_string();
        let broken = |e: anyhow::Error| format!("The connection broke off ({e:#})");
        let (mut s, mut r) = link.open().await.map_err(broken)?;
        wire::write_frame(&mut s, &Open::Join { proto: PROTO, share: inv.share.clone(), secret: inv.secret.clone(), name: self.cfg.name.clone() }).await.map_err(broken)?;
        let mut reply: OpenReply = match tokio::time::timeout(HELLO_TIME, wire::read_frame(&mut r)).await {
            Ok(Ok(x)) => x,
            _ => {
                link.close();
                return Err(refused());
            }
        };
        if reply == OpenReply::Pair {
            if !self.pair(&inv.share, "the other PC", &link, &mut s, &mut r).await {
                link.close();
                return Err("The code was not confirmed on both PCs; nothing was shared".into());
            }
            reply = match tokio::time::timeout(HELLO_TIME, wire::read_frame(&mut r)).await {
                Ok(Ok(x)) => x,
                _ => {
                    link.close();
                    return Err(refused());
                }
            };
        }
        let OpenReply::Welcome { name, folder: shared_name, can_write } = reply else {
            link.close();
            return Err(refused());
        };
        {
            let mut store = lock(&self.store);
            store.shares.push(ShareRec {
                id: inv.share.clone(),
                name: shared_name,
                folder: folder.clone(),
                role: Role::Guest,
                peer: Some(PeerRec { id: inv.device.clone(), fingerprint: fp, name, address: inv.address.clone() }),
                friend_can_write: can_write,
                paused: None,
                last_sync_ms: 0,
                last_result: String::new(),
                exclude: Vec::new(),
                invites: Vec::new(),
            });
            store.save();
        }
        lock(&self.live).remove(&inv.share);
        self.activate(&inv.share);
        self.start_guest_session(&inv.share, folder, link, s, r);
        Ok(())
    }

    /// Both sides of a first meeting: show the code, wait for this side's user, tell the other side, hear its answer.
    async fn pair(&self, share: &str, peer: &str, link: &Arc<dyn Link>, s: &mut SendHalf, r: &mut RecvHalf) -> bool {
        let Some(code) = link.pairing_code() else { return false };
        let (tx, rx) = oneshot::channel();
        lock(&self.pairings).insert(share.to_string(), tx);
        self.listener.pairing_code(share, peer, &code);
        let mine = tokio::time::timeout(PAIR_TIME, rx).await.ok().and_then(|r| r.ok()).unwrap_or(false);
        lock(&self.pairings).remove(share);
        if wire::write_frame(s, &PairConfirm { ok: mine }).await.is_err() {
            return false;
        }
        let theirs: Option<PairConfirm> = tokio::time::timeout(PAIR_TIME, wire::read_frame(r)).await.ok().and_then(|r| r.ok());
        mine && theirs.is_some_and(|t| t.ok)
    }

    /// An incoming connection: only a device stored for a share, or one with a good invite, gets any answer.
    async fn incoming(self: Arc<Self>, link: Arc<dyn Link>) {
        let fp = link.peer_fingerprint();
        let now = crate::now_ms();
        let (known, invites) = {
            let s = lock(&self.store);
            (s.knows(&fp), s.open_invites(now))
        };
        if fp.is_empty() || (!known && !invites) {
            link.close();
            return;
        }
        let first = async {
            let (s, mut r) = link.accept().await?;
            let open: Open = wire::read_frame(&mut r).await?;
            anyhow::Ok((s, r, open))
        };
        let Ok(Ok((mut s, mut r, open))) = tokio::time::timeout(HELLO_TIME, first).await else {
            link.close();
            return;
        };
        match open {
            Open::Resume { proto, share, name } if proto == PROTO => {
                let ok = {
                    let mut store = lock(&self.store);
                    match store.get_mut(&share) {
                        Some(rec) if rec.role == Role::Host && rec.paused.is_none() && rec.peer.as_ref().is_some_and(|p| p.fingerprint == fp) => {
                            if let Some(p) = rec.peer.as_mut() {
                                p.address = link.remote_address();
                                p.name = name;
                            }
                            let w = OpenReply::Welcome { name: self.cfg.name.clone(), folder: rec.name.clone(), can_write: rec.friend_can_write };
                            store.save();
                            Some(w)
                        }
                        _ => None,
                    }
                };
                let Some(welcome) = ok else {
                    log::info!("share: a connection for a share it is not stored for was dropped");
                    link.close();
                    return;
                };
                if wire::write_frame(&mut s, &welcome).await.is_err() {
                    link.close();
                    return;
                }
                self.start_host_session(&share, link, s, r);
            }
            Open::Join { proto, share, secret, name } if proto == PROTO => {
                let good = {
                    let mut store = lock(&self.store);
                    let hash = sha256_hex(&secret);
                    let rec = store.get_mut(&share).filter(|r| r.role == Role::Host && r.peer.is_none());
                    match rec.and_then(|r| r.invites.iter_mut().find(|i| i.secret_sha256 == hash)) {
                        Some(i) if !i.used && i.expires_ms > now => {
                            // single use: taken now, whatever comes of it
                            i.used = true;
                            store.save();
                            true
                        }
                        _ => false,
                    }
                };
                if !good {
                    log::info!("share: an invite that ran out, was used or is unknown was refused");
                    link.close();
                    return;
                }
                let paired = kit::is_paired(&self.cfg.identity_dir, &fp) || lock(&self.store).knows(&fp);
                if !paired {
                    if wire::write_frame(&mut s, &OpenReply::Pair).await.is_err() || !self.pair(&share, &name, &link, &mut s, &mut r).await {
                        link.close();
                        self.listener.changed();
                        return;
                    }
                }
                let welcome = {
                    let mut store = lock(&self.store);
                    // (another invite of the same share may have been taken during the pairing: the first one in keeps it)
                    let Some(rec) = store.get_mut(&share).filter(|r| r.peer.is_none()) else {
                        log::info!("share: a second friend for a share that has one was refused");
                        link.close();
                        return;
                    };
                    rec.peer = Some(PeerRec { id: kit::id_of(&fp), fingerprint: fp.clone(), name, address: link.remote_address() });
                    let w = OpenReply::Welcome { name: self.cfg.name.clone(), folder: rec.name.clone(), can_write: rec.friend_can_write };
                    store.save();
                    w
                };
                if wire::write_frame(&mut s, &welcome).await.is_err() {
                    link.close();
                    return;
                }
                self.start_host_session(&share, link, s, r);
            }
            _ => link.close(),
        }
    }

    /// The sharing PC's side of a connection: hears the friend's notices; syncs on connect.
    fn start_host_session(self: &Arc<Self>, id: &str, link: Arc<dyn Link>, ctl: SendHalf, mut ctl_recv: RecvHalf) {
        let sess = Arc::new(Session { link, ctl: tokio::sync::Mutex::new(ctl) });
        self.put_session(id, sess.clone());
        self.wake(id);
        let (inner, share) = (self.clone(), id.to_string());
        self.rt.spawn(async move {
            while let Ok(n) = wire::read_frame::<_, Notice>(&mut ctl_recv).await {
                match n {
                    Notice::Changed => inner.wake(&share),
                    Notice::Left => {
                        let who = lock(&inner.store).get(&share).and_then(|s| s.peer.as_ref().map(|p| p.name.clone())).unwrap_or_default();
                        inner.pause(&share, &format!("{who} left this shared folder; the files here stay"));
                        sess.link.close();
                        break;
                    }
                    Notice::CanWrite { .. } => {}
                }
            }
            inner.drop_session(&share, &sess);
        });
    }
}

/// One request from the sharing PC, answered inside the shared folder (see [`crate::folder`]).
async fn serve(folder: Option<Arc<Folder>>, share: &str, mut s: SendHalf, mut r: RecvHalf) -> anyhow::Result<()> {
    let req: Request = tokio::time::timeout(HELLO_TIME, wire::read_frame(&mut r)).await??;
    let fail = |e: leaf_vfs::FsError| {
        let (kind, text) = err_kind(&e);
        Reply::Err { kind, text }
    };
    let Some(folder) = folder.filter(|_| req.share == share) else {
        wire::write_frame(&mut s, &fail(leaf_vfs::FsError::Denied)).await?;
        return Ok(());
    };
    let blocking = |f: Box<dyn FnOnce(&Folder) -> Reply + Send>| {
        let folder = folder.clone();
        async move { tokio::task::spawn_blocking(move || f(&folder)).await.unwrap_or_else(|_| Reply::Err { kind: wire::ErrKind::Other, text: "failed".into() }) }
    };
    let reply = match req.op {
        Op::Read { path, offset } => {
            let f = folder.clone();
            match tokio::task::spawn_blocking(move || f.open_read(&path, offset)).await? {
                Ok((file, len)) => {
                    wire::write_frame(&mut s, &Reply::Data { len }).await?;
                    let mut file = tokio::io::AsyncReadExt::take(tokio::fs::File::from_std(file), len);
                    tokio::io::copy(&mut file, &mut s).await?;
                    s.shutdown().await?;
                    return Ok(());
                }
                Err(e) => fail(e),
            }
        }
        Op::Write { path } => {
            let f = folder.clone();
            match tokio::task::spawn_blocking(move || f.create_part(&path)).await? {
                Ok((file, at)) => {
                    let mut file = tokio::fs::File::from_std(file);
                    let r = async {
                        tokio::io::copy(&mut r, &mut file).await?;
                        file.flush().await?;
                        file.sync_all().await?;
                        anyhow::Ok(())
                    }
                    .await;
                    drop(file);
                    match r {
                        Ok(()) => Reply::Ok,
                        Err(e) => {
                            folder.discard_part(&at);
                            Reply::Err { kind: wire::ErrKind::Other, text: format!("the copy could not be written ({e:#})") }
                        }
                    }
                }
                Err(e) => fail(e),
            }
        }
        Op::List { path } => blocking(Box::new(move |f| f.list(&path).map_or_else(fail, |entries| Reply::List { entries }))).await,
        Op::Stat { path } => blocking(Box::new(move |f| f.stat(&path).map_or_else(fail, |entry| Reply::Stat { entry }))).await,
        Op::Mkdir { path } => blocking(Box::new(move |f| f.mkdir(&path).map_or_else(fail, |_| Reply::Ok))).await,
        Op::Rename { from, to } => blocking(Box::new(move |f| f.rename(&from, &to).map_or_else(fail, |_| Reply::Ok))).await,
        Op::Remove { path, recursive } => blocking(Box::new(move |f| f.remove(&path, recursive).map_or_else(fail, |_| Reply::Ok))).await,
        Op::SetTime { path, mtime_ms } => blocking(Box::new(move |f| f.set_time(&path, mtime_ms).map_or_else(fail, |_| Reply::Ok))).await,
        Op::Hash { path } => blocking(Box::new(move |f| f.hash(&path).map_or_else(fail, |hex| Reply::Hash { hex }))).await,
    };
    wire::write_frame(&mut s, &reply).await?;
    s.shutdown().await?;
    Ok(())
}
