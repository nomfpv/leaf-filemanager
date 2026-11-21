//! The app's one toolkit shim: what it needs of the GUI toolkit's platform layer that GPUI itself does not say the same way on every
//! system. Everything that talks to the operating system lives behind `leaf_sys`; this module only holds what needs a GPUI window or
//! our GPUI fork's knobs.

#[allow(dead_code)]
pub mod gpui;

use gpui_kit::Window;

/// The native handle of a GPUI window as a number (`HWND` on Windows); 0 where the window has none (Wayland gives a client none).
pub(crate) fn hwnd_of(window: &Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// The owner a system dialog or menu opened for `window` gets: its `HWND` on Windows (null when it has none).
pub(crate) fn owner_of(window: &Window) -> leaf_sys::Owner {
    leaf_sys::geom::owner_from_raw(hwnd_of(window))
}

/// The default log filter: the hybrid renderer of our GPUI fork logs its GPU/CPU switches at info level.
pub(crate) const LOG_FILTER: &str = "warn,gpui_windows=info";
