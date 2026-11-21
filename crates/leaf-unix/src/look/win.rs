//! Linux backend of rf_win::look::win. Status: STUB

use leaf_common::stub;

pub fn work_area_at_cursor() -> [f32; 4] {
    stub!("look::win::work_area_at_cursor")
}

pub fn work_area_of(_hwnd: isize) -> [f32; 4] {
    stub!("look::win::work_area_of")
}

pub fn allow_any_foreground() {
    stub!("look::win::allow_any_foreground")
}

pub fn set_topmost(_hwnd: isize, _on: bool) {
    stub!("look::win::set_topmost")
}

pub fn set_rect(_hwnd: isize, _rect: [f32; 4], _scale: f32) {
    stub!("look::win::set_rect")
}

pub fn set_show_in_taskbar(_hwnd: isize, _show: bool) {
    stub!("look::win::set_show_in_taskbar")
}

pub fn process_running(_exe: &str) -> bool {
    stub!("look::win::process_running")
}

pub fn pointer_in_top_strip(_hwnd: isize, _strip: i32) -> bool {
    stub!("look::win::pointer_in_top_strip")
}
