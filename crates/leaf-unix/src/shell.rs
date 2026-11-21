//! Linux backend of rf_win::shell. Status: STUB

use crate::Owner;
use leaf_common::{stub, stub_err};
use std::path::Path;

pub fn ensure_com() {
    stub!("shell::ensure_com")
}

pub fn open(_path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open")
}

pub fn open_with(_path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_with")
}

pub fn properties(_path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::properties")
}

pub fn open_terminal(_dir: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_terminal")
}

pub fn open_terminal_admin(_dir: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_terminal_admin")
}

pub fn resolve_shell_path(_s: &str) -> Option<std::path::PathBuf> {
    stub!("shell::resolve_shell_path")
}

pub fn suppress_critical_error_dialogs() {
    stub!("shell::suppress_critical_error_dialogs")
}

pub fn open_with_dialog(_owner: Owner, _path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_with_dialog")
}

pub fn terminal_exe(_kind: rf_core::terminal::Terminal) -> Option<std::path::PathBuf> {
    None
}

pub fn installed_terminals() -> &'static [rf_core::terminal::Terminal] {
    &[]
}

pub fn open_terminal_with(
    _kind: rf_core::terminal::Terminal,
    _custom: &str,
    _dir: &Path,
) -> anyhow::Result<()> {
    stub_err!("shell::open_terminal_with")
}