//! Linux backend of rf_win::kit. Status: STUB (only the shared `spring`)

use leaf_common::stub;

pub use leaf_common::kit::spring;

// TYPE-COPY(rf-win src/kit/mod.rs:10)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn contains(&self, _x: i32, _y: i32) -> bool {
        stub!("kit::Rect::contains")
    }
}

pub fn menu_orphaned(_outside: bool, _front_is_ours: bool, _open_ms: u64) -> bool {
    stub!("kit::menu_orphaned")
}

pub fn any_button_down() -> bool {
    stub!("kit::any_button_down")
}

pub fn cursor() -> Option<(i32, i32)> {
    stub!("kit::cursor")
}

pub fn monitor_of_window(_hwnd: isize) -> Option<isize> {
    stub!("kit::monitor_of_window")
}

pub fn current_sid() -> Option<String> {
    stub!("kit::current_sid")
}
