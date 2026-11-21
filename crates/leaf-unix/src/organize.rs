//! Linux backend of rf_win::organize. Status: STUB
//!
//! The rules' runner moves and recycles files: a data operation, so no run ever reports success here. Every step comes back as a
//! failure ("Not available on this system yet") and the progress callback is never called; a scan finds nothing. The plain data
//! types and their pure methods are rf-win's (`PORTABLE?`: the whole module is a candidate for leaf-common, Phase 2).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use leaf_common::stub::Unsupported;
use leaf_common::{stub, stub_err};
use rf_core::organize::safety::ProtectedPaths;
use rf_core::organize::{FileFact, FileKind, Step};

/// At most this many steps in a run the watcher starts (rf-win organize.rs:40).
const AUTO_CAP: usize = 500;

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
    // PORTABLE? (rf-win: the translated text of each reason)
    pub fn text(self) -> &'static str {
        stub!("organize::Why::text")
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
        stub_err!("organize::ShellRecycler::recycle")
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

/// Request/response: the one answer is the return value (no files); `classify` and `cancelled` are never called.
pub fn scan(_root: &Path, _subfolders: bool, _classify: fn(&str) -> FileKind, _protected: &ProtectedPaths, _cancelled: &dyn Fn() -> bool) -> Vec<FileFact> {
    stub!("organize::scan")
}

pub fn facts_for_names(_dir: &Path, _names: &[String], _classify: fn(&str) -> FileKind) -> Vec<FileFact> {
    stub!("organize::facts_for_names")
}

pub fn facts_for_paths(_paths: &[PathBuf], _classify: fn(&str) -> FileKind) -> Vec<FileFact> {
    stub!("organize::facts_for_paths")
}

/// Why the folder cannot be organized: here always, since nothing can be moved.
pub fn folder_problem(_root: &Path) -> Option<String> {
    // STUB(linux): not None (None means "fine, go ahead")
    stub::hit("organize::folder_problem");
    Some(Unsupported("organize::folder_problem").to_string())
}

pub fn exists(_p: &Path) -> bool {
    stub!("organize::exists")
}
