//! Linux backend of rf_win::shellfolder. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

/// Request/response: one empty batch, then `Err` (no shell namespace here).
pub fn list(_parsing: &str, _cancel: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<ShellChild>)) -> anyhow::Result<usize> {
    on_batch(Vec::new());
    stub_err!("shellfolder::list")
}

// TYPE-COPY(rf-win src/shellfolder.rs:32)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChild {
    pub name: String,
    pub parsing: String,
    pub is_folder: bool,
    pub fs_path: Option<PathBuf>,
    pub size: Option<u64>,
    pub modified_ms: Option<i64>,
}

// TYPE-COPY(rf-win src/shellfolder.rs:48)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub parsing: String,
    pub name: String,
    pub fs_path: Option<PathBuf>,
}

pub fn display_name(_parsing: &str) -> Option<String> {
    stub!("shellfolder::display_name")
}

pub fn browsable(_s: &str) -> Option<Place> {
    stub!("shellfolder::browsable")
}

pub fn devices() -> Vec<ShellChild> {
    stub!("shellfolder::devices")
}

pub fn copy_out(_sources: &[String], _dest: &Path) -> anyhow::Result<bool> {
    // STUB(linux): a data operation never answers success
    stub::hit("shellfolder::copy_out");
    Ok(false)
}

pub fn open(_parsing: &str) -> anyhow::Result<()> {
    stub_err!("shellfolder::open")
}

pub fn copy_in(_sources: &[PathBuf], _dest_parsing: &str, _tracker: std::sync::Arc<leaf_common::fileops::Tracker>) -> anyhow::Result<bool> {
    // STUB(linux): a data operation never answers success
    stub::hit("shellfolder::copy_in");
    Ok(false)
}

pub fn delete_items(_parsings: &[String], _tracker: std::sync::Arc<leaf_common::fileops::Tracker>) -> anyhow::Result<bool> {
    // STUB(linux): a data operation never answers success
    stub::hit("shellfolder::delete_items");
    Ok(false)
}

pub fn new_folder(_parent_parsing: &str, _name: &str) -> anyhow::Result<()> {
    stub_err!("shellfolder::new_folder")
}
