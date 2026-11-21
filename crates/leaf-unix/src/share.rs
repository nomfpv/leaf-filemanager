//! Linux backend of rf_win::share. Status: STUB

use leaf_common::stub_err;
use std::path::PathBuf;

// TYPE-COPY(rf-win share.rs): the storage-item reference dropped
pub struct Prepared {
    _p: (),
}

pub fn prepare(_paths: &[PathBuf]) -> anyhow::Result<Prepared> {
    stub_err!("share::prepare")
}

pub fn show(_hwnd: isize, _items: Prepared, _title: &str) -> anyhow::Result<()> {
    stub_err!("share::show")
}
