//! Linux backend of rf_win::git. Status: STUB

/// A data operation (it changes the repository): `Err`, and `on_progress` is never called.
pub fn fetch(_root: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<()> {
    stub_err!("git::fetch")
}
