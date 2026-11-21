//! Linux backend of rf_win::openrules. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

use rf_core::openrules::OpenRule;

pub fn set(_rules: Vec<OpenRule>) {
    stub!("openrules::set")
}

pub fn programs_for(_path: &Path) -> Vec<String> {
    stub!("openrules::programs_for")
}

// TYPE-COPY(rf-win src/openrules.rs:34)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Default,
    With(String),
    Fallback { program: String, error: String },
}

pub fn open(_path: &Path) -> anyhow::Result<Opened> {
    stub_err!("openrules::open")
}

pub fn open_in(_program: &str, _file: &Path) -> anyhow::Result<()> {
    stub_err!("openrules::open_in")
}
