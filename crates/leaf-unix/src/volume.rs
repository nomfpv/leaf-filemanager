//! Linux backend of rf_win::volume. Status: STUB

use leaf_common::stub_err;
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/volume.rs:17)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub root: PathBuf,
    pub label: String,
    pub filesystem: String,
    pub serial: u32,
    pub total: u64,
    pub free: u64,
    pub drive_type: String,
}

pub fn volume_info(_root: &Path) -> anyhow::Result<VolumeInfo> {
    stub_err!("volume::volume_info")
}

pub fn set_label(_root: &Path, _label: &str) -> std::io::Result<()> {
    stub_err!("volume::set_label")
}
