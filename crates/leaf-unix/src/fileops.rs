//! Linux backend of rf_win::fileops. Status: STUB

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leaf_common::{stub, stub_err};

pub use leaf_common::fileops::{ByteProgress, Tracker, Transfer};

/// Renames `path` to `new_name`: nothing is renamed on Linux yet. A data operation, so the answer is an error, never `Ok(false)`.
pub fn rename(_path: &Path, _new_name: &str) -> anyhow::Result<bool> {
    stub_err!("fileops::rename")
}

/// Plain rename that fails instead of silently replacing an existing target: nothing is renamed on Linux yet.
pub fn rename_no_replace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    stub_err!("fileops::rename_no_replace")
}

/// `name`, `name (2)`, … — the created path: no folder is created on Linux yet.
pub fn new_folder(_dir: &Path, _name: &str) -> anyhow::Result<PathBuf> {
    stub_err!("fileops::new_folder")
}

/// The created path of a new empty text file: no file is created on Linux yet.
pub fn new_text_file(_dir: &Path, _name: &str) -> anyhow::Result<PathBuf> {
    stub_err!("fileops::new_text_file")
}

/// A name under `dir` that nothing holds yet: no name is looked up on Linux yet.
pub fn unique_name(_dir: &Path, _stem: &str, _ext: &str) -> PathBuf {
    stub!("fileops::unique_name")
}

/// `transfer` with in-app progress: nothing is copied or moved on Linux yet. A data operation, so the answer is an error, never
/// `Ok(false)`.
pub fn transfer_tracked(_kind: Transfer, _sources: &[PathBuf], _dest_dir: &Path, _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    stub_err!("fileops::transfer_tracked")
}

/// `delete` with in-app progress: nothing is deleted on Linux yet. A data operation, so the answer is an error, never `Ok(false)`.
pub fn delete_tracked(_paths: &[PathBuf], _permanent: bool, _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    stub_err!("fileops::delete_tracked")
}
