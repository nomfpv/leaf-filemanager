//! The picture fallback for formats the system has no codec for (see rf-win's module of the same name). On Linux the pictures are
//! always decoded in this process, so `handles` is the JPEG XL / AVIF test and `thumbnail` the plain decode. Linux has no HEIF
//! extension to ask for: nothing is ever reported missing here (HEIC is not decoded at all, patents).

use std::path::Path;

use crate::thumbs::Bgra;

pub use leaf_decode::HEIF_NEEDS_EXTENSION;

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A JPEG XL or AVIF file: decoded by `leaf-decode`.
pub fn handles(path: &Path) -> bool {
    leaf_decode::handles_ext(&ext(path))
}

pub fn is_heif(path: &Path) -> bool {
    leaf_decode::is_heif_ext(&ext(path))
}

pub fn heif_known_missing() -> bool {
    false
}

pub(crate) fn bgra(img: leaf_decode::Rgba) -> Bgra {
    let leaf_decode::Rgba { width, height, mut data, .. } = img;
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Bgra { width, height, data }
}

/// The picture at `size` pixels on its long side, decoded here. `None`: not readable.
pub fn thumbnail(path: &Path, size: u32) -> Option<Bgra> {
    match leaf_decode::decode(path, size) {
        Ok(img) => Some(bgra(img)),
        Err(e) => {
            log::debug!("{}: {e}", path.display());
            None
        }
    }
}
