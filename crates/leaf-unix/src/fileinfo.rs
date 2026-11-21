//! Linux backend of rf_win::fileinfo. Status: STUB

/// A data operation (it writes attributes): `Err`, and `cancel` is never asked.
pub fn set_attrs_tree(_dir: &Path, _set: u32, _clear: u32, _cancel: &dyn Fn() -> bool) -> std::io::Result<usize> {
    stub_err!("fileinfo::set_attrs_tree")
}
