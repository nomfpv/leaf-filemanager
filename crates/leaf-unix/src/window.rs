//! Linux backend of rf_win::window. Status: STUB

use leaf_common::{stub, stub_err};

pub fn set_topmost(_hwnd: isize, _on: bool) -> anyhow::Result<()> {
    stub_err!("window::set_topmost")
}

pub fn is_topmost(_hwnd: isize) -> bool {
    stub!("window::is_topmost")
}

pub fn app_in_front() -> bool {
    stub!("window::app_in_front")
}

pub fn rect(_hwnd: isize) -> Option<([i32; 4], bool)> {
    stub!("window::rect")
}

pub fn place(_hwnd: isize, _r: [i32; 4]) {
    stub!("window::place")
}

pub fn place_quietly(_hwnd: isize, _r: [i32; 4]) {
    stub!("window::place_quietly")
}

pub fn is_minimized(_hwnd: isize) -> bool {
    stub!("window::is_minimized")
}

pub fn maximize(_hwnd: isize) {
    stub!("window::maximize")
}

pub fn set_min_size(_hwnd: isize, _size: Option<(u16, u16)>) {
    stub!("window::set_min_size")
}

pub fn set_app_id(_id: &str) {
    stub!("window::set_app_id")
}

pub fn set_dark_mode(_hwnd: isize, _dark: bool) {
    stub!("window::set_dark_mode")
}

pub fn set_small_corners(_hwnd: isize) {
    stub!("window::set_small_corners")
}

pub fn set_round_corners(_hwnd: isize) {
    stub!("window::set_round_corners")
}

pub fn bring_to_front(_hwnd: isize) {
    stub!("window::bring_to_front")
}

pub fn screen_color(_hwnd: isize, _x: i32, _y0: i32, _y1: i32) -> Option<[u8; 3]> {
    stub!("window::screen_color")
}

pub fn offscreen() -> bool {
    stub!("window::offscreen")
}

pub fn park_offscreen() {
    stub!("window::park_offscreen")
}
