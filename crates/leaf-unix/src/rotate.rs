//! Linux backend of rf_win::rotate. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

pub fn can_rotate(_p: &Path) -> bool {
    stub!("rotate::can_rotate")
}

pub fn rotate(_path: &Path, _clockwise: bool) -> anyhow::Result<()> {
    stub_err!("rotate::rotate")
}
