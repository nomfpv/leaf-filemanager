//! Linux backend of rf_win::look::decode. Status: REAL for pictures
//!
//! Pictures are decoded in this process: the `image` crate (PNG, JPEG, GIF incl. animation, WebP incl. animation, BMP, ICO, TIFF,
//! TGA, QOI, EXR, HDR, DDS, PNM) and `leaf-decode` (JPEG XL, AVIF). EXIF orientation is applied. Not here: SVG, RAW, PSD, design
//! files, HEIC (each answers an error, and the viewer shows its "could not be opened" line).

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, RgbaImage};

use crate::thumbs::Bgra;

/// When a photo was taken (EXIF): plain file reading, shared with Windows.
pub use leaf_common::photo::exif_taken_ms;

// TYPE-COPY(rf-win src/look/decode.rs:13)
pub struct Decoded {
    pub frames: Vec<(Bgra, u32)>,
    /// EXIF orientation (1 = upright). The frames come already turned.
    pub orientation: u8,
    pub exif: Vec<(String, String)>,
    /// The picture's own size (turned upright, not reduced).
    pub width: u32,
    pub height: u32,
}

/// Larger files than this are not read into memory for a picture.
const MAX_FILE: u64 = 400 * 1024 * 1024;

fn to_bgra(img: RgbaImage) -> Bgra {
    let (width, height) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Bgra { width, height, data }
}

fn shrink(img: RgbaImage, max_side: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let m = w.max(h);
    if m <= max_side || m == 0 || max_side == 0 {
        return img;
    }
    let s = max_side as f64 / m as f64;
    let (nw, nh) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
    image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
}

fn exif_fields(raw: &[u8]) -> (u8, Vec<(String, String)>) {
    let Ok(ex) = exif::Reader::new().read_raw(raw.to_vec()) else { return (1, Vec::new()) };
    let orientation = ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).map_or(1, |v| v.clamp(1, 8) as u8);
    let fields = ex
        .fields()
        .filter(|f| !matches!(f.tag, exif::Tag::MakerNote | exif::Tag::UserComment) && f.tag.description().is_some())
        .map(|f| (f.tag.to_string(), f.display_value().with_unit(&ex).to_string().trim_matches('"').to_string()))
        .filter(|(_, v)| !v.is_empty() && v.len() < 200)
        .collect();
    (orientation, fields)
}

fn orient(img: DynamicImage, o: u8) -> DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn frames_of(frames: image::Frames<'_>, max_side: u32) -> anyhow::Result<Vec<(Bgra, u32)>> {
    let mut out = Vec::new();
    for f in frames {
        let f = f?;
        let (n, d) = f.delay().numer_denom_ms();
        let ms = if d == 0 { 100 } else { (n / d).max(20) };
        out.push((to_bgra(shrink(f.into_buffer(), max_side)), ms));
    }
    Ok(out)
}

/// An animated GIF / APNG / WebP: its frames when there are several.
fn animated(bytes: &[u8], format: image::ImageFormat, max_side: u32) -> Option<Vec<(Bgra, u32)>> {
    use image::codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder};
    let frames = match format {
        image::ImageFormat::Gif => frames_of(GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames(), max_side).ok()?,
        image::ImageFormat::Png => {
            let d = PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.is_apng().ok()? {
                return None;
            }
            frames_of(d.apng().ok()?.into_frames(), max_side).ok()?
        }
        image::ImageFormat::WebP => {
            let d = WebPDecoder::new(Cursor::new(bytes)).ok()?;
            if !d.has_animation() {
                return None;
            }
            frames_of(d.into_frames(), max_side).ok()?
        }
        _ => return None,
    };
    (frames.len() > 1).then_some(frames)
}

fn with_image_crate(bytes: &[u8], max_side: u32) -> anyhow::Result<Decoded> {
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format().ok_or_else(|| anyhow!("unknown format"))?;
    if let Some(frames) = animated(bytes, format, max_side) {
        let (width, height) = (frames[0].0.width, frames[0].0.height);
        return Ok(Decoded { frames, orientation: 1, exif: Vec::new(), width, height });
    }
    let mut dec = reader.into_decoder()?;
    let (orientation, exif) = dec.exif_metadata().ok().flatten().map(|r| exif_fields(&r)).unwrap_or((1, Vec::new()));
    let img = orient(DynamicImage::from_decoder(dec)?, orientation);
    let (width, height) = (img.width(), img.height());
    Ok(Decoded { frames: vec![(to_bgra(shrink(img.into_rgba8(), max_side)), 0)], orientation, exif, width, height })
}

fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// The picture's size as shown (turned upright), from the file's header only; `None` when this backend cannot read it.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let ext = ext_of(path);
    if leaf_decode::handles_ext(&ext) {
        return leaf_decode::dimensions(path);
    }
    let r = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut d = r.into_decoder().ok()?;
    let (w, h) = d.dimensions();
    let o = d.exif_metadata().ok().flatten().map_or(1, |raw| exif_fields(&raw).0);
    Some(if (5..=8).contains(&o) { (h, w) } else { (w, h) })
}

/// The picture with its long side at most `max_side`: the image crate first, then `leaf-decode` for JPEG XL and AVIF.
pub fn decode_full(path: &Path, max_side: u32) -> anyhow::Result<Decoded> {
    let ext = ext_of(path);
    let len = std::fs::metadata(path).with_context(|| path.display().to_string())?.len();
    if len > MAX_FILE {
        bail!("file too large for a picture: {len} bytes");
    }
    if leaf_decode::handles_ext(&ext) {
        let img = leaf_decode::decode(path, max_side).map_err(|e| anyhow!("{}: {e}", ext.to_ascii_uppercase()))?;
        let (width, height) = img.full;
        return Ok(Decoded { frames: vec![(crate::picture_fallback::bgra(img), 0)], orientation: 1, exif: Vec::new(), width, height });
    }
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    with_image_crate(&bytes, max_side)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_decodes_and_shrinks() {
        let dir = std::env::temp_dir().join(format!("leaf-decode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.png");
        let mut img = RgbaImage::new(40, 20);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 0, 0, 255]);
        }
        img.save(&p).unwrap();
        assert_eq!(dimensions(&p), Some((40, 20)));
        let d = decode_full(&p, 10).unwrap();
        assert_eq!((d.width, d.height), (40, 20));
        let (b, _) = &d.frames[0];
        assert_eq!((b.width, b.height), (10, 5));
        assert_eq!(&b.data[0..4], &[0, 0, 255, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
