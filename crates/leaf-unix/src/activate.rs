//! Linux backend of rf_win::activate.
//!
//! rf-win's module starts a packaged (MSIX) app through the activation manager: nothing of that exists here, so `packaged` stays
//! quiet. What Linux has instead is the xdg-activation token: a second launch started from a desktop entry or a notification gets
//! `XDG_ACTIVATION_TOKEN` in its environment, hands it over with its request (see `single_instance`), and the running instance
//! offers it to the compositor when it raises its window (so that the window comes to the front instead of only flashing).

use std::sync::Mutex;

static TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Remembers the activation token a second launch brought (called by the socket's reader).
pub(crate) fn offer_token(token: &str) {
    if !token.is_empty() && token.len() < 256 {
        *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.to_string());
    }
}

/// The token of the latest request, once: a token is good for one activation.
pub fn take_token() -> Option<String> {
    TOKEN.lock().unwrap_or_else(|e| e.into_inner()).take()
}
