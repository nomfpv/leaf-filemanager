//! Linux backend of rf_win::fastcopy. Status: REAL (the interim engine of decision D2: copy and move without journal and resume).
//!
//! Data safety:
//! - The walk never follows a link: links are copied as links (`readlink` + `symlink`), files are opened with `O_NOFOLLOW`.
//!   FIFOs, sockets and device nodes are skipped and named in the error (opening a FIFO for reading would block forever; files are
//!   opened with `O_NONBLOCK` and checked with `fstat` before any byte is read, in case one was swapped in).
//! - Nothing at the destination is replaced unless the user chose "Replace" for it through `Tracker.ask`; new items are made with
//!   `O_EXCL` / `mkdir` / `RENAME_NOREPLACE`. A replace writes a temporary file in the destination folder, fsyncs and checks it, and
//!   only then renames it over the old file. A file can never replace a folder (or the other way round): such a conflict is skipped.
//! - A move on the same device is a no-replace rename. A move across devices copies, fsyncs, verifies (at least size and time), and
//!   only then removes exactly the source files that were verified, and their folders only when empty. A cancelled, failed or partial
//!   move never costs the source.
//! - Cancel removes the partly written file; what was finished stays.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use leaf_common::{stub, stub_err};

pub use leaf_common::fastcopy::{Conflict, Interrupted, Resolve, Verify};

use crate::fileops::{Tracker, Transfer, cpath, last_err};

/// Whether a copy into a WSL distribution drops the download mark beside it: there is no such copy here.
// TYPE-COPY(rf-win fastcopy.rs:520)
pub static DROP_WSL_ZONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bytes per `copy_file_range` call: small enough for smooth progress, pause and cancel.
const CHUNK: usize = 4 << 20;

/// What an item is, taken as it lies (`symlink_metadata`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Dir,
    File,
    Link,
    /// FIFO, socket, device node: never opened.
    Special,
}

fn kind(m: &std::fs::Metadata) -> Kind {
    let t = m.file_type();
    if t.is_symlink() {
        Kind::Link
    } else if t.is_dir() {
        Kind::Dir
    } else if t.is_file() {
        Kind::File
    } else {
        Kind::Special
    }
}

#[derive(Debug)]
struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}
impl std::error::Error for Cancelled {}

fn cancelled_err() -> io::Error {
    io::Error::other(Cancelled)
}

fn is_cancel(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|i| i.is::<Cancelled>())
}

/// One running copy or move: the tracker, the byte count, what failed.
struct Job<'a> {
    tracker: &'a Tracker,
    verify: Verify,
    total: u64,
    done: u64,
    started: Instant,
    /// Bytes copied since `started` (for the speed limit).
    paced: u64,
    failed: Vec<String>,
    failed_count: usize,
    skipped: Vec<String>,
    /// Decisions on conflicts, by source path.
    decide: HashMap<PathBuf, Resolve>,
    /// A move that must copy (tests: a move across devices on one device).
    force_copy: bool,
    temp_seq: u32,
}

impl Job<'_> {
    fn fail(&mut self, p: &Path, e: impl std::fmt::Display) {
        self.failed_count += 1;
        if self.failed.len() < 8 {
            self.failed.push(format!("{}: {e}", p.display()));
        }
        log::info!("copy: {} ({e})", p.display());
    }

    fn skip(&mut self, p: &Path) {
        if self.skipped.len() < 8 {
            self.skipped.push(p.display().to_string());
        }
        log::info!("copy: {} skipped (not a file, folder or link)", p.display());
    }

    /// Waits while paused; an error when cancelled.
    fn check(&self) -> io::Result<()> {
        while self.tracker.paused.load(Ordering::Relaxed) && !self.tracker.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if self.tracker.cancel.load(Ordering::Relaxed) { Err(cancelled_err()) } else { Ok(()) }
    }

    fn progress(&mut self, add: u64, name: Option<&Path>) {
        self.done += add;
        self.paced += add;
        self.tracker.bytes.set(self.done, self.total);
        let (t, d) = crate::fastdelete::units(self.total.max(1), self.done);
        (self.tracker.on_progress)(t, d, name.and_then(|n| n.file_name()).map(|n| n.to_string_lossy().into_owned()));
        // speed limit: sleep until the bytes so far fit the limit
        let limit = self.tracker.limit.load(Ordering::Relaxed);
        if limit > 0 {
            let due = Duration::from_secs_f64(self.paced as f64 / limit as f64);
            let spent = self.started.elapsed();
            if due > spent {
                std::thread::sleep((due - spent).min(Duration::from_secs(2)));
            }
        } else {
            self.started = Instant::now();
            self.paced = 0;
        }
    }

    fn temp_name(&mut self, dir: &Path) -> PathBuf {
        self.temp_seq += 1;
        crate::fileops::unique_name(dir, &format!(".leaf-part-{}-{}", std::process::id(), self.temp_seq), "")
    }
}

/// Sizes and conflicts of one source against its destination (no links followed). Adds to `bytes`; a folder that meets a folder is
/// merged (looked into), any other meeting is a conflict.
fn scan(src: &Path, dst: &Path, bytes: &mut u64, conflicts: &mut Vec<Conflict>, cancel: &std::sync::atomic::AtomicBool) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let Ok(sm) = std::fs::symlink_metadata(src) else { return };
    let sk = kind(&sm);
    match std::fs::symlink_metadata(dst) {
        Err(_) => *bytes += tree_bytes(src, cancel),
        Ok(dm) => {
            let dk = kind(&dm);
            if same_item(&sm, &dm) {
                // (one item under two paths: not a conflict, never offered for replacing)
                *bytes += tree_bytes(src, cancel);
            } else if sk == Kind::Dir && dk == Kind::Dir {
                if let Ok(rd) = std::fs::read_dir(src) {
                    for e in rd.flatten() {
                        scan(&e.path(), &dst.join(e.file_name()), bytes, conflicts, cancel);
                    }
                }
            } else {
                *bytes += tree_bytes(src, cancel);
                conflicts.push(Conflict {
                    src: src.to_path_buf(),
                    dst: dst.to_path_buf(),
                    src_dir: sk == Kind::Dir,
                    dst_dir: dk == Kind::Dir,
                    src_size: if sk == Kind::File { sm.len() } else { 0 },
                    dst_size: if dk == Kind::File { dm.len() } else { 0 },
                    src_time: sm.modified().ok(),
                    dst_time: dm.modified().ok(),
                });
            }
        }
    }
}

/// Bytes of the regular files under `p` (no links followed).
fn tree_bytes(p: &Path, cancel: &std::sync::atomic::AtomicBool) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else { return 0 };
    match kind(&m) {
        Kind::File => m.len(),
        Kind::Dir if !cancel.load(Ordering::Relaxed) => {
            std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| tree_bytes(&e.path(), cancel)).sum()).unwrap_or(0)
        }
        _ => 0,
    }
}

/// What a source item was when it was copied: removed after a move only while all of it still matches (a file saved or replaced
/// meanwhile has another inode, size, mtime or ctime and is kept).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ident {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Ident {
    fn of(m: &std::fs::Metadata) -> Self {
        Self { dev: m.dev(), ino: m.ino(), size: m.len(), mtime: (m.mtime(), m.mtime_nsec()), ctime: (m.ctime(), m.ctime_nsec()) }
    }
}

/// Whether two items are one and the same (one inode seen through two paths: a hard link, a bind mount, a share mounted twice).
fn same_item(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// The source items a copy has verified, as they were then, for removing exactly them after a move (children before their folder).
/// Folders carry only their device and inode (their times change as their contents go).
#[derive(Default)]
struct Copied {
    files: Vec<(PathBuf, Ident)>,
    dirs: Vec<(PathBuf, (u64, u64))>,
}

/// Removes one copied source file or link, but only while it is still the item that was copied.
fn remove_source_file(job: &mut Job, p: &Path, was: &Ident) {
    match std::fs::symlink_metadata(p) {
        Ok(now) if Ident::of(&now) == *was => {
            if let Err(e) = std::fs::remove_file(p) {
                job.fail(p, format_args!("copied, but the original could not be removed: {e}"));
            }
        }
        Ok(_) => job.fail(p, "changed during the move: the original was kept"),
        Err(_) => {}
    }
}

/// Removes one source folder after its contents went: only the same folder (device and inode), and only when empty.
fn remove_source_dir(job: &mut Job, p: &Path, was: (u64, u64)) {
    match std::fs::symlink_metadata(p) {
        Ok(now) if now.is_dir() && (now.dev(), now.ino()) == was => {
            if let Err(e) = std::fs::remove_dir(p) {
                job.fail(p, format_args!("copied, but the original folder could not be removed: {e}"));
            }
        }
        Ok(_) => job.fail(p, "changed during the move: the original was kept"),
        Err(_) => {}
    }
}

fn set_times_fd(fd: i32, m: &std::fs::Metadata) -> io::Result<()> {
    let ts = [
        libc::timespec { tv_sec: m.atime() as _, tv_nsec: m.atime_nsec() as _ },
        libc::timespec { tv_sec: m.mtime() as _, tv_nsec: m.mtime_nsec() as _ },
    ];
    // SAFETY: `ts` is an array of two timespecs, as futimens expects.
    if unsafe { libc::futimens(fd, ts.as_ptr()) } == 0 { Ok(()) } else { Err(last_err()) }
}

fn set_times_path(p: &Path, m: &std::fs::Metadata, nofollow: bool) -> io::Result<()> {
    let c = cpath(p)?;
    let ts = [
        libc::timespec { tv_sec: m.atime() as _, tv_nsec: m.atime_nsec() as _ },
        libc::timespec { tv_sec: m.mtime() as _, tv_nsec: m.mtime_nsec() as _ },
    ];
    let flags = if nofollow { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
    // SAFETY: `c` is NUL-terminated, `ts` is an array of two timespecs.
    if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), flags) } == 0 { Ok(()) } else { Err(last_err()) }
}

/// The mode bits kept on a copy: permissions and the sticky bit; set-user-ID and set-group-ID are dropped (the copy belongs to us).
fn kept_mode(m: &std::fs::Metadata) -> u32 {
    m.mode() & 0o1777
}

/// `copy_file_range` of up to `len` bytes; `Ok(0)` at the end of the file.
fn copy_range(from: &File, to: &File, len: usize) -> io::Result<usize> {
    // SAFETY: two descriptors we hold open, null offsets (the files' own positions are used), a plain length.
    let r = unsafe {
        libc::syscall(libc::SYS_copy_file_range, from.as_raw_fd(), std::ptr::null_mut::<i64>(), to.as_raw_fd(), std::ptr::null_mut::<i64>(), len, 0u32)
    };
    if r < 0 { Err(last_err()) } else { Ok(r as usize) }
}

/// Compares two files byte by byte (the copy read back: its cached pages are dropped first).
fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    // SAFETY: advice on a descriptor we hold; failure only means the cache is used.
    unsafe { libc::posix_fadvise(fb.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Copies one regular file to `dst`, which must not exist. Progress, pause, cancel and limit between chunks; fsync; mode and times
/// kept; checked as `verify` says (and always for size). On any failure the partly written `dst` is removed.
fn copy_file(job: &mut Job, src: &Path, dst: &Path, verify: Verify) -> io::Result<Ident> {
    let from = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(src)?;
    let sm = from.metadata()?;
    if !sm.file_type().is_file() {
        return Err(io::Error::other("not a regular file (changed while copying)"));
    }
    let to = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_CLOEXEC).open(dst)?;
    let r = (|| -> io::Result<()> {
        let mut plain = false;
        let mut buf = Vec::new();
        let mut copied = 0u64;
        loop {
            job.check()?;
            let n = if plain {
                if buf.is_empty() {
                    buf = vec![0u8; 1 << 20];
                }
                let n = (&from).read(&mut buf)?;
                (&to).write_all(&buf[..n])?;
                n
            } else {
                match copy_range(&from, &to, CHUNK) {
                    Ok(n) => n,
                    // (older kernels and some file systems: across devices, or not at all) the rest with read and write
                    Err(e) if copied == 0 && matches!(e.raw_os_error(), Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM)) => {
                        plain = true;
                        continue;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            };
            if n == 0 {
                break;
            }
            copied += n as u64;
            job.progress(n as u64, Some(src));
        }

        to.sync_all()?;
        // SAFETY: fchmod on a descriptor we hold.
        if unsafe { libc::fchmod(to.as_raw_fd(), kept_mode(&sm)) } != 0 {
            log::info!("copy: mode of {} not kept ({})", dst.display(), last_err());
        }
        set_times_fd(to.as_raw_fd(), &sm)?;
        let dm = to.metadata()?;
        if dm.len() != sm.len() || copied != sm.len() {
            return Err(io::Error::other("the copy has another size than the original"));
        }
        if verify == Verify::Quick && dm.mtime() != sm.mtime() {
            return Err(io::Error::other("the copy has another time than the original"));
        }
        if verify == Verify::Full && !same_bytes(src, dst)? {
            return Err(io::Error::other("the copy differs from the original"));
        }
        Ok(())
    })();
    drop(to);
    if r.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    r.map(|_| Ident::of(&sm))
}

/// Copies the tree at `src` to `dst` (which must not exist), links as links. Files that fail are named and left out; a cancel ends
/// it. `Ok(true)` when every part was copied. With `remove_now` (a move) each source file is removed right after its own copy was
/// verified, and each folder after its contents; otherwise `copied` lists the verified source items (a folder only when everything
/// inside it was copied), children before their folder.
fn copy_tree(job: &mut Job, src: &Path, dst: &Path, verify: Verify, copied: &mut Copied, remove_now: bool) -> io::Result<bool> {
    job.check()?;
    let m = std::fs::symlink_metadata(src)?;
    match kind(&m) {
        Kind::File => {
            let was = copy_file(job, src, dst, verify)?;
            if remove_now {
                remove_source_file(job, src, &was);
            } else {
                copied.files.push((src.to_path_buf(), was));
            }
            Ok(true)
        }
        Kind::Link => {
            let target = std::fs::read_link(src)?;
            std::os::unix::fs::symlink(&target, dst)?;
            let _ = set_times_path(dst, &m, true);
            // (the link read is the link copied only if it did not change around the read)
            let was = Ident::of(&m);
            if std::fs::symlink_metadata(src).map(|n| Ident::of(&n)).ok() != Some(was) {
                job.fail(src, "changed during the copy");
                return Ok(false);
            }
            if remove_now {
                remove_source_file(job, src, &was);
            } else {
                copied.files.push((src.to_path_buf(), was));
            }
            Ok(true)
        }
        Kind::Special => {
            job.skip(src);
            Ok(false)
        }
        Kind::Dir => {
            std::fs::DirBuilder::new().mode(0o700).create(dst)?;
            let mut all = true;
            for e in std::fs::read_dir(src)? {
                let e = e?;
                let s = e.path();
                match copy_tree(job, &s, &dst.join(e.file_name()), verify, copied, remove_now) {
                    Ok(c) => all &= c,
                    Err(e) if is_cancel(&e) => return Err(e),
                    Err(e) => {
                        job.fail(&s, e);
                        all = false;
                    }
                }
            }
            let _ = std::fs::set_permissions(dst, std::os::unix::fs::PermissionsExt::from_mode(kept_mode(&m)));
            let _ = set_times_path(dst, &m, false);
            if all {
                if remove_now {
                    remove_source_dir(job, src, (m.dev(), m.ino()));
                } else {
                    copied.dirs.push((src.to_path_buf(), (m.dev(), m.ino())));
                }
            }
            Ok(all)
        }
    }
}
/// Removes exactly the source items of a finished move that `copied` lists: files and links, then folders deepest first and only
/// when empty (something added meanwhile keeps its folder).
fn remove_copied(job: &mut Job, copied: &Copied) {
    for (f, was) in &copied.files {
        remove_source_file(job, f, was);
    }
    // (`dirs` lists children before their parents: a folder is pushed after its contents)
    for (d, was) in &copied.dirs {
        remove_source_dir(job, d, *was);
    }
}

/// `name (2).ext`, … beside `dst` (for "Keep both" and for a copy into the item's own folder).
fn keep_both_name(dst: &Path) -> PathBuf {
    let dir = dst.parent().unwrap_or(Path::new("."));
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_dir = std::fs::symlink_metadata(dst).is_ok_and(|m| m.is_dir());
    let p = Path::new(&name);
    match (p.file_stem(), p.extension()) {
        (Some(stem), Some(ext)) if !is_dir && !stem.is_empty() => crate::fileops::unique_name(dir, &stem.to_string_lossy(), &format!(".{}", ext.to_string_lossy())),
        _ => crate::fileops::unique_name(dir, &name, ""),
    }
}

/// One item of the job: `src` to `dst` with the conflict decisions. Pushes (source, made) into the tracker's results for every item
/// that was made at the destination.
fn item(job: &mut Job, kind_of_job: Transfer, src: &Path, dst: &Path) -> io::Result<()> {
    job.check()?;
    let sm = std::fs::symlink_metadata(src)?;
    let mut dst = dst.to_path_buf();
    let mut replace = false;
    if let Ok(dm) = std::fs::symlink_metadata(&dst) {
        let (sk, dk) = (kind(&sm), kind(&dm));
        if same_item(&sm, &dm) {
            // one item under two paths (hard link, bind mount, a share mounted twice): never replaced, never removed. A move has
            // nothing to do; a copy keeps both.
            if kind_of_job == Transfer::Move {
                return Ok(());
            }
            dst = keep_both_name(&dst);
            return copy_new(job, kind_of_job, src, &sm, dst, false);
        }
        // (a decision exists for every conflict that was asked, and for a copy into the item's own folder: keep both, never a merge
        // of a folder with itself)
        let decision = job.decide.get(src).copied();
        if sk == Kind::Dir && dk == Kind::Dir && decision.is_none() {
            // merge: each child on its own
            for e in std::fs::read_dir(src)? {
                let e = e?;
                match item(job, kind_of_job, &e.path(), &dst.join(e.file_name())) {
                    Err(e) if is_cancel(&e) => return Err(e),
                    Err(err) => job.fail(&e.path(), err),
                    Ok(()) => {}
                }
            }
            if kind_of_job == Transfer::Move {
                // (only when it is empty now: what stayed behind keeps it)
                let _ = std::fs::remove_dir(src);
            }
            return Ok(());
        }
        match decision.unwrap_or(Resolve::KeepBoth) {
            Resolve::Skip => return Ok(()),
            Resolve::KeepBoth => dst = keep_both_name(&dst),
            // a file never replaces a folder, nor a folder a file
            Resolve::Replace if (sk == Kind::Dir) != (dk == Kind::Dir) => return Ok(()),
            Resolve::Replace => replace = true,
        }
    }
    copy_new(job, kind_of_job, src, &sm, dst, replace)
}

/// The rest of [`item`]: `src` becomes `dst` (a free name, or the file the user chose to replace).
fn copy_new(job: &mut Job, kind_of_job: Transfer, src: &Path, sm: &std::fs::Metadata, dst: PathBuf, replace: bool) -> io::Result<()> {
    if kind(sm) == Kind::Special {
        job.skip(src);
        return Ok(());
    }
    let parent = dst.parent().unwrap_or(Path::new("."));
    let same_dev = !job.force_copy && std::fs::metadata(parent).is_ok_and(|pm| pm.dev() == sm.dev());
    if kind_of_job == Transfer::Move && same_dev {
        let r = if replace {
            // the user chose to replace this file: a plain rename replaces it in one step
            std::fs::rename(src, &dst)
        } else {
            crate::fileops::renameat2(src, &dst, libc::RENAME_NOREPLACE)
        };
        match r {
            Ok(()) => {
                job.progress(tree_bytes(&dst, &job.tracker.cancel), Some(src));
                job.tracker.results.lock().unwrap().push((src.to_path_buf(), dst));
                return Ok(());
            }
            // (a bind mount of the same file system: copy below)
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {}
            Err(e) => return Err(e),
        }
    }
    // copy (for a move: copy, verify, then remove the verified source)
    let verify = if kind_of_job == Transfer::Move && job.verify == Verify::Off { Verify::Quick } else { job.verify };
    let target = if replace { job.temp_name(parent) } else { dst.clone() };
    let mut copied = Copied::default();
    // (a replace removes the source only after the new file took the old one's place, below)
    let remove_now = kind_of_job == Transfer::Move && !replace;
    let r = copy_tree(job, src, &target, verify, &mut copied, remove_now);
    if replace {
        match &r {
            Ok(true) => {
                // the old file must still not be the source itself (seen through another path)
                if std::fs::symlink_metadata(&dst).is_ok_and(|dm| same_item(sm, &dm)) {
                    let _ = crate::fastdelete::remove_tree(&target);
                    return Ok(());
                }
                std::fs::rename(&target, &dst)?
            }
            _ => {
                // the old file stays as it was; the unfinished new one goes
                if std::fs::symlink_metadata(&target).is_ok() {
                    let _ = crate::fastdelete::remove_tree(&target);
                }
                return r.and(Ok(()));
            }
        }
    }
    // (a name that appeared meanwhile is not ours: it is never put in the results, so an undo never deletes it)
    let theirs = matches!(&r, Err(e) if e.kind() == io::ErrorKind::AlreadyExists);
    if !theirs && std::fs::symlink_metadata(&dst).is_ok() {
        job.tracker.results.lock().unwrap().push((src.to_path_buf(), dst.clone()));
    }
    r?;
    if kind_of_job == Transfer::Move && replace {
        remove_copied(job, &copied);
    }
    Ok(())
}

/// Copy or move with our own engine. Conflicts are asked once, as one list, through `Tracker.ask` (`None` from it cancels before
/// anything is changed; without it, both are kept). `Ok(true)` when everything was done, `Ok(false)` after a cancel, an error naming
/// what failed or was skipped (the rest is done).
pub fn transfer(kind: Transfer, sources: &[PathBuf], dest_dir: &Path, tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    run(kind, sources, dest_dir, tracker, false)
}

fn run(kind: Transfer, sources: &[PathBuf], dest_dir: &Path, tracker: Arc<Tracker>, force_copy: bool) -> anyhow::Result<bool> {
    anyhow::ensure!(std::fs::metadata(dest_dir).is_ok_and(|m| m.is_dir()), "{}: not a folder", dest_dir.display());
    let dest_abs = std::fs::canonicalize(dest_dir)?;
    // the destination folder and every folder above it, by device and inode (a bind mount or a second mount of the same share is
    // the same folder under another path)
    let dest_chain: Vec<(u64, u64)> = dest_abs.ancestors().filter_map(|a| std::fs::metadata(a).ok()).map(|m| (m.dev(), m.ino())).collect();
    let mut jobs = Vec::new();
    for s in sources {
        let name = s.file_name().ok_or_else(|| anyhow::anyhow!("{}: not an item that can be copied", s.display()))?;
        let m = std::fs::symlink_metadata(s).map_err(|e| anyhow::anyhow!("{}: {e}", s.display()))?;
        let parent = s.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if m.is_dir() && dest_chain.contains(&(m.dev(), m.ino())) {
            anyhow::bail!("{}: a folder cannot be copied or moved into itself", s.display());
        }
        let own_folder = std::fs::metadata(parent).is_ok_and(|p| Some(&(p.dev(), p.ino())) == dest_chain.first());
        if own_folder && kind == Transfer::Move {
            // moving into the folder it is in: nothing to do
            continue;
        }
        jobs.push((s.clone(), dest_dir.join(name), own_folder));
    }
    // sizes and conflicts (a copy into its own folder always keeps both: it is not asked)
    let mut total = 0;
    let mut conflicts = Vec::new();
    for (s, d, own) in &jobs {
        if *own {
            total += tree_bytes(s, &tracker.cancel);
        } else {
            scan(s, d, &mut total, &mut conflicts, &tracker.cancel);
        }
    }
    let mut decide = HashMap::new();
    if !conflicts.is_empty() {
        if let Some(ask) = &tracker.ask {
            let Some(answers) = ask(&conflicts) else { return Ok(false) };
            for (c, r) in conflicts.iter().zip(answers) {
                decide.insert(c.src.clone(), r);
            }
        }
    }
    for (s, _, own) in &jobs {
        if *own {
            decide.insert(s.clone(), Resolve::KeepBoth);
        }
    }
    let mut job = Job {
        tracker: &tracker,
        verify: tracker.verify_mode(),
        total,
        done: 0,
        started: Instant::now(),
        paced: 0,
        failed: Vec::new(),
        failed_count: 0,
        skipped: Vec::new(),
        decide,
        force_copy,
        temp_seq: 0,
    };
    job.progress(0, None);
    for (s, d, _) in &jobs {
        match item(&mut job, kind, s, d) {
            Err(e) if is_cancel(&e) => return Ok(false),
            Err(e) => job.fail(s, e),
            Ok(()) => {}
        }
    }
    let mut problems = Vec::new();
    if job.failed_count > 0 {
        problems.push(format!("{} item(s) failed: {}", job.failed_count, job.failed.join("; ")));
    }
    if !job.skipped.is_empty() {
        problems.push(format!("not copied (pipes, sockets, devices): {}", job.skipped.join(", ")));
    }
    anyhow::ensure!(problems.is_empty(), "{}", problems.join(". "));
    Ok(true)
}

/// Moves one item to `dst` (which must not exist) across devices, all or nothing: copied, fsynced and checked; when every part was
/// copied, the source is removed; otherwise the partial copy is removed and the source stays whole. (The trash's "Move to home
/// trash" and the restore of such an item.)
pub(crate) fn move_across(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let tracker = Tracker::new(Box::new(|_, _, _| {}), Default::default());
    let mut job = Job {
        tracker: &tracker,
        verify: Verify::Quick,
        total: 0,
        done: 0,
        started: Instant::now(),
        paced: 0,
        failed: Vec::new(),
        failed_count: 0,
        skipped: Vec::new(),
        decide: HashMap::new(),
        force_copy: true,
        temp_seq: 0,
    };
    let mut copied = Copied::default();
    let r = copy_tree(&mut job, src, dst, Verify::Quick, &mut copied, false);
    if !matches!(r, Ok(true)) || job.failed_count > 0 || !job.skipped.is_empty() {
        let theirs = matches!(&r, Err(e) if e.kind() == io::ErrorKind::AlreadyExists);
        if !theirs && std::fs::symlink_metadata(dst).is_ok() {
            let _ = crate::fastdelete::remove_tree(dst);
        }
        r?;
        anyhow::bail!("{}: not every part could be copied ({})", src.display(), [job.failed.join("; "), job.skipped.join(", ")].join(" "));
    }
    remove_copied(&mut job, &copied);
    anyhow::ensure!(job.failed_count == 0, "{}", job.failed.join("; "));
    Ok(())
}

/// Where journals live: this engine keeps none (no resume, decision D2).
pub fn set_journal_dir(_dir: PathBuf) {
    stub!("fastcopy::set_journal_dir")
}

/// Jobs left unfinished by an earlier run: none (no journal is written).
pub fn interrupted() -> Vec<Interrupted> {
    stub!("fastcopy::interrupted")
}

/// Forget an interrupted job: there are none.
pub fn discard(_journal: &Path) {
    stub!("fastcopy::discard")
}

/// Carries an interrupted job on: this engine cannot resume (decision D2).
pub fn resume(_journal: &Path, _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    stub_err!("fastcopy::resume")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/a.bin"), vec![7u8; 9 << 20]).unwrap();
        std::fs::write(root.join("src/sub/b.txt"), b"b").unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/o.txt"), b"o").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("src/link")).unwrap();
    }

    fn intact(root: &Path) {
        assert_eq!(std::fs::read(root.join("src/a.bin")).unwrap(), vec![7u8; 9 << 20]);
        assert_eq!(std::fs::read(root.join("src/sub/b.txt")).unwrap(), b"b");
        assert!(std::fs::symlink_metadata(root.join("src/link")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(root.join("outside/o.txt")).unwrap(), b"o");
    }

    #[test]
    fn a_cancelled_copy_or_move_leaves_the_source_whole_and_links_stay_links() {
        let root = std::env::temp_dir().join(format!("leaf-unix-fastcopy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        tree(&root);
        let dest = root.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        // cancel at the first chunk of the big file, for a copy and for a move that must copy (as across devices)
        for force in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let c = cancel.clone();
            let t = Arc::new(Tracker::new(Box::new(move |_, done, _| if done > 0 { c.store(true, Ordering::Relaxed) }), cancel));
            let kind = if force { Transfer::Move } else { Transfer::Copy };
            assert!(!run(kind, &[root.join("src")], &dest, t, force).unwrap(), "cancelled");
            intact(&root);
            assert!(std::fs::symlink_metadata(dest.join("src/a.bin")).is_err(), "the partly written file is removed");
            std::fs::remove_dir_all(dest.join("src")).unwrap();
        }
        // a whole copy: links copied as links, nothing of the outside folder comes along; existing names are kept, not replaced
        std::fs::write(dest.join("a.bin"), b"old").unwrap();
        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        assert!(run(Transfer::Copy, &[root.join("src"), root.join("src/a.bin")], &dest, t.clone(), false).unwrap());
        assert_eq!(std::fs::read_link(dest.join("src/link")).unwrap(), root.join("outside"));
        assert_eq!(std::fs::read(dest.join("a.bin")).unwrap(), b"old", "not replaced without being asked");
        assert_eq!(std::fs::read(dest.join("a (2).bin")).unwrap().len(), 9 << 20);
        assert_eq!(t.results.lock().unwrap().len(), 2);
        intact(&root);
        // a move across devices that completes: the source goes, the copy is whole
        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        let moved = root.join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        assert!(run(Transfer::Move, &[root.join("src")], &moved, t, true).unwrap());
        assert!(std::fs::symlink_metadata(root.join("src")).is_err());
        assert_eq!(std::fs::read(moved.join("src/sub/b.txt")).unwrap(), b"b");
        assert_eq!(std::fs::read(root.join("outside/o.txt")).unwrap(), b"o");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_same_item_under_two_paths_is_never_replaced_nor_removed() {
        // a hard link stands in for a bind mount or a share mounted twice: one inode under two paths
        let root = std::env::temp_dir().join(format!("leaf-unix-fastcopy-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/x.txt"), b"only copy").unwrap();
        std::fs::hard_link(root.join("a/x.txt"), root.join("b/x.txt")).unwrap();
        for force in [false, true] {
            let mut t = Tracker::new(Box::new(|_, _, _| {}), Default::default());
            t.ask = Some(Box::new(|c: &[Conflict]| Some(vec![Resolve::Replace; c.len()])));
            let _ = run(Transfer::Move, &[root.join("a/x.txt")], &root.join("b"), Arc::new(t), force);
            assert_eq!(std::fs::read(root.join("a/x.txt")).unwrap(), b"only copy");
            assert_eq!(std::fs::read(root.join("b/x.txt")).unwrap(), b"only copy");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
