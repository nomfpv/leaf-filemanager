//! Linux backend of rf_win::editors. Status: STUB

use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/editors.rs:18)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub name: &'static str,
    pub exe: PathBuf,
}

pub fn peek() -> (Vec<Editor>, bool) {
    stub!("editors::peek")
}

pub fn refresh() -> bool {
    stub!("editors::refresh")
}

pub fn installed() -> Vec<Editor> {
    stub!("editors::installed")
}

pub fn name_of(_list: &[Editor], _exe: &Path) -> Option<&'static str> {
    stub!("editors::name_of")
}
