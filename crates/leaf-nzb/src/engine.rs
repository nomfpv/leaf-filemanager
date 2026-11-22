//! Fetching a job's articles: per-server connection pools (one thread per connection), `BODY` requests pipelined a few at a
//! time, an article a server does not have handed to the next server of its priority, then to the next priority. Each
//! decoded part is written straight into its place in the file's `.leaf-part` file (positional writes, nothing cached on
//! disk); which parts are written is kept in the job folder so a restart carries on where it stopped.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::dial::Dialer;
use crate::nntp::{self, Body, Client};
use crate::nzb::{Segment, looks_obfuscated, safe_name};
use crate::server::Server;
use crate::yenc;

pub const PART_EXT: &str = ".leaf-part";
/// The job's state file in its folder.
pub const STATE_FILE: &str = ".leaf-nzb.json";
/// Requests sent on one connection before the first answer is read.
const PIPELINE: usize = 4;

/// One file of the NZB to fetch.
#[derive(Clone, Debug)]
pub struct FileJob {
    /// Its place in the NZB (the key of its state).
    pub index: usize,
    /// The name the NZB gives it.
    pub name: String,
    /// Unix seconds of the post (for the servers' retention).
    pub date: u64,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Default)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    /// Set: no new requests are sent until it is cleared.
    pub paused: Arc<AtomicBool>,
    /// Bytes per second over all connections (0 = none); may change while running.
    pub limit: Arc<AtomicU64>,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// What the NZB says the articles weigh: done (fetched or given up) and in all.
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Bytes read from the servers so far (for the speed).
    pub received: u64,
    pub articles_total: usize,
    pub articles_failed: usize,
    pub connections: usize,
    /// Set while the connections wait for the route (the VPN): leaf's words.
    pub waiting: Option<String>,
}

pub trait Host: Sync {
    fn progress(&self, p: &Progress);
    /// Whether `need` more bytes fit in `dir`.
    fn room(&self, _need: u64, _dir: &Path) -> bool {
        true
    }
}

#[derive(Debug)]
pub enum Error {
    Cancelled,
    NoServers,
    /// Not enough free space for what is left.
    NoRoom(u64),
    /// No server could be used (every one refused the sign-in or could not be reached); the last reason.
    Unreachable(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelled"),
            Error::NoServers => f.write_str("no Usenet server is set up"),
            Error::NoRoom(n) => write!(f, "{n} bytes do not fit"),
            Error::Unreachable(d) => write!(f, "no server could be used: {d}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

/// What is known of one file across runs (saved in [`STATE_FILE`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileState {
    /// The name its `.leaf-part` file has (the NZB's name when it was started).
    pub part: String,
    /// The name the poster's yEnc header gives.
    pub yenc_name: Option<String>,
    pub size: u64,
    /// Segments written (bit per segment, in the NZB's order).
    pub done: Vec<u64>,
    /// Segments no server had (the last run).
    pub failed: Vec<u64>,
    /// Parts written whose CRC did not match (kept: better than nothing for par2).
    pub bad_crc: u32,
    /// The final file (after the part file was renamed), relative to the job folder.
    pub finished: Option<String>,
}

fn bit(v: &[u64], i: usize) -> bool {
    v.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0)
}

fn set_bit(v: &mut Vec<u64>, i: usize, on: bool) {
    if v.len() <= i / 64 {
        v.resize(i / 64 + 1, 0);
    }
    if on {
        v[i / 64] |= 1 << (i % 64);
    } else {
        v[i / 64] &= !(1 << (i % 64));
    }
}

fn count(v: &[u64]) -> usize {
    v.iter().map(|w| w.count_ones() as usize).sum()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub files: BTreeMap<usize, FileState>,
}

impl State {
    pub fn load(dir: &Path) -> State {
        std::fs::read(dir.join(STATE_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        let path = dir.join(STATE_FILE);
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        if let Ok(b) = serde_json::to_vec(self)
            && std::fs::write(&tmp, b).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// One fetched file.
#[derive(Clone, Debug)]
pub struct Fetched {
    pub index: usize,
    /// The file in the job folder; `None` when not one part of it could be fetched.
    pub path: Option<PathBuf>,
    pub failed: usize,
    pub bad_crc: u32,
}

// ------------------------------------------------------------------------------------------------ who gets which article

#[derive(Clone, Copy)]
struct Art {
    file: usize,
    seg: usize,
}

struct Dispatch {
    arts: Vec<Art>,
    /// Bit per server: asked and did not have it.
    tried: Vec<u64>,
    level_of: Vec<usize>,
    /// The servers of each priority level that has any (bit masks), in order.
    levels: Vec<u64>,
    server_level: Vec<usize>,
    queue: Vec<VecDeque<usize>>,
    dead: u64,
    left: usize,
    failed: Vec<usize>,
    last_error: Option<String>,
}

enum Next {
    Queued,
    Failed,
}

impl Dispatch {
    fn escalate(&mut self, a: usize) -> Next {
        let out = self.tried[a] | self.dead;
        let mut l = self.level_of[a];
        while l < self.levels.len() && self.levels[l] & !out == 0 {
            l += 1;
        }
        if l >= self.levels.len() {
            self.left -= 1;
            self.failed.push(a);
            return Next::Failed;
        }
        self.level_of[a] = l;
        self.queue[l].push_back(a);
        Next::Queued
    }

    /// Up to `max` articles for server `s` (none it was asked about before, none older than its retention).
    fn take(&mut self, s: usize, max: usize, keeps: impl Fn(usize) -> bool) -> Vec<usize> {
        let l = self.server_level[s];
        let me = 1u64 << s;
        let mut got = Vec::new();
        let mut moved = Vec::new();
        let mut i = 0;
        while got.len() < max && i < self.queue[l].len() {
            let a = self.queue[l][i];
            if self.tried[a] & me != 0 {
                i += 1;
            } else if !keeps(self.arts[a].file) {
                self.tried[a] |= me;
                if (self.tried[a] | self.dead) & self.levels[l] == self.levels[l] {
                    self.queue[l].remove(i);
                    moved.push(a);
                } else {
                    i += 1;
                }
            } else {
                self.queue[l].remove(i);
                got.push(a);
            }
        }
        for a in moved {
            self.escalate(a);
        }
        got
    }

    fn missing(&mut self, a: usize, s: usize) {
        self.tried[a] |= 1 << s;
        self.escalate(a);
    }

    fn requeue(&mut self, a: usize) {
        let l = self.level_of[a];
        self.queue[l].push_front(a);
    }

    fn mark_dead(&mut self, s: usize) {
        self.dead |= 1 << s;
        for l in 0..self.queue.len() {
            let all: Vec<usize> = self.queue[l].drain(..).collect();
            for a in all {
                self.level_of[a] = l;
                if (self.tried[a] | self.dead) & self.levels[l] == self.levels[l] {
                    self.escalate(a);
                } else {
                    self.queue[l].push_back(a);
                }
            }
        }
    }
}

struct Limiter {
    window: Mutex<(Instant, u64)>,
}

impl Limiter {
    fn take(&self, n: u64, limit: u64) {
        if limit == 0 {
            return;
        }
        let wait = {
            let mut w = self.window.lock().unwrap_or_else(|e| e.into_inner());
            if w.0.elapsed() > Duration::from_secs(2) {
                *w = (Instant::now(), 0);
            }
            w.1 += n;
            let due = Duration::from_secs_f64(w.1 as f64 / limit as f64);
            due.saturating_sub(w.0.elapsed())
        };
        if !wait.is_zero() {
            std::thread::sleep(wait.min(Duration::from_secs(2)));
        }
    }
}

struct Shared<'a> {
    dir: &'a Path,
    files: &'a [FileJob],
    servers: &'a [Server],
    dialer: &'a dyn Dialer,
    ctl: &'a Control,
    dispatch: Mutex<Dispatch>,
    changed: Condvar,
    state: Mutex<State>,
    handles: Vec<Mutex<Option<Arc<File>>>>,
    done_bytes: AtomicU64,
    received: AtomicU64,
    connections: AtomicUsize,
    waiting: Mutex<Option<String>>,
    alive: Vec<AtomicUsize>,
    limiter: Limiter,
    now: u64,
}

#[cfg(windows)]
fn write_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "nothing written"));
        }
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, off)
}

impl Shared<'_> {
    fn handle(&self, fi: usize) -> io::Result<Arc<File>> {
        let mut h = self.handles[fi].lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = h.as_ref() {
            return Ok(f.clone());
        }
        let part = self.state.lock().unwrap_or_else(|e| e.into_inner()).files.get(&self.files[fi].index).map(|s| s.part.clone()).unwrap_or_default();
        let f = Arc::new(OpenOptions::new().create(true).write(true).truncate(false).open(self.dir.join(part))?);
        *h = Some(f.clone());
        Ok(f)
    }

    /// Writes a decoded part; `false` when its CRC did not match.
    fn store(&self, a: Art, part: yenc::Part) -> io::Result<bool> {
        let index = self.files[a.file].index;
        let grow = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let fs = st.files.entry(index).or_default();
            if fs.yenc_name.is_none() && !part.name.is_empty() {
                fs.yenc_name = Some(part.name.clone());
            }
            let grow = part.file_size > fs.size;
            if grow {
                fs.size = part.file_size;
            }
            grow.then_some(part.file_size)
        };
        let f = self.handle(a.file)?;
        if let Some(size) = grow
            && f.metadata().map(|m| m.len()).unwrap_or(0) < size
        {
            f.set_len(size)?;
        }
        write_at(&f, &part.data, part.offset)?;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let fs = st.files.entry(index).or_default();
        set_bit(&mut fs.done, a.seg, true);
        set_bit(&mut fs.failed, a.seg, false);
        if !part.crc_ok {
            fs.bad_crc += 1;
        }
        Ok(part.crc_ok)
    }

    fn resolved(&self, a: Art) {
        self.done_bytes.fetch_add(self.files[a.file].segments[a.seg].bytes, Ordering::Relaxed);
    }

    fn finished(&self) -> bool {
        self.dispatch.lock().unwrap_or_else(|e| e.into_inner()).left == 0
    }

    fn stop(&self) -> bool {
        self.ctl.cancel.load(Ordering::Relaxed) || self.finished()
    }

    /// One connection of server `s`.
    fn worker(&self, s: usize) {
        let server = &self.servers[s];
        let mut client: Option<(Client, u64)> = None;
        let mut fails = 0u32;
        loop {
            if self.stop() {
                break;
            }
            if self.ctl.paused.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if client.as_ref().is_some_and(|(_, g)| *g != self.dialer.generation()) {
                client = None;
            }
            if client.is_none() {
                let generation = self.dialer.generation();
                match Client::connect(server, self.dialer) {
                    Ok(c) => {
                        fails = 0;
                        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        self.connections.fetch_add(1, Ordering::Relaxed);
                        client = Some((c, generation));
                    }
                    Err(nntp::Error::Wait(w)) => {
                        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    Err(e) => {
                        let auth = matches!(e, nntp::Error::Auth(_));
                        fails += 1;
                        log::warn!("usenet: {} ({}): {e}", server.name, server.host);
                        self.dispatch.lock().unwrap_or_else(|e| e.into_inner()).last_error = Some(e.to_string());
                        if auth || fails >= 4 {
                            break;
                        }
                        let wait = Duration::from_secs(2u64 << fails.min(4));
                        let until = Instant::now() + wait;
                        while Instant::now() < until && !self.stop() {
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        continue;
                    }
                }
            }
            let batch: Vec<(usize, Art)> = {
                let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
                let got = d.take(s, PIPELINE, |file| server.keeps(self.files[file].date, self.now));
                if got.is_empty() {
                    let _unused = self.changed.wait_timeout(d, Duration::from_millis(300));
                    continue;
                }
                got.into_iter().map(|a| (a, d.arts[a])).collect()
            };
            let ids: Vec<&str> = batch.iter().map(|(_, art)| self.files[art.file].segments[art.seg].message_id.as_str()).collect();
            let mut answered = 0;
            let (c, _) = client.as_mut().expect("connected above");
            let result = c.bodies(&ids, |i, body, c| {
                answered = i + 1;
                let (a, art) = batch[i];
                let n = c.take_read();
                self.received.fetch_add(n, Ordering::Relaxed);
                self.limiter.take(n, self.ctl.limit.load(Ordering::Relaxed));
                let outcome = match body {
                    Body::Found(data, _) => match yenc::decode(&data) {
                        Ok(part) => match self.store(art, part) {
                            Ok(true) => Some(true),
                            // (the part is written; another server may have a good copy)
                            Ok(false) => None,
                            Err(e) => {
                                log::warn!("usenet: writing a part failed: {e}");
                                None
                            }
                        },
                        Err(e) => {
                            log::info!("usenet: {}: {e}", server.name);
                            None
                        }
                    },
                    Body::Missing => None,
                };
                let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
                match outcome {
                    Some(_) => {
                        d.left -= 1;
                        drop(d);
                        self.resolved(art);
                    }
                    None => {
                        let before = d.failed.len();
                        d.missing(a, s);
                        let gave_up = d.failed.len() > before;
                        drop(d);
                        if gave_up {
                            self.resolved(art);
                            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            let fs = st.files.entry(self.files[art.file].index).or_default();
                            if !bit(&fs.done, art.seg) {
                                set_bit(&mut fs.failed, art.seg, true);
                            }
                        }
                    }
                }
                self.changed.notify_all();
            });
            if let Err(e) = result {
                log::info!("usenet: {}: connection lost: {e}", server.name);
                let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
                for (a, _) in &batch[answered..] {
                    d.requeue(*a);
                }
                drop(d);
                self.changed.notify_all();
                if client.take().is_some() {
                    self.connections.fetch_sub(1, Ordering::Relaxed);
                }
                fails += 1;
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        if let Some((c, _)) = client.take() {
            self.connections.fetch_sub(1, Ordering::Relaxed);
            c.quit();
        }
        // the last connection of this server gave up: what it would have been asked goes to the others
        if !self.stop() && self.alive[s].fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
            let before = d.failed.len();
            d.mark_dead(s);
            let gone: Vec<usize> = d.failed[before..].to_vec();
            let arts: Vec<Art> = gone.iter().map(|a| d.arts[*a]).collect();
            drop(d);
            for art in arts {
                self.resolved(art);
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let fs = st.files.entry(self.files[art.file].index).or_default();
                set_bit(&mut fs.failed, art.seg, true);
            }
            self.changed.notify_all();
        }
    }
}

/// Fetches the articles of `files` into `dir`. `state` is the job's saved state (loaded by the caller, saved here as it
/// goes and at the end).
pub fn download(dir: &Path, files: &[FileJob], servers: &[Server], dialer: &dyn Dialer, ctl: &Control, host: &dyn Host, state: &mut State) -> Result<Vec<Fetched>, Error> {
    let servers: Vec<Server> = {
        let mut v: Vec<Server> = servers.iter().filter(|s| s.connections > 0).cloned().collect();
        v.sort_by_key(|s| s.priority);
        v.truncate(64);
        v
    };
    if servers.is_empty() {
        return Err(Error::NoServers);
    }
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let mut arts = Vec::new();
    let mut total_bytes = 0;
    let mut done_bytes = 0;
    for (fi, f) in files.iter().enumerate() {
        let fs = state.files.entry(f.index).or_default();
        if fs.part.is_empty() {
            fs.part = format!("{}{PART_EXT}", if f.name.is_empty() { format!("file{:03}", f.index + 1) } else { f.name.clone() });
        }
        for (si, seg) in f.segments.iter().enumerate() {
            total_bytes += seg.bytes;
            if fs.finished.is_some() || bit(&fs.done, si) {
                done_bytes += seg.bytes;
            } else {
                arts.push(Art { file: fi, seg: si });
            }
        }
    }
    let need = total_bytes - done_bytes;
    if need > 0 && !host.room(need, dir) {
        return Err(Error::NoRoom(need));
    }
    // the priority levels that have servers
    let mut levels: Vec<(crate::server::Priority, u64)> = Vec::new();
    let mut server_level = Vec::new();
    for (i, s) in servers.iter().enumerate() {
        if levels.last().is_none_or(|(p, _)| *p != s.priority) {
            levels.push((s.priority, 0));
        }
        let l = levels.len() - 1;
        levels[l].1 |= 1 << i;
        server_level.push(l);
    }
    let n = arts.len();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let dispatch = Dispatch {
        tried: vec![0; n],
        level_of: vec![0; n],
        levels: levels.iter().map(|(_, m)| *m).collect(),
        server_level,
        queue: {
            let mut q = vec![VecDeque::new(); levels.len()];
            q[0] = (0..n).collect();
            q
        },
        arts,
        dead: 0,
        left: n,
        failed: Vec::new(),
        last_error: None,
    };
    let shared = Shared {
        dir,
        files,
        servers: &servers,
        dialer,
        ctl,
        dispatch: Mutex::new(dispatch),
        changed: Condvar::new(),
        state: Mutex::new(std::mem::take(state)),
        handles: files.iter().map(|_| Mutex::new(None)).collect(),
        done_bytes: AtomicU64::new(done_bytes),
        received: AtomicU64::new(0),
        connections: AtomicUsize::new(0),
        waiting: Mutex::new(None),
        alive: servers.iter().map(|s| AtomicUsize::new(s.connections.clamp(1, crate::server::MAX_CONNECTIONS) as usize)).collect(),
        limiter: Limiter { window: Mutex::new((Instant::now(), 0)) },
        now,
    };
    let report = |sh: &Shared| {
        let d = sh.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        let failed = d.failed.len();
        drop(d);
        host.progress(&Progress {
            done_bytes: sh.done_bytes.load(Ordering::Relaxed),
            total_bytes,
            received: sh.received.load(Ordering::Relaxed),
            articles_total: files.iter().map(|f| f.segments.len()).sum(),
            articles_failed: failed,
            connections: sh.connections.load(Ordering::Relaxed),
            waiting: sh.waiting.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        });
    };
    if n > 0 {
        std::thread::scope(|scope| {
            for (s, server) in servers.iter().enumerate() {
                for _ in 0..server.connections.clamp(1, crate::server::MAX_CONNECTIONS) {
                    let sh = &shared;
                    scope.spawn(move || sh.worker(s));
                }
            }
            let mut saved = Instant::now();
            while !shared.stop() {
                report(&shared);
                if saved.elapsed() > Duration::from_secs(5) {
                    shared.state.lock().unwrap_or_else(|e| e.into_inner()).save(dir);
                    saved = Instant::now();
                }
                let d = shared.dispatch.lock().unwrap_or_else(|e| e.into_inner());
                let _unused = shared.changed.wait_timeout(d, Duration::from_millis(250));
            }
            shared.changed.notify_all();
        });
    }
    report(&shared);
    for h in &shared.handles {
        if let Some(f) = h.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = f.sync_all();
        }
    }
    *state = shared.state.into_inner().unwrap_or_else(|e| e.into_inner());
    let d = shared.dispatch.into_inner().unwrap_or_else(|e| e.into_inner());
    state.save(dir);
    if ctl.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    // nothing could be asked anywhere: every server was out (sign-in refused, unreachable)
    if n > 0 && d.dead.count_ones() as usize == servers.len() && d.failed.len() == n {
        return Err(Error::Unreachable(d.last_error.unwrap_or_default()));
    }
    // every segment of each file was asked: the part file becomes the file
    let mut out = Vec::new();
    for f in files {
        let fs = state.files.entry(f.index).or_default();
        let failed = count(&fs.failed);
        if let Some(done) = &fs.finished {
            out.push(Fetched { index: f.index, path: Some(dir.join(done)), failed, bad_crc: fs.bad_crc });
            continue;
        }
        let part = dir.join(&fs.part);
        if count(&fs.done) == 0 || !part.exists() {
            out.push(Fetched { index: f.index, path: None, failed, bad_crc: fs.bad_crc });
            continue;
        }
        let name = final_name(&f.name, fs.yenc_name.as_deref());
        let target = unique(dir, &name);
        std::fs::rename(&part, &target).map_err(Error::Io)?;
        fs.finished = target.file_name().map(|n| n.to_string_lossy().into_owned());
        out.push(Fetched { index: f.index, path: Some(target), failed, bad_crc: fs.bad_crc });
    }
    state.save(dir);
    Ok(out)
}

/// The NZB's name, or the yEnc header's when the NZB's is obfuscated and the header's is not.
pub fn final_name(nzb_name: &str, yenc_name: Option<&str>) -> String {
    let y = yenc_name.map(safe_name).filter(|y| !y.is_empty());
    let n = safe_name(nzb_name);
    match y {
        Some(y) if n.is_empty() || (looks_obfuscated(&n) && !looks_obfuscated(&y)) => y,
        _ if n.is_empty() => "file".to_string(),
        _ => n,
    }
}

/// `dir/name`, or `dir/name (2).ext`, ... when the name is taken.
pub fn unique(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..100_000).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(first)
}
