//! Duplicate finder, the walking and hashing part (the grouping rules are `rf_core::dupes`). Blocking: call off the UI thread.
//!
//! Candidates are grouped by size, then by a SHA-256 of the first and last 64 KiB, then by the SHA-256 of the whole file (files up to
//! 128 KiB are read whole at the second step, so they skip the third). Empty files, links of any kind (symbolic links, junctions,
//! cloud placeholders: every reparse point) and, unless asked for, hidden and system files are left out; hard links to one file count
//! as one. Folders on other drives are not entered (a mount point is a reparse point; on Linux the device number is compared), only
//! the roots that were chosen are walked. The work runs on a small pool of its own so the interface stays smooth.

use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rayon::prelude::*;
use rf_core::dupes::{Cand, DupGroup, group_by_size, sort_groups, split_by};
use sha2::{Digest, Sha256};

const EDGE: u64 = 64 * 1024;
const CHUNK: usize = 1 << 20;

pub struct Options {
    /// Hidden and system files and folders take part.
    pub include_hidden: bool,
    /// Worker threads for hashing (at least 1).
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { include_hidden: false, threads: 2 }
    }
}

/// Stages shown in the progress line.
pub const WALKING: u8 = 0;
pub const SAME_SIZE: u8 = 1;
pub const EDGES: u8 = 2;
pub const WHOLE: u8 = 3;

/// Shared counters the interface reads while the search runs.
#[derive(Default)]
pub struct Progress {
    /// Files seen by the walk.
    pub files: AtomicU64,
    /// Bytes hashed so far.
    pub bytes: AtomicU64,
    pub stage: AtomicU8,
}

fn hidden(m: &Metadata, name: &std::ffi::OsStr) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let _ = name;
        m.file_attributes() & (rf_core::attrs::HIDDEN | rf_core::attrs::SYSTEM) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = m;
        name.as_encoded_bytes().first() == Some(&b'.')
    }
}

fn is_link(m: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & rf_core::attrs::REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        m.file_type().is_symlink()
    }
}

fn modified_ms(m: &Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(unix)]
fn device(m: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(m)
}
#[cfg(not(unix))]
fn device(_: &Metadata) -> u64 {
    0
}

/// (volume, file index) of the file: equal for hard links of one file.
pub fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some((m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct ByHandle {
            attrs: u32,
            times: [u32; 6],
            volume: u32,
            size_hi: u32,
            size_lo: u32,
            links: u32,
            index_hi: u32,
            index_lo: u32,
        }
        unsafe extern "system" {
            fn GetFileInformationByHandle(h: *mut std::ffi::c_void, info: *mut ByHandle) -> i32;
        }
        let f = File::open(path).ok()?;
        let mut info = ByHandle::default();
        // SAFETY: the handle is open for the call and `info` is the struct the API fills.
        let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
        (ok != 0).then(|| (info.volume as u64, ((info.index_hi as u64) << 32) | info.index_lo as u64))
    }
}

/// Walks `roots` for regular files. `None` when cancelled.
fn walk(roots: &[PathBuf], opt: &Options, prog: &Progress, cancel: &AtomicBool) -> Option<Vec<Cand>> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let root_dev = std::fs::metadata(root).map(|m| device(&m)).unwrap_or(0);
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
                if is_link(&m) || (!opt.include_hidden && hidden(&m, &e.file_name())) {
                    continue;
                }
                if m.is_dir() {
                    if device(&m) == root_dev {
                        stack.push(e.path());
                    }
                } else if m.is_file() && m.len() > 0 {
                    let p = e.path();
                    if seen.insert(p.clone()) {
                        prog.files.fetch_add(1, Ordering::Relaxed);
                        out.push(Cand { path: p, size: m.len(), modified_ms: modified_ms(&m), id: None });
                    }
                }
            }
        }
    }
    Some(out)
}

/// SHA-256 of the first and last 64 KiB (the whole file when it is no longer than both).
fn edge_digest(path: &Path, size: u64, prog: &Progress) -> Option<[u8; 32]> {
    let mut f = File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; EDGE as usize];
    let first = size.min(EDGE) as usize;
    f.read_exact(&mut buf[..first]).ok()?;
    h.update(&buf[..first]);
    let mut read = first as u64;
    if size > EDGE {
        let tail = size.min(EDGE) as usize;
        f.seek(SeekFrom::Start(size - tail as u64)).ok()?;
        f.read_exact(&mut buf[..tail]).ok()?;
        h.update(&buf[..tail]);
        read += tail as u64;
    }
    prog.bytes.fetch_add(read, Ordering::Relaxed);
    Some(h.finalize().into())
}

/// SHA-256 of the whole file; `None` when it cannot be read or `cancel` fired.
pub fn full_digest(path: &Path, prog: Option<&Progress>, cancel: &AtomicBool) -> Option<[u8; 32]> {
    let mut f = File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        if let Some(p) = prog {
            p.bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    Some(h.finalize().into())
}

/// Finds the duplicates under `roots`, biggest waste first. `None` when cancelled.
pub fn find(roots: &[PathBuf], opt: &Options, prog: &Progress, cancel: &AtomicBool) -> Option<Vec<DupGroup>> {
    prog.stage.store(WALKING, Ordering::Relaxed);
    let cands = walk(roots, opt, prog, cancel)?;

    prog.stage.store(SAME_SIZE, Ordering::Relaxed);
    let groups = group_by_size(cands);
    // hard links are recognised only among files of equal size (the identity costs an open)
    let pool = rayon::ThreadPoolBuilder::new().num_threads(opt.threads.max(1)).thread_name(|i| format!("leaf-dupes-{i}")).build().ok()?;
    let groups = pool.install(|| {
        let flat: Vec<Cand> = groups.into_iter().flatten().collect::<Vec<_>>().into_par_iter().map(|mut c| {
            c.id = file_id(&c.path);
            c
        }).collect();
        group_by_size(flat)
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    prog.stage.store(EDGES, Ordering::Relaxed);
    let groups = pool.install(|| {
        let keyed: Vec<Vec<(Cand, Option<[u8; 32]>)>> = groups
            .into_par_iter()
            .map(|g| g.into_iter().map(|c| { let k = if cancel.load(Ordering::Relaxed) { None } else { edge_digest(&c.path, c.size, prog) }; (c, k) }).collect())
            .collect();
        let mut keys = std::collections::HashMap::new();
        let plain: Vec<Vec<Cand>> = keyed.into_iter().map(|g| g.into_iter().map(|(c, k)| { keys.insert(c.path.clone(), k); c }).collect()).collect();
        split_by(plain, |c| keys.get(&c.path).copied().flatten())
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    prog.stage.store(WHOLE, Ordering::Relaxed);
    let (done, todo): (Vec<_>, Vec<_>) = groups.into_iter().partition(|g| g[0].size <= 2 * EDGE);
    let mut finished: Vec<Vec<Cand>> = done;
    let whole = pool.install(|| {
        let mut keys = std::collections::HashMap::new();
        let hashed: Vec<(PathBuf, Option<[u8; 32]>)> =
            todo.iter().flatten().collect::<Vec<_>>().into_par_iter().map(|c| (c.path.clone(), full_digest(&c.path, Some(prog), cancel))).collect();
        keys.extend(hashed);
        split_by(todo, |c| keys.get(&c.path).copied().flatten())
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    finished.extend(whole);
    let mut out: Vec<DupGroup> = finished.into_iter().map(DupGroup::from).collect();
    sort_groups(&mut out);
    Some(out)
}
