//! Portable picture decoders in pure Rust (no system calls), the fallback for a system that has no codec for them:
//! JPEG XL and AVIF (Windows ships none: they come from Microsoft Store extensions), Photoshop files (the flattened
//! picture) and camera RAW files (the JPEG the camera embedded).
//!
//! Nothing here runs, allocates or initialises until `decode` or `dimensions` is called for a file.
//! HEIC is not here on purpose (patents and licences): the platform's own codec decides.

use std::path::Path;

mod avif;
mod jxl;
mod psd;
mod raw;

/// A decoded picture: straight-alpha RGBA, 8 bits, its long side at most the size asked.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// The picture's own (turned upright, not reduced) size.
    pub full: (u32, u32),
    /// EXIF orientation the pixels were already turned by (1 = none).
    pub orientation: u8,
    /// Camera / date rows from the file's own EXIF (RAW only).
    pub exif: Vec<(String, String)>,
}

/// Files read into memory for a picture are at most this large.
const MAX_FILE: u64 = 400 * 1024 * 1024;

/// Whether this extension (any case, no dot) is a format decoded here.
pub fn handles_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("jxl") || ext.eq_ignore_ascii_case("avif") || is_psd_ext(ext) || is_raw_ext(ext)
}

fn is_psd_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("psd") || ext.eq_ignore_ascii_case("psb")
}

/// Whether this extension (any case, no dot) is a camera RAW format.
pub fn is_raw_ext(ext: &str) -> bool {
    raw::EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// Whether this extension (any case, no dot) is HEIC / HEIF: not decoded here, only the system's own codec reads it.
pub fn is_heif_ext(ext: &str) -> bool {
    ["heic", "heif", "hif"].iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// The line shown in place of a HEIC picture the system cannot read (Windows: the store's HEIF Image Extension).
pub const HEIF_NEEDS_EXTENSION: &str = "HEIC needs the HEIF Image Extension from Microsoft";

fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Reduces `img` so its long side is at most `max_side` (never enlarged).
pub(crate) fn shrink(img: image::RgbaImage, max_side: u32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= max_side || long == 0 || max_side == 0 {
        return img;
    }
    let s = f64::from(max_side) / f64::from(long);
    let (nw, nh) = (((f64::from(w) * s).round() as u32).max(1), ((f64::from(h) * s).round() as u32).max(1));
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

/// Decodes the JPEG XL, AVIF, Photoshop or RAW file at `path`, its long side at most `max_side`.
pub fn decode(path: &Path, max_side: u32) -> anyhow::Result<Rgba> {
    let ext = ext_of(path);
    if is_raw_ext(&ext) {
        // only the parts of the file the preview is in are read
        return raw::decode(path, max_side);
    }
    let len = std::fs::metadata(path)?.len();
    anyhow::ensure!(len <= MAX_FILE, "file too large for a picture: {len} bytes");
    let bytes = std::fs::read(path)?;
    decode_bytes(&bytes, &ext_of(path), max_side)
}

/// As `decode`, from the file's bytes; `ext` names the format.
pub fn decode_bytes(bytes: &[u8], ext: &str, max_side: u32) -> anyhow::Result<Rgba> {
    match ext {
        "jxl" => jxl::decode(bytes, max_side),
        "avif" => avif::decode(bytes, max_side),
        "psd" | "psb" => psd::decode(bytes, max_side),
        e if is_raw_ext(e) => raw::decode_bytes(bytes, max_side),
        _ => anyhow::bail!("not a picture format decoded here"),
    }
}

/// The size of the picture as shown (turned upright), from the file's header only.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let ext = ext_of(path);
    if is_raw_ext(&ext) {
        return raw::dimensions(path);
    }
    if is_psd_ext(&ext) {
        return psd::dimensions(path);
    }
    if std::fs::metadata(path).ok()?.len() > MAX_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    match ext.as_str() {
        "jxl" => jxl::dimensions(&bytes),
        "avif" => avif::dimensions(&bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 x 80: red top-left quarter, green bottom-right quarter, blue elsewhere.
    fn check(file: &str) {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(file);
        let full = decode(&p, 4096).unwrap();
        assert_eq!((full.width, full.height), (120, 80));
        let px = |r: &Rgba, x: u32, y: u32| {
            let i = ((y * r.width + x) * 4) as usize;
            (r.data[i], r.data[i + 1], r.data[i + 2], r.data[i + 3])
        };
        let near = |a: (u8, u8, u8, u8), b: (u8, u8, u8)| a.0.abs_diff(b.0) < 40 && a.1.abs_diff(b.1) < 40 && a.2.abs_diff(b.2) < 40 && a.3 == 255;
        assert!(near(px(&full, 20, 10), (255, 0, 0)), "{:?}", px(&full, 20, 10));
        assert!(near(px(&full, 100, 70), (0, 255, 0)), "{:?}", px(&full, 100, 70));
        assert!(near(px(&full, 100, 10), (30, 60, 160)), "{:?}", px(&full, 100, 10));
        let small = decode(&p, 60).unwrap();
        assert_eq!((small.width, small.height, small.full), (60, 40, (120, 80)));
        assert_eq!(dimensions(&p), Some((120, 80)));
    }

    #[test]
    fn jpeg_xl_decodes() {
        check("quad.jxl");
    }

    #[test]
    fn avif_decodes() {
        check("quad.avif");
    }
}
