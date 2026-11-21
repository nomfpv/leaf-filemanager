//! Linux backend of rf_win::shell_window. Status: STUB

use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

pub fn wait_for_select(_folder: &Path, _wait: Duration) -> Option<PathBuf> {
    stub!("shell_window::wait_for_select")
}
