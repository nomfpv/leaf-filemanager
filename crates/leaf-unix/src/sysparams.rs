//! Linux backend of rf_win::sysparams. Status: REAL as constants (no system setting is read: Wayland has no common place for them).
//!
//! Animations stay on (the app's own settings can still turn them off); the pointer is never moved into a dialog (a Wayland client
//! cannot warp the pointer, and "snap to default button" is a Windows habit no desktop here has).

pub fn client_area_animation() -> bool {
    true
}

pub fn snap_to_default_button() -> bool {
    false
}

pub fn put_pointer_in(_hwnd: isize, _x: i32, _y: i32) -> bool {
    false
}
