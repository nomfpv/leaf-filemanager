//! Linux backend of the leaf-sys module `geom` (Windows side: rf-win geom.rs). Status: PARTIAL (`window_rect` is a stub)

use leaf_common::stub;

use crate::Owner;

/// A screen rectangle (device px): Win32's `RECT`, the same field names, so the app builds it with the same code. The preview
/// host's and the video presenter's stubs take it (`crate::geom::RECT`).
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A point on the screen (device px): Win32's `POINT`, the same field names.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

/// The owner for a native window handle kept as a number: there is none to keep on Linux.
#[inline]
pub fn owner_from_raw(_hwnd: isize) -> Owner {
    Owner
}

/// The screen rectangle of a window of ours (left, top, right, bottom); Wayland does not tell a client where its windows are.
pub fn window_rect(_hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    stub!("geom::window_rect")
}
