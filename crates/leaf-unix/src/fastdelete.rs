//! Linux backend of rf_win::fastdelete. Status: STUB

use std::path::PathBuf;
use std::sync::Arc;

use leaf_common::{stub, stub_err};

use crate::fileops::Tracker;

/// Permanently deletes `paths`: nothing is deleted on Linux yet. A data operation, so the answer is an error, never `Ok(false)`.
pub fn delete_permanent(_paths: &[PathBuf], _tracker: Arc<Tracker>, _threads: usize) -> anyhow::Result<bool> {
    stub_err!("fastdelete::delete_permanent")
}

/// Thread count for deleting under `paths`: nothing runs, so none.
pub fn threads_for(_paths: &[PathBuf]) -> usize {
    stub!("fastdelete::threads_for")
}
