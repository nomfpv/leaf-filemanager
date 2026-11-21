//! Photoshop files: the flattened picture the file keeps (`psd` crate, MIT/Apache-2.0).

use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail};

use crate::Rgba;

/// A picture of more pixels than this is not decoded (an RGBA buffer of four bytes each).
const MAX_PIXELS: u64 = 80_000_000;

/// The size from the 26-byte header: "8BPS", version, six reserved bytes, channels, then height and width.
fn header_size(h: &[u8]) -> Option<(u32, u32)> {
    if h.len() < 26 || &h[0..4] != b"8BPS" {
        return None;
    }
    let (height, width) = (u32::from_be_bytes(h[14..18].try_into().ok()?), u32::from_be_bytes(h[18..22].try_into().ok()?));
    (width > 0 && height > 0).then_some((width, height))
}

pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut h = [0u8; 26];
    std::fs::File::open(path).ok()?.read_exact(&mut h).ok()?;
    header_size(&h)
}

pub fn decode(bytes: &[u8], max_side: u32) -> anyhow::Result<Rgba> {
    let (w, h) = header_size(bytes).ok_or_else(|| anyhow!("psd: not a Photoshop file"))?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        bail!("psd: picture too large");
    }
    // a damaged file must not take the worker down
    let rgba = std::panic::catch_unwind(|| psd::Psd::from_bytes(bytes).map(|p| (p.width(), p.height(), p.rgba())))
        .map_err(|_| anyhow!("psd: unreadable"))?
        .map_err(|e| anyhow!("psd: {e}"))?;
    let img = image::RgbaImage::from_raw(rgba.0, rgba.1, rgba.2).ok_or_else(|| anyhow!("psd: pixels"))?;
    let full = img.dimensions();
    let img = crate::shrink(img, max_side);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: 1, exif: Vec::new() })
}
