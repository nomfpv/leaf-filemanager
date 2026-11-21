//! Linux backend of rf_win::hotkey. Status: STUB

use leaf_common::stub;

/// Never calls `f` (watch style): no global hot key is registered here.
pub fn set_handler(_f: impl Fn() + Send + 'static) {
    stub!("hotkey::set_handler")
}

// TYPE-COPY(rf-win src/hotkey.rs:25)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    Invalid,
    Taken,
}

pub fn set(_keys: Option<&str>) -> Result<(), HotkeyError> {
    // STUB(linux): no global hotkey; the quiet answer is "not a valid combination"
    stub::hit("hotkey::set");
    Err(HotkeyError::Invalid)
}
