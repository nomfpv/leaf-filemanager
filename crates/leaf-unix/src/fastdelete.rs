//! Linux backend of rf_win::fastdelete. Status: REAL (permanent delete, one thread).
//!
//! Data safety: the walk goes through folder descriptors (`openat` with `O_NOFOLLOW | O_DIRECTORY`, `fstatat` with
//! `AT_SYMLINK_NOFOLLOW`, `unlinkat`), so a link is removed as a link and its target is never entered, even when a folder is swapped
//! for a link while the walk runs. A folder on another device (a mount point inside the tree, or a chosen item that is itself a mount
//! point) is never entered nor removed: it is reported. Nothing is retried or handed to anyone else; what fails stays and is named in
//! the error.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::fileops::{Tracker, cpath, last_err};

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
pub(crate) fn lstat_at(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated, `st` is a valid out-pointer of the right type.
    let r = unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if r == 0 { Ok(st) } else { Err(last_err()) }
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid out-pointer of the right type.
    if unsafe { libc::fstat(fd, &mut st) } == 0 { Ok(st) } else { Err(last_err()) }
}

pub(crate) fn is_dir(st: &libc::stat) -> bool {
    st.st_mode & libc::S_IFMT == libc::S_IFDIR
}

/// Opens the folder `name` under `dir` without following a link (`ELOOP`/`ENOTDIR` when it is not a real folder).
pub(crate) fn open_dir_at(dir: RawFd, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated; a non-negative answer is a new descriptor that we own.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 { Err(last_err()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
}

/// Opens a folder by path (following links on the way: this is the folder the user chose, not something inside a tree).
pub(crate) fn open_dir(p: &Path) -> io::Result<OwnedFd> {
    let c = cpath(p)?;
    // SAFETY: as in `open_dir_at`.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 { Err(last_err()) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
}

/// The names in the open folder `dir` (without `.` and `..`).
pub(crate) fn names(dir: RawFd) -> io::Result<Vec<CString>> {
    // fdopendir takes the descriptor over: give it its own copy
    // SAFETY: dup of a descriptor we hold; the copy is handed to fdopendir (or closed on failure).
    let d = unsafe { libc::fcntl(dir, libc::F_DUPFD_CLOEXEC, 0) };
    if d < 0 {
        return Err(last_err());
    }
    let ds = unsafe { libc::fdopendir(d) };
    if ds.is_null() {
        let e = last_err();
        unsafe { libc::close(d) };
        return Err(e);
    }
    // (the copy shares the read position with `dir`: start from the beginning)
    unsafe { libc::rewinddir(ds) };
    let mut out = Vec::new();
    let res = loop {
        // SAFETY: errno is thread-local; reset so that the end of the folder can be told from an error.
        unsafe { *libc::__errno_location() = 0 };
        let e = unsafe { libc::readdir(ds) };
        if e.is_null() {
            let err = io::Error::last_os_error();
            break if err.raw_os_error().unwrap_or(0) == 0 { Ok(()) } else { Err(err) };
        }
        // SAFETY: d_name of an entry readdir returned is a NUL-terminated string valid until the next readdir call.
        let name = unsafe { CStr::from_ptr((*e).d_name.as_ptr()) };
        let b = name.to_bytes();
        if b != b"." && b != b".." {
            out.push(name.to_owned());
        }
    };
    unsafe { libc::closedir(ds) };
    res.map(|_| out)
}

fn unlink_at(dir: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: `name` is NUL-terminated.
    if unsafe { libc::unlinkat(dir, name.as_ptr(), flags) } == 0 { Ok(()) } else { Err(last_err()) }
}

/// Removing one tree: the device it must stay on, what it has done, what failed.
struct Walk<'a> {
    dev: libc::dev_t,
    tracker: &'a Tracker,
    total: u64,
    done: u64,
    failed: Vec<String>,
    failed_count: usize,
    cancelled: bool,
}

impl Walk<'_> {
    fn fail(&mut self, p: &Path, e: impl std::fmt::Display) {
        self.failed_count += 1;
        if self.failed.len() < 8 {
            self.failed.push(format!("{}: {e}", p.display()));
        }
        log::info!("delete: {} kept ({e})", p.display());
    }

    /// Waits while paused; `true` when the operation was cancelled.
    fn stop(&mut self) -> bool {
        while self.tracker.paused.load(Ordering::Relaxed) && !self.tracker.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if self.tracker.cancel.load(Ordering::Relaxed) {
            self.cancelled = true;
        }
        self.cancelled
    }

    fn tick(&mut self, p: &Path) {
        self.done += 1;
        if self.done % 64 == 0 || self.done == self.total {
            let (t, d) = units(self.total, self.done);
            (self.tracker.on_progress)(t, d, p.file_name().map(|n| n.to_string_lossy().into_owned()));
        }
    }

    /// Removes the entry `name` of the open folder `dir` (`path` is only for messages). `true` when it is gone.
    fn remove(&mut self, dir: RawFd, name: &CStr, path: &Path) -> bool {
        if self.stop() {
            return false;
        }
        let st = match lstat_at(dir, name) {
            Ok(st) => st,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return true,
            Err(e) => {
                self.fail(path, e);
                return false;
            }
        };
        if !is_dir(&st) {
            // a file, a link (the link itself), a socket, a device node...
            return match unlink_at(dir, name, 0) {
                Ok(()) => {
                    self.tick(path);
                    true
                }
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => true,
                Err(e) => {
                    self.fail(path, e);
                    false
                }
            };
        }
        if st.st_dev != self.dev {
            self.fail(path, "on another file system (a mount point): not entered");
            return false;
        }
        let sub = match open_dir_at(dir, name) {
            Ok(fd) => fd,
            Err(e) => {
                self.fail(path, e);
                return false;
            }
        };
        // (checked again on the open folder: it may have been swapped since the fstatat)
        match fstat(sub.as_raw_fd()) {
            Ok(s) if s.st_dev == self.dev && s.st_ino == st.st_ino => {}
            Ok(_) => {
                self.fail(path, "changed while it was being deleted: not entered");
                return false;
            }
            Err(e) => {
                self.fail(path, e);
                return false;
            }
        }
        let mut all = true;
        match names(sub.as_raw_fd()) {
            Ok(list) => {
                for n in list {
                    let child = path.join(std::ffi::OsStr::from_bytes(n.to_bytes()));
                    all &= self.remove(sub.as_raw_fd(), &n, &child);
                    if self.cancelled {
                        return false;
                    }
                }
            }
            Err(e) => {
                self.fail(path, e);
                return false;
            }
        }
        drop(sub);
        if !all {
            // (what is left inside keeps the folder; it is already counted as failed)
            return false;
        }
        match unlink_at(dir, name, libc::AT_REMOVEDIR) {
            Ok(()) => {
                self.tick(path);
                true
            }
            Err(e) => {
                self.fail(path, e);
                false
            }
        }
    }
}

/// Counts the entries `remove` will visit (same rules: no links followed, no other device), for the progress fraction.
fn count(dir: RawFd, name: &CStr, dev: libc::dev_t, cancel: &std::sync::atomic::AtomicBool) -> u64 {
    if cancel.load(Ordering::Relaxed) {
        return 0;
    }
    let Ok(st) = lstat_at(dir, name) else { return 0 };
    if !is_dir(&st) || st.st_dev != dev {
        return 1;
    }
    let Ok(sub) = open_dir_at(dir, name) else { return 1 };
    let mut n = 1;
    for c in names(sub.as_raw_fd()).unwrap_or_default() {
        n += count(sub.as_raw_fd(), &c, dev, cancel);
    }
    n
}

/// A fraction as the two `u32` work units of `on_progress`.
pub(crate) fn units(total: u64, done: u64) -> (u32, u32) {
    if total <= u32::MAX as u64 {
        (total as u32, done.min(total) as u32)
    } else {
        let scale = total.div_ceil(u32::MAX as u64);
        ((total / scale) as u32, (done.min(total) / scale) as u32)
    }
}

/// The parent folder (opened) and the last name of `p`; refuses `/`, `.`, `..` and an empty path.
fn split(p: &Path) -> anyhow::Result<(OwnedFd, CString)> {
    let name = p.file_name().ok_or_else(|| anyhow::anyhow!("{}: not an item that can be deleted", p.display()))?;
    let parent = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let dir = open_dir(parent).map_err(|e| anyhow::anyhow!("{}: {e}", parent.display()))?;
    Ok((dir, cpath(Path::new(name))?))
}

/// Permanently deletes `paths` (and everything in them). `Ok(true)` when all are gone, `Ok(false)` after a cancel (what was removed
/// up to then stays removed), an error naming what could not be removed (the rest is removed). `threads` is not used: one thread.
pub fn delete_permanent(paths: &[PathBuf], tracker: Arc<Tracker>, _threads: usize) -> anyhow::Result<bool> {
    let mut roots = Vec::new();
    for p in paths {
        roots.push((p, split(p)?));
    }
    let mut total = 0;
    let mut devs = Vec::new();
    for (_, (dir, name)) in &roots {
        let dev = fstat(dir.as_raw_fd())?.st_dev;
        devs.push(dev);
        total += count(dir.as_raw_fd(), name, dev, &tracker.cancel);
    }
    let mut w = Walk { dev: 0, tracker: &tracker, total, done: 0, failed: Vec::new(), failed_count: 0, cancelled: false };
    (tracker.on_progress)(units(total, 0).0, 0, None);
    for ((p, (dir, name)), dev) in roots.iter().zip(devs) {
        // the device of the folder that holds the item: a chosen folder that is itself a mount point is refused by the walk
        w.dev = dev;
        w.remove(dir.as_raw_fd(), name, p);
        if w.cancelled {
            return Ok(false);
        }
    }
    if w.failed_count > 0 {
        anyhow::bail!("{} item(s) could not be deleted: {}", w.failed_count, w.failed.join("; "));
    }
    Ok(true)
}

/// Removes the entry `name` of the open folder `dir` for good, quietly, by the same walk (the folder is not looked up again by path:
/// the trash opens its `files/` itself, without following links). `path` is only for messages.
pub(crate) fn remove_entry_at(dir: RawFd, name: &CStr, path: &Path) -> anyhow::Result<()> {
    let t = Tracker::new(Box::new(|_, _, _| {}), Default::default());
    let mut w = Walk { dev: fstat(dir)?.st_dev, tracker: &t, total: 0, done: 0, failed: Vec::new(), failed_count: 0, cancelled: false };
    w.remove(dir, name, path);
    anyhow::ensure!(w.failed_count == 0, "{} item(s) could not be deleted: {}", w.failed_count, w.failed.join("; "));
    Ok(())
}

/// Removes one tree for good, quietly (the trash's own clean-up, the source of a finished move into the home trash).
pub(crate) fn remove_tree(p: &Path) -> anyhow::Result<()> {
    let t = Tracker::new(Box::new(|_, _, _| {}), Default::default());
    delete_permanent(std::slice::from_ref(&p.to_path_buf()), Arc::new(t), 1).map(|_| ())
}

/// Thread count for deleting under `paths`: this engine runs on one thread.
pub fn threads_for(_paths: &[PathBuf]) -> usize {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_out_of_the_tree_are_removed_as_links_and_never_followed() {
        let root = std::env::temp_dir().join(format!("leaf-unix-fastdelete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (tree, outside) = (root.join("tree"), root.join("outside"));
        std::fs::create_dir_all(tree.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(outside.join("keep-dir")).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        std::fs::write(outside.join("keep-dir/inner.txt"), b"inner").unwrap();
        std::fs::write(tree.join("a.txt"), b"a").unwrap();
        std::fs::write(tree.join("sub/deeper/b.txt"), b"b").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("link-to-dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep.txt"), tree.join("sub/link-to-file")).unwrap();
        std::os::unix::fs::symlink(outside.join("keep-dir"), tree.join("sub/deeper/link-to-subdir")).unwrap();
        std::os::unix::fs::symlink("/nowhere/at/all", tree.join("dangling")).unwrap();
        // a chosen item that is itself a link: the link goes, its target stays
        let top_link = root.join("top-link");
        std::os::unix::fs::symlink(&outside, &top_link).unwrap();

        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        assert!(delete_permanent(&[tree.clone(), top_link.clone()], t, 1).unwrap());
        assert!(std::fs::symlink_metadata(&tree).is_err(), "the tree is gone");
        assert!(std::fs::symlink_metadata(&top_link).is_err(), "the chosen link is gone");
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read(outside.join("keep-dir/inner.txt")).unwrap(), b"inner");
        // a mount point is never entered (/proc is another file system; nothing is written there, the refusal comes first)
        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        assert!(delete_permanent(&[PathBuf::from("/proc")], t, 1).is_err());
        assert!(Path::new("/proc/self").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
