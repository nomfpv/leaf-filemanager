//! Linux backend of rf_win::clipboard. Status: STUB

use crate::Owner;
use leaf_common::fileops::Transfer;
use leaf_common::{stub, stub_err};
use std::path::PathBuf;

pub fn clear(_owner: Owner) -> anyhow::Result<()> {
    stub_err!("clipboard::clear")
}

pub fn sequence() -> u32 {
    stub!("clipboard::sequence")
}

pub fn set_files(_owner: Owner, _paths: &[PathBuf], _kind: Transfer) -> anyhow::Result<()> {
    stub_err!("clipboard::set_files")
}

pub fn has_files() -> bool {
    stub!("clipboard::has_files")
}

pub fn can_paste() -> bool {
    stub!("clipboard::can_paste")
}

pub fn get_files(_owner: Owner) -> anyhow::Result<Option<(Vec<PathBuf>, Transfer)>> {
    stub_err!("clipboard::get_files")
}
