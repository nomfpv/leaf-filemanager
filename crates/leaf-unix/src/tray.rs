//! Linux backend of rf_win::tray. Status: STUB

/// Never calls `f` (watch style): there is no tray icon here.
pub fn set_handler(_f: impl Fn(TrayEvent) + Send + 'static) {
    stub!("tray::set_handler")
}
