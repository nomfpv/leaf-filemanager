//! Linux backend of the leaf-sys module `input` (Windows side: rf-win input.rs). Status: STUB
//!
//! Wayland gives a client neither the keyboard's state outside an event nor the pointer's position on the screen; the app is to
//! take both from GPUI's events (step A5 and later). Until then: no key is down and the pointer is at (0, 0). The click and hover
//! times are the usual desktop defaults, not zero (a zero wait would turn every click into a "slow click").

use std::time::Duration;

use leaf_common::stub;

pub fn shift_down() -> bool {
    stub!("input::shift_down")
}

pub fn ctrl_down() -> bool {
    stub!("input::ctrl_down")
}

pub fn shift_down_now() -> bool {
    stub!("input::shift_down_now")
}

pub fn ctrl_down_now() -> bool {
    stub!("input::ctrl_down_now")
}

pub fn cursor_pos() -> crate::geom::POINT {
    stub!("input::cursor_pos")
}

pub fn double_click_ms() -> u32 {
    // STUB(linux): GTK's and Qt's default; 0 would end every double click at once
    stub::hit("input::double_click_ms");
    400
}

pub fn hover_time() -> Duration {
    // STUB(linux): Windows' default hover time; zero would select on every pass of the pointer
    stub::hit("input::hover_time");
    Duration::from_millis(400)
}

pub fn current_thread_id() -> u32 {
    stub!("input::current_thread_id")
}

pub fn release_capture() {
    stub!("input::release_capture")
}
