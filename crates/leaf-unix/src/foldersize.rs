//! Linux backend of rf_win::foldersize. Status: REAL
//!
//! A few worker threads (one per requested folder, at most four at a time) walk each folder with `std::fs`. Symlinks are neither
//! counted nor followed, a sub-folder on another device (a mount point) is skipped, unreadable folders are skipped silently, a
//! hard link is counted once per link (as on Windows). `allocated` is `st_blocks * 512`. The batch can be cancelled; a cancelled
//! walk is not cached. The cache is the same idea as Windows': keyed by path, validated against the root folder's mtime, and
//! readable from / writable to a small text file (values read from it are provisional).

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use rf_core::pathkey::{SEP, path_key_trimmed};

// TYPE-COPY(rf-win foldersize.rs:37)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderSize {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub allocated: Option<u64>,
}

/// Folder sizes shared by the Size column, the preview pane and Properties.
pub fn shared_cache() -> &'static FolderSizeCache {
    static CACHE: OnceLock<FolderSizeCache> = OnceLock::new();
    CACHE.get_or_init(|| FolderSizeCache::new(PERSIST_LIMIT))
}

pub const PERSIST_LIMIT: usize = 20_000;
const SAVE_DELAY: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    files: AtomicU64,
    dirs: AtomicU64,
    allocated: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> FolderSize {
        FolderSize {
            bytes: self.bytes.load(Ordering::Relaxed),
            files: self.files.load(Ordering::Relaxed),
            dirs: self.dirs.load(Ordering::Relaxed),
            allocated: Some(self.allocated.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Default)]
struct Slot {
    c: Counters,
    done: AtomicBool,
    /// Value remembered from an earlier run, shown until the new measurement ends.
    seed: Option<FolderSize>,
}

// TYPE-COPY(rf-win foldersize.rs:321)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeState {
    Partial(u64),
    Provisional(FolderSize),
    Done(FolderSize),
}

/// Sizes of several folders, measured together. Each folder finishes on its own; results also land in [`shared_cache`].
// TYPE-COPY(rf-win foldersize.rs:333)
pub struct SizeBatch {
    slots: Vec<Slot>,
    stop: AtomicBool,
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// Walks `root`, adding to `c`. Stops soon when `stop()` says so (returns false then).
fn walk(root: &Path, c: &Counters, stop: &dyn Fn() -> bool) -> bool {
    let Ok(rm) = std::fs::symlink_metadata(root) else { return true };
    let dev = rm.dev();
    let mut todo = vec![root.to_path_buf()];
    // a file with more than one link counts once, by (device, inode)
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut n = 0u32;
    while let Some(dir) = todo.pop() {
        if stop() {
            return false;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let (mut bytes, mut files, mut dirs, mut alloc) = (0u64, 0u64, 0u64, 0u64);
        for e in rd.flatten() {
            n = n.wrapping_add(1);
            if n % 2048 == 0 && stop() {
                return false;
            }
            let Ok(m) = e.metadata() else { continue };
            let ft = m.file_type();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if m.dev() == dev {
                    dirs += 1;
                    todo.push(e.path());
                }
            } else {
                files += 1;
                if m.nlink() > 1 && !seen.insert((m.dev(), m.ino())) {
                    continue;
                }
                bytes += m.len();
                alloc += m.blocks() * 512;
            }
        }
        c.bytes.fetch_add(bytes, Ordering::Relaxed);
        c.files.fetch_add(files, Ordering::Relaxed);
        c.dirs.fetch_add(dirs, Ordering::Relaxed);
        c.allocated.fetch_add(alloc, Ordering::Relaxed);
    }
    true
}

impl SizeBatch {
    /// Starts at once and returns immediately (nothing touches the disk on the calling thread).
    pub fn start(dirs: Vec<PathBuf>) -> Arc<Self> {
        let b = Arc::new(SizeBatch { slots: dirs.iter().map(|d| Slot { seed: shared_cache().provisional(d), ..Default::default() }).collect(), stop: AtomicBool::new(false) });
        let dirs = Arc::new(dirs);
        let next = Arc::new(AtomicUsize::new(0));
        let workers = dirs.len().min(4);
        for k in 0..workers {
            let (b, dirs, next) = (b.clone(), dirs.clone(), next.clone());
            let spawned = std::thread::Builder::new().name(format!("leaf-dirsize-{k}")).spawn({
                let (b, dirs, next) = (b.clone(), dirs.clone(), next.clone());
                move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= dirs.len() {
                        break;
                    }
                    run_one(&b, i, &dirs[i]);
                }
            });
            if spawned.is_err() {
                // no thread: do this worker's share here is not wanted (UI thread); leave the slots to the other workers, and if
                // there are none, finish them as empty so nobody waits for ever
                if k == 0 {
                    for s in &b.slots {
                        s.done.store(true, Ordering::Release);
                    }
                }
            }
        }
        b
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn state(&self, i: usize) -> SizeState {
        let s = &self.slots[i];
        if s.done.load(Ordering::Acquire) {
            SizeState::Done(s.c.snapshot())
        } else if let Some(seed) = s.seed {
            SizeState::Provisional(seed)
        } else {
            SizeState::Partial(s.c.bytes.load(Ordering::Relaxed))
        }
    }

    /// Running totals (final once [`finished`](Self::finished)).
    pub fn totals(&self, i: usize) -> FolderSize {
        self.slots[i].c.snapshot()
    }

    pub fn finished(&self) -> bool {
        self.slots.iter().all(|s| s.done.load(Ordering::Acquire))
    }

    /// Stops all walks soon; results are then not cached.
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // folders not started yet finish at once (run_one sees the flag)
    }
}

fn run_one(b: &SizeBatch, i: usize, dir: &Path) {
    let slot = &b.slots[i];
    if b.stop.load(Ordering::Relaxed) {
        slot.done.store(true, Ordering::Release);
        return;
    }
    if let Some(c) = shared_cache().get(dir) {
        slot.c.bytes.store(c.bytes, Ordering::Relaxed);
        slot.c.files.store(c.files, Ordering::Relaxed);
        slot.c.dirs.store(c.dirs, Ordering::Relaxed);
        slot.c.allocated.store(c.allocated.unwrap_or(0), Ordering::Relaxed);
        slot.done.store(true, Ordering::Release);
        return;
    }
    // mtime before walking: a change during the walk then invalidates the entry
    let before = dir_mtime(dir);
    let finished = walk(dir, &slot.c, &|| b.stop.load(Ordering::Relaxed));
    if finished && !b.stop.load(Ordering::Relaxed) {
        shared_cache().insert_at(dir, slot.c.snapshot(), before);
    }
    slot.done.store(true, Ordering::Release);
}

struct CacheEntry {
    mtime: Option<SystemTime>,
    size: FolderSize,
    seq: u64,
    provisional: bool,
}

/// The cache of measured folders, keyed by path and validated against the root folder's mtime.
// TYPE-COPY(rf-win foldersize.rs:439)
pub struct FolderSizeCache {
    map: Mutex<HashMap<String, CacheEntry>>,
    seq: AtomicU64,
    capacity: usize,
    dirty: AtomicBool,
}

fn key(dir: &Path) -> String {
    path_key_trimmed(dir)
}

impl FolderSizeCache {
    pub fn new(capacity: usize) -> Self {
        FolderSizeCache { map: Mutex::new(HashMap::new()), seq: AtomicU64::new(1), capacity: capacity.max(16), dirty: AtomicBool::new(false) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CacheEntry>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Cached size if it was measured in this run and the folder's mtime is unchanged.
    pub fn get(&self, dir: &Path) -> Option<FolderSize> {
        let mtime = dir_mtime(dir)?;
        let map = self.lock();
        let e = map.get(&key(dir))?;
        (!e.provisional && e.mtime == Some(mtime)).then_some(e.size)
    }

    /// The value remembered from an earlier run, if that is all there is.
    pub fn provisional(&self, dir: &Path) -> Option<FolderSize> {
        self.lock().get(&key(dir)).filter(|e| e.provisional).map(|e| e.size)
    }

    pub fn insert(&self, dir: &Path, size: FolderSize) {
        self.insert_at(dir, size, dir_mtime(dir));
    }

    fn insert_at(&self, dir: &Path, size: FolderSize, mtime: Option<SystemTime>) {
        {
            let mut map = self.lock();
            if map.len() >= self.capacity {
                let mut seqs: Vec<u64> = map.values().map(|e| e.seq).collect();
                seqs.sort_unstable();
                let cut = seqs[seqs.len() / 2];
                map.retain(|_, e| e.seq > cut);
            }
            map.insert(key(dir), CacheEntry { mtime, size, seq: self.seq.fetch_add(1, Ordering::Relaxed), provisional: false });
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Forgets `dir` and every cached folder below it.
    pub fn invalidate(&self, dir: &Path) {
        let k = key(dir);
        let prefix = if k.ends_with(SEP) { k.clone() } else { format!("{k}{SEP}") };
        self.lock().retain(|p, _| *p != k && !p.starts_with(&prefix));
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.lock().clear();
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Returns the cached value, or measures (blocking: call off the UI thread) and stores it. A cancelled walk is an error and is
    /// not cached. `progress` gets the running totals about every 100 ms.
    pub fn get_or_compute(&self, dir: &Path, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(&FolderSize)) -> anyhow::Result<FolderSize> {
        if let Some(s) = self.get(dir) {
            progress(&s);
            return Ok(s);
        }
        anyhow::ensure!(dir.is_dir(), "Folder not found: {}", dir.display());
        let before = dir_mtime(dir);
        // measured afresh in a worker, so the progress callback runs on this thread
        let c = Arc::new(Counters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        {
            let (c, stop, finished, dir) = (c.clone(), stop.clone(), finished.clone(), dir.to_path_buf());
            std::thread::Builder::new().name("leaf-dirsize-one".into()).spawn(move || {
                walk(&dir, &c, &|| stop.load(Ordering::Relaxed));
                finished.store(true, Ordering::Release);
            })?;
        }
        let mut since = std::time::Instant::now();
        while !finished.load(Ordering::Acquire) {
            if cancelled() {
                stop.store(true, Ordering::Relaxed);
                anyhow::bail!("cancelled");
            }
            std::thread::sleep(Duration::from_millis(5));
            if since.elapsed() >= Duration::from_millis(100) {
                since = std::time::Instant::now();
                progress(&c.snapshot());
            }
        }
        let total = c.snapshot();
        progress(&total);
        self.insert_at(dir, total, before);
        Ok(total)
    }

    /// Writes the `limit` most recently used folders to `path` (one tab-separated line each, oldest first), into a temporary file
    /// next to it that is then renamed over it. This is leaf's own cache file.
    pub fn save_to(&self, path: &Path, limit: usize) -> std::io::Result<()> {
        let mut rows: Vec<(u64, String, FolderSize)> = self.lock().iter().map(|(k, e)| (e.seq, k.clone(), e.size)).collect();
        rows.sort_unstable_by_key(|r| r.0);
        let skip = rows.len().saturating_sub(limit);
        let mut out = String::from("LEAFSIZE 1\n");
        for (_, k, s) in &rows[skip..] {
            if k.contains(['\n', '\t']) {
                continue;
            }
            out.push_str(&format!("{}\t{}\t{}\t{}\t{k}\n", s.bytes, s.files, s.dirs, s.allocated.map_or(-1i128, |a| a as i128)));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Reads a file written by [`save_to`](Self::save_to); the values enter as provisional. A missing or foreign file is ignored.
    /// Returns how many folders were taken.
    pub fn load_from(&self, path: &Path) -> usize {
        let Ok(text) = std::fs::read_to_string(path) else { return 0 };
        let mut lines = text.lines();
        if lines.next() != Some("LEAFSIZE 1") {
            return 0;
        }
        let mut rows = Vec::new();
        for l in lines {
            let mut f = l.splitn(5, '\t');
            let (Some(b), Some(fi), Some(d), Some(a), Some(k)) = (f.next(), f.next(), f.next(), f.next(), f.next()) else { continue };
            let (Ok(bytes), Ok(files), Ok(dirs), Ok(al)) = (b.parse::<u64>(), fi.parse::<u64>(), d.parse::<u64>(), a.parse::<i128>()) else { continue };
            rows.push((k.to_string(), FolderSize { bytes, files, dirs, allocated: (al >= 0).then_some(al as u64) }));
        }
        let mut map = self.lock();
        let skip = rows.len().saturating_sub(self.capacity.saturating_sub(map.len()));
        let mut taken = 0;
        for (k, size) in rows.into_iter().skip(skip) {
            if let std::collections::hash_map::Entry::Vacant(v) = map.entry(k) {
                v.insert(CacheEntry { mtime: None, size, seq: 0, provisional: true });
                taken += 1;
            }
        }
        taken
    }

    /// Keeps this cache in the file `path`: reads it on a background thread, then writes it a few seconds after changes.
    pub fn persist(&'static self, path: PathBuf) {
        let _ = std::thread::Builder::new().name("leaf-dirsize-save".into()).spawn(move || {
            let n = self.load_from(&path);
            log::debug!("foldersize: {n} remembered sizes from {}", path.display());
            loop {
                std::thread::sleep(SAVE_DELAY);
                if self.dirty.swap(false, Ordering::Relaxed)
                    && let Err(e) = self.save_to(&path, PERSIST_LIMIT)
                {
                    log::debug!("foldersize: could not write {}: {e}", path.display());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_a_tree_and_skips_links() {
        let root = std::env::temp_dir().join(format!("leaf-fsize-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("x.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(root.join("a/b/y.bin"), vec![0u8; 500]).unwrap();
        std::os::unix::fs::symlink(&root, root.join("a/loop")).unwrap();
        let cache = FolderSizeCache::new(16);
        let s = cache.get_or_compute(&root, &|| false, &mut |_| {}).unwrap();
        assert_eq!((s.bytes, s.files, s.dirs), (1500, 2, 2));
        assert!(s.allocated.unwrap() >= 1500 || s.allocated.unwrap() == 0);
        assert_eq!(cache.get(&root), Some(s));
        let _ = std::fs::remove_dir_all(&root);
    }
}
