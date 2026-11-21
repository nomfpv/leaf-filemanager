//! Linux backend of rf_win::fileinfo. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

/// A data operation (it writes attributes): `Err`, and `cancel` is never asked.
pub fn set_attrs_tree(_dir: &Path, _set: u32, _clear: u32, _cancel: &dyn Fn() -> bool) -> std::io::Result<usize> {
    stub_err!("fileinfo::set_attrs_tree")
}

// TYPE-COPY(rf-win src/fileinfo.rs:18)
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub is_dir: bool,
    pub size: u64,
    pub size_on_disk: Option<u64>,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub accessed_ms: i64,
    pub attrs: u32,
    pub opens_with: Option<String>,
}

pub fn info(_path: &Path) -> std::io::Result<Info> {
    stub_err!("fileinfo::info")
}

pub fn opens_with(_path: &Path) -> Option<String> {
    stub!("fileinfo::opens_with")
}

pub fn zone_blocked(_path: &Path) -> bool {
    stub!("fileinfo::zone_blocked")
}

pub fn streams(_path: &Path) -> Vec<(String, u64)> {
    stub!("fileinfo::streams")
}

pub fn mark_downloaded(_path: &Path, _from: &str) -> std::io::Result<()> {
    stub_err!("fileinfo::mark_downloaded")
}

pub fn unblock(_path: &Path) -> std::io::Result<()> {
    stub_err!("fileinfo::unblock")
}

pub const ARCHIVE: u32 = 0x20;

pub const NOT_INDEXED: u32 = 0x2000;

pub const COMPRESSED: u32 = 0x800;

pub const ENCRYPTED: u32 = 0x4000;

pub fn set_attrs(_path: &Path, _set: u32, _clear: u32) -> std::io::Result<()> {
    stub_err!("fileinfo::set_attrs")
}

pub fn set_compressed(_path: &Path, _on: bool) -> std::io::Result<()> {
    stub_err!("fileinfo::set_compressed")
}

pub fn set_encrypted(_path: &Path, _on: bool) -> std::io::Result<()> {
    stub_err!("fileinfo::set_encrypted")
}

pub fn volume_caps(_path: &Path) -> (bool, bool) {
    stub!("fileinfo::volume_caps")
}

pub fn share_of(_dir: &Path) -> Option<String> {
    stub!("fileinfo::share_of")
}
