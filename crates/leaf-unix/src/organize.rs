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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use leaf_common::stub;
use leaf_common::stub::Unsupported;
use rf_core::organize::safety::ProtectedPaths;
use rf_core::organize::{FileFact, FileKind, Step};
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

/// A data operation: nothing is moved, every step is a failure, `progress` is never called.
pub fn run(steps: &[Step], _opts: &RunOptions, _recycler: &dyn Recycler, _progress: &dyn Fn(usize, usize, &str), _cancel: &AtomicBool) -> Report {
    // STUB(linux): a Report that says nothing was done and why
    stub::hit("organize::run");
    Report { failed: unavailable("organize::run", steps.iter().map(|s| s.source.clone())), ..Report::default() }
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
