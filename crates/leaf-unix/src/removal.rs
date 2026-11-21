//! Linux backend of rf_win::removal. Status: STUB
//!
//! The worked example of a watch-style callback: nothing is watched, so the callback is never called (there is no request to
//! answer). Device removal on Linux is a later job (udisks).

use std::sync::mpsc::Sender;

use leaf_common::stub;

/// Never calls `on_query_remove`: no removal is watched yet.
pub fn start(_on_query_remove: impl Fn(char, Sender<()>) + Send + 'static) {
    stub!("removal::start")
}
