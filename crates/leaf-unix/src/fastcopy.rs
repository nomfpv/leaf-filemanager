//! Linux backend of rf_win::fastcopy. Status: STUB

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leaf_common::{stub, stub_err};

pub use leaf_common::fastcopy::{Conflict, Interrupted, Resolve, Verify};

use crate::fileops::{Tracker, Transfer};

/// Whether a copy into a WSL distribution drops the download mark beside it: there is no such copy here.
// TYPE-COPY(rf-win fastcopy.rs:520)
pub static DROP_WSL_ZONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Copy or move with our own engine: nothing is copied or moved on Linux yet. A data operation, so the answer is an error, never
/// `Ok(false)`.
pub fn transfer(_kind: Transfer, _sources: &[PathBuf], _dest_dir: &Path, _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    stub_err!("fastcopy::transfer")
}

/// Where journals live: nothing is journaled on Linux yet.
pub fn set_journal_dir(_dir: PathBuf) {
    stub!("fastcopy::set_journal_dir")
}

/// Jobs left unfinished by an earlier run: none.
pub fn interrupted() -> Vec<Interrupted> {
    stub!("fastcopy::interrupted")
}

/// Forget an interrupted job: nothing is forgotten here.
pub fn discard(_journal: &Path) {
    stub!("fastcopy::discard")
}

/// Carries an interrupted job on: nothing is resumed on Linux yet. A data operation, so the answer is an error, never `Ok(false)`.
pub fn resume(_journal: &Path, _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    stub_err!("fastcopy::resume")
}
