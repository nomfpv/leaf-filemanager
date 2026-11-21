//! Linux backend of rf_win::fastcopy. Status: REAL (the interim engine of decision D2: copy and move without journal and resume).
//!
//! Data safety:
//! - The walk never follows a link: links are copied as links (`readlink` + `symlink`), files are opened with `O_NOFOLLOW`.
//!   FIFOs, sockets and device nodes are skipped and named in the error (opening a FIFO for reading would block forever; files are
//!   opened with `O_NONBLOCK` and checked with `fstat` before any byte is read, in case one was swapped in).
//! - Nothing at the destination is replaced unless the user chose "Replace" for it through `Tracker.ask`; new items are made with
//!   `O_EXCL` / `mkdir` / `RENAME_NOREPLACE`. A replace writes a temporary file in the destination folder, fsyncs and checks it, and
//!   only then renames it over the old file. A file can never replace a folder (or the other way round): such a conflict is skipped.
//! - A move on the same device is a no-replace rename. A move across devices copies each file, fsyncs it and the folder holding its
//!   name, verifies it (at least the size plus the first and last MiB read back), and right then removes that source file, but only
//!   while its (dev, ino, size, mtime, ctime) still match what was copied; folders go only when empty. Mounts inside the folder are
//!   not moved. One item seen under two paths (same dev+ino) is never replaced nor removed. A cancelled, failed or partial move never
//!   costs a source that was not copied.
//! - Cancel removes the partly written file; what was finished stays.
//!
//! Crash-safe (the journal format of `leaf_copy::journal`, as on Windows): every file is written to `.name.<tag>.rfpart` beside its
//! target, fsynced and only then renamed to its name (without replacing, unless the user chose "Replace"), so a finished name is
//! always a whole file and is never opened for writing again. The job's sources, destination and conflict answers go to a journal
//! (synced) before the first byte is copied; each part file's progress is recorded after an `fdatasync` of the part. If the app dies,
//! or the job ends because the disk went away or filled up, [`interrupted`] offers it on the next start and [`resume`] carries it
//! on with the same answers: a file continues at its recorded offset only while the part still holds those bytes and the source has
//! the size and time it had (`leaf_copy::resume::decide`), else it starts over; a finished copy is checked and kept.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use leaf_copy::journal::{self as jrn, field_path, path_field};
use leaf_copy::resume::{Decision, Meta, Started};

pub use leaf_common::fastcopy::{Conflict, Interrupted, Resolve, Verify};

use crate::fileops::{Tracker, Transfer, cpath, last_err};

/// Whether a copy into a WSL distribution drops the download mark beside it: there is no such copy here.
// TYPE-COPY(rf-win fastcopy.rs:520)
pub static DROP_WSL_ZONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bytes per `copy_file_range` call: small enough for smooth progress, pause and cancel.
const CHUNK: usize = 4 << 20;
/// A part file's progress is synced and recorded after this many bytes (what a crash can cost of one file).
const RECORD_EVERY: u64 = if cfg!(test) { 1 << 20 } else { 64 << 20 };
/// Finished-file records are flushed to disk at most this often (a lost one only means that file is checked again on resume).
const JOURNAL_SYNC: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------- the job journal
//
// One file per running job in the journal folder (records as in rf-win, see `leaf_copy::journal`; paths through `path_field`):
//   B kind(c|m) 0 verify(0|1|2) dest sources…   the job (as on Windows; this engine plans as it walks, so no D/N/S/W/J records)
//   X answer(r|s|k) src                         the answer to one conflict
//   P                                           the job is complete (synced) and copying starts
//   K src dst                                   "keep both" chose this name for src (synced at once: a resume must use it again)
//   Y part size mtime_s mtime_ns                a part file was started for a source of this size and time
//   O part offset                               the part holds `offset` synced bytes
//   G dst                                       a file was finished at dst
// Closing a journal deletes it, so what's left in the folder is exactly the jobs that were cut off.

static JOURNAL_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The journal of a running job.
struct Jr {
    w: jrn::Writer,
    last_sync: Instant,
    path: PathBuf,
}

impl Jr {
    fn record(&mut self, fields: &[&str], sync_now: bool) {
        let r = self.w.record(fields);
        if sync_now || self.last_sync.elapsed() >= JOURNAL_SYNC {
            let _ = r.and_then(|_| self.w.sync()).map_err(|e| log::info!("journal: {}: {e}", self.path.display()));
            self.last_sync = Instant::now();
        }
    }
}

/// What the journal of a cut-off job knew, for carrying it on.
#[derive(Default)]
struct ResumeState {
    /// Part files and the source each was started for, with the last recorded offset.
    started: HashMap<PathBuf, Started>,
    /// "Keep both" names chosen before the cut, by source.
    kept: HashMap<PathBuf, PathBuf>,
}

fn meta_of(m: &std::fs::Metadata) -> Meta {
    Meta { size: m.len(), mtime: (m.mtime(), m.mtime_nsec()) }
}

/// Whether an error means the disk went away or is full: the job stops resumable (its journal and part files stay for a resume),
/// instead of ending with failures.
fn is_cut(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EIO | libc::ENODEV | libc::ENXIO | libc::ENOSPC | libc::EDQUOT | libc::ESTALE | libc::ENOTCONN | libc::EROFS))
}

/// The temporary name beside `dst` that a file is written to (`.name.<tag>.rfpart`; the tag is the journal's, the same when the
/// job is resumed, so the part is found again). A name too long for that is replaced by a hash of it.
fn part_name(dst: &Path, tag: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let name = dst.file_name().unwrap_or_default();
    let mut n = std::ffi::OsString::from(".");
    if name.as_bytes().len() + tag.len() + 9 <= 255 {
        n.push(name);
    } else {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.as_bytes().hash(&mut h);
        n.push(format!("{:016x}", h.finish()));
    }
    n.push(format!(".{tag}.rfpart"));
    dst.with_file_name(n)
}

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
    /// A move that copies stays on the device of its item: another mount inside (a share, a stick, a bind mount) is neither copied
    /// nor removed. `None` for a copy.
    stay_on: Option<u64>,
    /// Files go through part files (the engine's jobs; the trash's and the organizer's single moves write their target directly).
    parts: bool,
    /// In part file names: the journal's id, else the process id.
    tag: String,
    jr: Option<Jr>,
    /// A resumed job: what its journal knew.
    resume: Option<ResumeState>,
    /// The disk went away or filled up: the journal stays for a resume.
    cut: bool,
}

impl<'a> Job<'a> {
    fn new(tracker: &'a Tracker, verify: Verify, force_copy: bool, stay_on: Option<u64>) -> Self {
        Job {
            tracker,
            verify,
            total: 0,
            done: 0,
            started: Instant::now(),
            paced: 0,
            failed: Vec::new(),
            failed_count: 0,
            skipped: Vec::new(),
            decide: HashMap::new(),
            force_copy,
            temp_seq: 0,
            stay_on,
            parts: false,
            tag: std::process::id().to_string(),
            jr: None,
            resume: None,
            cut: false,
        }
    }

    fn record(&mut self, fields: &[&str], sync_now: bool) {
        if let Some(j) = &mut self.jr {
            j.record(fields, sync_now);
        }
    }

    /// "Keep both" picked `dst` for `src`: recorded at once, so a resume writes into the same name.
    fn note_kept(&mut self, src: &Path, dst: &Path) {
        self.record(&["K", &path_field(src), &path_field(dst)], true);
    }

    fn fail(&mut self, p: &Path, e: impl std::fmt::Display) {
        self.failed_count += 1;
        if self.failed.len() < 8 {
            self.failed.push(format!("{}: {e}", p.display()));
        }
        log::info!("copy: {} ({e})", p.display());
    }

    /// A failure with an I/O error: a disk that went away or filled up also marks the job as cut (resumable).
    fn fail_err(&mut self, p: &Path, e: io::Error) {
        if is_cut(&e) {
            self.cut = true;
        }
        self.fail(p, e);
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
pub(crate) struct Ident {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Ident {
    pub(crate) fn of(m: &std::fs::Metadata) -> Self {
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

/// Makes the entries of folder `p` durable (a new file's or folder's name), so that a source is never removed while its copy could
/// still vanish in a power cut.
fn sync_dir(p: &Path) -> io::Result<()> {
    File::open(p)?.sync_all()
}

/// The folder `dst` lies in.
fn parent_of(dst: &Path) -> &Path {
    dst.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
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

/// Clones all of `from` into the empty `to` where the file system shares blocks (`FICLONE`); false where it can't.
fn reflink(from: &File, to: &File) -> bool {
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // SAFETY: an ioctl on two descriptors we hold; it changes nothing on failure.
    unsafe { libc::ioctl(to.as_raw_fd(), FICLONE as _, from.as_raw_fd()) == 0 }
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

/// Compares the first and the last MiB of two files of length `len` (the copy read back: its cached pages are dropped first).
fn same_ends(a: &Path, b: &Path, len: u64) -> io::Result<bool> {
    use std::os::unix::fs::FileExt;
    const SPAN: u64 = 1 << 20;
    let (fa, fb) = (File::open(a)?, File::open(b)?);
    // SAFETY: advice on a descriptor we hold; failure only means the cache is used.
    unsafe { libc::posix_fadvise(fb.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    let mut spans = vec![(0, len.min(SPAN))];
    if len > SPAN {
        spans.push((len - len.min(SPAN), len.min(SPAN)));
    }
    let (mut ba, mut bb) = (vec![0u8; SPAN as usize], vec![0u8; SPAN as usize]);
    for (at, n) in spans {
        let n = n as usize;
        fa.read_exact_at(&mut ba[..n], at)?;
        fb.read_exact_at(&mut bb[..n], at)?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
    Ok(true)
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
/// kept; checked as `verify` says (always the size; Quick adds the first and last MiB read back, Full every byte). On any failure the
/// partly written `dst` is removed.
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
        // Quick here: the size (always checked above) plus the first and the last MiB read back from the disk. (The time is not a
        // check: it was just set from the original.)
        if verify == Verify::Quick && !same_ends(src, dst, sm.len())? {
            return Err(io::Error::other("the copy differs from the original"));
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

/// Checks a finished copy as `verify` says (the size and time were compared by the caller).
fn verified(src: &Path, dst: &Path, len: u64, verify: Verify) -> io::Result<bool> {
    match verify {
        Verify::Off => Ok(true),
        Verify::Quick => same_ends(src, dst, len),
        Verify::Full => same_bytes(src, dst),
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: act as a file system without `renameat2` flags (NFS, FUSE without rename2).
    static NO_RENAME2: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Renames `from` to `to` without replacing anything there (`AlreadyExists` then, nothing touched). Where the file system refuses
/// `RENAME_NOREPLACE` (NFS rejects any rename flag; FUSE file systems without rename2, such as ntfs-3g, sshfs and gvfs mounts, answer
/// EINVAL), the name is claimed first with an empty file made `O_EXCL` and the part renamed over that placeholder; the placeholder goes
/// again if the rename fails.
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    let forced = NO_RENAME2.with(|f| f.get());
    #[cfg(not(test))]
    let forced = false;
    let e = if forced { io::Error::from_raw_os_error(libc::EINVAL) } else {
        match crate::fileops::renameat2(from, to, libc::RENAME_NOREPLACE) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        }
    };
    if !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
        return Err(e);
    }
    let claim = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(to)?;
    let ino = claim.metadata()?.ino();
    drop(claim);
    std::fs::rename(from, to).inspect_err(|_| {
        // (only our placeholder: a name someone put there meanwhile is theirs)
        if std::fs::symlink_metadata(to).is_ok_and(|m| m.ino() == ino && m.len() == 0) {
            let _ = std::fs::remove_file(to);
        }
    })
}

/// Copies one regular file to `dst` through its part file (see the module text): a fresh part, or on a resume the part carried on
/// where the journal says, or nothing when `dst` already is the finished copy. The part is fsynced, checked as `verify` says and
/// renamed to `dst`: without replacing anything, unless `replace` (the user chose it; never when `dst` is the source itself under
/// another name). A finished `dst` is never opened for writing. On a failure the part is removed, except when the disk went away
/// or filled up (kept for a resume).
fn copy_file_part(job: &mut Job, src: &Path, dst: &Path, verify: Verify, replace: bool) -> io::Result<Ident> {
    let from = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(src)?;
    let sm = from.metadata()?;
    if !sm.file_type().is_file() {
        return Err(io::Error::other("not a regular file (changed while copying)"));
    }
    let part = part_name(dst, &job.tag);
    let pf = path_field(&part);
    let decision = match &job.resume {
        Some(rs) => {
            let dm = std::fs::symlink_metadata(dst).ok().filter(|m| m.file_type().is_file()).map(|m| meta_of(&m));
            let have = std::fs::symlink_metadata(&part).ok().filter(|m| m.file_type().is_file()).map(|m| m.len());
            leaf_copy::resume::decide(rs.started.get(&part).copied(), meta_of(&sm), have, dm)
        }
        None => Decision::Restart,
    };
    let start = match decision {
        Decision::Skip => {
            let _ = std::fs::remove_file(&part);
            if !verified(src, dst, sm.len(), verify)? {
                return Err(io::Error::other("a file of the same size and time is there but differs from the original: it was kept"));
            }
            job.progress(sm.len(), Some(src));
            job.record(&["G", &path_field(dst)], false);
            return Ok(Ident::of(&sm));
        }
        Decision::ResumeAt(at) => {
            log::info!("copy: carrying on {} at {at} bytes", dst.display());
            at
        }
        Decision::Restart => {
            // (a part of ours from an earlier try, or a crash: the tag makes the name ours)
            let _ = std::fs::remove_file(&part);
            0
        }
    };
    let to = if start > 0 {
        let f = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&part)?;
        f.set_len(start)?;
        f
    } else {
        let f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_CLOEXEC).open(&part)?;
        job.record(&["Y", &pf, &sm.len().to_string(), &sm.mtime().to_string(), &sm.mtime_nsec().to_string()], false);
        f
    };
    let r = (|| -> io::Result<()> {
        use std::io::Seek;
        (&from).seek(io::SeekFrom::Start(start))?;
        (&to).seek(io::SeekFrom::Start(start))?;
        job.progress(start, Some(src));
        let mut plain = false;
        let mut buf = Vec::new();
        let mut copied = start;
        let mut recorded = start;
        // a whole new copy on a file system that shares blocks (btrfs, XFS, bcachefs): one clone, no bytes moved
        // (not in tests: they cut copies off between chunks)
        let cloned = !cfg!(test) && start == 0 && sm.len() > 0 && reflink(&from, &to);
        if cloned {
            copied = sm.len();
            job.progress(copied, Some(src));
        }
        while !cloned {
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
                    Err(e) if copied == start && matches!(e.raw_os_error(), Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM)) => {
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
            if job.jr.is_some() && copied - recorded >= RECORD_EVERY {
                // the bytes first, then the record that promises them
                to.sync_data()?;
                recorded = copied;
                job.record(&["O", &pf, &copied.to_string()], false);
            }
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
        if !verified(src, &part, sm.len(), verify)? {
            return Err(io::Error::other("the copy differs from the original"));
        }
        if replace {
            // the old file must still not be the source itself (seen through another path)
            if std::fs::symlink_metadata(dst).is_ok_and(|m| same_item(&sm, &m)) {
                return Err(io::Error::other("the file to replace is the original itself: it was kept"));
            }
            std::fs::rename(&part, dst)
        } else {
            rename_noreplace(&part, dst)
        }
    })();
    drop(to);
    match r {
        Ok(()) => {
            job.record(&["G", &path_field(dst)], false);
            Ok(Ident::of(&sm))
        }
        Err(e) => {
            if !is_cut(&e) {
                let _ = std::fs::remove_file(&part);
            }
            Err(e)
        }
    }
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
            let was = if job.parts { copy_file_part(job, src, dst, verify, false)? } else { copy_file(job, src, dst, verify)? };
            if job.stay_on.is_some() {
                // (a move: the copy's name must be on disk before any source goes)
                if let Err(e) = sync_dir(parent_of(dst)) {
                    job.fail(src, format_args!("copied, but the copy could not be made durable, the original was kept: {e}"));
                    return Ok(false);
                }
            }
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
            if job.stay_on.is_some() && sync_dir(parent_of(dst)).is_err() {
                job.fail(src, "copied, but the copy could not be made durable, the original was kept");
                return Ok(false);
            }
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
            if let Some(dev) = job.stay_on {
                let mount = m.dev() != dev || cpath(src).is_ok_and(|c| crate::fastdelete::is_mount_root_at(libc::AT_FDCWD, &c, src));
                if mount {
                    job.fail(src, "a mount point: not moved");
                    return Ok(false);
                }
            }
            std::fs::DirBuilder::new().mode(0o700).create(dst)?;
            if job.stay_on.is_some() {
                sync_dir(parent_of(dst))?;
            }
            let mut all = true;
            for e in std::fs::read_dir(src)? {
                let e = e?;
                let s = e.path();
                match copy_tree(job, &s, &dst.join(e.file_name()), verify, copied, remove_now) {
                    Ok(c) => all &= c,
                    Err(e) if is_cancel(&e) => return Err(e),
                    Err(e) => {
                        job.fail_err(&s, e);
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
    // (a resumed job: the name "keep both" chose before the cut, carried on whatever is there now)
    let kept = job.resume.as_ref().and_then(|r| r.kept.get(src)).cloned();
    if let Some(k) = &kept {
        dst = k.clone();
    }
    if let Ok(dm) = std::fs::symlink_metadata(&dst) {
        let (sk, dk) = (kind(&sm), kind(&dm));
        if same_item(&sm, &dm) {
            // one item under two paths (hard link, bind mount, a share mounted twice): never replaced, never removed. A move has
            // nothing to do; a copy keeps both.
            if kind_of_job == Transfer::Move {
                return Ok(());
            }
            dst = keep_both_name(&dst);
            job.note_kept(src, &dst);
            return copy_new(job, kind_of_job, src, &sm, dst, false);
        }
        // (a decision exists for every conflict that was asked, and for a copy into the item's own folder: keep both, never a merge
        // of a folder with itself)
        let decision = if kept.is_some() { None } else { job.decide.get(src).copied() };
        if decision.is_none() && job.resume.is_some() && sk == dk && sk != Kind::Dir && finished_copy(src, &sm, &dst, &dm) {
            // a resumed job: this item was finished before the cut
            return already_copied(job, kind_of_job, src, &sm, &dst);
        }
        if sk == Kind::Dir && dk == Kind::Dir && decision.is_none() {
            // merge: each child on its own
            for e in std::fs::read_dir(src)? {
                let e = e?;
                match item(job, kind_of_job, &e.path(), &dst.join(e.file_name())) {
                    Err(e) if is_cancel(&e) => return Err(e),
                    Err(err) => job.fail_err(&e.path(), err),
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
            Resolve::KeepBoth => {
                dst = keep_both_name(&dst);
                job.note_kept(src, &dst);
            }
            // a file never replaces a folder, nor a folder a file
            Resolve::Replace if (sk == Kind::Dir) != (dk == Kind::Dir) => return Ok(()),
            Resolve::Replace => replace = true,
        }
    }
    copy_new(job, kind_of_job, src, &sm, dst, replace)
}

/// Whether `dst` is the finished copy of `src` (a resumed job): a file of the same size and time (the copy's time is set from the
/// original after its last byte, and its name only appears then), or a link to the same target.
fn finished_copy(src: &Path, sm: &std::fs::Metadata, dst: &Path, dm: &std::fs::Metadata) -> bool {
    match kind(sm) {
        Kind::File => meta_of(sm) == meta_of(dm),
        Kind::Link => std::fs::read_link(src).ok().is_some_and(|t| std::fs::read_link(dst).ok() == Some(t)),
        _ => false,
    }
}

/// A resumed job meets an item it finished before the cut: counted, checked (a move's at least quickly, before its original goes),
/// put in the results (an undo takes it back), and for a move the original removed, once the copy's name is durable.
fn already_copied(job: &mut Job, kind_of_job: Transfer, src: &Path, sm: &std::fs::Metadata, dst: &Path) -> io::Result<()> {
    let verify = if kind_of_job == Transfer::Move && job.verify == Verify::Off { Verify::Quick } else { job.verify };
    if kind(sm) == Kind::File && !verified(src, dst, sm.len(), verify)? {
        return Err(io::Error::other("a file of the same size and time is there but differs from the original: both were kept"));
    }
    if kind(sm) == Kind::File {
        // (a part of ours left for this name by the cut: the finished copy wins)
        let part = part_name(dst, &job.tag);
        if std::fs::symlink_metadata(&part).is_ok_and(|m| m.file_type().is_file()) {
            let _ = std::fs::remove_file(&part);
        }
    }
    job.progress(if kind(sm) == Kind::File { sm.len() } else { 0 }, Some(src));
    job.tracker.results.lock().unwrap().push((src.to_path_buf(), dst.to_path_buf()));
    if kind_of_job == Transfer::Move {
        sync_dir(parent_of(dst))?;
        remove_source_file(job, src, &Ident::of(sm));
    }
    Ok(())
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
    if replace && job.parts && kind(sm) == Kind::File {
        // a file the user chose to replace: its part file takes the old one's place after the last byte (the old one stays as it
        // was until then, and whole if anything fails)
        let was = copy_file_part(job, src, &dst, verify, true)?;
        job.tracker.results.lock().unwrap().push((src.to_path_buf(), dst.clone()));
        if kind_of_job == Transfer::Move {
            sync_dir(parent_of(&dst))?;
            remove_source_file(job, src, &was);
        }
        return Ok(());
    }
    let target = if replace { job.temp_name(parent) } else { dst.clone() };
    let mut copied = Copied::default();
    // (a replace removes the source only after the new file took the old one's place, below)
    let remove_now = kind_of_job == Transfer::Move && !replace;
    job.stay_on = (kind_of_job == Transfer::Move).then(|| sm.dev());
    let r = copy_tree(job, src, &target, verify, &mut copied, remove_now);
    if replace {
        match &r {
            Ok(true) => {
                // the old file must still not be the source itself (seen through another path)
                if std::fs::symlink_metadata(&dst).is_ok_and(|dm| same_item(sm, &dm)) {
                    let _ = crate::fastdelete::remove_tree(&target);
                    return Ok(());
                }
                std::fs::rename(&target, &dst)?;
                if kind_of_job == Transfer::Move {
                    sync_dir(parent_of(&dst))?;
                }
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
    run(kind, sources, dest_dir, tracker, false, Start::New(JOURNAL_DIR.get().cloned()))
}

/// How a run begins: a new job (journaled in this folder, if any), or a cut-off one carried on with its journal and answers.
enum Start {
    New(Option<PathBuf>),
    Resumed(Jr, ResumeState, HashMap<PathBuf, Resolve>),
}

fn answer_code(r: Resolve) -> &'static str {
    match r {
        Resolve::Replace => "r",
        Resolve::Skip => "s",
        Resolve::KeepBoth => "k",
    }
}

fn run(kind: Transfer, sources: &[PathBuf], dest_dir: &Path, tracker: Arc<Tracker>, force_copy: bool, start: Start) -> anyhow::Result<bool> {
    let resumed = matches!(start, Start::Resumed(..));
    anyhow::ensure!(std::fs::metadata(dest_dir).is_ok_and(|m| m.is_dir()), "{}: not a folder", dest_dir.display());
    let dest_abs = std::fs::canonicalize(dest_dir)?;
    // the destination folder and every folder above it, by device and inode (a bind mount or a second mount of the same share is
    // the same folder under another path)
    let dest_chain: Vec<(u64, u64)> = dest_abs.ancestors().filter_map(|a| std::fs::metadata(a).ok()).map(|m| (m.dev(), m.ino())).collect();
    let mut jobs = Vec::new();
    for s in sources {
        let name = s.file_name().ok_or_else(|| anyhow::anyhow!("{}: not an item that can be copied", s.display()))?;
        let m = match std::fs::symlink_metadata(s) {
            Ok(m) => m,
            // (a resumed move: this one went before the cut)
            Err(_) if resumed => continue,
            Err(e) => anyhow::bail!("{}: {e}", s.display()),
        };
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
    let (mut decide, jr, rs) = match start {
        // (a resumed job is not asked again: its answers are the journal's)
        Start::Resumed(jr, rs, decide) => (decide, Some(jr), Some(rs)),
        Start::New(dir) => {
            let mut decide = HashMap::new();
            if !conflicts.is_empty() {
                if let Some(ask) = &tracker.ask {
                    let Some(answers) = ask(&conflicts) else { return Ok(false) };
                    for (c, r) in conflicts.iter().zip(answers) {
                        decide.insert(c.src.clone(), r);
                    }
                }
            }
            let jr = dir.and_then(|d| journal_start(&d, kind, tracker.verify_mode(), sources, dest_dir, &decide));
            (decide, jr, None)
        }
    };
    for (s, _, own) in &jobs {
        if *own {
            decide.insert(s.clone(), Resolve::KeepBoth);
        }
    }
    let mut job = Job::new(&tracker, tracker.verify_mode(), force_copy, None);
    job.total = total;
    job.decide = decide;
    job.parts = true;
    if let Some(stem) = jr.as_ref().and_then(|j| j.path.file_stem()) {
        job.tag = stem.to_string_lossy().into_owned();
    }
    job.jr = jr;
    job.resume = rs;
    job.progress(0, None);
    let mut cancelled = false;
    for (s, d, _) in &jobs {
        match item(&mut job, kind, s, d) {
            Err(e) if is_cancel(&e) => {
                cancelled = true;
                break;
            }
            Err(e) => job.fail_err(s, e),
            Ok(()) => {}
        }
    }
    if let Some(mut j) = job.jr.take() {
        if job.cut && !cancelled {
            // the disk went away or filled up: the job stays in the journal folder and is offered again at the next start
            let _ = j.w.sync();
            log::info!("copy: cut off, resumable: {}", j.path.display());
        } else {
            // done, failed or cancelled by the user: nothing to resume
            drop(j.w);
            let _ = std::fs::remove_file(&j.path);
        }
    }
    if cancelled {
        return Ok(false);
    }
    let mut problems = Vec::new();
    if job.failed_count > 0 {
        problems.push(format!("{} item(s) failed: {}", job.failed_count, job.failed.join("; ")));
    }
    if !job.skipped.is_empty() {
        problems.push(format!("not copied (pipes, sockets, devices): {}", job.skipped.join(", ")));
    }
    if job.cut {
        problems.push("the disk went away or is full: the rest can be carried on from the next start".to_string());
    }
    anyhow::ensure!(problems.is_empty(), "{}", problems.join(". "));
    Ok(true)
}

/// Moves one item to `dst` (which must not exist) across devices, all or nothing: copied, fsynced and checked; when every part was
/// copied, the source is removed; otherwise the partial copy is removed and the source stays whole. (The trash's "Move to home
/// trash" and the restore of such an item.)
pub(crate) fn move_across(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let tracker = Tracker::new(Box::new(|_, _, _| {}), Default::default());
    let mut job = Job::new(&tracker, Verify::Quick, true, std::fs::symlink_metadata(src).ok().map(|m| m.dev()));
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

/// One regular file copied to `dst` (which must not exist: `O_EXCL`, a taken name is `AlreadyExists` and is not touched), fsynced,
/// checked (the size, the first and the last MiB read back) and its name made durable in its folder. The source is opened without
/// following a link and must be a regular file. The answer is the source as it was when it was opened, for the caller to compare
/// before it removes anything. On any failure after `dst` was made, the partial `dst` is removed. (The organizer's move to another
/// device; nothing here removes a source.)
pub(crate) fn copy_one_file(src: &Path, dst: &Path) -> io::Result<Ident> {
    let tracker = Tracker::new(Box::new(|_, _, _| {}), Default::default());
    let mut job = Job::new(&tracker, Verify::Quick, true, None);
    let was = copy_file(&mut job, src, dst, Verify::Quick)?;
    if let Err(e) = sync_dir(parent_of(dst)) {
        let _ = std::fs::remove_file(dst);
        return Err(e);
    }
    Ok(was)
}

/// Where journals live; set once at startup. Without it nothing is journaled (and nothing can be resumed).
pub fn set_journal_dir(dir: PathBuf) {
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
    let _ = JOURNAL_DIR.set(dir);
}

/// A new job's journal in `dir`: the job and its answers, synced before anything is copied. `None` (the job runs unjournaled) when
/// it can't be written.
fn journal_start(dir: &Path, kind: Transfer, verify: Verify, sources: &[PathBuf], dest: &Path, decide: &HashMap<PathBuf, Resolve>) -> Option<Jr> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let path = dir.join(format!("{}-{stamp}-{}.jrn", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let mut w = jrn::Writer::create(&path).map_err(|e| log::warn!("journal: {}: {e}", path.display())).ok()?;
    let written = (|| -> io::Result<()> {
        let mut head = vec!["B".to_string(), (if kind == Transfer::Move { "m" } else { "c" }).into(), "0".into(), (verify as u8).to_string(), path_field(dest)];
        head.extend(sources.iter().map(|x| path_field(x)));
        w.record(&head.iter().map(String::as_str).collect::<Vec<_>>())?;
        for (src, r) in decide {
            w.record(&["X", answer_code(*r), &path_field(src)])?;
        }
        w.record(&["P"])?;
        w.sync()
    })();
    if let Err(e) = written {
        log::warn!("journal: {}: {e}", path.display());
        drop(w);
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(Jr { w, last_sync: Instant::now(), path })
}

struct Loaded {
    info: Interrupted,
    records: Vec<Vec<String>>,
}

fn load(path: &Path) -> Option<Loaded> {
    let records = jrn::read(path).ok()?;
    let head = records.first().filter(|r| r.len() >= 5 && r[0] == "B")?;
    if !records.iter().any(|r| r[0] == "P") {
        return None;
    }
    let kind = if head[1] == "m" { Transfer::Move } else { Transfer::Copy };
    let verify = match head[3].as_str() {
        "1" => Verify::Quick,
        "2" => Verify::Full,
        _ => Verify::Off,
    };
    let sources = head[5..].iter().map(|f| field_path(f)).collect::<Option<Vec<_>>>()?;
    let dest = field_path(&head[4])?;
    // (this engine walks as it copies: the files known are the ones it began)
    let count = |t: &str| records.iter().filter(|r| r[0] == t && r.len() >= 2).map(|r| r[1].as_str()).collect::<HashSet<_>>().len();
    let files = count("Y").max(count("G"));
    let done = count("G");
    Some(Loaded { info: Interrupted { journal: path.to_path_buf(), kind, verify, sources, dest, files, done }, records })
}

/// Jobs left unfinished by an earlier run. Journals that never got past planning (nothing was copied) are removed; journals still
/// open (a running job, maybe in another window's process) are left alone.
pub fn interrupted() -> Vec<Interrupted> {
    let Some(dir) = JOURNAL_DIR.get() else { return Vec::new() };
    interrupted_in(dir)
}

fn interrupted_in(dir: &Path) -> Vec<Interrupted> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jrn") || jrn::in_use(&path) {
            continue;
        }
        match load(&path) {
            Some(l) => out.push(l.info),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    out.sort_by(|a, b| a.journal.cmp(&b.journal));
    out
}

/// Forget an interrupted job: what it finished stays; its part files (named with its tag, hidden) go.
pub fn discard(journal: &Path) {
    if jrn::in_use(journal) {
        return;
    }
    if let Ok(records) = jrn::read(journal) {
        for r in records.iter().filter(|r| r[0] == "Y" && r.len() >= 2) {
            if let Some(p) = field_path(&r[1]).filter(|p| p.extension().is_some_and(|x| x == "rfpart")) {
                if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file()) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    let _ = std::fs::remove_file(journal);
}

/// Carries an interrupted job on with its original answers: no conflict is asked again. Items finished before the cut are checked
/// against their sources and kept (a move then removes their originals); a part file goes on from its recorded offset when it and
/// the source are as the journal left them, else that file is copied again; moves remove an original only after its copy is
/// complete, fsynced and checked.
pub fn resume(journal: &Path, tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    let Some(l) = load(journal) else {
        discard(journal);
        return Ok(true);
    };
    let mut rs = ResumeState::default();
    let mut decide = HashMap::new();
    let num = |s: &str| s.parse::<i64>().ok();
    for r in &l.records {
        match (r[0].as_str(), r.len()) {
            ("X", 3) => {
                let a = match r[1].as_str() {
                    "r" => Resolve::Replace,
                    "s" => Resolve::Skip,
                    _ => Resolve::KeepBoth,
                };
                if let Some(p) = field_path(&r[2]) {
                    decide.insert(p, a);
                }
            }
            ("K", 3) => {
                if let (Some(s), Some(d)) = (field_path(&r[1]), field_path(&r[2])) {
                    rs.kept.insert(s, d);
                }
            }
            ("Y", 5) => {
                if let (Some(p), Some(size), Some(s), Some(ns)) = (field_path(&r[1]), r[2].parse::<u64>().ok(), num(&r[3]), num(&r[4])) {
                    rs.started.insert(p, Started { src: Meta { size, mtime: (s, ns) }, offset: 0 });
                }
            }
            ("O", 3) => {
                if let (Some(p), Some(off)) = (field_path(&r[1]), r[2].parse::<u64>().ok()) {
                    if let Some(st) = rs.started.get_mut(&p) {
                        st.offset = off;
                    }
                }
            }
            _ => {}
        }
    }
    tracker.set_verify(l.info.verify);
    // a clean journal (no torn tail) to go on recording into
    let w = jrn::rewrite(journal, &l.records)?;
    let jr = Jr { w, last_sync: Instant::now(), path: journal.to_path_buf() };
    run(l.info.kind, &l.info.sources, &l.info.dest, tracker, false, Start::Resumed(jr, rs, decide))
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
            assert!(!run(kind, &[root.join("src")], &dest, t, force, Start::New(None)).unwrap(), "cancelled");
            if force {
                // a move removes each source right after its own copy: every item is whole in exactly one of the two places
                let one = |rel: &str| {
                    let (s, d) = (std::fs::symlink_metadata(root.join(rel)).is_ok(), std::fs::symlink_metadata(dest.join(rel)).is_ok());
                    assert!(s != d, "{rel}: in exactly one place");
                };
                one("src/a.bin");
                one("src/sub/b.txt");
                one("src/link");
                if let Ok(b) = std::fs::read(root.join("src/a.bin")) {
                    assert_eq!(b, vec![7u8; 9 << 20]);
                }
                assert_eq!(std::fs::read(root.join("outside/o.txt")).unwrap(), b"o");
                // put back what went, for the next steps
                for rel in ["src/sub/b.txt", "src/link"] {
                    if std::fs::symlink_metadata(root.join(rel)).is_err() {
                        std::fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
                        std::fs::rename(dest.join(rel), root.join(rel)).unwrap();
                    }
                }
            }
            intact(&root);
            assert!(std::fs::symlink_metadata(dest.join("src/a.bin")).is_err(), "the partly written file is removed");
            std::fs::remove_dir_all(dest.join("src")).unwrap();
        }
        // a whole copy: links copied as links, nothing of the outside folder comes along; existing names are kept, not replaced
        std::fs::write(dest.join("a.bin"), b"old").unwrap();
        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        assert!(run(Transfer::Copy, &[root.join("src"), root.join("src/a.bin")], &dest, t.clone(), false, Start::New(None)).unwrap());
        assert_eq!(std::fs::read_link(dest.join("src/link")).unwrap(), root.join("outside"));
        assert_eq!(std::fs::read(dest.join("a.bin")).unwrap(), b"old", "not replaced without being asked");
        assert_eq!(std::fs::read(dest.join("a (2).bin")).unwrap().len(), 9 << 20);
        assert_eq!(t.results.lock().unwrap().len(), 2);
        intact(&root);
        // a move across devices that completes: the source goes, the copy is whole
        let t = Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()));
        let moved = root.join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        assert!(run(Transfer::Move, &[root.join("src")], &moved, t, true, Start::New(None)).unwrap());
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
            let _ = run(Transfer::Move, &[root.join("a/x.txt")], &root.join("b"), Arc::new(t), force, Start::New(None));
            assert_eq!(std::fs::read(root.join("a/x.txt")).unwrap(), b"only copy");
            assert_eq!(std::fs::read(root.join("b/x.txt")).unwrap(), b"only copy");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    // ------------------------------------------------ the journal and resume

    fn quiet() -> Arc<Tracker> {
        Arc::new(Tracker::new(Box::new(|_, _, _| {}), Default::default()))
    }

    /// A temp tree with a journal folder.
    fn fresh(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("leaf-unix-resume-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("dest")).unwrap();
        std::fs::create_dir_all(root.join("jobs")).unwrap();
        (root.clone(), root.join("jobs"))
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3] ^ seed).collect()
    }

    /// Runs a copy of `sources` into `dest` that dies (a panic, as a crash: nothing is cleaned up) at the second progress report of
    /// the file named `die_in`, after its first chunk was written and recorded. Returns the journal left behind.
    fn copy_and_die(sources: &[PathBuf], dest: &Path, jobs: &Path, die_in: &str) -> PathBuf {
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (seen2, die) = (seen.clone(), die_in.to_string());
        let t = Arc::new(Tracker::new(
            Box::new(move |_, _, name| {
                if name.as_deref() == Some(die.as_str()) && seen2.fetch_add(1, Ordering::Relaxed) >= 1 {
                    panic!("the app was killed");
                }
            }),
            Default::default(),
        ));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(Transfer::Copy, sources, dest, t, false, Start::New(Some(jobs.to_path_buf())))));
        assert!(r.is_err(), "the copy died midway");
        let left = interrupted_in(jobs);
        assert_eq!(left.len(), 1, "the cut-off job is offered");
        left[0].journal.clone()
    }

    fn parts_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rfpart")).collect()
    }

    #[test]
    fn a_copy_killed_midway_resumes_byte_identical() {
        let (root, jobs) = fresh("killed");
        let big = pattern(9 << 20, 1);
        std::fs::write(root.join("src/big.bin"), &big).unwrap();
        let journal = copy_and_die(&[root.join("src/big.bin")], &root.join("dest"), &jobs, "big.bin");
        let parts = parts_in(&root.join("dest"));
        assert_eq!(parts.len(), 1, "the part file stays after a crash");
        assert!(std::fs::symlink_metadata(root.join("dest/big.bin")).is_err(), "no half file under the real name");
        let part_ino = std::fs::metadata(&parts[0]).unwrap().ino();
        assert!(std::fs::metadata(&parts[0]).unwrap().len() >= RECORD_EVERY);
        assert!(resume(&journal, quiet()).unwrap());
        assert_eq!(std::fs::read(root.join("dest/big.bin")).unwrap(), big, "byte-identical");
        assert_eq!(std::fs::metadata(root.join("dest/big.bin")).unwrap().ino(), part_ino, "carried on in the same part file, not started over");
        assert!(parts_in(&root.join("dest")).is_empty());
        assert!(!journal.exists(), "a finished job's journal is gone");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_resumed_copy_never_truncates_a_complete_destination_file() {
        let (root, jobs) = fresh("complete");
        let (a, b) = (pattern(2 << 20, 2), pattern(9 << 20, 3));
        std::fs::write(root.join("src/a.bin"), &a).unwrap();
        std::fs::write(root.join("src/b.bin"), &b).unwrap();
        let journal = copy_and_die(&[root.join("src/a.bin"), root.join("src/b.bin")], &root.join("dest"), &jobs, "b.bin");
        let done = root.join("dest/a.bin");
        assert_eq!(std::fs::read(&done).unwrap(), a, "the first file was finished before the cut");
        let ino = std::fs::metadata(&done).unwrap().ino();
        // a stale part for the finished file (as if its rename record were lost): it must not be carried over the finished copy
        let tag = journal.file_stem().unwrap().to_string_lossy().into_owned();
        std::fs::write(part_name(&done, &tag), b"stale").unwrap();
        assert!(resume(&journal, quiet()).unwrap());
        assert_eq!(std::fs::read(&done).unwrap(), a, "never truncated nor rewritten");
        assert_eq!(std::fs::metadata(&done).unwrap().ino(), ino, "the same file, left alone");
        assert_eq!(std::fs::read(root.join("dest/b.bin")).unwrap(), b);
        assert!(std::fs::symlink_metadata(root.join("dest/a (2).bin")).is_err(), "not taken for a conflict");
        assert!(parts_in(&root.join("dest")).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_changed_source_restarts_the_file() {
        let (root, jobs) = fresh("changed");
        std::fs::write(root.join("src/big.bin"), pattern(9 << 20, 4)).unwrap();
        let journal = copy_and_die(&[root.join("src/big.bin")], &root.join("dest"), &jobs, "big.bin");
        assert_eq!(parts_in(&root.join("dest")).len(), 1);
        // the same size, other bytes and another time
        let changed = pattern(9 << 20, 5);
        std::fs::write(root.join("src/big.bin"), &changed).unwrap();
        let f = std::fs::File::options().write(true).open(root.join("src/big.bin")).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(60)).unwrap();
        drop(f);
        assert!(resume(&journal, quiet()).unwrap());
        // (carried on, the first chunk would still hold the old bytes)
        assert_eq!(std::fs::read(root.join("dest/big.bin")).unwrap(), changed, "the source as it is now, copied again from the start");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_move_does_not_delete_the_source_when_the_destination_write_fails() {
        use std::os::unix::fs::PermissionsExt;
        let (root, jobs) = fresh("movefail");
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/sub/a.bin"), pattern(3 << 20, 6)).unwrap();
        std::fs::write(root.join("src/b.txt"), b"b").unwrap();
        // the destination folder exists (merged) and refuses new files
        std::fs::create_dir_all(root.join("dest/src/sub")).unwrap();
        std::fs::set_permissions(root.join("dest/src/sub"), std::fs::Permissions::from_mode(0o500)).unwrap();
        let r = run(Transfer::Move, &[root.join("src")], &root.join("dest"), quiet(), true, Start::New(Some(jobs.clone())));
        std::fs::set_permissions(root.join("dest/src/sub"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(r.is_err(), "the failed file is named");
        assert_eq!(std::fs::read(root.join("src/sub/a.bin")).unwrap(), pattern(3 << 20, 6), "the original stays whole");
        assert!(std::fs::symlink_metadata(root.join("dest/src/sub/a.bin")).is_err());
        assert!(parts_in(&root.join("dest/src/sub")).is_empty());
        // what could be written was moved: its original went only after its copy was complete
        assert_eq!(std::fs::read(root.join("dest/src/b.txt")).unwrap(), b"b");
        assert!(std::fs::symlink_metadata(root.join("src/b.txt")).is_err());
        assert!(interrupted_in(&jobs).is_empty(), "a plain failure is not offered for resuming");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_symlink_is_copied_as_a_link() {
        let (root, jobs) = fresh("link");
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/o.txt"), b"o").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("src/dir-link")).unwrap();
        std::os::unix::fs::symlink(root.join("outside/o.txt"), root.join("src/file-link")).unwrap();
        assert!(run(Transfer::Copy, &[root.join("src")], &root.join("dest"), quiet(), false, Start::New(Some(jobs))).unwrap());
        for (l, to) in [("dir-link", root.join("outside")), ("file-link", root.join("outside/o.txt"))] {
            let d = root.join("dest/src").join(l);
            assert!(std::fs::symlink_metadata(&d).unwrap().file_type().is_symlink(), "{l}: a link");
            assert_eq!(std::fs::read_link(&d).unwrap(), to);
        }
        assert_eq!(std::fs::read(root.join("outside/o.txt")).unwrap(), b"o", "nothing written through a link");
        assert_eq!(std::fs::read_dir(root.join("outside")).unwrap().count(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn a_copy_without_rename_flags_claims_the_name_and_never_replaces() {
        let (root, jobs) = fresh("norename2");
        NO_RENAME2.with(|f| f.set(true));
        std::fs::write(root.join("src/a.bin"), pattern(3 << 20, 8)).unwrap();
        std::fs::write(root.join("src/b.txt"), b"new").unwrap();
        std::fs::write(root.join("dest/b.txt"), b"old").unwrap();
        let r = run(Transfer::Copy, &[root.join("src/a.bin"), root.join("src/b.txt")], &root.join("dest"), quiet(), false, Start::New(Some(jobs)));
        assert!(r.unwrap());
        assert_eq!(std::fs::read(root.join("dest/a.bin")).unwrap(), pattern(3 << 20, 8), "copied through the placeholder");
        assert_eq!(std::fs::read(root.join("dest/b.txt")).unwrap(), b"old", "not replaced");
        assert_eq!(std::fs::read(root.join("dest/b (2).txt")).unwrap(), b"new");
        assert!(parts_in(&root.join("dest")).is_empty());
        // a name that is taken: refused, both left as they were
        std::fs::write(root.join("p"), b"part").unwrap();
        let e = rename_noreplace(&root.join("p"), &root.join("dest/b.txt")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(root.join("dest/b.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(root.join("p")).unwrap(), b"part");
        NO_RENAME2.with(|f| f.set(false));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
