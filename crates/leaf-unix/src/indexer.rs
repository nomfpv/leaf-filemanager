//! Linux backend of rf_win::indexer. Status: REAL (a name index per root, kept fresh by inotify)
//!
//! [`start`] loads each root's saved index (searchable at once), walks the root again on its own thread (cancellable) and swaps the
//! fresh index in. A recursive `notify` watcher (inotify) collects changed folders; every second they are listed again
//! ([`Index::refresh_dir`]). Indexes are saved after a build and at most every ten minutes while they change, and when [`stop`]
//! runs. The files live in `dir` (the app passes `XDG_CACHE_HOME/leaf/index`). Folders in the exclusion list
//! ([`crate::nameindex`]) are not walked. Without an index, [`search`] answers `None` and the caller walks the disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use parking_lot::{Mutex, RwLock};

use crate::nameindex::Index;
use crate::search::Matcher;

// TYPE-COPY(rf-win src/indexer.rs:23)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing usable yet: the first walk is running.
    Building,
    /// Searchable (a saved index may be refreshing behind it).
    Ready,
    /// The root couldn't be indexed.
    Failed,
}

struct Slot {
    root: PathBuf,
    file: PathBuf,
    index: RwLock<Option<Index>>,
    state: AtomicU8,
    dirty: Mutex<HashSet<PathBuf>>,
    changed_since_save: AtomicBool,
}

struct Service {
    slots: Vec<Arc<Slot>>,
    cancel: Arc<AtomicBool>,
    _watchers: Vec<notify::RecommendedWatcher>,
}

static SERVICE: OnceLock<Mutex<Option<Service>>> = OnceLock::new();

fn service() -> &'static Mutex<Option<Service>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

fn file_for(dir: &Path, root: &Path) -> PathBuf {
    let tag: String = root.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    dir.join(format!("{}.leafidx", tag.trim_matches('_')))
}

/// Indexes `roots`, saving the indexes in `dir`. Replaces a running set.
pub fn start(roots: Vec<PathBuf>, dir: PathBuf) {
    stop();
    let _ = std::fs::create_dir_all(&dir);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut slots = Vec::new();
    let mut watchers = Vec::new();
    for root in roots {
        let slot = Arc::new(Slot {
            file: file_for(&dir, &root),
            root,
            index: RwLock::new(None),
            state: AtomicU8::new(State::Building as u8),
            dirty: Mutex::new(HashSet::new()),
            changed_since_save: AtomicBool::new(false),
        });
        // changed folders, collected from the start (a walk running meanwhile misses nothing: the fresh index is refreshed with them)
        let s2 = slot.clone();
        let ignore = dir.clone();
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            let mut d = s2.dirty.lock();
            for p in ev.paths {
                if p.starts_with(&ignore) {
                    continue;
                }
                if let Some(parent) = p.parent() {
                    d.insert(parent.to_path_buf());
                }
            }
        }) {
            Ok(mut w) => match w.watch(&slot.root, RecursiveMode::Recursive) {
                Ok(()) => watchers.push(w),
                Err(e) => log::info!("indexer: no watch on {}: {e}", slot.root.display()),
            },
            Err(e) => log::info!("indexer: watcher: {e}"),
        }
        slots.push(slot);
    }
    for slot in &slots {
        let (slot, cancel) = (slot.clone(), cancel.clone());
        let _ = std::thread::Builder::new().name("leaf-indexer".into()).spawn(move || run(slot, cancel));
    }
    *service().lock() = Some(Service { slots, cancel, _watchers: watchers });
}

/// Stops indexing (the indexes are dropped; saved files stay).
pub fn stop() {
    if let Some(s) = service().lock().take() {
        s.cancel.store(true, Ordering::Relaxed);
        for slot in &s.slots {
            save(slot);
        }
    }
}

fn save(slot: &Slot) {
    if !slot.changed_since_save.swap(false, Ordering::Relaxed) {
        return;
    }
    if let Some(ix) = slot.index.read().as_ref()
        && let Err(e) = ix.save(&slot.file)
    {
        log::info!("indexer: saving {}: {e}", slot.file.display());
    }
}

fn run(slot: Arc<Slot>, cancel: Arc<AtomicBool>) {
    if let Some(ix) = Index::load(&slot.file, &slot.root) {
        *slot.index.write() = Some(ix);
        slot.state.store(State::Ready as u8, Ordering::Relaxed);
    }
    // the walk reads the disk at the same pace for a long time: stay out of the way
    match Index::build(&slot.root, &cancel) {
        Some(mut fresh) => {
            // what changed while walking
            let d: Vec<PathBuf> = slot.dirty.lock().drain().collect();
            for dir in d {
                fresh.refresh_dir(&dir, &cancel);
            }
            *slot.index.write() = Some(fresh);
            slot.state.store(State::Ready as u8, Ordering::Relaxed);
            slot.changed_since_save.store(true, Ordering::Relaxed);
            save(&slot);
        }
        None => {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            if slot.index.read().is_none() {
                slot.state.store(State::Failed as u8, Ordering::Relaxed);
                return;
            }
        }
    }
    let mut last_save = Instant::now();
    while !cancel.load(Ordering::Relaxed) {
        for _ in 0..5 {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let d: Vec<PathBuf> = slot.dirty.lock().drain().collect();
        if !d.is_empty() {
            if let Some(ix) = slot.index.write().as_mut() {
                for dir in d {
                    ix.refresh_dir(&dir, &cancel);
                }
            }
            slot.changed_since_save.store(true, Ordering::Relaxed);
        }
        if last_save.elapsed() > Duration::from_secs(600) {
            save(&slot);
            last_save = Instant::now();
        }
    }
}

/// How long a search waits for an index that is being changed.
const SEARCH_WAIT: Duration = Duration::from_secs(2);

fn slot_for(under: &Path) -> Option<Arc<Slot>> {
    let g = service().lock();
    let s = g.as_ref()?;
    let slot = s.slots.iter().filter(|s| under.starts_with(&s.root)).max_by_key(|s| s.root.as_os_str().len())?.clone();
    (slot.state.load(Ordering::Relaxed) == State::Ready as u8).then_some(slot)
}

/// Searches the index whose root holds `under`: `None` when no ready index covers it (search the disk then).
pub fn search(pattern: &str, under: &Path, include_hidden: bool, max: usize) -> Option<Vec<PathBuf>> {
    let slot = slot_for(under)?;
    let g = slot.index.try_read_for(SEARCH_WAIT)?;
    let ix = g.as_ref()?;
    if !ix.covers(under) {
        return None;
    }
    let m = Matcher::new(pattern);
    let mut found = Vec::new();
    ix.matches(Some(under), include_hidden, &mut |n| m.is_match(n), &mut |p, _| {
        found.push(p);
        found.len() < max
    });
    Some(found)
}

/// [`search`] with the best matches first: `rank` scores a matching name (None: not wanted); shallower paths first among equals.
pub fn search_ranked(pattern: &str, under: &Path, include_hidden: bool, max: usize, rank: &(dyn Fn(&str) -> Option<i32> + Sync)) -> Option<Vec<(i32, PathBuf)>> {
    let slot = slot_for(under)?;
    let g = slot.index.try_read_for(SEARCH_WAIT)?;
    let ix = g.as_ref()?;
    if !ix.covers(under) {
        return None;
    }
    let m = Matcher::new(pattern);
    let mut scored: Vec<(i32, PathBuf)> = Vec::new();
    let score = std::cell::Cell::new(0);
    ix.matches(
        Some(under),
        include_hidden,
        &mut |n| {
            if !m.is_match(n) {
                return false;
            }
            match rank(n) {
                Some(s) => {
                    score.set(s);
                    true
                }
                None => false,
            }
        },
        &mut |p, _| {
            scored.push((score.get(), p));
            true
        },
    );
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.components().count().cmp(&b.1.components().count())));
    scored.truncate(max);
    Some(scored)
}

/// (root, state, live entries, bytes in memory) per root.
pub fn status() -> Vec<(PathBuf, State, usize, usize)> {
    let g = service().lock();
    let Some(s) = g.as_ref() else { return Vec::new() };
    s.slots
        .iter()
        .map(|slot| {
            let state = match slot.state.load(Ordering::Relaxed) {
                x if x == State::Ready as u8 => State::Ready,
                x if x == State::Failed as u8 => State::Failed,
                _ => State::Building,
            };
            let (live, mem) = slot.index.try_read().and_then(|i| i.as_ref().map(|i| (i.live(), i.memory()))).unwrap_or((0, 0));
            (slot.root.clone(), state, live, mem)
        })
        .collect()
}

/// What is worth indexing: the home folder and the other fixed local disks (not the system folders, not network or removable
/// mounts: walking them would wake disks or hit the network).
pub fn local_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    let home = dirs::home_dir();
    if let Some(h) = &home {
        v.push(h.clone());
    }
    for d in crate::drives::list_drives() {
        if d.kind != crate::drives::DriveKind::Fixed || d.root == Path::new("/") {
            continue;
        }
        if ["/usr", "/var", "/opt", "/srv", "/tmp", "/etc"].iter().any(|s| d.root.starts_with(s)) {
            continue;
        }
        if v.iter().any(|r| d.root.starts_with(r) || r.starts_with(&d.root)) {
            continue;
        }
        v.push(d.root);
    }
    v
}
