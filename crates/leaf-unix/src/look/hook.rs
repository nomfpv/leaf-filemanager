//! Linux backend of rf_win::look::hook. Status: STUB
//!
//! No global keyboard hook (Wayland gives a client none): [`start`] hands out an inert handle and never calls `on_event`.

use leaf_common::stub;

use crate::look::keys::LookAction;
use crate::look::target::TargetKind;

// TYPE-COPY(rf-win look/hook.rs:23)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookEvent {
    pub action: LookAction,
    pub target: TargetKind,
    pub hwnd: isize,
}

pub fn set_own_windows(_hwnds: Vec<isize>) {
    stub!("look::hook::set_own_windows")
}

/// The running hook: nothing runs here.
// TYPE-COPY(rf-win look/hook.rs:99): the hook and dispatch threads dropped
pub struct HookHandle {
    _p: (),
}

impl HookHandle {
    pub fn stop(self) {
        stub!("look::hook::HookHandle::stop")
    }
}

/// Never calls `on_event` (watch style).
pub fn start(_on_event: impl Fn(LookEvent) + Send + 'static) -> HookHandle {
    stub::hit("look::hook::start");
    HookHandle { _p: () }
}
