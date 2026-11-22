//! Pictures Windows has no codec of its own for. JPEG XL and AVIF come from the store's extensions (or from
//! nothing): when WIC cannot read them, `leaf-decode` (pure Rust, no system calls) does. HEIC is left to WIC
//! alone (patents and licences): when its extension is missing the preview says so in one quiet line.
//!
//! Nothing here runs until such a file is shown: `leaf-decode` has no start-up work, and the HEIF check below
//! is made the first time a HEIC/HEIF picture is asked for.

use std::path::Path;

use crate::thumbs::Bgra;

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A JPEG XL or AVIF file: decoded by `leaf-decode` when WIC cannot.
pub fn handles(path: &Path) -> bool {
    leaf_decode::handles_ext(&ext(path))
}

/// A HEIC / HEIF file (only WIC reads these).
pub fn is_heif(path: &Path) -> bool {
    leaf_decode::is_heif_ext(&ext(path))
}

/// The line shown in place of a HEIC picture Windows cannot read.
pub use leaf_decode::HEIF_NEEDS_EXTENSION;

static HEIF_MISSING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether this Windows has no HEIF codec at all (the extension is not installed): asked once, on the first
/// HEIC picture, and remembered. Blocking COM: for a worker thread.
pub fn heif_codec_missing() -> bool {
    *HEIF_MISSING.get_or_init(|| {
        use windows::Win32::Graphics::Imaging::{CLSID_WICHeifDecoder, IWICBitmapDecoder};
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        crate::shell::ensure_com();
        unsafe { CoCreateInstance::<_, IWICBitmapDecoder>(&CLSID_WICHeifDecoder, None, CLSCTX_INPROC_SERVER) }.is_err()
    })
}

/// The answer of `heif_codec_missing` if it was asked already, else `false`: never blocks (for the UI thread,
/// which only needs it after a HEIC picture failed on a worker).
pub fn heif_known_missing() -> bool {
    HEIF_MISSING.get().copied().unwrap_or(false)
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

/// An SVG or SVGZ file: drawn by `resvg` (the same render as the Space viewer), which Windows has no thumbnail for.
pub fn is_svg(path: &Path) -> bool {
    matches!(ext(path).as_str(), "svg" | "svgz")
}

/// Largest SVG file drawn for a thumbnail (a hostile or huge one is left alone, not parsed).
const MAX_SVG: u64 = 16 << 20;

/// The SVG at `size` pixels on its long side. `None`: too big, not an SVG, or the render failed (never panics).
pub fn svg_thumbnail(path: &Path, size: u32) -> Option<Bgra> {
    if std::fs::metadata(path).ok()?.len() > MAX_SVG {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let drawn = std::panic::catch_unwind(|| crate::look::decode::svg(&bytes, size.clamp(16, 1024))).ok()?;
    match drawn {
        Ok(mut d) if !d.frames.is_empty() => Some(d.frames.swap_remove(0).0),
        Ok(_) => None,
        Err(e) => {
            log::debug!("{}: {e}", path.display());
            None
        }
    }
}

/// A format the `image` crate reads and Windows has no thumbnail for (the Space viewer decodes these the same way).
pub fn is_crate_format(path: &Path) -> bool {
    matches!(ext(path).as_str(), "tga" | "qoi" | "exr" | "hdr" | "dds" | "pnm" | "pbm" | "pgm" | "ppm" | "pam")
}

/// Largest file decoded for a thumbnail by `image_thumbnail`.
const MAX_CRATE_FILE: u64 = 64 << 20;

/// The picture at `size` pixels on its long side, decoded by the `image` crate. `None`: too big, unreadable, or the decoder
/// gave up (never panics). The decoder's own allocation limit caps a header that claims a huge picture.
pub fn image_thumbnail(path: &Path, size: u32) -> Option<Bgra> {
    if std::fs::metadata(path).ok()?.len() > MAX_CRATE_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let drawn = std::panic::catch_unwind(|| crate::look::decode::with_image_crate(&bytes, size.clamp(16, 1024))).ok()?;
    match drawn {
        Ok(mut d) if !d.frames.is_empty() => Some(d.frames.swap_remove(0).0),
        Ok(_) => None,
        Err(e) => {
            log::debug!("{}: {e}", path.display());
            None
        }
    }
}

/// A thumbnail leaf draws itself for the files Windows has none for: SVG, and the `image` crate's formats (and a plain `.ico`).
pub fn own_thumbnail(path: &Path, size: u32) -> Option<Bgra> {
    if is_svg(path) {
        svg_thumbnail(path, size)
    } else if is_crate_format(path) || ext(path) == "ico" {
        image_thumbnail(path, size)
    } else {
        None
    }
}
