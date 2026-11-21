//! Window handles and rectangles as the app hands them to this crate (moved from rf-app fluent_menu.rs, preview.rs,
//! properties_view.rs, settings_view.rs and look/viewers/video.rs; Linux twin: leaf-unix geom.rs, the same names).

/// A screen rectangle (device px), as the preview host and the video presenter take it: Win32's own `RECT`.
pub use windows::Win32::Foundation::RECT;

/// A point on the screen (device px): Win32's own `POINT` (`.x`, `.y`).
pub use windows::Win32::Foundation::POINT;

/// The owner for a native window handle kept as a number (`hwnd_of`); 0 gives the null `HWND` (no owner).
#[inline]
pub fn owner_from_raw(hwnd: isize) -> crate::Owner {
    windows::Win32::Foundation::HWND(hwnd as *mut _)
}

/// The screen rectangle of the window `hwnd` (device px) as (left, top, right, bottom); `None` when it is gone.
pub fn window_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    let mut r = windows::Win32::Foundation::RECT::default();
    // SAFETY: a plain read of a window's rectangle.
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(windows::Win32::Foundation::HWND(hwnd as *mut _), &mut r).ok()? };
    Some((r.left, r.top, r.right, r.bottom))
}
