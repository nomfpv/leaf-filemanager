//! Linux backend of rf_win::look::target. Status: STUB

use std::path::PathBuf;

use leaf_common::stub;

// TYPE-COPY(rf-win look/target.rs:21)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Explorer,
    Desktop,
    Dialog,
    Everything,
    DirectoryOpus,
    None,
}

pub fn selected_path(_kind: TargetKind, _hwnd: isize) -> Option<PathBuf> {
    stub!("look::target::selected_path")
}
