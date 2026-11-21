//! Linux backend of rf_win::sendto. Status: STUB

use crate::Owner;
use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/sendto.rs:24)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub name: String,
}

pub fn targets() -> Vec<Target> {
    stub!("sendto::targets")
}

pub fn send(_owner: Owner, _target: &Path, _items: &[PathBuf]) -> anyhow::Result<()> {
    stub_err!("sendto::send")
}
