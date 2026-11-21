//! Linux backend of rf_win::network. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

pub fn server_root(_path: &Path) -> Option<String> {
    stub!("network::server_root")
}

// TYPE-COPY(rf-win src/network.rs:27)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub remark: String,
}

pub fn shares(_server: &str) -> std::io::Result<Vec<Share>> {
    stub_err!("network::shares")
}

pub fn network_shortcuts() -> Vec<(String, PathBuf)> {
    stub!("network::network_shortcuts")
}

pub fn map_drive_dialog(_owner: isize) {
    stub!("network::map_drive_dialog")
}

pub fn disconnect_drive_dialog(_owner: isize) {
    stub!("network::disconnect_drive_dialog")
}

// TYPE-COPY(rf-win src/network.rs:142)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkType {
    Wifi,
    Ethernet,
    Other,
}

pub fn lan_link() -> Option<(LinkType, u32)> {
    stub!("network::lan_link")
}
