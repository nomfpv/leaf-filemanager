//! HTTP/HTTPS downloads for leaf: several connections to one file when the server answers byte ranges (segments that split
//! again while they run, so no connection sits idle at the end), resume from a small state file beside the `.leaf-part` file,
//! retries with backoff, a speed limit, a queue, and a final rename only when the file is complete. Blocking threads only: no
//! async runtime. The caller (leaf's Status Center) supplies the progress sink and the answer to "that name exists".

mod headers;
mod host;
mod http;
mod mark;
mod name;
pub mod queue;
pub mod store;
#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use headers::Headers;
pub use host::set_host_cap;
pub use name::{file_name, percent_decode, sanitize};

/// A segment is not split into pieces smaller than this.
const MIN_SEG: u64 = 1 << 20;
/// A running segment is split only when more than twice this is left.
const MIN_STEAL: u64 = 512 << 10;
/// A file smaller than this starts with about one connection per `PER_CONN` bytes.
const BIG: u64 = 64 << 20;
const PER_CONN: u64 = 4 << 20;
/// Connections added each time the total speed still rises.
const GROW_BY: usize = 4;
const BUF: usize = 256 << 10;
/// Connections a download of `len` bytes starts with.
fn start_count(o: &Options, len: u64) -> usize {
    let n = o.segments.max(1) as u64;
    let n = if len >= BIG { n } else { n.min(len.div_ceil(PER_CONN)) };
    n.min(len.div_ceil(MIN_SEG)).max(1) as usize
}

/// The extension of the file being written, and of its state file after it.
pub const PART_EXT: &str = ".leaf-part";
const STATE_EXT: &str = ".state";

pub struct Options {
    /// Connections a file starts with (when the server takes ranges); fewer for a small file (about one per 4 MiB).
    pub segments: usize,
    /// Connections to one file at most: more are opened while the total speed still rises.
    pub max_segments: usize,
    /// Tries after the first one, for a request or a segment, before the download fails.
    pub retries: u32,
    /// Wait before the first retry; doubles each time (30 s at most).
    pub backoff: Duration,
    pub user_agent: String,
    pub connect_timeout: Duration,
    /// A connection that sends nothing for this long counts as cut.
    pub read_timeout: Duration,
    /// The SHA-256 (64 hex digits) the finished file must have; a different one fails the download and keeps the partial file.
    pub sha256: Option<String>,
    /// Request headers for this download only (a browser's Cookie, Authorization, Referer, User-Agent). Memory only: never in the
    /// part state or any file, never logged (the type shows only the names). Cookie and Authorization are dropped when a redirect
    /// leaves the address's origin (scheme, host or port: https to http on the same host too).
    pub headers: Headers,
}

impl Default for Options {
    fn default() -> Self {
        Self { segments: 16, max_segments: 32, retries: 6, backoff: Duration::from_secs(1), user_agent: "leaf".into(), connect_timeout: Duration::from_secs(15), read_timeout: Duration::from_secs(30), sha256: None, headers: Headers::default() }
    }
}

/// What the caller can change or ask while a download runs.
#[derive(Clone, Default)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    /// Set: the connections go quiet (nothing is read) until it is cleared again.
    pub paused: Arc<AtomicBool>,
    /// Bytes per second over all connections; 0 = no limit. May change while running.
    pub limit: Arc<AtomicU64>,
}

pub struct Progress<'a> {
    /// The name the file will have.
    pub name: &'a str,
    pub done: u64,
    pub total: Option<u64>,
    /// Connections open right now.
    pub connections: usize,
    /// The part that was on disk from an earlier run.
    pub resumed: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolve {
    Replace,
    KeepBoth,
    Skip,
}

/// The caller's side. All three are called on the thread that called [`download`].
pub trait Host {
    fn progress(&self, p: &Progress);
    /// The name is taken: replace it, keep both (a new name) or skip. `incoming` is the size the server announced.
    fn exists(&self, path: &Path, incoming: Option<u64>) -> Resolve;
    /// Whether `need` more bytes fit in `dir` (the default lets everything through).
    fn room(&self, _need: u64, _dir: &Path) -> bool {
        true
    }
}

#[derive(Debug)]
pub enum Outcome {
    Saved(PathBuf),
    Skipped,
}

#[derive(Debug)]
pub enum Error {
    Cancelled,
    /// Not an http(s) address, or one the client refuses.
    BadUrl(String),
    /// The server's answer (404, 403, ...).
    Http(u16),
    /// Connection, timeout, a server error: tried again, this is what is left after the last try.
    Network(String),
    /// The certificate or the secure channel was refused.
    Secure(String),
    /// The file on the server is not the one the first request saw.
    Changed,
    /// The disk cannot hold it (bytes needed).
    NoRoom(u64),
    /// The server is busy or limits requests (429, 503): tried again with fewer connections.
    Busy(u16),
    /// The server stopped answering byte ranges part-way.
    NoRanges,
    /// The finished file is not the one the checksum names.
    Checksum { expected: String, got: String },
    Io(std::io::Error),
}

impl Error {
    fn retriable(&self) -> bool {
        matches!(self, Error::Network(_) | Error::Busy(_) | Error::NoRanges)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::BadUrl(s) | Error::Network(s) | Error::Secure(s) => write!(f, "{s}"),
            Error::Http(s) => write!(f, "HTTP {s}"),
            Error::Changed => write!(f, "the file on the server changed"),
            Error::NoRoom(n) => write!(f, "{n} bytes do not fit"),
            Error::Busy(s) => write!(f, "HTTP {s}"),
            Error::NoRanges => write!(f, "the server stopped answering byte ranges"),
            Error::Checksum { expected, got } => write!(f, "SHA-256 is {got}, expected {expected}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// An address with a password in it (`https://user:secret@host/`): leaf does not take those (the password would be kept in a file).
pub fn has_password(url: &str) -> bool {
    let rest = url.trim().split_once("://").map_or("", |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit_once('@').is_some_and(|(info, _)| info.contains(':'))
}

/// The address of the checksum file that sits beside `url`: the same address with `.sha256` after the file name.
pub fn sidecar_url(url: &str) -> String {
    let url = url.trim();
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    format!("{}.sha256{}", &url[..cut], &url[cut..])
}

/// The SHA-256 a checksum file names: its first 64-hex-digit word (`sha256sum` format: the digest, then the file name).
pub fn parse_sha256(text: &str) -> Option<String> {
    text.split_whitespace().map(|w| w.trim_start_matches('\\')).find(|w| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_ascii_lowercase)
}

/// Reads the checksum file beside `url` (`url.sha256`); `None` when there is none or it holds no digest.
pub fn fetch_sidecar(url: &str, opts: &Options) -> Option<String> {
    let agent = http::agent(opts);
    let mut resp = agent.get(sidecar_url(url)).call().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.body_mut().with_config().limit(64 << 10).read_to_string().ok()?;
    parse_sha256(&text)
}

fn sha256_of(path: &Path, cancel: &AtomicBool) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; BUF];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn is_http_url(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    (l.starts_with("http://") || l.starts_with("https://")) && !l.contains(char::is_whitespace)
}

// ---------------------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct State {
    url: String,
    len: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    /// (start, next byte to fetch, end)
    segs: Vec<(u64, u64, u64)>,
}

fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(PART_EXT);
    PathBuf::from(s)
}

fn state_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push(STATE_EXT);
    PathBuf::from(s)
}

fn read_state(part: &Path) -> Option<State> {
    serde_json::from_slice(&std::fs::read(state_path(part)).ok()?).ok()
}

fn write_state(part: &Path, st: &State) {
    let p = state_path(part);
    let mut tmp = p.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if let Ok(bytes) = serde_json::to_vec(st)
        && std::fs::write(&tmp, bytes).is_ok()
        && std::fs::rename(&tmp, &p).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Removes the state file (and a half-written one); tries again a few times where something briefly holds the file (a scanner).
fn remove_state(part: &Path) {
    let st = state_path(part);
    let mut tmp = st.as_os_str().to_owned();
    tmp.push(".tmp");
    for p in [PathBuf::from(tmp), st] {
        for _ in 0..5 {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => std::thread::sleep(Duration::from_millis(50)),
                _ => break,
            }
        }
    }
}

fn discard(part: &Path) {
    let _ = std::fs::remove_file(part);
    remove_state(part);
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// A name in `dir` that is free: `name`, else `stem (2).ext`, ...; a name whose partial file belongs to another address is not free,
/// one whose partial file belongs to this address is (the earlier run is carried on).
fn unique_target(dir: &Path, name: &str, url: &str) -> PathBuf {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 2.. {
        let cand = dir.join(format!("{stem} ({i}){ext}"));
        let part = part_path(&cand);
        let mine = !exists(&part) || read_state(&part).is_some_and(|s| s.url == url);
        if !exists(&cand) && mine {
            return cand;
        }
    }
    unreachable!()
}

#[cfg(unix)]
fn write_at(f: &File, b: &[u8], o: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, b, o)
}
#[cfg(windows)]
fn write_at(f: &File, b: &[u8], o: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, b, o)
}

fn write_all_at(f: &File, mut b: &[u8], mut o: u64) -> std::io::Result<()> {
    while !b.is_empty() {
        let n = write_at(f, b, o)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        b = &b[n..];
        o += n as u64;
    }
    Ok(())
}

fn backoff(o: &Options, attempt: u32) -> Duration {
    (o.backoff * 2u32.saturating_pow(attempt.min(8))).min(Duration::from_secs(30).max(o.backoff))
}

fn with_retries<T>(o: &Options, ctl: &Control, mut f: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    let mut attempt = 0;
    loop {
        if ctl.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        match f() {
            Err(e) if e.retriable() && attempt < o.retries => {
                log::debug!("download: {e}, trying again");
                http::sleep_checking(backoff(o, attempt), &|| ctl.cancel.load(Ordering::Relaxed));
                attempt += 1;
            }
            r => return r,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------

/// Downloads `url` into `dir` and returns where it went. Blocks; run it on its own thread.
pub fn download(url: &str, dir: &Path, opts: &Options, ctl: &Control, host: &dyn Host) -> Result<Outcome, Error> {
    let url = url.trim();
    if !is_http_url(url) {
        return Err(Error::BadUrl(url.to_string()));
    }
    let agent = http::agent(opts);
    for round in 0..2 {
        match run(&agent, url, dir, opts, ctl, host) {
            // the file changed while it was fetched: once more, from the start
            Err(Error::Changed) if round == 0 => log::info!("download: the file changed on the server, starting again"),
            r => return r,
        }
    }
    Err(Error::Changed)
}

fn run(agent: &ureq::Agent, url: &str, dir: &Path, opts: &Options, ctl: &Control, host: &dyn Host) -> Result<Outcome, Error> {
    let probe = with_retries(opts, ctl, || http::probe(agent, url, &opts.headers))?;
    let mut name = name::file_name(probe.disposition.as_deref(), url);
    if name == "download" && probe.disposition.is_none() {
        name = name::file_name(None, &probe.uri);
    }
    let mut target = dir.join(&name);
    let mut replace = false;
    if exists(&target) {
        match host.exists(&target, probe.len) {
            Resolve::Replace => replace = true,
            Resolve::KeepBoth => target = unique_target(dir, &name, url),
            Resolve::Skip => return Ok(Outcome::Skipped),
        }
    }
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
    let part = part_path(&target);
    let want = match probe.len {
        Some(len) if probe.ranges && len > 0 => start_count(opts, len),
        _ => 1,
    };
    // (what the other downloads from this server leave under the cap)
    let lease = host::Lease::take(host::host_of(&probe.uri), want);
    let net = Net { agent, probe: &probe, opts, ctl, limiter: Mutex::new(Instant::now()), allowed: AtomicUsize::new(lease.count()), lease };
    let job = Job { net, url, part: &part, host, name: &name };
    let r = match probe.len {
        Some(len) if probe.ranges && len > 0 => job.segmented(len),
        len => job.single(len),
    };
    let r = r.and_then(|()| match opts.sha256.as_deref().map(str::trim) {
        Some(want) => {
            let got = sha256_of(&part, &ctl.cancel)?;
            if got.eq_ignore_ascii_case(want) { Ok(()) } else { Err(Error::Checksum { expected: want.to_ascii_lowercase(), got }) }
        }
        None => Ok(()),
    });
    match r {
        Ok(()) => {
            let mut dest = target;
            if !replace && exists(&dest) {
                dest = unique_target(dir, &name, url);
            }
            std::fs::rename(&part, &dest)?;
            remove_state(&part);
            // (as a browser would: SmartScreen and Protected View still warn about it)
            mark::mark(&dest, &probe.uri, opts.headers.get("Referer"));
            Ok(Outcome::Saved(dest))
        }
        Err(Error::Cancelled) => {
            discard(&part);
            Err(Error::Cancelled)
        }
        Err(Error::Changed) => {
            discard(&part);
            Err(Error::Changed)
        }
        Err(e) => Err(e),
    }
}

struct Seg {
    start: u64,
    pos: AtomicU64,
    end: AtomicU64,
    active: AtomicBool,
}

impl Seg {
    fn new(start: u64, pos: u64, end: u64) -> Arc<Seg> {
        Arc::new(Seg { start, pos: AtomicU64::new(pos), end: AtomicU64::new(end), active: AtomicBool::new(false) })
    }
    fn done(&self) -> u64 {
        self.pos.load(Ordering::Relaxed).min(self.end.load(Ordering::Relaxed)).saturating_sub(self.start)
    }
}

/// What the connections share (the caller's host is not part of it: only the coordinating thread talks to that).
struct Net<'a> {
    agent: &'a ureq::Agent,
    probe: &'a http::Probe,
    opts: &'a Options,
    ctl: &'a Control,
    limiter: Mutex<Instant>,
    /// Connections that may run now: the server answering 429 or 503 (or refusing ranges) lowers it.
    allowed: AtomicUsize,
    /// What this download holds of its host's cap.
    lease: host::Lease,
}

struct Job<'a> {
    net: Net<'a>,
    url: &'a str,
    part: &'a Path,
    host: &'a dyn Host,
    name: &'a str,
}

impl Net<'_> {
    fn cancelled(&self) -> bool {
        self.ctl.cancel.load(Ordering::Relaxed)
    }

    /// The server is overloaded or refuses ranges: fewer connections from here on (half, or one).
    fn fewer_connections(&self, to_one: bool) {
        let cur = self.allowed.load(Ordering::Relaxed);
        let now = if to_one { 1 } else { (cur / 2).max(1) };
        if now < cur {
            log::info!("download: the server limits the connections, {cur} -> {now}");
            self.allowed.store(now, Ordering::Relaxed);
            self.lease.shrink_to(now);
        }
    }

    /// Holds the connections back while paused.
    fn wait_paused(&self) {
        while self.ctl.paused.load(Ordering::Relaxed) && !self.cancelled() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The speed limit: after `n` bytes were read, wait until the limit allows the next ones.
    fn throttle(&self, n: usize) {
        let lim = self.ctl.limit.load(Ordering::Relaxed);
        if lim == 0 {
            return;
        }
        let wait = {
            let mut next = self.limiter.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            // (a pause or an idle spell does not bank time: at most a quarter second of burst)
            if *next + Duration::from_millis(250) < now {
                *next = now - Duration::from_millis(250);
            }
            *next += Duration::from_secs_f64(n as f64 / lim as f64);
            next.saturating_duration_since(now)
        };
        http::sleep_checking(wait, &|| self.cancelled());
    }
}

impl Job<'_> {
    fn cancelled(&self) -> bool {
        self.net.cancelled()
    }

    fn segmented(&self, len: u64) -> Result<(), Error> {
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(self.part)?;
        let resumed = read_state(self.part).filter(|st| self.state_fits(st, len) && file.metadata().is_ok_and(|m| m.len() == len));
        let segs: Vec<Arc<Seg>> = match resumed {
            Some(st) => st.segs.iter().map(|&(s, p, e)| Seg::new(s, p.clamp(s, e), e)).collect(),
            None => {
                // (a different or stale partial file is not trusted)
                file.set_len(0)?;
                let n = start_count(self.net.opts, len) as u64;
                let size = len.div_ceil(n);
                (0..n).map(|i| Seg::new(i * size, i * size, ((i + 1) * size).min(len))).filter(|s| s.start < s.end.load(Ordering::Relaxed)).collect()
            }
        };
        let had: u64 = segs.iter().map(|s| s.done()).sum();
        if !self.host.room(len - had, self.part.parent().unwrap_or(Path::new("."))) {
            return Err(Error::NoRoom(len - had));
        }
        if had == 0 {
            // (the whole length at once: a disk that is too small says so now, not after hours)
            file.set_len(len)?;
        }
        let table = Mutex::new(segs);
        let failure: Mutex<Option<Error>> = Mutex::new(None);
        let stop = AtomicBool::new(false);
        let alive = AtomicUsize::new(0);
        let workers = start_count(self.net.opts, len);
        let workers = workers.min(self.net.allowed.load(Ordering::Relaxed)).max(1);
        let w = Worker { net: &self.net, file: &file, table: &table, failure: &failure, stop: &stop };
        std::thread::scope(|sc| {
            let spawn = |index: usize| {
                alive.fetch_add(1, Ordering::SeqCst);
                let (w, alive) = (&w, &alive);
                std::thread::Builder::new()
                    .name("leaf-download".into())
                    .spawn_scoped(sc, move || {
                        w.work(index);
                        alive.fetch_sub(1, Ordering::SeqCst);
                    })
                    .map(drop)
                    .unwrap_or_else(|_| {
                        alive.fetch_sub(1, Ordering::SeqCst);
                    });
            };
            for index in 0..workers {
                spawn(index);
            }
            // more connections while the total speed still rises (not under a speed limit, not after the server asked for fewer)
            let mut spawned = workers;
            let (mut window, mut window_done, mut last_rate, mut flat) = (Instant::now(), had, 0.0f64, 0u32);
            let mut saved = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let (done, active) = {
                    let t = table.lock().unwrap_or_else(|e| e.into_inner());
                    (t.iter().map(|s| s.done()).sum::<u64>(), t.iter().filter(|s| s.active.load(Ordering::Relaxed) && s.pos.load(Ordering::Relaxed) < s.end.load(Ordering::Relaxed)).count())
                };
                self.host.progress(&Progress { name: self.name, done, total: Some(len), connections: active, resumed: had });
                if saved.elapsed() >= Duration::from_secs(1) {
                    saved = Instant::now();
                    self.save(&table, len);
                }
                if window.elapsed() >= Duration::from_secs(3) && !self.net.ctl.paused.load(Ordering::Relaxed) {
                    let rate = done.saturating_sub(window_done) as f64 / window.elapsed().as_secs_f64();
                    (window, window_done) = (Instant::now(), done);
                    flat = if rate > last_rate * 1.1 { 0 } else { flat + 1 };
                    last_rate = rate;
                    let room = self.net.opts.max_segments.max(workers).saturating_sub(spawned);
                    let steady = self.net.allowed.load(Ordering::Relaxed) == spawned && self.net.ctl.limit.load(Ordering::Relaxed) == 0;
                    if flat < 2 && steady && room > 0 && len - done > 8 * PER_CONN {
                        let add = self.net.lease.grow(GROW_BY.min(room));
                        if add > 0 {
                            spawned += add;
                            self.net.allowed.store(spawned, Ordering::Relaxed);
                            for index in spawned - add..spawned {
                                spawn(index);
                            }
                        }
                    }
                }
                if done >= len || self.cancelled() || failure.lock().unwrap_or_else(|e| e.into_inner()).is_some() || alive.load(Ordering::SeqCst) == 0 {
                    break;
                }
            }
            stop.store(true, Ordering::SeqCst);
        });
        let done: u64 = table.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|s| s.done()).sum();
        if let Some(e) = failure.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.save(&table, len);
            return Err(e);
        }
        if done >= len {
            self.host.progress(&Progress { name: self.name, done: len, total: Some(len), connections: 0, resumed: had });
            return Ok(());
        }
        self.save(&table, len);
        Err(if self.cancelled() { Error::Cancelled } else { Error::Network("the connections ended early".into()) })
    }

    fn state_fits(&self, st: &State, len: u64) -> bool {
        if st.url != self.url || st.len != len || st.etag != self.net.probe.etag || st.last_modified != self.net.probe.last_modified {
            return false;
        }
        // the pieces must tile the file
        let mut v = st.segs.clone();
        v.sort_unstable();
        let mut at = 0;
        for (s, p, e) in v {
            if s != at || e < s || p < s || p > e {
                return false;
            }
            at = e;
        }
        at == len
    }

    fn save(&self, table: &Mutex<Vec<Arc<Seg>>>, len: u64) {
        let segs = table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|s| {
                let e = s.end.load(Ordering::Relaxed);
                (s.start, s.pos.load(Ordering::Relaxed).min(e), e)
            })
            .collect();
        write_state(self.part, &State { url: self.url.to_string(), len, etag: self.net.probe.etag.clone(), last_modified: self.net.probe.last_modified.clone(), segs });
    }

    /// One connection, from the start; for a server without ranges or without a length. Nothing is kept for a resume.
    fn single(&self, len: Option<u64>) -> Result<(), Error> {
        discard(self.part);
        if len.is_some_and(|l| !self.host.room(l, self.part.parent().unwrap_or(Path::new(".")))) {
            return Err(Error::NoRoom(len.unwrap_or(0)));
        }
        let mut attempt = 0;
        loop {
            let file = OpenOptions::new().write(true).create(true).truncate(true).open(self.part)?;
            let r = self.single_try(&file, len);
            match r {
                Err(e) if e.retriable() && attempt < self.net.opts.retries => {
                    http::sleep_checking(backoff(self.net.opts, attempt), &|| self.cancelled());
                    attempt += 1;
                    if self.cancelled() {
                        return Err(Error::Cancelled);
                    }
                }
                r => return r,
            }
        }
    }

    fn single_try(&self, file: &File, len: Option<u64>) -> Result<(), Error> {
        let mut body = http::open_range(self.net.agent, &self.net.probe.uri, 0, len.unwrap_or(0), true, self.net.probe)?;
        let mut buf = vec![0u8; BUF];
        let mut at = 0u64;
        let mut shown = Instant::now() - Duration::from_secs(1);
        loop {
            if self.cancelled() {
                return Err(Error::Cancelled);
            }
            self.net.wait_paused();
            let n = body.read(&mut buf).map_err(|e| Error::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            write_all_at(file, &buf[..n], at)?;
            at += n as u64;
            self.net.throttle(n);
            if shown.elapsed() >= Duration::from_millis(100) {
                shown = Instant::now();
                self.host.progress(&Progress { name: self.name, done: at, total: len, connections: 1, resumed: 0 });
            }
        }
        if len.is_some_and(|l| at < l) {
            return Err(Error::Network("the connection ended early".into()));
        }
        self.host.progress(&Progress { name: self.name, done: at, total: len, connections: 0, resumed: 0 });
        Ok(())
    }
}

struct Worker<'a> {
    net: &'a Net<'a>,
    file: &'a File,
    table: &'a Mutex<Vec<Arc<Seg>>>,
    failure: &'a Mutex<Option<Error>>,
    stop: &'a AtomicBool,
}

impl Worker<'_> {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.net.cancelled()
    }

    /// Worker `index` stops when the connections allowed fall to its number or below.
    fn surplus(&self, index: usize) -> bool {
        index >= self.net.allowed.load(Ordering::Relaxed)
    }

    fn work(&self, index: usize) {
        while !self.stopped() {
            if self.surplus(index) {
                return;
            }
            let Some(seg) = self.claim() else { return };
            let r = self.run_seg(&seg, index);
            seg.active.store(false, Ordering::SeqCst);
            if let Err(e) = r {
                let mut f = self.failure.lock().unwrap_or_else(|e| e.into_inner());
                if f.is_none() && !matches!(e, Error::Cancelled) {
                    *f = Some(e);
                }
                return;
            }
        }
    }

    /// A piece nobody has, else half of what the slowest running piece has left.
    fn claim(&self) -> Option<Arc<Seg>> {
        let mut t = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = t.iter().find(|s| !s.active.load(Ordering::SeqCst) && s.pos.load(Ordering::Relaxed) < s.end.load(Ordering::Relaxed)) {
            s.active.store(true, Ordering::SeqCst);
            return Some(s.clone());
        }
        let victim = t.iter().filter(|s| s.active.load(Ordering::SeqCst)).max_by_key(|s| s.end.load(Ordering::Relaxed).saturating_sub(s.pos.load(Ordering::Relaxed)))?.clone();
        let (pos, end) = (victim.pos.load(Ordering::Relaxed), victim.end.load(Ordering::Relaxed));
        let left = end.saturating_sub(pos);
        if left < 2 * MIN_STEAL {
            return None;
        }
        let mid = pos + left / 2;
        victim.end.store(mid, Ordering::SeqCst);
        let s = Seg::new(mid, mid, end);
        s.active.store(true, Ordering::SeqCst);
        t.push(s.clone());
        Some(s)
    }

    fn run_seg(&self, seg: &Seg, index: usize) -> Result<(), Error> {
        let net = self.net;
        let mut attempt = 0u32;
        let mut buf = vec![0u8; BUF];
        loop {
            if self.stopped() {
                return Err(Error::Cancelled);
            }
            net.wait_paused();
            let (pos, end) = (seg.pos.load(Ordering::SeqCst), seg.end.load(Ordering::SeqCst));
            if pos >= end {
                return Ok(());
            }
            let before = pos;
            let r = http::open_range(net.agent, &net.probe.uri, pos, end, false, net.probe).and_then(|mut body| {
                let mut progressed = false;
                loop {
                    if self.stopped() {
                        return Err(Error::Cancelled);
                    }
                    // paused: the connection is let go, the request is made again from here after
                    if net.ctl.paused.load(Ordering::Relaxed) {
                        return Ok(progressed);
                    }
                    let (pos, end) = (seg.pos.load(Ordering::SeqCst), seg.end.load(Ordering::SeqCst));
                    if pos >= end {
                        return Ok(true);
                    }
                    let want = ((end - pos) as usize).min(buf.len());
                    let n = body.read(&mut buf[..want]).map_err(|e| Error::Network(e.to_string()))?;
                    if n == 0 {
                        return Err(Error::Network("the connection ended early".into()));
                    }
                    write_all_at(self.file, &buf[..n], pos)?;
                    seg.pos.store(pos + n as u64, Ordering::SeqCst);
                    progressed = true;
                    net.throttle(n);
                }
            });
            match r {
                Ok(progressed) => {
                    if progressed {
                        attempt = 0;
                    }
                }
                Err(e) if e.retriable() && attempt < net.opts.retries => {
                    if seg.pos.load(Ordering::SeqCst) > before {
                        attempt = 0;
                    }
                    log::debug!("download: {e}, trying again");
                    match e {
                        Error::Busy(_) => net.fewer_connections(false),
                        Error::NoRanges => net.fewer_connections(true),
                        _ => {}
                    }
                    http::sleep_checking(backoff(net.opts, attempt), &|| self.stopped());
                    attempt += 1;
                    // this connection is no longer allowed: the piece is let go for the ones that are
                    if self.surplus(index) {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}
