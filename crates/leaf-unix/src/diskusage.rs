//! Linux backend of rf_win::diskusage. Status: REAL
//!
//! One background thread walks the folder with `std::fs` and keeps it as the same compact struct-of-arrays tree rf-win has: every
//! directory's entries get a contiguous run of ids reserved when the directory is listed, so a parent's id is always smaller than
//! its children's and one backwards pass adds every entry to its parent. Symlinks are neither counted nor followed, a sub-folder on
//! another device is left out, unreadable folders are counted in `skipped`. A hard link is counted once, by (device, inode): the
//! later names of the same file stay in the tree with size 0. The size on disk is `st_blocks * 512`.
//! (rf-win walks with a rayon pool; this walk is one thread, which a spinning disk prefers anyway.)

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// TYPE-COPY(rf-win diskusage.rs:32)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Logical,
    OnDisk,
}

// TYPE-COPY(rf-win diskusage.rs:42)
#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub bytes: AtomicU64,
}

const DIR: u8 = 1;
/// No parent: the root.
const NONE: u32 = u32::MAX;

/// The finished tree. Id 0 is the folder that was walked.
// TYPE-COPY(rf-win diskusage.rs:48): the cluster size dropped, the size on disk is kept per entry
pub struct DiskTree {
    root: PathBuf,
    parent: Vec<u32>,
    name_off: Vec<u32>,
    name_len: Vec<u16>,
    size: Vec<u64>,
    alloc: Vec<u64>,
    files: Vec<u32>,
    first: Vec<u32>,
    count: Vec<u32>,
    flags: Vec<u8>,
    names: Vec<u8>,
    skipped: u32,
    cancelled: bool,
}

/// One listed directory, waiting to be put into the arrays.
struct Chunk {
    base: u32,
    parent: u32,
    names: Vec<u8>,
    lens: Vec<u16>,
    sizes: Vec<u64>,
    allocs: Vec<u64>,
    flags: Vec<u8>,
}

impl DiskTree {
    /// Walks `root` (blocking; `cancel` stops it early, and the tree then holds what was found so far).
    pub fn build(root: &Path, cancel: &AtomicBool, progress: &Progress) -> DiskTree {
        let mut skipped = 0u32;
        let mut next = 1u32;
        let mut chunks: Vec<Chunk> = Vec::new();
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        match std::fs::symlink_metadata(root) {
            Ok(m) if m.is_dir() => {
                let dev = m.dev();
                let mut todo: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
                while let Some((dir, id)) = todo.pop() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let rd = match std::fs::read_dir(&dir) {
                        Ok(rd) => rd,
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    };
                    let (mut names, mut lens, mut sizes, mut allocs, mut flags, mut subs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                    let (mut bytes, mut entries) = (0u64, 0u64);
                    for e in rd.flatten() {
                        let Ok(m) = e.metadata() else { continue };
                        let ft = m.file_type();
                        if ft.is_symlink() {
                            continue;
                        }
                        let is_dir = ft.is_dir();
                        if is_dir && m.dev() != dev {
                            continue;
                        }
                        let name = e.file_name().to_string_lossy().into_owned();
                        let n = floor_boundary(&name, u16::MAX as usize);
                        let (size, alloc) = if is_dir || (m.nlink() > 1 && !seen.insert((m.dev(), m.ino()))) { (0, 0) } else { (m.len(), m.blocks() * 512) };
                        if is_dir {
                            subs.push((flags.len() as u32, e.path()));
                        }
                        lens.push(n as u16);
                        names.extend_from_slice(&name.as_bytes()[..n]);
                        sizes.push(size);
                        allocs.push(alloc);
                        flags.push(if is_dir { DIR } else { 0 });
                        bytes += size;
                        entries += 1;
                    }
                    if lens.is_empty() {
                        continue;
                    }
                    progress.entries.fetch_add(entries, Ordering::Relaxed);
                    progress.bytes.fetch_add(bytes, Ordering::Relaxed);
                    let base = next;
                    next = next.saturating_add(lens.len() as u32);
                    for (k, p) in subs {
                        todo.push((p, base + k));
                    }
                    chunks.push(Chunk { base, parent: id, names, lens, sizes, allocs, flags });
                }
            }
            _ => skipped += 1,
        }
        let total = next as usize;
        chunks.sort_unstable_by_key(|c| c.base);
        let mut t = DiskTree {
            root: root.to_path_buf(),
            parent: vec![NONE; total],
            name_off: vec![0; total],
            name_len: vec![0; total],
            size: vec![0; total],
            alloc: vec![0; total],
            files: vec![0; total],
            first: vec![0; total],
            count: vec![0; total],
            flags: vec![0; total],
            names: Vec::with_capacity(chunks.iter().map(|c| c.names.len()).sum()),
            skipped,
            cancelled: cancel.load(Ordering::Relaxed),
        };
        t.flags[0] = DIR;
        for c in chunks {
            let p = c.parent as usize;
            t.count[p] = c.lens.len() as u32;
            t.first[p] = c.base;
            let mut off = 0usize;
            for (k, (&len, &f)) in c.lens.iter().zip(&c.flags).enumerate() {
                let i = c.base as usize + k;
                t.parent[i] = c.parent;
                t.name_off[i] = t.names.len() as u32;
                t.name_len[i] = len;
                t.flags[i] = f;
                t.names.extend_from_slice(&c.names[off..off + len as usize]);
                off += len as usize;
                if f & DIR == 0 {
                    t.size[i] = c.sizes[k];
                    t.alloc[i] = c.allocs[k];
                    t.files[i] = 1;
                }
            }
        }
        // a child's id is always greater than its parent's: one pass backwards totals everything
        for i in (1..total).rev() {
            let p = t.parent[i];
            if p != NONE {
                let p = p as usize;
                t.size[p] += t.size[i];
                t.alloc[p] += t.alloc[i];
                t.files[p] = t.files[p].saturating_add(t.files[i]);
            }
        }
        t
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Entries, the root included.
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.len() <= 1
    }

    /// The walk was stopped before it finished.
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    /// Folders that could not be read.
    pub fn skipped(&self) -> u32 {
        self.skipped
    }

    /// Bytes held (arrays and names).
    pub fn memory(&self) -> usize {
        self.parent.capacity() * (4 + 4 + 2 + 8 + 8 + 4 + 4 + 4 + 1) + self.names.capacity()
    }

    pub fn name(&self, i: u32) -> &str {
        let (o, l) = (self.name_off[i as usize] as usize, self.name_len[i as usize] as usize);
        std::str::from_utf8(&self.names[o..o + l]).unwrap_or("")
    }

    pub fn is_dir(&self, i: u32) -> bool {
        self.flags[i as usize] & DIR != 0
    }

    pub fn parent(&self, i: u32) -> Option<u32> {
        Some(self.parent[i as usize]).filter(|&p| p != NONE)
    }

    pub fn size(&self, i: u32) -> u64 {
        self.size[i as usize]
    }

    /// Size on disk (`st_blocks * 512`; a folder's is the sum of what is below it).
    pub fn allocated(&self, i: u32) -> u64 {
        self.alloc[i as usize]
    }

    pub fn weight(&self, i: u32, m: Metric) -> u64 {
        match m {
            Metric::Logical => self.size(i),
            Metric::OnDisk => self.allocated(i),
        }
    }

    /// Files below `i` (1 for a file).
    pub fn files(&self, i: u32) -> u32 {
        self.files[i as usize]
    }

    /// The ids of `i`'s direct children (empty for files and empty folders).
    pub fn children(&self, i: u32) -> std::ops::Range<u32> {
        let f = self.first[i as usize];
        f..f + self.count[i as usize]
    }

    /// `i`'s children, biggest first (ties by name).
    pub fn sorted_children(&self, i: u32, m: Metric) -> Vec<u32> {
        let mut v: Vec<u32> = self.children(i).collect();
        v.sort_unstable_by(|&a, &b| self.weight(b, m).cmp(&self.weight(a, m)).then_with(|| self.name(a).cmp(self.name(b))));
        v
    }

    /// Full path of `i`.
    pub fn path(&self, i: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = i;
        while let Some(p) = self.parent(cur) {
            parts.push(cur);
            cur = p;
        }
        let mut path = self.root.clone();
        for &c in parts.iter().rev() {
            path.push(self.name(c));
        }
        path
    }

    /// Id of the entry at `path` if it lies in the tree (names are exact: case counts on Linux).
    pub fn find(&self, path: &Path) -> Option<u32> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut cur = 0u32;
        for comp in rel.components() {
            let want = comp.as_os_str().to_string_lossy();
            cur = self.children(cur).find(|&c| self.name(c) == want)?;
        }
        Some(cur)
    }
}

/// The largest cut of `s` at or below `max` bytes that falls between characters.
fn floor_boundary(s: &str, max: usize) -> usize {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// A walk running in the background. Dropping it (closing the tab) stops the walk.
// TYPE-COPY(rf-win diskusage.rs:345)
pub struct DiskScan {
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    result: Arc<Mutex<Option<DiskTree>>>,
}

impl DiskScan {
    pub fn start(root: PathBuf) -> DiskScan {
        let scan = DiskScan { cancel: Arc::default(), progress: Arc::default(), result: Arc::default() };
        let (cancel, progress, result) = (scan.cancel.clone(), scan.progress.clone(), scan.result.clone());
        let spawned = std::thread::Builder::new().name("leaf-diskusage".into()).spawn({
            let root = root.clone();
            move || {
                let t = DiskTree::build(&root, &cancel, &progress);
                *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
            }
        });
        if spawned.is_err() {
            // no thread: hand out an empty, unfinished tree so whoever polls stops waiting
            let mut t = DiskTree::build(Path::new("/nonexistent-leaf-diskusage"), &AtomicBool::new(true), &Progress::default());
            t.root = root;
            *scan.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
        }
        scan
    }

    /// (entries, bytes) found so far.
    pub fn progress(&self) -> (u64, u64) {
        (self.progress.entries.load(Ordering::Relaxed), self.progress.bytes.load(Ordering::Relaxed))
    }

    /// The tree once the walk has finished (taken: later calls return `None`).
    pub fn take(&self) -> Option<DiskTree> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for DiskScan {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn totals_are_subtree_sums_and_hard_links_count_once() {
        let root = std::env::temp_dir().join(format!("leaf-diskusage-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/deep")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("top.txt"), vec![1u8; 100]).unwrap();
        fs::write(root.join("a/one.bin"), vec![1u8; 1000]).unwrap();
        fs::write(root.join("a/deep/two.mp4"), vec![1u8; 5000]).unwrap();
        fs::hard_link(root.join("a/deep/two.mp4"), root.join("a/two-again.mp4")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("a/loop")).unwrap();
        let progress = Progress::default();
        let t = DiskTree::build(&root, &AtomicBool::new(false), &progress);
        assert_eq!(t.size(0), 6100);
        assert_eq!(t.files(0), 4);
        let a = t.find(&root.join("a")).unwrap();
        assert_eq!(t.size(a), 6000);
        let deep = t.find(&root.join("a/deep")).unwrap();
        assert_eq!(t.parent(deep), Some(a));
        assert_eq!(t.path(deep), root.join("a/deep"));
        let empty = t.find(&root.join("empty")).unwrap();
        assert!(t.is_dir(empty) && t.children(empty).is_empty());
        for i in 0..t.len() as u32 {
            if t.is_dir(i) {
                assert_eq!(t.children(i).map(|c| t.size(c)).sum::<u64>(), t.size(i));
            }
        }
        assert_eq!(t.name(t.sorted_children(0, Metric::Logical)[0]), "a");
        let cancelled = DiskTree::build(&root, &AtomicBool::new(true), &Progress::default());
        assert!(cancelled.cancelled() && cancelled.len() <= 1);
        let scan = DiskScan::start(root.clone());
        let mut tree = None;
        for _ in 0..200 {
            tree = scan.take();
            if tree.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(tree.expect("scan finished").size(0), 6100);
        let _ = fs::remove_dir_all(&root);
    }
}
