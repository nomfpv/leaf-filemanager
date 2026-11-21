//! The knobs of our GPUI Windows fork (`gpui_windows`, github.com/gelenmedya/gpui-pre-windows) that the app turns: the hybrid
//! GPU/CPU renderer, the drag-and-drop effect hint, and two switches. On Windows these ARE the fork's items (re-exported, no
//! wrapper). Elsewhere the same names exist and do nothing, so the app compiles unchanged; the Linux GPUI backend has no such knobs.

#[cfg(windows)]
pub use gpui_windows::{KEEP_ACTIVE_LOOK, SMOOTH_SCROLL, dnd_hint, hybrid};

/// Keeps a window drawn as active while one of our pop-ups has the focus. Read by nothing off Windows.
#[cfg(not(windows))]
pub static KEEP_ACTIVE_LOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Smooth wheel scrolling. Read by nothing off Windows.
#[cfg(not(windows))]
pub static SMOOTH_SCROLL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The hybrid renderer's API (the fork's hybrid.rs), with nothing behind it: one renderer, no switches.
#[cfg(not(windows))]
pub mod hybrid {
    use std::sync::atomic::{AtomicU8, Ordering};

    /// User-facing renderer selection.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    #[repr(u8)]
    pub enum RendererMode {
        Auto = 0,
        Gpu = 1,
        Cpu = 2,
    }

    /// Latest measurements of the load monitor (none here).
    #[derive(Copy, Clone, Debug, Default)]
    pub struct LoadSnapshot {
        pub gpu_percent: Option<f32>,
        pub cpu_percent: Option<f32>,
        pub last_frame_ms: f32,
    }

    // (kept so that `mode` gives back what `set_mode` was told)
    static MODE: AtomicU8 = AtomicU8::new(RendererMode::Auto as u8);

    pub fn mode() -> RendererMode {
        match MODE.load(Ordering::Relaxed) {
            1 => RendererMode::Gpu,
            2 => RendererMode::Cpu,
            _ => RendererMode::Auto,
        }
    }

    pub fn set_mode(mode: RendererMode) {
        MODE.store(mode as u8, Ordering::Relaxed);
    }

    pub fn switch_count() -> u32 {
        0
    }

    pub fn current_backend_label() -> &'static str {
        "?"
    }

    pub fn load_snapshot() -> LoadSnapshot {
        LoadSnapshot::default()
    }

    pub fn set_fast_start(_on: bool) {}

    pub fn gpu_device_ms() -> Option<u32> {
        None
    }
}

/// The fork's drag-and-drop effect hint (dnd_hint.rs), with nothing behind it: every effect is allowed, nothing is told.
#[cfg(not(windows))]
pub mod dnd_hint {
    pub const NONE: u32 = 0;
    pub const COPY: u32 = 1;
    pub const MOVE: u32 = 2;
    pub const LINK: u32 = 4;

    /// The effects the drag's source allows.
    pub fn allowed() -> u32 {
        COPY | MOVE
    }

    /// The effect the drop target would take.
    pub fn set(_effect: u32) {}

    /// Reads the paths of a drag's virtual files (the data object is the OS's).
    pub type VirtualReader = fn(data: *mut std::ffi::c_void, fetch: bool) -> Option<Vec<std::path::PathBuf>>;

    pub fn set_virtual_reader(_reader: VirtualReader) {}
}
