//! Linux backend of the leaf-sys module `input` (Windows side: rf-win input.rs). Status: REAL (from the app's events)
//!
//! Wayland gives a client neither the keyboard's state outside an event nor the pointer's position on the screen. The app feeds both
//! from the events GPUI delivers (`note_modifiers`, `note_pointer`, called from the shim on the window's root), and the reads here give
//! the last state it was told: Shift and Ctrl as of the last key or mouse event, the pointer as of the last mouse event, in window
//! coordinates (what a menu placed at it needs inside GPUI's window). Before the first event: no key down, pointer at (0, 0). The
//! "now" reads are the same last-known state: a drop from another program carries its own modifiers in its events.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::Relaxed};
use std::time::Duration;

static SHIFT: AtomicBool = AtomicBool::new(false);
static CTRL: AtomicBool = AtomicBool::new(false);
static POS_X: AtomicI32 = AtomicI32::new(0);
static POS_Y: AtomicI32 = AtomicI32::new(0);

/// Records the modifier keys of the latest event.
pub fn note_modifiers(shift: bool, ctrl: bool) {
    SHIFT.store(shift, Relaxed);
    CTRL.store(ctrl, Relaxed);
}

/// Records the pointer position of the latest mouse event.
pub fn note_pointer(x: i32, y: i32) {
    POS_X.store(x, Relaxed);
    POS_Y.store(y, Relaxed);
}

pub fn shift_down() -> bool {
    SHIFT.load(Relaxed)
}

pub fn ctrl_down() -> bool {
    CTRL.load(Relaxed)
}

pub fn shift_down_now() -> bool {
    shift_down()
}

pub fn ctrl_down_now() -> bool {
    ctrl_down()
}

pub fn cursor_pos() -> crate::geom::POINT {
    crate::geom::POINT { x: POS_X.load(Relaxed), y: POS_Y.load(Relaxed) }
}

/// GTK's and Qt's default; 0 would end every double click at once.
pub fn double_click_ms() -> u32 {
    400
}

/// The usual hover time; zero would select on every pass of the pointer.
pub fn hover_time() -> Duration {
    Duration::from_millis(400)
}

/// No thread ids to hand over on Linux.
pub fn current_thread_id() -> u32 {
    0
}

/// Nothing holds a capture across threads here.
pub fn release_capture() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_event_is_what_is_read() {
        note_modifiers(true, false);
        note_pointer(12, 34);
        assert!(shift_down() && !ctrl_down() && shift_down_now());
        assert_eq!(cursor_pos(), crate::geom::POINT { x: 12, y: 34 });
    }
}
