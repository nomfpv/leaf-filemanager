//! Linux backend of rf_win::hotkey. Status: STUB

/// Never calls `f` (watch style): no global hot key is registered here.
pub fn set_handler(_f: impl Fn() + Send + 'static) {
    stub!("hotkey::set_handler")
}
