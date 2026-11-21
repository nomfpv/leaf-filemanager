//! Linux backend of rf_win::folder_custom. Status: STUB

use crate::Owner;
use leaf_common::{stub, stub_err};
use std::path::Path;

pub fn folder_icon(_dir: &Path) -> Option<(String, i32)> {
    stub!("folder_custom::folder_icon")
}

pub fn set_folder_icon(_dir: &Path, _icon: Option<(&str, i32)>) -> anyhow::Result<()> {
    stub_err!("folder_custom::set_folder_icon")
}

pub fn pick_icon(_owner: Owner, _file: &str, _index: i32) -> Option<(String, i32)> {
    stub!("folder_custom::pick_icon")
}
