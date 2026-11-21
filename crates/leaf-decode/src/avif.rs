//! AVIF: a small reader of the HEIF container (the one picture and its alpha plane, turned and mirrored as
//! the file says), and the AV1 pictures inside decoded by `rav1d` (BSD-2-Clause, a Rust port of dav1d).
//! Grid images (a picture cut into tiles) and animations are not read: they are an error, not a wrong picture.

use anyhow::{Context, Result, anyhow, bail, ensure};
use rav1d::include::dav1d::common::Dav1dDataProps;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{dav1d_close, dav1d_data_create, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data};
use std::collections::HashMap;
use std::ptr::NonNull;

use crate::Rgba;

// ---------------------------------------------------------------------------------------------- container

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.d.len()).ok_or_else(|| anyhow!("truncated"))?;
        let s = &self.d[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn uint(&mut self, n: usize) -> Result<u64> {
        Ok(self.take(n)?.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.uint(1)? as u8)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(self.uint(2)? as u16)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(self.uint(4)? as u32)
    }
    fn left(&self) -> usize {
        self.d.len() - self.at
    }
}

/// The boxes at one level: (type, payload).
fn boxes(d: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut r = Reader { d, at: 0 };
    let mut out = Vec::new();
    while r.left() >= 8 {
        let size = u64::from(r.u32()?);
        let ty: [u8; 4] = r.take(4)?.try_into()?;
        let len = match size {
            0 => r.left() as u64,
            1 => r.uint(8)?.checked_sub(16).ok_or_else(|| anyhow!("box size"))?,
            n => n.checked_sub(8).ok_or_else(|| anyhow!("box size"))?,
        };
        ensure!(len as usize <= r.left(), "box past the end");
        out.push((ty, r.take(len as usize)?));
    }
    Ok(out)
}

#[derive(Clone)]
enum Prop {
    Size(u32, u32),
    Aux(String),
    Rotate(u8),
    Mirror(u8),
    Other,
}

#[derive(Default)]
struct Item {
    ty: [u8; 4],
    /// (offset, length) of each piece in the file (or in `idat`)
    extents: Vec<(u64, u64)>,
    in_idat: bool,
    props: Vec<usize>,
}

struct Heif<'a> {
    file: &'a [u8],
    idat: &'a [u8],
    primary: u32,
    items: HashMap<u32, Item>,
    props: Vec<Prop>,
    /// (from, to) of every "auxl" reference
    aux: Vec<(u32, u32)>,
}

fn parse(file: &[u8]) -> Result<Heif<'_>> {
    let top = boxes(file)?;
    let meta = top.iter().find(|(t, _)| t == b"meta").context("no meta box")?.1;
    ensure!(meta.len() > 4, "short meta");
    let mut h = Heif { file, idat: &[], primary: 0, items: HashMap::new(), props: Vec::new(), aux: Vec::new() };
    let mut ipma: &[u8] = &[];
    let mut have_primary = false;
    for (t, p) in boxes(&meta[4..])? {
        match &t {
            b"pitm" => {
                let mut r = Reader { d: p, at: 0 };
                let v = r.u8()?;
                r.take(3)?;
                h.primary = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
                have_primary = true;
            }
            b"idat" => h.idat = p,
            b"iinf" => {
                let mut r = Reader { d: p, at: 0 };
                let v = r.u8()?;
                r.take(3)?;
                if v == 0 {
                    r.u16()?;
                } else {
                    r.u32()?;
                }
                for (t, ip) in boxes(&p[r.at..])? {
                    if &t != b"infe" {
                        continue;
                    }
                    let mut r = Reader { d: ip, at: 0 };
                    let v = r.u8()?;
                    r.take(3)?;
                    if v < 2 {
                        continue;
                    }
                    let id = if v == 2 { u32::from(r.u16()?) } else { r.u32()? };
                    r.u16()?;
                    let ty: [u8; 4] = r.take(4)?.try_into()?;
                    h.items.entry(id).or_default().ty = ty;
                }
            }
            b"iloc" => {
                let mut r = Reader { d: p, at: 0 };
                let v = r.u8()?;
                r.take(3)?;
                let a = r.u8()?;
                let b = r.u8()?;
                let (osz, lsz, bsz) = (usize::from(a >> 4), usize::from(a & 15), usize::from(b >> 4));
                let isz = if v == 1 || v == 2 { usize::from(b & 15) } else { 0 };
                let count = if v < 2 { u32::from(r.u16()?) } else { r.u32()? };
                for _ in 0..count {
                    let id = if v < 2 { u32::from(r.u16()?) } else { r.u32()? };
                    let method = if v == 1 || v == 2 { r.u16()? & 15 } else { 0 };
                    r.u16()?;
                    let base = r.uint(bsz)?;
                    let n = r.u16()?;
                    let it = h.items.entry(id).or_default();
                    it.in_idat = method == 1;
                    ensure!(method <= 1, "unsupported item storage");
                    for _ in 0..n {
                        r.uint(isz)?;
                        let off = r.uint(osz)?;
                        let len = r.uint(lsz)?;
                        it.extents.push((base.checked_add(off).ok_or_else(|| anyhow!("offset"))?, len));
                    }
                }
            }
            b"iprp" => {
                for (t, ip) in boxes(p)? {
                    match &t {
                        b"ipco" => {
                            for (pt, pp) in boxes(ip)? {
                                h.props.push(match &pt {
                                    b"ispe" if pp.len() >= 12 => {
                                        Prop::Size(u32::from_be_bytes(pp[4..8].try_into()?), u32::from_be_bytes(pp[8..12].try_into()?))
                                    }
                                    b"auxC" if pp.len() > 4 => {
                                        let s = &pp[4..];
                                        Prop::Aux(String::from_utf8_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())]).into_owned())
                                    }
                                    b"irot" if !pp.is_empty() => Prop::Rotate(pp[0] & 3),
                                    b"imir" if !pp.is_empty() => Prop::Mirror(pp[0] & 1),
                                    _ => Prop::Other,
                                });
                            }
                        }
                        b"ipma" => ipma = ip,
                        _ => {}
                    }
                }
            }
            b"iref" => {
                let mut r = Reader { d: p, at: 0 };
                let v = r.u8()?;
                r.take(3)?;
                for (t, rp) in boxes(&p[r.at..])? {
                    if &t != b"auxl" {
                        continue;
                    }
                    let mut r = Reader { d: rp, at: 0 };
                    let from = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
                    let n = r.u16()?;
                    for _ in 0..n {
                        let to = if v == 0 { u32::from(r.u16()?) } else { r.u32()? };
                        h.aux.push((from, to));
                    }
                }
            }
            _ => {}
        }
    }
    ensure!(have_primary, "no primary picture");
    if !ipma.is_empty() {
        let mut r = Reader { d: ipma, at: 0 };
        let v = r.u8()?;
        let flags = r.uint(3)?;
        let n = r.u32()?;
        for _ in 0..n {
            let id = if v < 1 { u32::from(r.u16()?) } else { r.u32()? };
            let k = r.u8()?;
            for _ in 0..k {
                let idx = if flags & 1 == 1 { r.u16()? & 0x7fff } else { u16::from(r.u8()? & 0x7f) };
                if idx > 0 {
                    h.items.entry(id).or_default().props.push(usize::from(idx) - 1);
                }
            }
        }
    }
    Ok(h)
}

impl Heif<'_> {
    fn data(&self, id: u32) -> Result<Vec<u8>> {
        let it = self.items.get(&id).context("missing item")?;
        let src = if it.in_idat { self.idat } else { self.file };
        let mut out = Vec::new();
        for &(off, len) in &it.extents {
            let (off, len) = (off as usize, len as usize);
            let end = off.checked_add(len).filter(|e| *e <= src.len()).ok_or_else(|| anyhow!("item past the end"))?;
            out.extend_from_slice(&src[off..end]);
        }
        Ok(out)
    }

    fn alpha(&self) -> Option<u32> {
        self.aux.iter().find_map(|&(from, to)| {
            let is_alpha = to == self.primary
                && self.items.get(&from).is_some_and(|it| {
                    &it.ty == b"av01" && it.props.iter().any(|&p| matches!(self.props.get(p), Some(Prop::Aux(s)) if s.contains("alpha")))
                });
            is_alpha.then_some(from)
        })
    }

    /// The stored size of the primary picture and the turns / mirrors that put it upright (in the file's order).
    fn geometry(&self) -> Result<((u32, u32), Vec<Prop>)> {
        let it = self.items.get(&self.primary).context("missing primary item")?;
        if &it.ty == b"grid" {
            bail!("an AVIF picture made of tiles is not supported");
        }
        ensure!(&it.ty == b"av01", "not an AV1 picture");
        let mut size = None;
        let mut ops = Vec::new();
        for &p in &it.props {
            match self.props.get(p) {
                Some(Prop::Size(w, h)) => size = Some((*w, *h)),
                Some(p @ (Prop::Rotate(_) | Prop::Mirror(_))) => ops.push(p.clone()),
                _ => {}
            }
        }
        Ok((size.context("no size")?, ops))
    }
}

fn upright_size((w, h): (u32, u32), ops: &[Prop]) -> (u32, u32) {
    ops.iter().fold((w, h), |(w, h), op| if matches!(op, Prop::Rotate(a) if a % 2 == 1) { (h, w) } else { (w, h) })
}

pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let h = parse(bytes).ok()?;
    let (size, ops) = h.geometry().ok()?;
    Some(upright_size(size, &ops))
}

// ---------------------------------------------------------------------------------------------- AV1

/// One decoded AV1 picture's planes, copied out (the decoder's own are released at once).
struct Planes {
    w: usize,
    h: usize,
    layout: u32,
    bpc: u32,
    full_range: bool,
    matrix: u32,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    cw: usize,
}

struct Decoder(Option<Dav1dContext>);

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { dav1d_close(NonNull::new(&mut self.0 as *mut _)) };
    }
}

const EAGAIN: i32 = -11;

fn av1(data: &[u8]) -> Result<Planes> {
    unsafe {
        let mut s = std::mem::MaybeUninit::<Dav1dSettings>::uninit();
        dav1d_default_settings(NonNull::new(s.as_mut_ptr()).unwrap());
        let mut s = s.assume_init();
        // one thread: pictures are made by several workers at once, and a thumbnail is small
        s.n_threads = 1;
        s.max_frame_delay = 1;
        let mut dec = Decoder(None);
        let r = dav1d_open(NonNull::new(&mut dec.0 as *mut _), NonNull::new(&mut s as *mut _));
        ensure!(r.0 == 0, "AV1 decoder did not start ({})", r.0);
        let mut d = Dav1dData { data: None, sz: 0, r#ref: None, m: Dav1dDataProps { timestamp: i64::MIN, duration: 0, offset: -1, size: 0, user_data: Default::default() } };
        let p = dav1d_data_create(NonNull::new(&mut d as *mut _), data.len());
        ensure!(!p.is_null(), "out of memory");
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let mut pic = Dav1dPicture::default();
        loop {
            if d.sz > 0 {
                let r = dav1d_send_data(dec.0.clone(), NonNull::new(&mut d as *mut _));
                if r.0 < 0 && r.0 != EAGAIN {
                    bail!("AV1 data not accepted ({})", r.0);
                }
            }
            let r = dav1d_get_picture(dec.0.clone(), NonNull::new(&mut pic as *mut _));
            if r.0 == 0 {
                break;
            }
            if r.0 != EAGAIN {
                bail!("AV1 picture not decoded ({})", r.0);
            }
            ensure!(d.sz > 0, "AV1 data holds no picture");
        }
        let out = copy_planes(&pic);
        dav1d_picture_unref(NonNull::new(&mut pic as *mut _));
        out
    }
}

unsafe fn copy_planes(pic: &Dav1dPicture) -> Result<Planes> {
    let (w, h) = (pic.p.w as usize, pic.p.h as usize);
    let (layout, bpc) = (pic.p.layout, pic.p.bpc as u32);
    ensure!(w > 0 && h > 0 && (8..=16).contains(&bpc), "unusable AV1 picture");
    let hdr = pic.seq_hdr.context("no sequence header")?;
    let (full_range, matrix) = unsafe { (hdr.as_ref().color_range != 0, hdr.as_ref().mtrx) };
    let (cw, ch) = match layout {
        0 => (0, 0),
        1 => (w.div_ceil(2), h.div_ceil(2)),
        2 => (w.div_ceil(2), h),
        _ => (w, h),
    };
    let plane = |idx: usize, pw: usize, ph: usize| -> Result<Vec<u16>> {
        let Some(base) = pic.data[idx] else { return Ok(Vec::new()) };
        let stride = pic.stride[usize::from(idx > 0)];
        let mut out = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            let line = unsafe { (base.as_ptr() as *const u8).offset(row as isize * stride) };
            if bpc == 8 {
                out.extend(unsafe { std::slice::from_raw_parts(line, pw) }.iter().map(|v| u16::from(*v)));
            } else {
                out.extend((0..pw).map(|x| unsafe { std::ptr::read_unaligned((line as *const u16).add(x)) }));
            }
        }
        Ok(out)
    };
    Ok(Planes { w, h, layout, bpc, full_range, matrix, y: plane(0, w, h)?, u: plane(1, cw, ch)?, v: plane(2, cw, ch)?, cw })
}

// ---------------------------------------------------------------------------------------------- colour

fn to_rgb(p: &Planes) -> Vec<u8> {
    let max = ((1u32 << p.bpc) - 1) as f32;
    let shift = p.bpc - 8;
    let (ymin, yscale, cmid, cscale) = if p.full_range {
        (0.0, max, (1u32 << (p.bpc - 1)) as f32, max)
    } else {
        ((16u32 << shift) as f32, (219u32 << shift) as f32, (128u32 << shift) as f32, (224u32 << shift) as f32)
    };
    // matrix 0 (identity) in 4:4:4: the three planes are G, B, R
    let identity = p.matrix == 0 && p.layout == 3;
    let (kr, kb) = match p.matrix {
        2 | 4 | 5 | 6 => (0.299f32, 0.114f32),
        9 | 10 => (0.2627, 0.0593),
        7 => (0.212, 0.087),
        _ => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    let (cr_r, cb_b) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
    let (cb_g, cr_g) = (-cb_b * kb / kg, -cr_r * kr / kg);
    let mut out = vec![0u8; p.w * p.h * 3];
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    for y in 0..p.h {
        let row = &mut out[y * p.w * 3..(y + 1) * p.w * 3];
        for x in 0..p.w {
            let yy = (f32::from(p.y[y * p.w + x]) - ymin) / yscale;
            let px = &mut row[x * 3..x * 3 + 3];
            if p.layout == 0 {
                let g = q(yy);
                px.copy_from_slice(&[g, g, g]);
                continue;
            }
            let ci = match p.layout {
                1 => (y / 2) * p.cw + x / 2,
                2 => y * p.cw + x / 2,
                _ => y * p.cw + x,
            };
            if identity {
                let (g, b, r) = (f32::from(p.y[y * p.w + x]) / max, f32::from(p.u[ci]) / max, f32::from(p.v[ci]) / max);
                px.copy_from_slice(&[q(r), q(g), q(b)]);
                continue;
            }
            let cb = (f32::from(p.u[ci]) - cmid) / cscale;
            let cr = (f32::from(p.v[ci]) - cmid) / cscale;
            px.copy_from_slice(&[q(yy + cr_r * cr), q(yy + cb_g * cb + cr_g * cr), q(yy + cb_b * cb)]);
        }
    }
    out
}

/// The alpha plane as 0..=255.
fn alpha_of(p: &Planes) -> Vec<u8> {
    let max = ((1u32 << p.bpc) - 1) as f32;
    let (lo, span) = if p.full_range { (0.0, max) } else { ((16u32 << (p.bpc - 8)) as f32, (219u32 << (p.bpc - 8)) as f32) };
    p.y.iter().map(|v| (((f32::from(*v) - lo) / span).clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
}

pub fn decode(bytes: &[u8], max_side: u32) -> Result<Rgba> {
    let h = parse(bytes)?;
    let (_, ops) = h.geometry()?;
    let main = av1(&h.data(h.primary)?)?;
    let rgb = to_rgb(&main);
    let alpha = match h.alpha() {
        Some(id) => av1(&h.data(id)?).ok().filter(|a| a.w == main.w && a.h == main.h).map(|a| alpha_of(&a)),
        None => None,
    };
    let mut rgba = Vec::with_capacity(main.w * main.h * 4);
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        rgba.extend_from_slice(&[px[0], px[1], px[2], alpha.as_ref().map_or(255, |a| a[i])]);
    }
    let img = image::RgbaImage::from_raw(main.w as u32, main.h as u32, rgba).ok_or_else(|| anyhow!("size"))?;
    let mut img = crate::shrink(img, max_side);
    let full = upright_size((main.w as u32, main.h as u32), &ops);
    for op in &ops {
        img = match op {
            Prop::Rotate(1) => image::imageops::rotate270(&img),
            Prop::Rotate(2) => image::imageops::rotate180(&img),
            Prop::Rotate(3) => image::imageops::rotate90(&img),
            // axis 0 mirrors about the vertical axis (left and right swap)
            Prop::Mirror(0) => image::imageops::flip_horizontal(&img),
            Prop::Mirror(_) => image::imageops::flip_vertical(&img),
            _ => img,
        };
    }
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: 1, exif: Vec::new() })
}
