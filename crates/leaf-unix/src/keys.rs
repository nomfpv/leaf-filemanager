//! Linux backend of rf_win::keys. Status: REAL (answers "none")
//!
//! `shifted_char` exists so the app can bind a shortcut to the character a layout puts on Shift+digit. Here GPUI's own key names are
//! used as they come, and asking the keyboard layout for a virtual key's shifted character has no answer without a Windows key code:
//! no extra binding is made.

pub fn shifted_char(_vk: u16) -> Option<String> {
    None
}
