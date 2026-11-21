//! HEIC / HEIF (phone photos) through `heifer` (MIT OR Apache-2.0, pure Rust). On Windows the system's own codec is asked first
//! and this decoder only answers when it cannot; on Linux it is the only one.

use anyhow::{anyhow, ensure};
use heifer::heifer_isobmff::{HeifFile, boxes::Property};

use crate::Rgba;

/// The most pixels of any picture decoded (a 200 MP phone photo is about 2^27).
const MAX_PIXELS: u64 = 1 << 27;

/// The picture's size as shown (the rotation applied), from the file's headers only.
fn shown_size(file: &HeifFile<'_>) -> Option<(u32, u32)> {
    let id = file.primary_id;
    let (w, h) = file.image_size(id).ok().flatten()?;
    let turned = file.item_properties(id).ok()?.any(|p| matches!(p, Property::Rotation(r) if r % 2 == 1));
    Some(if turned { (h, w) } else { (w, h) })
}

pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    shown_size(&HeifFile::parse(bytes).ok()?)
}

pub fn decode(bytes: &[u8], max_side: u32) -> anyhow::Result<Rgba> {
    let file = HeifFile::parse(bytes).map_err(|e| anyhow!("heic: {e}"))?;
    let size = shown_size(&file);
    if let Some((w, h)) = size {
        ensure!(u64::from(w) * u64::from(h) <= MAX_PIXELS, "heic: picture too large");
    }
    let rows = exif_rows(bytes);
    // the small picture most phone files carry, when it is big enough for what is asked: far quicker than decoding the photo
    if let Ok(Some(t)) = heifer::decode_thumbnail(bytes)
        && t.width.max(t.height) >= max_side
        && let Some(full) = size
        && let Some(img) = image::RgbaImage::from_raw(t.width, t.height, t.to_rgba8())
    {
        let img = crate::shrink(img, max_side);
        return Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: 1, exif: rows });
    }
    // two threads for the tiles: several thumbnail workers decode at once and the window keeps its cores
    let opts = heifer::Options::new().with_max_threads(2).with_max_pixels(MAX_PIXELS);
    let img = heifer::decode_srgb_rgba8(bytes, &opts).map_err(|e| anyhow!("heic: {e}"))?;
    let full = (img.width, img.height);
    let img = image::RgbaImage::from_raw(img.width, img.height, img.data).ok_or_else(|| anyhow!("heic: pixels"))?;
    let img = crate::shrink(img, max_side);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: 1, exif: rows })
}

fn exif_rows(bytes: &[u8]) -> Vec<(String, String)> {
    heifer::metadata::read_metadata(bytes).ok().and_then(|m| m.exif).map(|raw| crate::raw::exif_rows(&raw).1).unwrap_or_default()
}
