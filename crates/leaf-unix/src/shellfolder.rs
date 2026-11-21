//! Linux backend of rf_win::shellfolder. Status: STUB

/// Request/response: one empty batch, then `Err` (no shell namespace here).
pub fn list(_parsing: &str, _cancel: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<ShellChild>)) -> anyhow::Result<usize> {
    on_batch(Vec::new());
    stub_err!("shellfolder::list")
}
