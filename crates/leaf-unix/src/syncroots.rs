//! Linux backend of rf_win::syncroots. Status: STUB

use leaf_common::stub;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/syncroots.rs:18)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRoot {
    pub name: String,
    pub path: PathBuf,
    pub provider: String,
}

pub fn list() -> Vec<SyncRoot> {
    stub!("syncroots::list")
}
