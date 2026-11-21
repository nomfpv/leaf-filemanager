//! Linux backend of rf_win::dialogs. Status: STUB

use crate::Owner;
use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/dialogs.rs:17)
pub enum Pick<'a> {
    File {
        filters: &'a [(&'a str, &'a str)],
    },
    Folder,
    Save {
        filters: &'a [(&'a str, &'a str)],
        default_name: &'a str,
    },
}

pub fn pick(_owner: Owner, _title: &str, _kind: Pick<'_>) -> Option<PathBuf> {
    stub!("dialogs::pick")
}

pub fn starts_with_windows() -> bool {
    stub!("dialogs::starts_with_windows")
}

pub fn starts_with_windows_as(_name: &str) -> bool {
    stub!("dialogs::starts_with_windows_as")
}

pub fn set_start_with_windows(_exe: &Path, _on: bool) -> anyhow::Result<()> {
    stub_err!("dialogs::set_start_with_windows")
}

pub fn set_start_with_windows_as(_name: &str, _exe: &Path, _args: &[&str], _on: bool) -> anyhow::Result<()> {
    stub_err!("dialogs::set_start_with_windows_as")
}
