//! Camera RAW files: the largest JPEG picture the camera embedded in them (files without one are developed: `raw_develop`).
//!
//! TIFF-based files (CR2, NEF, ARW, DNG, ORF, RW2, PEF, SRW, ...) are walked IFD by IFD for the JPEG a preview or thumbnail
//! points at; CR3 (ISO base media boxes) gives its `PRVW` and `THMB` boxes; RAF has the JPEG's place in its header. A file
//! of another layout is scanned for JPEG starts. Only the needed parts of the file are read. The turn-upright orientation
//! comes from the file's own first IFD (CR3: its `CMT1` box), else from the JPEG's own EXIF.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, bail};

use crate::Rgba;

/// A preview JPEG larger than this is not read.
const MAX_JPEG: u64 = 32 << 20;
/// The embedded JPEG's pixels at most (a broken header never asks for a huge buffer).
const MAX_PIXELS: u64 = 100_000_000;
/// How much of the start of a file is read for the camera / date rows, and scanned when the layout is unknown.
const EXIF_HEAD: u64 = 4 << 20;
const SCAN_LIMIT: u64 = 64 << 20;

pub const EXTENSIONS: &[&str] =
    &["arw", "cr2", "cr3", "crw", "dng", "erf", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f"];

#[derive(Clone, Copy)]
struct Order(bool);

impl Order {
    fn u16(self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.0 { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) }
    }
    fn u32(self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.0 { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    }
}

struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    val: [u8; 4],
}

fn scalar(o: Order, e: &Entry) -> Option<u32> {
    match e.typ {
        3 if e.count >= 1 => Some(u32::from(o.u16(&e.val))),
        4 | 13 if e.count >= 1 => Some(o.u32(&e.val)),
        1 if e.count >= 1 => Some(u32::from(e.val[0])),
        _ => None,
    }
}

fn read_ifd<R: Read + Seek>(r: &mut R, o: Order, off: u64, len: u64) -> Option<(Vec<Entry>, u64)> {
    if off.checked_add(2)? > len {
        return None;
    }
    r.seek(SeekFrom::Start(off)).ok()?;
    let mut n = [0u8; 2];
    r.read_exact(&mut n).ok()?;
    let n = usize::from(o.u16(&n));
    if n == 0 || n > 1024 || off + 2 + n as u64 * 12 + 4 > len {
        return None;
    }
    let mut buf = vec![0u8; n * 12 + 4];
    r.read_exact(&mut buf).ok()?;
    let es = buf[..n * 12]
        .chunks_exact(12)
        .map(|c| Entry { tag: o.u16(&c[0..2]), typ: o.u16(&c[2..4]), count: o.u32(&c[4..8]), val: [c[8], c[9], c[10], c[11]] })
        .collect();
    Some((es, u64::from(o.u32(&buf[n * 12..]))))
}

fn tiff_order(head: &[u8]) -> Option<Order> {
    if head.len() < 8 {
        return None;
    }
    let o = match &head[0..2] {
        b"II" => Order(true),
        b"MM" => Order(false),
        _ => return None,
    };
    // 42: TIFF; 0x55: Panasonic RW2; 0x4F52 / 0x5352: Olympus ORF
    matches!(o.u16(&head[2..4]), 42 | 0x55 | 0x4F52 | 0x5352).then_some(o)
}

#[derive(Default)]
struct Found {
    /// (offset, length) of JPEG candidates.
    cands: Vec<(u64, u64)>,
    orientation: Option<u8>,
}

/// The JPEG pictures the IFD chain of a TIFF-based file points at, and the first IFD's orientation.
fn tiff_walk<R: Read + Seek>(r: &mut R, len: u64, o: Order, first: u64) -> Found {
    let mut out = Found::default();
    let mut todo = vec![first];
    let mut seen: Vec<u64> = Vec::new();
    while let Some(off) = todo.pop() {
        if seen.len() >= 24 || seen.contains(&off) {
            continue;
        }
        seen.push(off);
        let Some((es, next)) = read_ifd(r, o, off, len) else { continue };
        let (mut jo, mut jl, mut so, mut sl, mut comp, mut photo) = (None, None, None, None, 0, 0);
        let mut subs = Vec::new();
        for e in &es {
            match e.tag {
                0x0112 if seen.len() == 1 => out.orientation = scalar(o, e).filter(|v| (1..=8).contains(v)).map(|v| v as u8),
                0x0201 => jo = scalar(o, e),
                0x0202 => jl = scalar(o, e),
                0x0103 => comp = scalar(o, e).unwrap_or(0),
                0x0106 => photo = scalar(o, e).unwrap_or(0),
                0x0111 if e.count == 1 => so = scalar(o, e),
                0x0117 if e.count == 1 => sl = scalar(o, e),
                // Panasonic JpgFromRaw: the length is the count, the offset the value
                0x002E if e.typ == 7 => out.cands.push((u64::from(o.u32(&e.val)), u64::from(e.count))),
                0x014A if matches!(e.typ, 4 | 13) && e.count >= 1 => {
                    if e.count == 1 {
                        subs.push(u64::from(o.u32(&e.val)));
                    } else {
                        let n = e.count.min(8) as usize;
                        let mut b = vec![0u8; n * 4];
                        if r.seek(SeekFrom::Start(u64::from(o.u32(&e.val)))).is_ok() && r.read_exact(&mut b).is_ok() {
                            subs.extend(b.chunks_exact(4).map(|c| u64::from(o.u32(c))));
                        }
                    }
                }
                _ => {}
            }
        }
        if let (Some(a), Some(b)) = (jo, jl) {
            out.cands.push((u64::from(a), u64::from(b)));
        }
        // a strip that is a JPEG and not the sensor data (32803 CFA, 34892 linear raw)
        if let (Some(a), Some(b)) = (so, sl)
            && matches!(comp, 6 | 7)
            && !matches!(photo, 32803 | 34892)
        {
            out.cands.push((u64::from(a), u64::from(b)));
        }
        todo.extend(subs);
        if next != 0 {
            todo.push(next);
        }
    }
    out
}

fn be32(b: &[u8]) -> u64 {
    u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn find_jpeg_start(b: &[u8]) -> Option<usize> {
    b.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF])
}

#[derive(Default)]
struct Cr3 {
    found: Found,
    cmt1: Vec<u8>,
    cmt2: Vec<u8>,
}

fn box_header<R: Read + Seek>(r: &mut R, pos: u64, end: u64) -> Option<(u64, [u8; 4], u64)> {
    if pos + 8 > end {
        return None;
    }
    r.seek(SeekFrom::Start(pos)).ok()?;
    let mut h = [0u8; 16];
    r.read_exact(&mut h[..8]).ok()?;
    let ty = [h[4], h[5], h[6], h[7]];
    let (size, hdr) = match be32(&h[0..4]) {
        1 => {
            r.read_exact(&mut h[8..16]).ok()?;
            (u64::from_be_bytes(h[8..16].try_into().ok()?), 16)
        }
        0 => (end - pos, 8),
        n => (n, 8),
    };
    (size >= hdr && pos + size <= end).then_some((size, ty, hdr))
}

fn plausible(ty: &[u8; 4]) -> bool {
    ty.iter().all(|c| c.is_ascii_alphanumeric() || *c == b' ')
}

fn cr3_walk<R: Read + Seek>(r: &mut R, start: u64, end: u64, depth: u8, out: &mut Cr3) {
    let mut pos = start;
    for _ in 0..64 {
        let Some((size, ty, hdr)) = box_header(r, pos, end) else { return };
        let (body, bend) = (pos + hdr, pos + size);
        match &ty {
            b"moov" if depth < 4 => cr3_walk(r, body, bend, depth + 1, out),
            b"uuid" if depth < 4 => {
                // 16 bytes of identifier; the box holding the large preview has 8 more bytes before its first box
                for skip in [16, 24] {
                    if box_header(r, body + skip, bend).is_some_and(|(_, t, _)| plausible(&t)) {
                        cr3_walk(r, body + skip, bend, depth + 1, out);
                        break;
                    }
                }
            }
            b"THMB" | b"PRVW" => {
                let mut b = vec![0u8; ((bend - body) as usize).min(64)];
                if r.seek(SeekFrom::Start(body)).is_ok() && r.read_exact(&mut b).is_ok() {
                    if let Some(i) = find_jpeg_start(&b) {
                        out.found.cands.push((body + i as u64, bend - body - i as u64));
                    }
                }
            }
            b"CMT1" | b"CMT2" if bend - body <= 1 << 16 => {
                let mut b = vec![0u8; (bend - body) as usize];
                if r.seek(SeekFrom::Start(body)).is_ok() && r.read_exact(&mut b).is_ok() {
                    if &ty == b"CMT1" { out.cmt1 = b } else { out.cmt2 = b }
                }
            }
            _ => {}
        }
        pos = bend;
    }
}

/// JPEG starts in the first `SCAN_LIMIT` bytes, the biggest picture first (for a layout not known here).
fn scan<R: Read + Seek>(r: &mut R, len: u64) -> Found {
    let mut buf = Vec::new();
    if r.seek(SeekFrom::Start(0)).is_err() || r.by_ref().take(len.min(SCAN_LIMIT)).read_to_end(&mut buf).is_err() {
        return Found::default();
    }
    let mut hits = Vec::new();
    let mut i = 0;
    while let Some(p) = find_jpeg_start(&buf[i..]) {
        let at = i + p;
        let rd = image::ImageReader::with_format(Cursor::new(&buf[at..]), image::ImageFormat::Jpeg);
        if let Ok((w, h)) = rd.into_dimensions() {
            hits.push((u64::from(w) * u64::from(h), at as u64));
        }
        i = at + 1;
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    Found { cands: hits.into_iter().take(4).map(|(_, at)| (at, (len - at).min(MAX_JPEG))).collect(), orientation: None }
}

struct Preview {
    found: Found,
    /// The text rows (camera, date, ...) from the file's own EXIF.
    exif: Vec<(String, String)>,
}

pub(crate) fn exif_rows(raw: &[u8]) -> (Option<u8>, Vec<(String, String)>) {
    let Ok(ex) = exif::Reader::new().read_raw(raw.to_vec()) else { return (None, Vec::new()) };
    let o = ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).filter(|v| (1..=8).contains(v)).map(|v| v as u8);
    let rows = ex
        .fields()
        .filter(|f| !matches!(f.tag, exif::Tag::MakerNote | exif::Tag::UserComment) && f.tag.description().is_some())
        .map(|f| (f.tag.to_string(), f.display_value().with_unit(&ex).to_string().trim_matches('"').to_string()))
        .filter(|(_, v)| !v.is_empty() && v.len() < 200)
        .collect();
    (o, rows)
}

fn locate<R: Read + Seek>(r: &mut R, len: u64) -> Preview {
    let mut head = [0u8; 96];
    let n = r.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    let mut exif = Vec::new();
    let mut found = Found::default();
    if let Some(o) = tiff_order(head) {
        found = tiff_walk(r, len, o, u64::from(o.u32(&head[4..8])));
        let mut buf = Vec::new();
        if r.seek(SeekFrom::Start(0)).is_ok() && r.by_ref().take(len.min(EXIF_HEAD)).read_to_end(&mut buf).is_ok() {
            exif = exif_rows(&buf).1;
        }
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let mut c = Cr3::default();
        cr3_walk(r, 0, len, 0, &mut c);
        if let Some(o) = tiff_order(&c.cmt1) {
            let mut cur = Cursor::new(&c.cmt1);
            let ifd = tiff_walk(&mut cur, c.cmt1.len() as u64, o, u64::from(o.u32(&c.cmt1[4..8])));
            c.found.orientation = ifd.orientation;
        }
        for part in [&c.cmt1, &c.cmt2] {
            exif.extend(exif_rows(part).1);
        }
        found = c.found;
    } else if head.len() >= 92 && head.starts_with(b"FUJIFILM") {
        // RAF: the JPEG's offset and length are in the header (big endian)
        found.cands.push((be32(&head[84..88]), be32(&head[88..92])));
    }
    Preview { found, exif }
}

pub(crate) fn orient(img: image::DynamicImage, o: u8) -> image::DynamicImage {
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

fn read_range<R: Read + Seek>(r: &mut R, file_len: u64, (off, n): (u64, u64)) -> Option<Vec<u8>> {
    if n < 512 || n > MAX_JPEG || off.checked_add(n)? > file_len {
        return None;
    }
    r.seek(SeekFrom::Start(off)).ok()?;
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b).ok()?;
    (b.starts_with(&[0xFF, 0xD8, 0xFF])).then_some(b)
}

/// The biggest candidate that really is a decodable picture: (its bytes, its decoder reader's size).
fn pick<R: Read + Seek>(r: &mut R, len: u64, mut f: Found) -> anyhow::Result<(Vec<u8>, Option<u8>)> {
    f.cands.sort_by(|a, b| b.1.cmp(&a.1));
    f.cands.dedup();
    for &c in f.cands.iter().take(6) {
        let Some(b) = read_range(r, len, c) else { continue };
        match image::ImageReader::with_format(Cursor::new(&b), image::ImageFormat::Jpeg).into_dimensions() {
            // a lossless-JPEG sensor strip gives no dimensions here, or no picture below
            Ok((w, h)) if u64::from(w) * u64::from(h) <= MAX_PIXELS && w > 0 && h > 0 => return Ok((b, f.orientation)),
            _ => {}
        }
    }
    bail!("no embedded JPEG")
}

fn preview<R: Read + Seek>(r: &mut R, len: u64) -> anyhow::Result<(Vec<u8>, u8, Vec<(String, String)>)> {
    let Preview { found, mut exif } = locate(r, len);
    let (jpeg, mut orientation) = match pick(r, len, found) {
        Ok(p) => p,
        Err(_) => {
            let s = scan(r, len);
            pick(r, len, s)?
        }
    };
    if orientation.is_none() || exif.is_empty() {
        // RAF and unknown layouts: the JPEG carries the camera's EXIF
        if let Some(raw) = jpeg_exif(&jpeg) {
            let (o, rows) = exif_rows(&raw);
            orientation = orientation.or(o);
            if exif.is_empty() {
                exif = rows;
            }
        }
    }
    Ok((jpeg, orientation.unwrap_or(1), exif))
}

/// The TIFF block of a JPEG's APP1 Exif segment.
fn jpeg_exif(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF {
        let m = jpeg[i + 1];
        if m == 0xDA || m == 0xD9 {
            break;
        }
        let n = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        let body = jpeg.get(i + 4..i + 2 + n)?;
        if m == 0xE1 && body.starts_with(b"Exif\0\0") {
            return Some(body[6..].to_vec());
        }
        i += 2 + n;
    }
    None
}

pub fn decode(path: &Path, max_side: u32) -> anyhow::Result<Rgba> {
    let mut f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let len = f.metadata()?.len();
    match decode_from(&mut f, len, max_side) {
        Ok(r) => Ok(r),
        // no usable embedded picture: the sensor data is developed (see `raw_develop` for the policy)
        Err(e) => crate::raw_develop::develop(path, max_side).map_err(|d| anyhow::anyhow!("{e}; develop: {d}")),
    }
}

pub fn decode_bytes(bytes: &[u8], max_side: u32) -> anyhow::Result<Rgba> {
    decode_from(&mut Cursor::new(bytes), bytes.len() as u64, max_side)
}

/// The camera / date rows of the file's own EXIF (empty when none can be read).
pub(crate) fn exif_rows_of(path: &Path) -> Vec<(String, String)> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(len) = f.metadata().map(|m| m.len()) else { return Vec::new() };
    locate(&mut f, len).exif
}

fn decode_from<R: Read + Seek>(r: &mut R, len: u64, max_side: u32) -> anyhow::Result<Rgba> {
    let (jpeg, o, exif) = preview(r, len)?;
    let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).map_err(|e| anyhow::anyhow!("raw preview: {e}"))?;
    let img = orient(img, o).into_rgba8();
    let full = img.dimensions();
    let img = crate::shrink(img, max_side);
    Ok(Rgba { width: img.width(), height: img.height(), data: img.into_raw(), full, orientation: o, exif })
}

/// The preview's size as shown (turned upright).
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let (jpeg, o, _) = preview(&mut f, len).ok()?;
    let (w, h) = image::ImageReader::with_format(Cursor::new(&jpeg), image::ImageFormat::Jpeg).into_dimensions().ok()?;
    Some(if (5..=8).contains(&o) { (h, w) } else { (w, h) })
}
