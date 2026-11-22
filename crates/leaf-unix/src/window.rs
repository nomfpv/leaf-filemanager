//! Linux backend of rf_win::window.
//!
//! rf-win's functions work on a window handle (`HWND`) and move, size and order windows from outside the toolkit. A Linux window has
//! no such handle (the app's `hwnd_of` answers 0, and the code that needs one returns before it calls here), and Wayland does not let
//! a program place its own windows at all. What the main window needs on Linux goes through the toolkit instead, in the app:
//! - size, maximized state and the display are remembered in the settings (`MainPlacement`) and given back as the window's
//!   `WindowBounds` when it opens (a position is honoured on X11 only: Wayland compositors place windows themselves, so a restored
//!   window comes up where the compositor puts it, at the remembered size);
//! - the smallest size is the window's `window_min_size` option and the Wayland app id / X11 class its `app_id` option (the desktop
//!   file `leaf.desktop` gives the window its icon and name).
//!
//! So here the handle functions say what is true: nothing to read, nothing placed, nothing minimized, and "always on top" is
//! refused (Wayland has no such request; some desktops offer it in the window's own menu).

use leaf_common::stub_err;

/// Wayland has no "always on top" request for a client; the compositor's window menu does it.
pub fn set_topmost(_hwnd: isize, _on: bool) -> anyhow::Result<()> {
    stub_err!("window::set_topmost")
}

pub fn is_topmost(_hwnd: isize) -> bool {
    false
}

/// Whether one of our windows has the focus is the toolkit's to say; a handle-less backend cannot, so "not in front": a finished
/// long operation then notifies, which is the safe side.
pub fn app_in_front() -> bool {
    false
}

/// The window's rectangle and whether it is maximized: no handle, nothing to read (the app asks the toolkit).
pub fn rect(_hwnd: isize) -> Option<([i32; 4], bool)> {
    None
}

/// Placing a window from outside is not possible (Wayland) or not through a handle (X11): nothing is moved.
pub fn place(_hwnd: isize, _r: [i32; 4]) {}

pub fn place_quietly(_hwnd: isize, _r: [i32; 4]) {}

pub fn is_minimized(_hwnd: isize) -> bool {
    false
}

/// Maximizing goes through the toolkit (`WindowBounds::Maximized` when the window opens); a handle does nothing.
pub fn maximize(_hwnd: isize) {}

/// The smallest size is the window option `window_min_size` of the toolkit; a handle does nothing.
pub fn set_min_size(_hwnd: isize, _size: Option<(u16, u16)>) {}

/// The app id is the window option `app_id` (Wayland app id / X11 class); there is no process-wide id to set.
pub fn set_app_id(_id: &str) {}

/// Dark title bars and corner shapes are Windows' (DWM); the desktop draws the frame here.
pub fn set_dark_mode(_hwnd: isize, _dark: bool) {}

/// The desktop draws the frame here; nothing to take away.
pub fn hide_system_caption(_hwnd: isize) {}

pub fn set_small_corners(_hwnd: isize) {}

pub fn set_round_corners(_hwnd: isize) {}

/// The compositor's blur (KWin's) is asked for elsewhere: nothing to do here.
pub fn set_clear_blur(_hwnd: isize) -> bool {
    false
}

/// A client cannot raise itself on Wayland; it asks for attention with an xdg-activation token (see `activate`).
pub fn bring_to_front(_hwnd: isize) {}

/// The screen's pixels behind a window: reading the screen is not allowed to a client here.
pub fn screen_color(_hwnd: isize, _x: i32, _y0: i32, _y1: i32) -> Option<[u8; 3]> {
    None
}

/// The pixels on screen over a window: reading the screen is not allowed to a client here.
pub fn capture_client(_hwnd: isize) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// The windows of a test run parked off the screen (Windows only: the headless runs here use their own compositor).
pub fn offscreen() -> bool {
    false
}

pub fn park_offscreen() {}

/// Whether no window of leaf is on screen (minimised, or hidden to the tray). Not known here: say no.
pub fn app_hidden() -> bool {
    false
}
