//! The picture fallback for formats the system has no codec for (see rf-win's module of the same name). Linux has no HEIF
//! extension to ask for: nothing is ever reported missing here.

pub use leaf_decode::HEIF_NEEDS_EXTENSION;

pub fn is_heif(path: &std::path::Path) -> bool {
    leaf_decode::is_heif_ext(path.extension().and_then(|e| e.to_str()).unwrap_or(""))
}

pub fn heif_known_missing() -> bool {
    false
}
