//! Camera RAW development: the sensor data decoded by `rawler` (pure Rust) and turned into an sRGB picture.
//!
//! Policy (one place, here):
//! - thumbnails, the preview pane and the viewer's first picture: the JPEG the camera embedded (`raw::decode`); a file
//!   without a usable one is developed (`raw::decode` falls back to `develop`);
//! - the Space viewer then asks `wants_develop`: true when the embedded JPEG's long side is under half the sensor's, and
//!   shows `decode_developed` in its place when it is ready;
//! - the result is at most `max_side` on its long side; when that is at most half the sensor's, a Bayer sensor is
//!   binned 2x2 (one RGB pixel per 2x2 cell: no demosaic, a quarter of the memory), else `rawler`'s demosaic runs
//!   (PPG for Bayer, bilinear for X-Trans).
//!
//! Steps: black and white levels, demosaic, the camera's as-shot white balance and the camera-to-sRGB matrix (all `rawler`'s),
//! then here a soft highlight knee and the sRGB curve (a lookup table), the file's orientation, the size cap.
//! Untrusted files: the file size and the sensor's pixel count are capped before anything is allocated for them, and every
//! step runs under `catch_unwind` (the decoder library has `todo!()` / `assert!` on unusual files).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use anyhow::{Context, bail};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::{Dim2, Point, Rect};
use rawler::pixarray::Color2D;
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use rawler::{RawImage, global_loader};

use crate::Rgba;

/// Files larger than this are not developed.
const MAX_FILE: u64 = 400 << 20;
/// A sensor with more pixels than this is not developed (a hostile header never asks for a huge buffer).
const MAX_SENSOR_PIXELS: usize = 150_000_000;
/// Above this many pixels the full-size demosaic is not run (its working memory is some 25 bytes a pixel): binned instead.
const MAX_FULL_PIXELS: usize = 64_000_000;
/// The knee of the highlight roll-off (linear light): below it the picture is untouched.
const KNEE: f32 = 0.8;
/// Lookup table: linear values 0..2 in `LUT_SCALE` steps per 1.0.
const LUT_SCALE: f32 = 8192.0;

fn open(path: &Path) -> anyhow::Result<RawSource> {
    let len = std::fs::metadata(path).with_context(|| path.display().to_string())?.len();
    anyhow::ensure!(len <= MAX_FILE, "file too large for a picture: {len} bytes");
    RawSource::new(path).with_context(|| path.display().to_string())
}

fn guarded<T>(f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => bail!("raw development failed on this file"),
    }
}

/// The picture area of the sensor: the camera's recommended crop, else its active area, else everything.
fn picture_area(raw: &RawImage) -> Rect {
    let full = Rect::new(Point::new(0, 0), Dim2::new(raw.width, raw.height));
    let r = raw.crop_area.or(raw.active_area).unwrap_or(full);
    if r.d.w == 0 || r.d.h == 0 || r.p.x.saturating_add(r.d.w) > raw.width || r.p.y.saturating_add(r.d.h) > raw.height { full } else { r }
}

/// The sensor's picture size, from the file's header (the sensor data is not decompressed): (width, height), not turned.
pub(crate) fn sensor_size(path: &Path) -> Option<(u32, u32)> {
    guarded(|| {
        let src = open(path)?;
        let raw = global_loader().decode(&src, &RawDecodeParams::default(), true).map_err(|e| anyhow::anyhow!("{e}"))?;
        let a = picture_area(&raw);
        Ok((a.d.w as u32, a.d.h as u32))
    })
    .ok()
}

/// Whether the viewer should develop this file after showing the embedded JPEG: the JPEG is under half the sensor's size.
pub(crate) fn wants_develop(path: &Path) -> bool {
    let Some((ew, eh)) = crate::raw::dimensions(path) else { return false };
    let Some((sw, sh)) = sensor_size(path) else { return false };
    u64::from(ew.max(eh)) * 2 < u64::from(sw.max(sh))
}

/// The tone curve as a table: linear light in, an 8-bit sRGB value out. A soft knee above `KNEE` (the slope stays 1 at the
/// knee, the curve flattens towards 1.0), then the sRGB transfer function.
fn lut() -> Vec<u8> {
    let n = (LUT_SCALE * 2.0) as usize + 1;
    (0..n)
        .map(|i| {
            let mut v = i as f32 / LUT_SCALE;
            if v > KNEE {
                let k = 1.0 - KNEE;
                v = KNEE + k * (1.0 - (-(v - KNEE) / k).exp());
            }
            let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        })
        .collect()
}

fn to_rgba(px: &[[f32; 3]], w: u32, h: u32) -> anyhow::Result<image::RgbaImage> {
    let t = lut();
    let top = t.len() - 1;
    let mut out = Vec::with_capacity(px.len() * 4);
    for p in px {
        for c in p {
            let i = if c.is_nan() { 0 } else { ((c.max(0.0) * LUT_SCALE) as usize).min(top) };
            out.push(t[i]);
        }
        out.push(255);
    }
    image::RgbaImage::from_raw(w, h, out).context("developed picture size")
}

/// One RGB pixel per 2x2 cell of a Bayer sensor, black and white levels applied, in camera colour (white balance and the
/// matrix follow). `None`: not a 2x2 RGB pattern or not plain integer / float samples.
fn bin(raw: &RawImage, area: Rect) -> Option<(Vec<f32>, usize, usize)> {
    let RawPhotometricInterpretation::Cfa(cfg) = &raw.photometric else { return None };
    if raw.cpp != 1 || cfg.cfa.width != 2 || cfg.cfa.height != 2 || !cfg.cfa.is_rgb() {
        return None;
    }
    let (x0, y0) = (area.p.x + (area.p.x & 1), area.p.y + (area.p.y & 1));
    let w = (area.p.x + area.d.w).saturating_sub(x0) / 2;
    let h = (area.p.y + area.d.h).saturating_sub(y0) / 2;
    let have = match &raw.data {
        RawImageData::Integer(d) => d.len(),
        RawImageData::Float(d) => d.len(),
    };
    if w == 0 || h == 0 || have < raw.width * raw.height {
        return None;
    }
    let (black, white) = (raw.blacklevel.as_bayer_array(), raw.whitelevel.as_bayer_array());
    let scale: [f32; 4] = std::array::from_fn(|i| 1.0 / (white[i] - black[i]).max(1.0));
    let mut out = Vec::with_capacity(w * h * 3);
    let sample = |r: usize, c: usize| -> f32 {
        let i = r * raw.width + c;
        match &raw.data {
            RawImageData::Integer(d) => f32::from(d[i]),
            RawImageData::Float(d) => d[i],
        }
    };
    for oy in 0..h {
        for ox in 0..w {
            let (mut sum, mut cnt) = ([0f32; 3], [0f32; 3]);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (r, c) = (y0 + oy * 2 + dy, x0 + ox * 2 + dx);
                let k = (r & 1) * 2 + (c & 1);
                let ch = cfg.cfa.color_at(r, c).min(2);
                sum[ch] += ((sample(r, c) - black[k]).max(0.0) * scale[k]).min(1.0);
                cnt[ch] += 1.0;
            }
            out.extend((0..3).map(|i| sum[i] / cnt[i].max(1.0)));
        }
    }
    Some((out, w, h))
}

fn orient(img: image::RgbaImage, o: u16) -> image::RgbaImage {
    crate::raw::orient(image::DynamicImage::ImageRgba8(img), o as u8).into_rgba8()
}

/// The camera RAW file at `path` developed to an sRGB picture, its long side at most `max_side`.
pub(crate) fn develop(path: &Path, max_side: u32) -> anyhow::Result<Rgba> {
    let mut rgba = guarded(|| develop_inner(path, max_side))?;
    rgba.exif = crate::raw::exif_rows_of(path);
    rgba.exif.push(("Developed".into(), "by leaf".into()));
    Ok(rgba)
}

fn develop_inner(path: &Path, max_side: u32) -> anyhow::Result<Rgba> {
    let src = open(path)?;
    let mut raw = rawler::decode(&src, &RawDecodeParams::default()).map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(src);
    anyhow::ensure!(raw.width > 0 && raw.height > 0 && raw.width.saturating_mul(raw.height) <= MAX_SENSOR_PIXELS, "sensor size not supported");
    let area = picture_area(&raw);
    let long = area.d.w.max(area.d.h);
    let want_half = (max_side as usize).saturating_mul(2) <= long || area.d.w * area.d.h > MAX_FULL_PIXELS;
    let (orientation, (w, h)) = (raw.orientation.to_u16().max(1), (area.d.w as u32, area.d.h as u32));

    let (px, pw, ph): (Vec<[f32; 3]>, usize, usize) = match want_half.then(|| bin(&raw, area)).flatten() {
        Some((data, bw, bh)) => {
            // the binned data is a small 3-colour picture: white balance and the matrix are applied by the library
            raw.data = RawImageData::Float(data);
            (raw.width, raw.height, raw.cpp) = (bw, bh, 3);
            raw.photometric = RawPhotometricInterpretation::LinearRaw;
            raw.active_area = None;
            raw.crop_area = None;
            let dev = RawDevelop::new_with(&[ProcessingStep::Calibrate]);
            three(dev.develop_intermediate(&raw))?
        }
        None => {
            anyhow::ensure!(area.d.w * area.d.h <= MAX_FULL_PIXELS, "picture too large to develop here");
            let dev = RawDevelop::new_with(&[
                ProcessingStep::Rescale,
                ProcessingStep::Demosaic,
                ProcessingStep::FujiRotate,
                ProcessingStep::CropActiveArea,
                ProcessingStep::WhiteBalance,
                ProcessingStep::Calibrate,
                ProcessingStep::CropDefault,
            ]);
            three(dev.develop_intermediate(&raw))?
        }
    };
    drop(raw);
    let img = crate::shrink(to_rgba(&px, pw as u32, ph as u32)?, max_side);
    drop(px);
    let img = orient(img, orientation);
    let full = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: orientation as u8, exif: Vec::new() })
}

/// Pixels (linear, 3 colours) of the library's result.
fn three(r: rawler::Result<Intermediate>) -> anyhow::Result<(Vec<[f32; 3]>, usize, usize)> {
    match r.map_err(|e| anyhow::anyhow!("{e}"))? {
        Intermediate::ThreeColor(c) => Ok(unwrap3(c)),
        Intermediate::Monochrome(m) => {
            let (w, h) = (m.width, m.height);
            Ok((m.data.iter().map(|v| [*v; 3]).collect(), w, h))
        }
        Intermediate::FourColor(_) => bail!("four-colour sensor not supported"),
    }
}

fn unwrap3(c: Color2D<f32, 3>) -> (Vec<[f32; 3]>, usize, usize) {
    let (w, h) = (c.width, c.height);
    (c.into_inner(), w, h)
}
