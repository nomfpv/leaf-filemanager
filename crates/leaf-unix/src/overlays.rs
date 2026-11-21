//! Linux backend of rf_win::overlays. Status: STUB

use crate::thumbs::Bgra;
use leaf_common::stub;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn indexes(_dir: &Path, _names: Vec<String>, _cancel: Arc<AtomicBool>) -> Vec<(String, i32)> {
    stub!("overlays::indexes")
}

pub fn picture(_index: i32, _px: u32) -> Option<Bgra> {
    stub!("overlays::picture")
}
