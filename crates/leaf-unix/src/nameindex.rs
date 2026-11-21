//! Linux backend of rf_win::nameindex. Status: STUB

use leaf_common::stub;
use std::path::PathBuf;

pub fn set_excluded(_dirs: &[PathBuf]) {
    stub!("nameindex::set_excluded")
}

pub fn default_excluded_names() -> &'static [&'static str] {
    stub!("nameindex::default_excluded_names")
}
