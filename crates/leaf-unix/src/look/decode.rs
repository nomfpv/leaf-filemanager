//! Linux backend of rf_win::look::decode. Status: STUB

use crate::thumbs::Bgra;
use leaf_common::{stub, stub_err};
use std::path::Path;

/// When a photo was taken (EXIF): plain file reading, shared with Windows.
pub use leaf_common::photo::exif_taken_ms;

// TYPE-COPY(rf-win src/look/decode.rs:13)
pub struct Decoded {
    pub frames: Vec<(Bgra, u32)>,
    pub orientation: u8,
    pub exif: Vec<(String, String)>,
    pub width: u32,
    pub height: u32,
}

pub fn dimensions(_path: &Path) -> Option<(u32, u32)> {
    stub!("look::decode::dimensions")
}

pub fn decode_full(_path: &Path, _max_side: u32) -> anyhow::Result<Decoded> {
    stub_err!("look::decode::decode_full")
}
