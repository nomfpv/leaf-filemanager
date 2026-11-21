//! Linux backend of rf_win::taskbar_progress. Status: STUB

use leaf_common::stub;

// TYPE-COPY(rf-win src/taskbar_progress.rs:8)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    None,
    Busy,
    Normal(f32),
    Paused(f32),
    Error(f32),
}

pub fn set(_hwnd: isize, _state: State) {
    stub!("taskbar_progress::set")
}
