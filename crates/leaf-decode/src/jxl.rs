//! JPEG XL through `jxl-oxide` (MIT OR Apache-2.0, pure Rust).

use jxl_oxide::{EnumColourEncoding, JxlImage, RenderingIntent};

use crate::Rgba;

pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let img = JxlImage::builder().read(std::io::Cursor::new(bytes)).ok()?;
    let o = img.image_header().metadata.orientation;
    let (w, h) = (img.width(), img.height());
    Some(if o >= 5 { (h, w) } else { (w, h) })
}

pub fn decode(bytes: &[u8], max_side: u32) -> anyhow::Result<Rgba> {
    let mut img = JxlImage::builder().read(std::io::Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("jxl: {e}"))?;
    // a picture of too many pixels is not rendered (a broken header never asks for a huge buffer)
    anyhow::ensure!(u64::from(img.width()) * u64::from(img.height()) <= 100_000_000, "jxl: picture too large");
    // the screen's colours; an image that cannot be converted without a colour engine keeps its own
    img.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
    let render = img.render_frame(0).map_err(|e| anyhow::anyhow!("jxl: {e}"))?;
    let mut stream = render.stream();
    let (w, h, ch) = (stream.width(), stream.height(), stream.channels() as usize);
    anyhow::ensure!(w > 0 && h > 0 && (1..=4).contains(&ch), "jxl: unusable picture");
    let mut src = vec![0u8; w as usize * h as usize * ch];
    stream.write_to_buffer(&mut src);
    let mut data = Vec::with_capacity(w as usize * h as usize * 4);
    for px in src.chunks_exact(ch) {
        match ch {
            1 => data.extend_from_slice(&[px[0], px[0], px[0], 255]),
            2 => data.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
            3 => data.extend_from_slice(&[px[0], px[1], px[2], 255]),
            _ => data.extend_from_slice(&[px[0], px[1], px[2], px[3]]),
        }
    }
    let img = image::RgbaImage::from_raw(w, h, data).ok_or_else(|| anyhow::anyhow!("jxl: size"))?;
    let img = crate::shrink(img, max_side);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full: (w, h), orientation: 1, exif: Vec::new() })
}
