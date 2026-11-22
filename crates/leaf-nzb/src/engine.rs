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
/// Answers in a row on one connection that are not articles (refusals, answers out of step, oversized ones): the
/// connection gives up.
const MAX_PROTOCOL_ERRORS: u32 = 8;
/// Times one article may break a connection before it counts as missing on that server.
const MAX_STRIKES: u32 = 3;
/// Download threads that may die of a bug before the job stops.
const MAX_PANICS: usize = 4;

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
    /// A download thread stopped on a bug, again and again.
    Crashed,
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
            Error::Crashed => f.write_str("a download thread stopped unexpectedly"),
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
    /// The [`Nzb::fingerprint`] of the job this state belongs to (0: written before leaf kept it).
    ///
    /// [`Nzb::fingerprint`]: crate::nzb::Nzb::fingerprint
    #[serde(default)]
    pub nzb: u64,
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
    /// Every segment of it was written.
    pub complete: bool,
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

/// What one connection thread holds that others need back when it dies: the articles it took, and whether it counts as a
/// connection.
#[derive(Default)]
struct Slot {
    inflight: Vec<usize>,
    connected: bool,
}

enum Stored {
    /// The part does not fit the file the NZB describes (a size or place far off): dropped as damaged.
    Damaged,
    /// The disk refused it (full, gone, no permission).
    Io(io::Error),
}

enum Outcome {
    Done,
    /// Not usable from this server: the next one is asked.
    Missing,
    /// Not answered: back in the queue.
    Again,
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
    /// The most bytes each file may have (from the NZB's own sizes): what a yEnc header says beyond it is not believed.
    bound: Vec<u64>,
    slots: Vec<Mutex<Slot>>,
    panics: AtomicUsize,
    /// Set: the job stops (the disk refused a write; threads keep dying).
    fatal: Mutex<Option<Error>>,
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
    fn store(&self, a: Art, part: yenc::Part) -> Result<bool, Stored> {
        let index = self.files[a.file].index;
        // (the yEnc header's size and place are the poster's word: bounded by what the NZB says the file weighs)
        let bound = self.bound[a.file];
        let end = part.offset.saturating_add(part.data.len() as u64);
        if part.file_size > bound || end > bound || (part.file_size > 0 && end > part.file_size) {
            return Err(Stored::Damaged);
        }
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
        let f = self.handle(a.file).map_err(Stored::Io)?;
        if let Some(size) = grow
            && f.metadata().map(|m| m.len()).unwrap_or(0) < size
        {
            f.set_len(size).map_err(Stored::Io)?;
        }
        write_at(&f, &part.data, part.offset).map_err(Stored::Io)?;
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
        self.ctl.cancel.load(Ordering::Relaxed) || self.finished() || self.fatal.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn set_fatal(&self, e: Error) {
        let mut f = self.fatal.lock().unwrap_or_else(|e| e.into_inner());
        if f.is_none() {
            *f = Some(e);
        }
        drop(f);
        self.changed.notify_all();
    }

    /// Article `a` is not to be had from server `s`: the next server is asked, or it is given up.
    fn not_here(&self, a: usize, s: usize) {
        let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        let art = d.arts[a];
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

    fn opened(&self, w: usize) {
        self.connections.fetch_add(1, Ordering::Relaxed);
        self.slots[w].lock().unwrap_or_else(|e| e.into_inner()).connected = true;
    }

    fn closed(&self, w: usize) {
        let mut slot = self.slots[w].lock().unwrap_or_else(|e| e.into_inner());
        if std::mem::take(&mut slot.connected) {
            self.connections.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// A connection thread died: what it held goes back to the others.
    fn recover(&self, w: usize) {
        let inflight = std::mem::take(&mut self.slots[w].lock().unwrap_or_else(|e| e.into_inner()).inflight);
        self.closed(w);
        let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        for a in inflight {
            d.requeue(a);
        }
        drop(d);
        self.changed.notify_all();
    }

    /// The data of every part file on the disk (before a state that marks its parts done is saved).
    fn sync(&self) -> bool {
        let open: Vec<Arc<File>> = self.handles.iter().filter_map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone()).collect();
        open.iter().all(|f| f.sync_data().is_ok())
    }

    /// Why a failure now is not the server's: the route is away (the VPN) or changed since `generation`.
    fn route_trouble(&self, generation: u64) -> Option<String> {
        self.dialer.wait_reason().or_else(|| (self.dialer.generation() != generation).then(String::new))
    }

    /// One connection of server `s` (thread `w`).
    fn worker(&self, s: usize, w: usize) {
        let server = &self.servers[s];
        let mut client: Option<(Client, u64)> = None;
        let mut fails = 0u32;
        // answers in a row that were not articles; how often each article broke this connection
        let mut protocol = 0u32;
        let mut strikes: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
        loop {
            if self.stop() {
                break;
            }
            if self.ctl.paused.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            // the route changed (the VPN, or the Usenet route setting): closed, dialled again on the route of now
            if client.as_ref().is_some_and(|(_, g)| *g != self.dialer.generation()) {
                client = None;
                self.closed(w);
            }
            if client.is_none() {
                let generation = self.dialer.generation();
                match Client::connect(server, self.dialer) {
                    Ok(c) => {
                        fails = 0;
                        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        self.opened(w);
                        client = Some((c, generation));
                    }
                    Err(nntp::Error::Wait(why)) => {
                        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    Err(e) => {
                        // (a tunnel that is not up or just changed: a wait, never a count towards failing)
                        if let Some(why) = self.route_trouble(generation) {
                            if !why.is_empty() {
                                *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                            }
                            std::thread::sleep(Duration::from_secs(1));
                            continue;
                        }
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
            self.slots[w].lock().unwrap_or_else(|e| e.into_inner()).inflight = batch.iter().map(|(a, _)| *a).collect();
            let reqs: Vec<(&str, u64)> = batch
                .iter()
                .map(|(_, art)| {
                    let seg = &self.files[art.file].segments[art.seg];
                    (seg.message_id.as_str(), seg.bytes)
                })
                .collect();
            let mut answered = 0;
            let (c, generation) = client.as_mut().expect("connected above");
            let generation = *generation;
            let result = c.bodies(&reqs, |i, body, c| {
                answered = i + 1;
                let (a, art) = batch[i];
                let n = c.take_read();
                self.received.fetch_add(n, Ordering::Relaxed);
                self.limiter.take(n, self.ctl.limit.load(Ordering::Relaxed));
                let outcome = match body {
                    Body::Found(data, _) => {
                        protocol = 0;
                        match yenc::decode(&data) {
                            Ok(part) => match self.store(art, part) {
                                Ok(true) => Outcome::Done,
                                // (the part is written; another server may have a good copy)
                                Ok(false) => Outcome::Missing,
                                Err(Stored::Damaged) => {
                                    log::info!("usenet: {}: a part does not fit its file; dropped", server.name);
                                    Outcome::Missing
                                }
                                // a full or failing disk: the job stops and keeps what it has (never fetched again from the
                                // backup servers as if it were missing)
                                Err(Stored::Io(e)) => {
                                    log::warn!("usenet: writing a part failed: {e}");
                                    self.set_fatal(Error::Io(e));
                                    Outcome::Again
                                }
                            },
                            Err(e) => {
                                log::info!("usenet: {}: {e}", server.name);
                                Outcome::Missing
                            }
                        }
                    }
                    Body::Missing => {
                        protocol = 0;
                        Outcome::Missing
                    }
                    Body::Refused(code) => {
                        protocol += 1;
                        log::info!("usenet: {}: article refused ({code})", server.name);
                        Outcome::Missing
                    }
                };
                match outcome {
                    Outcome::Done => {
                        self.dispatch.lock().unwrap_or_else(|e| e.into_inner()).left -= 1;
                        self.resolved(art);
                    }
                    Outcome::Missing => self.not_here(a, s),
                    Outcome::Again => self.dispatch.lock().unwrap_or_else(|e| e.into_inner()).requeue(a),
                }
                self.slots[w].lock().unwrap_or_else(|e| e.into_inner()).inflight.retain(|x| *x != a);
                self.changed.notify_all();
            });
            self.slots[w].lock().unwrap_or_else(|e| e.into_inner()).inflight.clear();
            let error = match result {
                Ok(()) if protocol < MAX_PROTOCOL_ERRORS => continue,
                Ok(()) => None,
                Err(e) => Some(e),
            };
            let rest: Vec<usize> = batch[answered.min(batch.len())..].iter().map(|(a, _)| *a).collect();
            let trouble = self.route_trouble(generation).is_some();
            if let Some(e) = &error {
                log::info!("usenet: {}: connection lost: {e}", server.name);
            }
            // the article the answer broke at: counted against this server (an oversized one at once), unless the route was
            // the cause
            let mut give_back = rest.as_slice();
            if !trouble && let (Some(e), Some(first)) = (&error, rest.first()) {
                let strike = strikes.entry(*first).or_insert(0);
                *strike += 1;
                if matches!(e, nntp::Error::TooLarge | nntp::Error::Protocol(_) | nntp::Error::BadId) {
                    protocol += 1;
                }
                if matches!(e, nntp::Error::TooLarge | nntp::Error::BadId) || *strike >= MAX_STRIKES {
                    strikes.remove(first);
                    self.not_here(*first, s);
                    give_back = &rest[1..];
                }
            }
            let mut d = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
            for a in give_back {
                d.requeue(*a);
            }
            drop(d);
            self.changed.notify_all();
            if client.take().is_some() {
                self.closed(w);
            }
            if !trouble {
                fails += 1;
                if protocol >= MAX_PROTOCOL_ERRORS {
                    log::warn!("usenet: {}: too many answers that are not articles; this connection stops", server.name);
                    self.dispatch.lock().unwrap_or_else(|e| e.into_inner()).last_error = Some("the server keeps refusing articles".into());
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if let Some((c, _)) = client.take() {
            self.closed(w);
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
    let mut total_bytes: u64 = 0;
    let mut done_bytes: u64 = 0;
    let mut bad_ids = 0;
    let mut bound = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let fs = state.files.entry(f.index).or_default();
        if fs.part.is_empty() {
            fs.part = format!("{}{PART_EXT}", if f.name.is_empty() { format!("file{:03}", f.index + 1) } else { f.name.clone() });
        }
        // (decoded parts are smaller than their articles; an article without a stated size may be up to the read cap)
        let wire = f.segments.iter().map(|s| if s.bytes == 0 { nntp::MAX_BODY } else { s.bytes }).fold(0u64, u64::saturating_add);
        bound.push(wire.saturating_add(wire / 16).saturating_add(1 << 20));
        for (si, seg) in f.segments.iter().enumerate() {
            total_bytes = total_bytes.saturating_add(seg.bytes);
            if fs.finished.is_some() || bit(&fs.done, si) {
                done_bytes = done_bytes.saturating_add(seg.bytes);
            } else if !nntp::valid_id(&seg.message_id) {
                // (an id the parser refused: never asked, missing)
                done_bytes = done_bytes.saturating_add(seg.bytes);
                set_bit(&mut fs.failed, si, true);
                bad_ids += 1;
            } else {
                arts.push(Art { file: fi, seg: si });
            }
        }
    }
    let need = total_bytes.saturating_sub(done_bytes);
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
        bound,
        slots: servers.iter().flat_map(|s| (0..s.connections.clamp(1, crate::server::MAX_CONNECTIONS)).map(|_| Mutex::new(Slot::default()))).collect(),
        panics: AtomicUsize::new(0),
        fatal: Mutex::new(None),
    };
    let report = |sh: &Shared| {
        let d = sh.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        let failed = d.failed.len() + bad_ids;
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
            let mut w = 0;
            for (s, server) in servers.iter().enumerate() {
                for _ in 0..server.connections.clamp(1, crate::server::MAX_CONNECTIONS) {
                    let sh = &shared;
                    scope.spawn(move || {
                        loop {
                            // (a bug in one thread never hangs the job: its articles go back, the thread starts again)
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sh.worker(s, w))).is_ok() {
                                break;
                            }
                            log::warn!("usenet: a download thread stopped unexpectedly");
                            sh.recover(w);
                            if sh.panics.fetch_add(1, Ordering::AcqRel) + 1 >= MAX_PANICS {
                                sh.set_fatal(Error::Crashed);
                                break;
                            }
                        }
                    });
                    w += 1;
                }
            }
            let mut saved = Instant::now();
            while !shared.stop() {
                report(&shared);
                if saved.elapsed() > Duration::from_secs(5) {
                    // (what the state marks done is on the disk first: a snapshot, then the data synced, then the state)
                    let snapshot = shared.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if shared.sync() {
                        snapshot.save(dir);
                    }
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
    if let Some(e) = shared.fatal.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
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
        let complete = (0..f.segments.len()).all(|i| bit(&fs.done, i));
        if let Some(done) = &fs.finished {
            out.push(Fetched { index: f.index, path: Some(dir.join(done)), failed, bad_crc: fs.bad_crc, complete });
            continue;
        }
        let part = dir.join(&fs.part);
        if count(&fs.done) == 0 || !part.exists() {
            out.push(Fetched { index: f.index, path: None, failed, bad_crc: fs.bad_crc, complete: false });
            continue;
        }
        let name = final_name(&f.name, fs.yenc_name.as_deref());
        let target = unique(dir, &name);
        std::fs::rename(&part, &target).map_err(Error::Io)?;
        fs.finished = target.file_name().map(|n| n.to_string_lossy().into_owned());
        out.push(Fetched { index: f.index, path: Some(target), failed, bad_crc: fs.bad_crc, complete });
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
