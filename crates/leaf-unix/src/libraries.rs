//! Linux backend of rf_win::libraries. Status: STUB

use leaf_common::stub;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/libraries.rs:18)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub file: PathBuf,
    pub name: String,
    pub folders: Vec<PathBuf>,
    pub default_save: Option<PathBuf>,
}

pub fn load(_file: &std::path::Path) -> Option<Library> {
    stub!("libraries::load")
}

pub fn list() -> Vec<Library> {
    stub!("libraries::list")
}
