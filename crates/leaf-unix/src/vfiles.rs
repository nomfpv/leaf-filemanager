//! Linux backend of rf_win::vfiles. Status: STUB

/// A data operation: nothing is pasted, `progress` is never called, the answer is `Err`.
pub fn paste_clipboard(_dest: &Path, _progress: &mut dyn FnMut(Progress), _cancel: &AtomicBool) -> anyhow::Result<Pasted> {
    stub_err!("vfiles::paste_clipboard")
}
