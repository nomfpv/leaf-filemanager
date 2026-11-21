//! Linux backend of rf_win::localized. Status: STUB

use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub fn folder_label(_dir: &Path, _attrs: Option<u32>) -> Option<Arc<str>> {
    stub!("localized::folder_label")
}

pub fn label_if_known(_dir: &Path) -> Option<Arc<str>> {
    stub!("localized::label_if_known")
}

pub fn is_known(_dir: &Path) -> bool {
    stub!("localized::is_known")
}

pub fn warm<I: IntoIterator<Item = PathBuf>>(_dirs: I) -> usize {
    stub!("localized::warm")
}

pub fn is_network(_p: &Path) -> bool {
    stub!("localized::is_network")
}

pub fn expand_env(_s: &str) -> String {
    stub!("localized::expand_env")
}
