//! The app's one toolkit shim: what it needs of the GUI toolkit's platform layer that GPUI itself does not say the same way on every
//! system. Everything that talks to the operating system lives behind `leaf_sys`; this module only holds what needs a GPUI window or
//! our GPUI fork's knobs.

#[allow(dead_code)]
pub mod gpui;
pub(crate) mod fonts;

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
#[cfg(windows)]
pub(crate) const LOG_FILTER: &str = "warn,gpui_windows=info";
/// Off Windows: GPUI warns once when `.SystemUIFont` has to go through its fallback list (it lands on the desktop's face, which the
/// theme then names), and zbus warns when the private session has no portal settings service; neither is a fault.
#[cfg(not(windows))]
pub(crate) const LOG_FILTER: &str = "warn,gpui::text_system=error,zbus=error";

/// Keeps the system layer's last-known modifier keys and pointer position current from the events GPUI delivers to `el` (put it on the
/// window's root). Off Windows that is the only source for `leaf_sys::input` (Wayland says neither outside an event); on Windows the
/// calls land in no-ops.
pub(crate) fn watch_input<E: gpui_kit::InteractiveElement>(el: E) -> E {
    use gpui_kit::{Modifiers, Pixels, Point};
    fn note(m: &Modifiers, pos: Option<Point<Pixels>>) {
        leaf_sys::input::note_modifiers(m.shift, m.control);
        if let Some(p) = pos {
            leaf_sys::input::note_pointer(f32::from(p.x).round() as i32, f32::from(p.y).round() as i32);
        }
    }
    el.on_modifiers_changed(|e, _, _| note(&e.modifiers, None))
        .capture_any_mouse_down(|e, _, _| note(&e.modifiers, Some(e.position)))
        .capture_any_mouse_up(|e, _, _| note(&e.modifiers, Some(e.position)))
        .on_mouse_move(|e, _, _| note(&e.modifiers, Some(e.position)))
}
