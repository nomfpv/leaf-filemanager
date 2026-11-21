//! Linux backend of rf_win::vfiles. Status: STUB

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use leaf_common::{stub, stub_err};

/// A data operation: nothing is pasted, `progress` is never called, the answer is `Err`.
pub fn paste_clipboard(_dest: &Path, _progress: &mut dyn FnMut(Progress), _cancel: &AtomicBool) -> anyhow::Result<Pasted> {
    stub_err!("vfiles::paste_clipboard")
}

pub fn clipboard_has() -> bool {
    stub!("vfiles::clipboard_has")
}

pub fn drag_paths(_data: *mut std::ffi::c_void, _fetch: bool) -> Option<Vec<PathBuf>> {
    stub!("vfiles::drag_paths")
}

// TYPE-COPY(rf-win vfiles.rs:171)
pub struct Progress<'a> {
    pub bytes: (u64, u64),
    pub files: (usize, usize),
    pub current: Option<&'a str>,
}

// TYPE-COPY(rf-win vfiles.rs:182)
#[derive(Debug, Default)]
pub struct Pasted {
    pub arrived: Vec<PathBuf>,
    pub failed: usize,
    pub first_error: Option<String>,
    pub cancelled: bool,
}
