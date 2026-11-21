//! Linux backend of rf_win::tray. Status: STUB

use leaf_common::stub;

/// Never calls `f` (watch style): there is no tray icon here.
pub fn set_handler(_f: impl Fn(TrayEvent) + Send + 'static) {
    stub!("tray::set_handler")
}

// TYPE-COPY(rf-win src/tray.rs:29)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    NewWindow,
    Settings,
    Quit,
}

// TYPE-COPY(rf-win src/tray.rs:40)
#[derive(Clone)]
pub struct Labels {
    pub tooltip: String,
    pub open: String,
    pub new_window: String,
    pub settings: String,
    pub quit: String,
}

pub fn show(_labels: Labels) {
    stub!("tray::show")
}

pub fn hide() {
    stub!("tray::hide")
}

pub fn notify(_title: &str, _body: &str) {
    stub!("tray::notify")
}
