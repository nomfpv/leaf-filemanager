//! Linux backend of rf_win::security. Status: STUB

use anyhow::Result;
use leaf_common::{stub, stub_err};
use std::path::Path;

// TYPE-COPY(rf-win src/security.rs:33)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Security {
    pub owner: Option<String>,
    pub entries: Vec<Entry>,
    pub null_dacl: bool,
    pub protected: bool,
}

pub fn read(_path: &Path) -> Result<Security> {
    stub_err!("security::read")
}

// TYPE-COPY(rf-win src/security.rs:186)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    Full,
    Modify,
    ReadExecute,
    List,
    Read,
    Write,
    Special,
}

pub const RIGHTS: [Right; 7] = [
    Right::Full,
    Right::Modify,
    Right::ReadExecute,
    Right::List,
    Right::Read,
    Right::Write,
    Right::Special,
];

pub fn ticks(_mask: u32, _is_dir: bool) -> Vec<Right> {
    stub!("security::ticks")
}

// TYPE-COPY(rf-win security.rs:21)
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub sid: String,
    pub allow: u32,
    pub deny: u32,
}
