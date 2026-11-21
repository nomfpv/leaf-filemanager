//! Linux backend of rf_win::ctxmenu. Status: STUB

use crate::Owner;
use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

pub use leaf_common::ctxmenu::{ExtraItem, MenuOutcome, ShellIcon, ShellItem, ShellTarget};

pub fn show_for_items(
    _owner: Owner,
    _items: &[PathBuf],
    _screen: (i32, i32),
    _extended: bool,
    _extra: &[ExtraItem],
) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_items")
}

pub fn show_for_one(
    _owner: Owner,
    _item: &Path,
    _screen: (i32, i32),
    _extended: bool,
    _extra: &[ExtraItem],
) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_one")
}

pub fn show_for_background(
    _owner: Owner,
    _dir: &Path,
    _screen: (i32, i32),
    _extended: bool,
    _extra: &[ExtraItem],
) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_background")
}

pub fn build(_owner: Owner, _target: &ShellTarget, _extended: bool) -> anyhow::Result<(u64, Vec<ShellItem>)> {
    stub_err!("ctxmenu::build")
}

pub fn invoke(_owner: Owner, _session: u64, _id: u32, _screen: (i32, i32)) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::invoke")
}

pub fn invoke_verb(_owner: Owner, _target: &ShellTarget, _verb: &str, _screen: (i32, i32)) -> anyhow::Result<()> {
    stub_err!("ctxmenu::invoke_verb")
}

pub fn show_simple(_owner: Owner, _screen: (i32, i32), _items: &[ExtraItem]) -> Option<u32> {
    stub!("ctxmenu::show_simple")
}
