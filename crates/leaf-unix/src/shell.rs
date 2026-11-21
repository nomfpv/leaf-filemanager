//! Linux backend of rf_win::shell. Status: STUB

use std::path::{Path, PathBuf};

use leaf_common::stub;
use rf_core::terminal::Terminal;

pub fn open_terminal(_dir: &Path) -> anyhow::Result<()> {
    stub!("shell::open_terminal")
}

pub fn terminal_exe(_kind: Terminal) -> Option<PathBuf> {
    stub!("shell::terminal_exe")
}

pub fn installed_terminals() -> &'static [Terminal] {
    stub!("shell::installed_terminals")
}

pub fn open_terminal_with(_kind: Terminal, _custom: &str, _dir: &Path) -> anyhow::Result<()> {
    stub!("shell::open_terminal_with")
}

pub fn open_terminal_admin(_dir: &Path) -> anyhow::Result<()> {
    stub!("shell::open_terminal_admin")
}
