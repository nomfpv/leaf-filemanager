//! Linux backend of rf_win::shortcut. Status: STUB

use leaf_common::stub_err;
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/shortcut.rs:15)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub target: PathBuf,
    pub args: String,
    pub working_dir: PathBuf,
    pub icon: String,
    pub icon_index: i32,
}

pub fn resolve(_path: &Path) -> anyhow::Result<ShortcutInfo> {
    stub_err!("shortcut::resolve")
}

pub fn create(_link_path: &Path, _target: &Path) -> anyhow::Result<()> {
    stub_err!("shortcut::create")
}

// TYPE-COPY(rf-win src/shortcut.rs:76)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub target: String,
    pub args: String,
    pub working_dir: String,
    pub comment: String,
    pub show: i32,
    pub hotkey: u16,
    pub icon: String,
    pub icon_index: i32,
    pub run_as_admin: bool,
}

pub fn details(_path: &Path) -> anyhow::Result<Details> {
    stub_err!("shortcut::details")
}

pub fn save(_path: &Path, _d: &Details) -> anyhow::Result<()> {
    stub_err!("shortcut::save")
}
