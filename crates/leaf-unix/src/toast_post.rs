//! Linux backend of rf_win::toast_post. Status: STUB

/// `Err`: no notification is posted, so `on_activate` is never called (nothing can be clicked).
pub fn show(_app_id: &str, _toast: &Toast, _on_activate: impl Fn(String) + Send + Sync + 'static) -> anyhow::Result<()> {
    stub_err!("toast_post::show")
}
