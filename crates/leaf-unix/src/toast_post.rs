//! Linux backend of rf_win::toast_post. Status: STUB

use leaf_common::{stub, stub_err};

/// `Err`: no notification is posted, so `on_activate` is never called (nothing can be clicked).
pub fn show(_app_id: &str, _toast: &Toast, _on_activate: impl Fn(String) + Send + Sync + 'static) -> anyhow::Result<()> {
    stub_err!("toast_post::show")
}

// TYPE-COPY(rf-win src/toast_post.rs:25)
pub struct Button {
    pub label: String,
    pub argument: String,
}

// TYPE-COPY(rf-win src/toast_post.rs:32)
pub struct Toast<'a> {
    pub tag: &'a str,
    pub title: &'a str,
    pub lines: &'a [&'a str],
    pub launch: &'a str,
    pub buttons: &'a [Button],
    pub stays: bool,
}

pub fn remove(_app_id: &str, _tag: &str) {
    stub!("toast_post::remove")
}
