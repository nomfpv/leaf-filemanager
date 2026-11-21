//! Linux backend of rf_win::organize. Status: PARTIAL (facts are real: scan, facts_for_names, facts_for_paths, exists,
//! folder_problem; the runner is not yet)
//!
//! Facts (read only, nothing is ever written): one `lstat` per item, links are never followed nor entered, names that are not
//! UTF-8 are left out (a step for a lossy name could never find its file), only regular files become facts (FIFOs, sockets and
//! device nodes are not files to organize). Hidden means a leading dot, for files and for the folders a scan goes into. The EXIF
//! date (`taken_ms`) is not read here, as in rf-win: the caller fills it through the portable code when a step asks for it.
//!
//! The runner moves and recycles files: a data operation, so no run ever reports success while it is a stub. Every step comes back
//! as a failure ("Not available on this system yet") and the progress callback is never called.

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use leaf_common::stub;
use leaf_common::stub::Unsupported;
use rf_core::organize::safety::{ProtectedPaths, is_inside, is_partial_download, norm};
use rf_core::organize::{Dest, FileFact, FileKind, Step};
use rf_core::{Entry, EntryKind, attrs};

/// Marks a user-facing text (the app translates it), as in rf-win.
fn tx(s: &'static str) -> &'static str {
    s
}

/// A file that changed more recently than this is not touched by an automatic run (rf-win organize.rs:38).
pub const SETTLE_MS: i64 = 10_000;
/// At most this many steps in a run the watcher starts (rf-win organize.rs:40).
pub const AUTO_CAP: usize = 500;

// TYPE-COPY(rf-win organize.rs:44)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Gone,
    NotAFile,
    Hidden,
    Partial,
    OutsideRoot,
    Link,
    InUse,
    TooYoung,
    NoRecycleBin,
    BinRefuses,
    Changed,
}

impl Why {
    // (rf-win's texts, word for word: the app's translation table has them)
    pub fn text(self) -> &'static str {
        match self {
            Why::Gone => tx("The file is gone"),
            Why::NotAFile => tx("Not a file"),
            Why::Hidden => tx("Hidden or system file"),
            Why::Partial => tx("Still being written (partial download)"),
            Why::OutsideRoot => tx("Outside the watched folder"),
            Why::Link => tx("A link (symbolic) file"),
            Why::InUse => tx("Another app is using it"),
            Why::TooYoung => tx("Changed a moment ago: waiting a little"),
            Why::NoRecycleBin => tx("This drive has no Recycle Bin"),
            Why::BinRefuses => tx("The Recycle Bin is off or too small for this file"),
            Why::Changed => tx("Changed since it was planned"),
        }
    }

    // PORTABLE? (copied: pure)
    pub fn retry_later(self) -> bool {
        matches!(self, Why::InUse | Why::TooYoung | Why::Changed)
    }
}

// TYPE-COPY(rf-win organize.rs:86)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    pub path: PathBuf,
    pub why: Why,
}

// TYPE-COPY(rf-win organize.rs:92)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub path: PathBuf,
    pub message: String,
}

// TYPE-COPY(rf-win organize.rs:99)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub token: u64,
    pub moves: Vec<(PathBuf, PathBuf)>,
    pub recycled: Vec<(PathBuf, PathBuf)>,
    pub created_dirs: Vec<PathBuf>,
}

impl Record {
    // PORTABLE? (copied: pure)
    pub fn count(&self) -> usize {
        self.moves.len() + self.recycled.len()
    }
}

// TYPE-COPY(rf-win organize.rs:117)
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub record: Record,
    pub failed: Vec<Failure>,
    pub skipped: Vec<Skip>,
    pub deferred_by_cap: usize,
    pub cancelled: bool,
}

impl Report {
    // PORTABLE? (copied: pure)
    pub fn retry_later(&self) -> Vec<PathBuf> {
        self.skipped.iter().filter(|s| s.why.retry_later()).map(|s| s.path.clone()).collect()
    }
}

// TYPE-COPY(rf-win organize.rs:134)
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    pub automatic: bool,
    pub cap: usize,
    pub now_ms: i64,
}

impl RunOptions {
    // PORTABLE? (copied: plain data)
    pub fn manual(now_ms: i64) -> Self {
        Self { automatic: false, cap: usize::MAX, now_ms }
    }

    // PORTABLE? (copied: plain data)
    pub fn automatic(now_ms: i64) -> Self {
        Self { automatic: true, cap: AUTO_CAP, now_ms }
    }
}

/// Sends files to the Recycle Bin, and only there.
pub trait Recycler {
    fn can_recycle(&self, path: &Path) -> bool;
    fn bin_takes(&self, _path: &Path) -> bool {
        true
    }
    fn recycle(&self, paths: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, PathBuf)>>;
}

/// The system's bin: none here yet, so nothing can go there.
// TYPE-COPY(rf-win organize.rs:168)
pub struct ShellRecycler {
    pub tracker: Arc<crate::fileops::Tracker>,
}

impl Recycler for ShellRecycler {
    fn can_recycle(&self, _path: &Path) -> bool {
        stub!("organize::ShellRecycler::can_recycle")
    }

    fn bin_takes(&self, _path: &Path) -> bool {
        stub!("organize::ShellRecycler::bin_takes")
    }

    fn recycle(&self, _paths: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
        leaf_common::stub_err!("organize::ShellRecycler::recycle")
    }
}

/// Every step of `paths` as a failure: "not available" (a data operation never reports success).
fn unavailable(name: &'static str, paths: impl Iterator<Item = PathBuf>) -> Vec<Failure> {
    let message = Unsupported(name).to_string();
    paths.map(|path| Failure { path, message: message.clone() }).collect()
}

// ---------------------------------------------------------------- the moves (the only code here that changes files)

/// Why one move did not happen. Whatever the reason, the file is whole at its old path (or, for `Placed`, whole at the new one).
#[derive(Debug)]
enum MoveErr {
    /// The name (and, when numbered, every numbered name) is taken: nothing was replaced.
    Taken,
    /// The source is not the file that was copied any more (saved or replaced meanwhile): it stays, the copy was taken back.
    Changed,
    Io(std::io::Error),
}

impl std::fmt::Display for MoveErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveErr::Taken => f.write_str(tx("The name is already taken at the destination")),
            MoveErr::Changed => f.write_str(tx("Changed during the move: the original was kept")),
            MoveErr::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for MoveErr {
    fn from(e: std::io::Error) -> Self {
        MoveErr::Io(e)
    }
}

/// The names a move may use: `target` itself, then (when `numbered`) `name (2).ext` … `name (60).ext` beside it, as rf-win.
fn candidates(target: &Path, numbered: bool) -> Vec<PathBuf> {
    let mut out = vec![target.to_path_buf()];
    if numbered {
        let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.extend((2..=60u32).map(|n| dir.join(rf_core::organize::numbered_name(&name, n))));
    }
    out
}

static COPY_TOKENS: AtomicU64 = AtomicU64::new(1);

/// Moves the regular file `src` to `target` (or, `numbered`, to the first free numbered name beside it) and answers where it went.
/// Never replaces anything: on the same device each name is tried with a no-replace rename (`renameat2(RENAME_NOREPLACE)`, or
/// link-then-unlink where the file system lacks the flag); across devices see [`move_by_copy`].
fn move_file(src: &Path, target: &Path, numbered: bool) -> Result<PathBuf, MoveErr> {
    for cand in candidates(target, numbered) {
        match crate::fileops::rename_no_replace(src, &cand) {
            Ok(()) => return Ok(cand),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => return move_by_copy(src, target, numbered),
            Err(e) => return Err(e.into()),
        }
    }
    Err(MoveErr::Taken)
}

/// A move to another device, for one regular file:
/// 1. copied to a name of its own in the destination folder (`.leaf-moving-<pid>-<n>.part`: hidden, a partial-download name every
///    organizer pass ignores), with `O_EXCL`, fsynced and checked, its name made durable;
/// 2. the source compared with what was copied (device, inode, size, mtime, ctime): a source saved meanwhile keeps its place and the
///    copy goes;
/// 3. the copy renamed to the first free name without replacing (same folder, so the same device);
/// 4. the folder fsynced, the source compared once more, and only then unlinked. If it cannot be unlinked, or changed, the copy is
///    taken back (only the very inode this function made) and the file stays where it was.
///
/// A crash at any point leaves the source whole; at worst a hidden `.part` copy or a second copy beside it, never no copy at all.
fn move_by_copy(src: &Path, target: &Path, numbered: bool) -> Result<PathBuf, MoveErr> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // 1. the copy, under a name of its own
    let mut made = None;
    for _ in 0..8 {
        let tmp = dir.join(format!(".leaf-moving-{}-{}.part", std::process::id(), COPY_TOKENS.fetch_add(1, Ordering::Relaxed)));
        match crate::fastcopy::copy_one_file(src, &tmp) {
            Ok(was) => {
                made = Some((tmp, was));
                break;
            }
            // (somebody holds that name: another one, never theirs touched)
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let Some((tmp, was)) = made else { return Err(MoveErr::Taken) };
    let copy_id = match std::fs::symlink_metadata(&tmp) {
        Ok(m) => (m.dev(), m.ino()),
        Err(e) => return Err(e.into()),
    };
    // 2. still the file that was copied?
    if !still(src, &was) {
        remove_own_file(&tmp, copy_id);
        return Err(MoveErr::Changed);
    }
    // 3. into place, never replacing
    let mut placed = None;
    for cand in candidates(target, numbered) {
        match crate::fileops::rename_no_replace(&tmp, &cand) {
            Ok(()) => {
                placed = Some(cand);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                remove_own_file(&tmp, copy_id);
                return Err(e.into());
            }
        }
    }
    let Some(at) = placed else {
        remove_own_file(&tmp, copy_id);
        return Err(MoveErr::Taken);
    };
    // 4. durable first, then the source (only while it is still what was copied)
    if let Err(e) = sync_dir(dir) {
        remove_own_file(&at, copy_id);
        return Err(e.into());
    }
    if !still(src, &was) {
        remove_own_file(&at, copy_id);
        return Err(MoveErr::Changed);
    }
    if let Err(e) = remove_source(src) {
        remove_own_file(&at, copy_id);
        return Err(e.into());
    }
    if let Some(p) = src.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = sync_dir(p);
    }
    Ok(at)
}

/// Is `p` (not followed) still the very file `was` describes?
fn still(p: &Path, was: &crate::fastcopy::Ident) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| crate::fastcopy::Ident::of(&m) == *was)
}

fn sync_dir(p: &Path) -> std::io::Result<()> {
    std::fs::File::open(p)?.sync_all()
}

/// Removes the source of a move once its copy is in place and durable. The one call that takes a user's file away: only ever after
/// the same bytes are at their new name, and only after [`still`] said it is the file that was copied.
fn remove_source(from: &Path) -> std::io::Result<()> {
    std::fs::remove_file(from)
}

/// Removes a copy this module made itself a moment ago, and only while `p` is still that copy (`(dev, ino)`).
fn remove_own_file(p: &Path, id: (u64, u64)) {
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && (m.dev(), m.ino()) == id) {
        let _ = std::fs::remove_file(p);
    }
}

/// Removes a folder, but only while it is empty (`rmdir` fails otherwise, and on a link): the folders a run made itself.
fn remove_empty_dir(p: &Path) {
    let _ = std::fs::remove_dir(p);
}

/// `..` or `.` inside a path: never followed by a step (the lexical "inside the root" check could be fooled by it).
fn has_dots(p: &Path) -> bool {
    p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// Can the file be taken now, or does another program have it open for writing (a download, a save in progress)? A read lease
/// (`F_SETLEASE`, `F_RDLCK`) is refused with `EAGAIN` exactly while some process has the file open for writing; it is taken and given
/// back at once. Where a lease cannot be had at all (not the owner, a network or FUSE file system) nobody can tell: free.
fn free_to_take(path: &Path) -> bool {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) {
        Ok(f) => f,
        // (a lease another program holds, Samba's say)
        Err(e) if e.raw_os_error() == Some(libc::EWOULDBLOCK) => return false,
        Err(_) => return true,
    };
    // SAFETY: fcntl on a descriptor we hold, with an integer argument.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLEASE, libc::F_RDLCK) };
    if r == 0 {
        // SAFETY: as above.
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLEASE, libc::F_UNLCK) };
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN)
}

/// The checks every step passes just before it runs (the plan may be minutes old). Not followed: a link is refused as a link.
fn inspect(step: &Step, opts: &RunOptions) -> Result<(), Why> {
    let src = &step.source;
    if has_dots(src) || has_dots(&step.root) {
        return Err(Why::OutsideRoot);
    }
    let meta = std::fs::symlink_metadata(src).map_err(|_| Why::Gone)?;
    if meta.file_type().is_symlink() {
        return Err(Why::Link);
    }
    if !meta.file_type().is_file() {
        return Err(Why::NotAFile);
    }
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if is_hidden_name(&name) {
        return Err(Why::Hidden);
    }
    if is_partial_download(&name) {
        return Err(Why::Partial);
    }
    let parent = src.parent().map(norm).unwrap_or_default();
    let root = norm(&step.root);
    if !(parent == root || (step.subfolders && is_inside(&parent, &root))) {
        return Err(Why::OutsideRoot);
    }
    // a hidden or linked folder between the watched folder and the file (.git, .stversions ...) puts the file out of bounds, as it
    // does in a scan
    if let Some(why) = bad_ancestor(src, &root) {
        return Err(why);
    }
    // not the file that was planned any more
    let modified = meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000;
    if step.modified_ms > 0 && (meta.len() != step.size || (modified - step.modified_ms).abs() > 1000) {
        return Err(Why::Changed);
    }
    if !free_to_take(src) {
        return Err(Why::InUse);
    }
    if opts.automatic {
        // (ctime too: a download renamed to its final name a moment ago has an old mtime but a new ctime)
        let changed = meta.ctime() * 1000 + meta.ctime_nsec() / 1_000_000;
        let newest = modified.max(changed).max(time_ms(meta.created()));
        if opts.now_ms - newest < SETTLE_MS {
            return Err(Why::TooYoung);
        }
    }
    Ok(())
}

/// The folders from the file's own up to (not including) the watched folder: any that is hidden or a link makes the file off
/// limits.
fn bad_ancestor(src: &Path, root_norm: &str) -> Option<Why> {
    let mut cur = src.parent();
    while let Some(dir) = cur {
        let n = norm(dir);
        if n == root_norm || !is_inside(&n, root_norm) {
            break;
        }
        match std::fs::symlink_metadata(dir) {
            Ok(m) => {
                if m.file_type().is_symlink() {
                    return Some(Why::Link);
                }
                if dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                    return Some(Why::Hidden);
                }
            }
            Err(_) => return Some(Why::Gone),
        }
        cur = dir.parent();
    }
    None
}

/// Makes `dir` and its missing parents, one `mkdir` each (it never follows a link at the name it makes; a name that appeared
/// meanwhile is accepted only when it is a real folder, and is not counted as made). The existing part of the path is the folder the
/// user chose and is taken as it resolves. The ones made go into `created`, outermost first.
fn ensure_dir(dir: &Path, created: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut cur = dir;
    loop {
        match std::fs::metadata(cur) {
            Ok(m) => {
                if !m.is_dir() {
                    return Err(std::io::Error::other(tx("The destination is not a folder: a file has the same name")));
                }
                break;
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            Err(_) => {
                // (a dangling link is "not found" when followed, but holds the name: it is not made over)
                if std::fs::symlink_metadata(cur).is_ok() {
                    return Err(std::io::Error::other(tx("The destination is not a folder: a file has the same name")));
                }
                missing.push(cur.to_path_buf());
                match cur.parent() {
                    Some(p) if !p.as_os_str().is_empty() => cur = p,
                    _ => break,
                }
            }
        }
    }
    for d in missing.into_iter().rev() {
        match std::fs::create_dir(&d) {
            Ok(()) => created.push(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && std::fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir()) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

/// Runs the steps. `progress(done, total, name)` is called before each one; `cancel` is looked at between steps. Every step ends in
/// exactly one of: the record (moved or recycled), `skipped` (with the reason), `failed` (with the message), or not reached
/// (`cancelled`, `deferred_by_cap`).
pub fn run(steps: &[Step], opts: &RunOptions, recycler: &dyn Recycler, progress: &dyn Fn(usize, usize, &str), cancel: &AtomicBool) -> Report {
    let mut rep = Report { record: Record { token: TOKENS.fetch_add(1, Ordering::Relaxed), ..Record::default() }, ..Report::default() };
    let take = steps.len().min(opts.cap);
    rep.deferred_by_cap = steps.len() - take;
    let mut bin: Vec<&Step> = Vec::new();
    for (i, st) in steps[..take].iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        let name = st.source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        progress(i, take, &name);
        if let Err(why) = inspect(st, opts) {
            rep.skipped.push(Skip { path: st.source.clone(), why });
            continue;
        }
        match &st.dest {
            Dest::Recycle => {
                if !recycler.can_recycle(&st.source) {
                    rep.skipped.push(Skip { path: st.source.clone(), why: Why::NoRecycleBin });
                } else if !recycler.bin_takes(&st.source) {
                    rep.skipped.push(Skip { path: st.source.clone(), why: Why::BinRefuses });
                } else {
                    bin.push(st);
                }
            }
            Dest::Move(dest) => match do_move(&st.source, dest, &mut rep.record.created_dirs) {
                Ok(done) => rep.record.moves.push((st.source.clone(), done)),
                Err(MoveErr::Changed) => rep.skipped.push(Skip { path: st.source.clone(), why: Why::Changed }),
                Err(e) => rep.failed.push(Failure { path: st.source.clone(), message: e.to_string() }),
            },
        }
    }
    // The bin goes last, in one go, which can be long after the first check (copies to other devices in between): every file is
    // looked at once more right before it is sent.
    let mut checked: Vec<&Step> = Vec::new();
    if !rep.cancelled {
        for st in bin {
            match inspect(st, opts) {
                Ok(()) => checked.push(st),
                Err(why) => rep.skipped.push(Skip { path: st.source.clone(), why }),
            }
        }
    }
    if !checked.is_empty() && !rep.cancelled {
        let paths: Vec<PathBuf> = checked.iter().map(|s| s.source.clone()).collect();
        progress(take, take, "");
        match recycler.recycle(&paths) {
            Ok(pairs) => {
                for p in &paths {
                    if !pairs.iter().any(|(orig, _)| norm(orig) == norm(p)) {
                        rep.failed.push(Failure { path: p.clone(), message: tx("Could not be sent to the Recycle Bin").to_string() });
                    }
                }
                rep.record.recycled = pairs;
            }
            Err(e) => {
                for p in paths {
                    rep.failed.push(Failure { path: p, message: format!("{e:#}") });
                }
            }
        }
    }
    progress(take, take, "");
    rep
}

/// Moves one file; a name that turns out to be taken (someone else was faster than the plan) gets a number. Returns the path it
/// ended up at. Folders made for a move that did not happen are taken away again (innermost first, only while empty).
fn do_move(src: &Path, dest: &Path, created: &mut Vec<PathBuf>) -> Result<PathBuf, MoveErr> {
    let made_before = created.len();
    let r = move_into(src, dest, created);
    if r.is_err() {
        for d in created.drain(made_before..).rev() {
            remove_empty_dir(&d);
        }
    }
    r
}

fn move_into(src: &Path, dest: &Path, created: &mut Vec<PathBuf>) -> Result<PathBuf, MoveErr> {
    if has_dots(dest) || dest.file_name().is_none() {
        return Err(MoveErr::Io(std::io::Error::other(tx("The destination is not a folder: a file has the same name"))));
    }
    if let Some(dir) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir(dir, created).map_err(|e| MoveErr::Io(std::io::Error::other(format!("{}: {e}", tx("Could not create the destination folder")))))?;
    }
    move_file(src, dest, true)
}

// TYPE-COPY(rf-win organize.rs:570)
#[derive(Debug, Clone, Default)]
pub struct UndoReport {
    pub restored: usize,
    pub failed: Vec<Failure>,
}

/// A data operation: nothing is moved back, every move is a failure.
pub fn undo_moves(rec: &Record) -> UndoReport {
    // STUB(linux): an UndoReport that says nothing was restored and why
    stub::hit("organize::undo_moves");
    UndoReport { restored: 0, failed: unavailable("organize::undo_moves", rec.moves.iter().map(|(_, to)| to.clone())) }
}

/// A data operation: nothing is moved again, every move is a failure.
pub fn redo_moves(rec: &Record) -> UndoReport {
    // STUB(linux): an UndoReport that says nothing was redone and why
    stub::hit("organize::redo_moves");
    UndoReport { restored: 0, failed: unavailable("organize::redo_moves", rec.moves.iter().map(|(from, _)| from.clone())) }
}

// ---------------------------------------------------------------- facts (read only)

fn time_ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// Hidden on Linux: a leading dot (there is no system attribute).
fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// The fact of a regular file `dir/name` whose own metadata (`lstat`) is `m`. `None` for anything that is not a regular file
/// (a folder, a link, a FIFO, a socket, a device node).
fn fact_of(dir: &Path, name: &str, m: &std::fs::Metadata, classify: fn(&str) -> FileKind) -> Option<FileFact> {
    if !m.file_type().is_file() {
        return None;
    }
    let modified = m.mtime() * 1000 + m.mtime_nsec() / 1_000_000;
    let created = time_ms(m.created());
    let a = if is_hidden_name(name) { attrs::HIDDEN } else { 0 };
    // (Entry gives the extension exactly as a listing has it: lower case)
    let e = Entry::new(name, EntryKind::File, m.len(), modified, created, a);
    let path = dir.join(name);
    let mut f = FileFact::new(path.clone(), m.len(), modified, created, classify(&e.ext));
    f.hidden = e.is_hidden() || e.is_system();
    // a `.desktop` link file is the shortcut here: a link to a rule's own destination stays where it is
    if &*e.ext == "desktop" {
        f.link_target = crate::shortcut::resolve(&path).ok().map(|i| i.target).filter(|t| !t.as_os_str().is_empty());
    }
    Some(f)
}

/// Every file of `root` (and of the folders below it, with `subfolders`) as facts. Links are neither listed nor entered; hidden
/// folders and the app's own folders are skipped; at most 24 levels down. `cancelled` is polled between folders and every 4096
/// entries.
pub fn scan(root: &Path, subfolders: bool, classify: fn(&str) -> FileKind, protected: &ProtectedPaths, cancelled: &dyn Fn() -> bool) -> Vec<FileFact> {
    let mut facts = Vec::new();
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    'dirs: while let Some((dir, depth)) = stack.pop() {
        if cancelled() {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                log::debug!("organize scan {}: {e}", dir.display());
                continue;
            }
        };
        let mut subdirs = Vec::new();
        for (i, item) in rd.enumerate() {
            if i % 4096 == 4095 && cancelled() {
                break 'dirs;
            }
            let Ok(item) = item else { break };
            let raw = item.file_name();
            // (a name that is not UTF-8 cannot be named again exactly by a step: left alone)
            let Some(name) = raw.to_str() else { continue };
            // (DirEntry::metadata is an lstat: a link is seen as a link)
            let Ok(m) = item.metadata() else { continue };
            let t = m.file_type();
            if t.is_dir() {
                if subfolders && depth < 24 && !is_hidden_name(name) {
                    subdirs.push(dir.join(name));
                }
            } else if let Some(f) = fact_of(&dir, name, &m, classify) {
                facts.push(f);
            }
        }
        for d in subdirs {
            if !protected.is_app_file(&d) {
                stack.push((d, depth + 1));
            }
        }
    }
    facts
}

/// Facts for some names of one folder (what a folder watcher reported): one `lstat` each; names that are gone, folders, links and
/// special files drop out. The cost does not depend on the folder's size.
pub fn facts_for_names(dir: &Path, names: &[String], classify: fn(&str) -> FileKind) -> Vec<FileFact> {
    let mut out = Vec::new();
    for n in names {
        if n.is_empty() || n == "." || n == ".." || n.contains(['/', '\0']) {
            continue;
        }
        if let Ok(m) = std::fs::symlink_metadata(dir.join(n))
            && let Some(f) = fact_of(dir, n, &m, classify)
        {
            out.push(f);
        }
    }
    out
}

/// The same for full paths (the watcher of a folder with its subfolders).
pub fn facts_for_paths(paths: &[PathBuf], classify: fn(&str) -> FileKind) -> Vec<FileFact> {
    let mut out = Vec::new();
    for p in paths {
        let (Some(dir), Some(name)) = (p.parent(), p.file_name().and_then(|n| n.to_str())) else { continue };
        out.extend(facts_for_names(dir, &[name.to_string()], classify));
    }
    out
}

/// Can the folder be opened to list its files? `None`: yes. Otherwise the reason, from the system (a folder that was renamed, a
/// drive that is not mounted, no access): a run over such a folder must not pass for "nothing matches".
pub fn folder_problem(root: &Path) -> Option<String> {
    std::fs::read_dir(root).err().map(|e| e.to_string())
}

/// Does something exist at `p` (a file, a folder, a broken link)? Not followed.
pub fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway folder under the test's own temp dir (the run sets `TMPDIR` to a fresh one).
    pub(super) struct Dir(pub PathBuf);
    impl Dir {
        pub fn new(name: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("leaf-organize-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
        pub fn p(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
        pub fn write(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.p(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        }
        pub fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.p(rel)).unwrap()
        }
        pub fn names(&self, rel: &str) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.p(rel)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn step(d: &Dir, src: &Path, dest: Dest) -> Step {
        Step { source: src.to_path_buf(), dest, rule_id: 1, rule_name: "r".into(), root: d.0.clone(), subfolders: false, size: 0, modified_ms: 0 }
    }

    pub(super) struct NoBin;
    impl Recycler for NoBin {
        fn can_recycle(&self, _: &Path) -> bool {
            false
        }
        fn recycle(&self, _: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
            panic!("must not be asked")
        }
    }

    pub(super) fn go(steps: &[Step]) -> Report {
        run(steps, &RunOptions::manual(0), &NoBin, &|_, _, _| {}, &AtomicBool::new(false))
    }

    #[test]
    fn a_taken_target_name_is_never_replaced() {
        let d = Dir::new("taken");
        let a = d.write("a.txt", "new");
        d.write("hedef/a.txt", "old");
        d.write("hedef/a (2).txt", "old2");
        let link = d.p("l.txt");
        std::os::unix::fs::symlink(d.p("hedef/a.txt"), &link).unwrap();
        let rep = go(&[step(&d, &a, Dest::Move(d.p("hedef/a.txt"))), step(&d, &link, Dest::Move(d.p("hedef/l.txt")))]);
        assert!(rep.failed.is_empty(), "{rep:?}");
        assert_eq!(rep.record.moves, [(a.clone(), d.p("hedef/a (3).txt"))]);
        assert_eq!(rep.skipped, [Skip { path: link.clone(), why: Why::Link }]);
        assert_eq!((d.read("hedef/a.txt"), d.read("hedef/a (2).txt"), d.read("hedef/a (3).txt")), ("old".into(), "old2".into(), "new".into()));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "a link is never moved");
        // the move to another device (copy, check, place without replacing, then remove the source), run on one device here
        let b = d.write("b.txt", "bee");
        assert_eq!(move_by_copy(&b, &d.p("hedef/a.txt"), true).unwrap(), d.p("hedef/a (4).txt"));
        assert_eq!(d.read("hedef/a (4).txt"), "bee");
        assert!(!exists(&b));
        // not numbered (an undo): the taken name stays theirs, the source stays whole, no copy is left behind
        let c = d.write("c.txt", "sea");
        assert!(matches!(move_by_copy(&c, &d.p("hedef/a.txt"), false), Err(MoveErr::Taken)));
        assert!(matches!(move_file(&c, &d.p("hedef/a.txt"), false), Err(MoveErr::Taken)));
        assert_eq!((d.read("c.txt"), d.read("hedef/a.txt")), ("sea".into(), "old".into()));
        assert_eq!(d.names("hedef"), ["a (2).txt", "a (3).txt", "a (4).txt", "a.txt"]);
        // a file where a destination folder would have to be: a failure, nothing made, the file stays
        let rep = go(&[step(&d, &c, Dest::Move(d.p("hedef/a.txt/c.txt")))]);
        assert_eq!(rep.failed.len(), 1);
        assert!(rep.record.created_dirs.is_empty() && exists(&c));
    }
}
