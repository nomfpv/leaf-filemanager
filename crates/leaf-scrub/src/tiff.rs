//! TIFF: the IFD chain is rewritten IN PLACE. Only the tags that describe how to decode and show the picture stay (a fixed list);
//! every other tag (Exif and GPS pointers, maker notes, XMP, IPTC, Photoshop data, camera, software, dates, artist, copyright,
//! description, ... and every tag leaf does not know) is dropped from its IFD, its out-of-line value bytes are overwritten with
//! zeros, and the Exif / GPS / interoperability sub-IFDs are zeroed whole. Nothing changes size, so the image strips and tiles
//! are never moved, and the read-back compares them byte for byte with the original.
//! RAW files built on TIFF (DNG, CR2, NEF, ARW ...) are refused before this runs.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub(crate) const RAW_TEXT: &str = "camera RAW files carry the camera's data inside the picture data: convert to another format first";

/// Tags that describe the picture itself. ICC (34675) is added when the colour profile is kept.
pub(crate) const KEEP: &[u16] = &[
    254, 255, 256, 257, 258, 259, 262, 263, 264, 265, 266, 273, 274, 277, 278, 279, 280, 281, 282, 283, 284, 292, 293, 296, 301, 317, 318, 319,
    320, 322, 323, 324, 325, 338, 339, 340, 341, 347, 512, 513, 514, 515, 517, 518, 519, 520, 521, 529, 530, 531, 532,
];

#[derive(Clone, Copy)]
struct Rd {
    le: bool,
}

impl Rd {
    fn u16(&self, d: &[u8], p: usize) -> Result<usize> {
        let b = d.get(p..p + 2).ok_or_else(|| ScrubError::Malformed("a TIFF table is cut off".into()))?;
        Ok(if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) } as usize)
    }
    fn u32(&self, d: &[u8], p: usize) -> Result<usize> {
        let b = d.get(p..p + 4).ok_or_else(|| ScrubError::Malformed("a TIFF table is cut off".into()))?;
        Ok(if self.le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } as usize)
    }
}

fn type_size(t: usize) -> Option<usize> {
    match t {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

struct Entry {
    tag: usize,
    ty: usize,
    count: usize,
    /// position of the 12-byte entry
    at: usize,
    /// the bytes of the value (inline in the entry, or out of line)
    data: (usize, usize),
    inline: bool,
}

fn bad<T>(w: &str) -> Result<T> {
    Err(ScrubError::Malformed(w.into()))
}

fn read_ifd(d: &[u8], rd: Rd, off: usize) -> Result<(Vec<Entry>, usize)> {
    let n = rd.u16(d, off)?;
    let end = off + 2 + 12 * n + 4;
    if end > d.len() {
        return bad("a TIFF directory runs past the end of the file");
    }
    let mut v = Vec::new();
    for i in 0..n {
        let at = off + 2 + 12 * i;
        let (tag, ty, count) = (rd.u16(d, at)?, rd.u16(d, at + 2)?, rd.u32(d, at + 4)?);
        let size = type_size(ty).ok_or_else(|| ScrubError::Unsupported("the TIFF has a value type leaf does not know".into()))?;
        let total = size.checked_mul(count).ok_or_else(|| ScrubError::Malformed("a TIFF value is too big".into()))?;
        let (data, inline) = if total <= 4 {
            ((at + 8, at + 8 + total), true)
        } else {
            let o = rd.u32(d, at + 8)?;
            let e = o.checked_add(total).filter(|e| *e <= d.len()).ok_or_else(|| ScrubError::Malformed("a TIFF value runs past the end of the file".into()))?;
            if o < 8 {
                return bad("a TIFF value points into the header");
            }
            ((o, e), false)
        };
        v.push(Entry { tag, ty, count, at, data, inline });
    }
    let next = rd.u32(d, off + 2 + 12 * n)?;
    Ok((v, next))
}

fn numbers(d: &[u8], rd: Rd, e: &Entry) -> Result<Vec<usize>> {
    let size = type_size(e.ty).unwrap_or(1);
    let mut out = Vec::new();
    for i in 0..e.count {
        let p = e.data.0 + i * size;
        out.push(match e.ty {
            3 => rd.u16(d, p)?,
            4 => rd.u32(d, p)?,
            _ => return Err(ScrubError::Unsupported("an offset table of the TIFF has a type leaf does not know".into())),
        });
    }
    Ok(out)
}

struct Plan {
    patches: Vec<(usize, Vec<u8>)>,
    findings: Vec<Finding>,
    notes: Vec<String>,
    /// the picture data (strips, tiles) and the kept structure: must be identical in the result
    strips: Vec<(usize, usize)>,
}

fn label_of(ctx: exif::Context, tag: usize) -> String {
    let desc = exif::Tag(ctx, tag as u16).description().map(|s| s.to_string()).unwrap_or_else(|| format!("tag 0x{tag:04x}"));
    match (ctx, tag) {
        (exif::Context::Exif, 0x927C) => "Maker note".into(),
        (exif::Context::Tiff, 700) => "XMP".into(),
        (exif::Context::Tiff, 33723) => "IPTC data".into(),
        (exif::Context::Tiff, 34377) => "Photoshop data".into(),
        (exif::Context::Gps, _) => format!("GPS {desc}"),
        _ => desc,
    }
}

fn value_of(d: &[u8], rd: Rd, e: &Entry) -> String {
    let (a, b) = e.data;
    match e.ty {
        2 => String::from_utf8_lossy(&d[a..b]).trim_end_matches('\0').to_string(),
        3 | 4 if e.count == 1 => numbers(d, rd, e).ok().and_then(|v| v.first().map(|n| n.to_string())).unwrap_or_default(),
        5 if e.count <= 3 => (0..e.count)
            .filter_map(|i| Some(format!("{}/{}", rd.u32(d, a + i * 8).ok()?, rd.u32(d, a + i * 8 + 4).ok()?)))
            .collect::<Vec<_>>()
            .join(", "),
        _ => format!("{} bytes", b - a),
    }
}

/// An IFD that goes whole (Exif, GPS, interoperability): every entry listed, every value and the table itself zeroed.
fn zero_ifd(d: &[u8], rd: Rd, off: usize, ctx: exif::Context, depth: u8, p: &mut Plan, zero: &mut Vec<(usize, usize)>) -> Result<()> {
    if depth > 3 {
        return bad("TIFF directories are nested too deep");
    }
    if off < 8 {
        return bad("a TIFF directory points into the header");
    }
    let (entries, _) = read_ifd(d, rd, off)?;
    zero.push((off, off + 2 + 12 * entries.len() + 4));
    for e in &entries {
        if e.tag == 0xA005 && ctx == exif::Context::Exif {
            zero_ifd(d, rd, rd.u32(d, e.at + 8)?, exif::Context::Interop, depth + 1, p, zero)?;
            continue;
        }
        p.findings.push(Finding::new(label_of(ctx, e.tag), value_of(d, rd, e), true));
        if !e.inline {
            zero.push(e.data);
        }
    }
    Ok(())
}

fn plan(d: &[u8], opts: &Options) -> Result<Plan> {
    let rd = match d.get(0..4) {
        Some(b"II*\0") => Rd { le: true },
        Some(b"MM\0*") => Rd { le: false },
        Some(b"II+\0") | Some(b"MM\0+") => return Err(ScrubError::Unsupported("BigTIFF files (over 4 GB) are not cleaned".into())),
        _ => return bad("no TIFF header"),
    };
    let mut p = Plan { patches: vec![], findings: vec![], notes: vec![], strips: vec![] };
    let mut zero: Vec<(usize, usize)> = Vec::new();
    let mut kept: Vec<(usize, usize)> = vec![(0, 8)];
    let mut seen = BTreeSet::new();
    let mut off = rd.u32(d, 4)?;
    let mut ifds = 0;
    while off != 0 {
        if off < 8 || !seen.insert(off) || ifds >= 4096 {
            return bad("the TIFF directory chain is damaged");
        }
        ifds += 1;
        let (entries, next) = read_ifd(d, rd, off)?;
        kept.push((off, off + 2 + 12 * entries.len() + 4));
        let mut keep_entries: Vec<&Entry> = Vec::new();
        let mut offs: Vec<usize> = vec![];
        let mut counts: Vec<usize> = vec![];
        let (mut tile_offs, mut tile_counts) = (vec![], vec![]);
        let mut jpeg: (Option<usize>, Option<usize>) = (None, None);
        for e in &entries {
            if e.tag == 50706 || e.tag == 50707 {
                return Err(ScrubError::Unsupported(RAW_TEXT.into()));
            }
            if e.tag == 330 {
                return Err(ScrubError::Unsupported("the TIFF holds sub-pictures (SubIFDs), leaf cannot clean those safely".into()));
            }
            let is_icc = e.tag == 34675;
            let keep = KEEP.contains(&(e.tag as u16)) || (is_icc && opts.keep_color_profile);
            if keep {
                match e.tag {
                    273 => offs = numbers(d, rd, e)?,
                    279 => counts = numbers(d, rd, e)?,
                    324 => tile_offs = numbers(d, rd, e)?,
                    325 => tile_counts = numbers(d, rd, e)?,
                    513 => jpeg.0 = numbers(d, rd, e)?.first().copied(),
                    514 => jpeg.1 = numbers(d, rd, e)?.first().copied(),
                    _ => {}
                }
                if !e.inline {
                    kept.push(e.data);
                }
                keep_entries.push(e);
                continue;
            }
            match e.tag {
                0x8769 => zero_ifd(d, rd, rd.u32(d, e.at + 8)?, exif::Context::Exif, 0, &mut p, &mut zero)?,
                0x8825 => zero_ifd(d, rd, rd.u32(d, e.at + 8)?, exif::Context::Gps, 0, &mut p, &mut zero)?,
                _ => {
                    p.findings.push(Finding::new(label_of(exif::Context::Tiff, e.tag), value_of(d, rd, e), true));
                    if !e.inline {
                        zero.push(e.data);
                    }
                }
            }
        }
        if is_some_icc(&entries) && opts.keep_color_profile {
            p.findings.push(Finding::new("Colour profile", "", false));
        }
        for (o, c) in [(&offs, &counts), (&tile_offs, &tile_counts)] {
            if o.len() != c.len() {
                return bad("the picture data table of the TIFF is inconsistent");
            }
            for (a, n) in o.iter().zip(c) {
                let end = a.checked_add(*n).filter(|e| *e <= d.len()).ok_or_else(|| ScrubError::Malformed("the picture data is cut off".into()))?;
                p.strips.push((*a, end));
            }
        }
        if let (Some(a), Some(n)) = jpeg {
            let end = a.checked_add(n).filter(|e| *e <= d.len()).ok_or_else(|| ScrubError::Malformed("the picture data is cut off".into()))?;
            p.strips.push((a, end));
        }
        if p.strips.is_empty() {
            return bad("the TIFF has no picture data");
        }
        if keep_entries.len() != entries.len() {
            let mut b = Vec::with_capacity(2 + 12 * entries.len() + 4);
            let cnt = keep_entries.len() as u16;
            b.extend_from_slice(&if rd.le { cnt.to_le_bytes() } else { cnt.to_be_bytes() });
            for e in &keep_entries {
                b.extend_from_slice(&d[e.at..e.at + 12]);
            }
            b.extend_from_slice(&d[off + 2 + 12 * entries.len()..off + 2 + 12 * entries.len() + 4]);
            b.resize(2 + 12 * entries.len() + 4, 0);
            p.patches.push((off, b));
        }
        off = next;
    }
    kept.extend(p.strips.iter().copied());
    for z in &zero {
        if kept.iter().any(|k| z.0 < k.1 && k.0 < z.1) {
            return Err(ScrubError::Unsupported("metadata shares bytes with the picture data or the picture's own tables, it cannot be erased without damaging the picture".into()));
        }
    }
    for (a, b) in zero {
        p.patches.push((a, vec![0u8; b - a]));
    }
    p.notes.push("The picture data (strips and tiles) is not touched. Tags leaf does not know are removed with the rest.".into());
    Ok(p)
}

fn is_some_icc(e: &[Entry]) -> bool {
    e.iter().any(|e| e.tag == 34675)
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let d = crate::image::read_all(path)?;
    let p = plan(&d, opts)?;
    Ok(Inspection { format: "TIFF".into(), findings: p.findings, notes: p.notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let mut d = crate::image::read_all(src)?;
    let first = plan(&d, opts)?;
    let original = d.clone();
    for (off, bytes) in &first.patches {
        d[*off..*off + bytes.len()].copy_from_slice(bytes);
    }
    let after = plan(&d, opts)?;
    if after.findings.iter().any(|f| f.remove) || !after.patches.is_empty() {
        return Err(ScrubError::Verify("tags are still in the TIFF".into()));
    }
    for (a, b) in &first.strips {
        if d[*a..*b] != original[*a..*b] {
            return Err(ScrubError::Verify("the picture data changed".into()));
        }
    }
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, &d)?;
    f.sync_all()?;
    Ok(())
}
