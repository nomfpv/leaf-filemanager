//! Linux backend of rf_win::sysparams. Status: STUB

use leaf_common::stub;

pub fn client_area_animation() -> bool {
    stub!("sysparams::client_area_animation")
}

pub fn snap_to_default_button() -> bool {
    stub!("sysparams::snap_to_default_button")
}

pub fn put_pointer_in(_hwnd: isize, _x: i32, _y: i32) -> bool {
    stub!("sysparams::put_pointer_in")
}
