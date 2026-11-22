//! HEIC / HEIF / AVIF: cleaned at container level, the coded picture is never decoded or re-encoded.
//!
//! What is done, and it is the same in every case: nothing changes size, so no offset in the file moves (the `iloc` table, the
//! `mdat` box and every box length stay valid).
//! - the payload of every Exif item, XMP / other `mime` item and `xml ` item is OVERWRITTEN WITH ZEROS of the same length (wherever
//!   `iloc` says it is: in `mdat`, or in `idat`), and the item's type in `iinf` is changed to `free` (an unknown type that every
//!   reader skips), its name and content type are zeroed. The item is not deleted from `iinf` / `iloc` / `iref`: removing it would
//!   mean moving every later offset, which is how pictures get damaged;
//! - `udta` and XMP `uuid` boxes are zeroed and renamed to `free`; the contents of existing `free` / `skip` boxes are zeroed;
//! - rotation and mirroring (`irot`, `imir`) and the colour description stay, they are part of how the picture is shown.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use std::fs;
use std::path::Path;

type Patch = (usize, Vec<u8>);

const XMP_UUID: [u8; 16] = [0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF, 0xAC];

fn bad<T>(w: &str) -> Result<T> {
    Err(ScrubError::Malformed(w.into()))
}

#[derive(Clone, Copy)]
struct B {
    off: usize,
    hdr: usize,
    ty: [u8; 4],
    end: usize,
}

impl B {
    fn body(&self) -> usize {
        self.off + self.hdr
    }
}

fn be16(d: &[u8], p: usize) -> Result<usize> {
    d.get(p..p + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or_else(|| ScrubError::Malformed("a table is cut off".into()))
}
fn be32(d: &[u8], p: usize) -> Result<usize> {
    d.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| ScrubError::Malformed("a table is cut off".into()))
}
fn be_n(d: &[u8], p: usize, n: usize) -> Result<u64> {
    if n == 0 {
        return Ok(0);
    }
    if n > 8 {
        return bad("a table field is too wide");
    }
    let s = d.get(p..p + n).ok_or_else(|| ScrubError::Malformed("a table is cut off".into()))?;
    Ok(s.iter().fold(0u64, |a, b| (a << 8) | *b as u64))
}

fn boxes(d: &[u8], start: usize, end: usize) -> Result<Vec<B>> {
    let mut out = Vec::new();
    let mut p = start;
    while p < end {
        if end - p < 8 {
            return bad("a box header is cut off");
        }
        let s32 = be32(d, p)?;
        let ty: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (hdr, size) = match s32 {
            0 => (8, end - p),
            1 => {
                if end - p < 16 {
                    return bad("a box header is cut off");
                }
                let s = u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap());
                (16, usize::try_from(s).map_err(|_| ScrubError::Malformed("a box is too big".into()))?)
            }
            n => (8, n),
        };
        if size < hdr || size > end - p {
            return bad("a box runs past its parent");
        }
        out.push(B { off: p, hdr, ty, end: p + size });
        p += size;
    }
    Ok(out)
}

fn cstr_end(d: &[u8], from: usize, end: usize) -> Result<usize> {
    d[from.min(end)..end].iter().position(|c| *c == 0).map(|i| from + i).ok_or_else(|| ScrubError::Malformed("an item name is not terminated".into()))
}

struct Item {
    id: u32,
    ty: [u8; 4],
    type_off: usize,
    /// byte ranges of the name and the content type (zeroed when the item goes)
    strings: Vec<(usize, usize)>,
    content_type: String,
}

impl Item {
    fn goes(&self) -> bool {
        &self.ty == b"Exif" || &self.ty == b"mime" || &self.ty == b"xml "
    }
    fn label(&self) -> &'static str {
        if &self.ty == b"Exif" {
            "Exif"
        } else if &self.ty == b"mime" && (self.content_type.contains("xmp") || self.content_type.contains("rdf")) {
            "XMP"
        } else if &self.ty == b"mime" {
            "Embedded data (other)"
        } else {
            "Embedded XML"
        }
    }
}

fn parse_iinf(d: &[u8], b: B) -> Result<Vec<Item>> {
    let d = &d[..b.end];
    let mut p = b.body();
    let ver = *d.get(p).ok_or_else(|| ScrubError::Malformed("iinf is cut off".into()))?;
    p += 4;
    p += if ver == 0 { 2 } else { 4 };
    let mut items = Vec::new();
    for e in boxes(d, p, b.end)? {
        if &e.ty != b"infe" {
            continue;
        }
        let mut q = e.body();
        let v = *d.get(q).ok_or_else(|| ScrubError::Malformed("an item entry is cut off".into()))?;
        if v < 2 {
            return Err(ScrubError::Unsupported("the file uses an old item table (infe version below 2) that leaf cannot classify".into()));
        }
        q += 4;
        let id = if v == 3 {
            let x = be32(d, q)? as u32;
            q += 4;
            x
        } else {
            let x = be16(d, q)? as u32;
            q += 2;
            x
        };
        q += 2; // protection index
        let ty: [u8; 4] = d.get(q..q + 4).ok_or_else(|| ScrubError::Malformed("an item entry is cut off".into()))?.try_into().unwrap();
        let type_off = q;
        q += 4;
        let mut strings = Vec::new();
        let name_end = cstr_end(d, q, e.end)?;
        strings.push((q, name_end));
        let mut content_type = String::new();
        if &ty == b"mime" {
            let cs = name_end + 1;
            let ce = cstr_end(d, cs, e.end)?;
            content_type = String::from_utf8_lossy(&d[cs..ce]).into_owned();
            strings.push((cs, ce));
        }
        items.push(Item { id, ty, type_off, strings, content_type });
    }
    Ok(items)
}

struct Extent {
    item: u32,
    /// absolute position in the file
    at: usize,
    len: usize,
}

/// The data ranges of every item, resolved to file positions (`idat` data is addressed inside the idat box).
fn parse_iloc(d: &[u8], b: B, idat: Option<B>) -> Result<Vec<Extent>> {
    // every read stays inside the box, and every count read from it is checked against the bytes it would need
    let d = &d[..b.end];
    let mut p = b.body();
    let ver = *d.get(p).ok_or_else(|| ScrubError::Malformed("iloc is cut off".into()))?;
    if ver > 2 {
        return Err(ScrubError::Unsupported("the item location table has a version leaf does not know".into()));
    }
    p += 4;
    let sizes = be16(d, p)?;
    p += 2;
    let (offset_size, length_size, base_size, index_size) = ((sizes >> 12) & 15, (sizes >> 8) & 15, (sizes >> 4) & 15, if ver > 0 { sizes & 15 } else { 0 });
    if [offset_size, length_size, base_size, index_size].iter().any(|n| !matches!(n, 0 | 4 | 8)) {
        return bad("the item location table has a field width that is not allowed");
    }
    let per_extent = index_size + offset_size + length_size;
    if per_extent == 0 {
        return bad("the item location table has extents of zero width");
    }
    let count = if ver < 2 {
        let c = be16(d, p)?;
        p += 2;
        c
    } else {
        let c = be32(d, p)?;
        p += 4;
        c
    };
    let per_item = if ver < 2 { 2 } else { 4 } + if ver > 0 { 2 } else { 0 } + 2 + base_size + 2;
    if count.checked_mul(per_item).is_none_or(|n| n > d.len() - p) {
        return bad("the item location table lists more items than it holds");
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let id = if ver < 2 {
            let x = be16(d, p)?;
            p += 2;
            x as u32
        } else {
            let x = be32(d, p)?;
            p += 4;
            x as u32
        };
        let mut method = 0;
        if ver > 0 {
            method = be16(d, p)? & 15;
            p += 2;
        }
        let dref = be16(d, p)?;
        p += 2;
        let base = be_n(d, p, base_size)? as usize;
        p += base_size;
        let ext_count = be16(d, p)?;
        p += 2;
        if ext_count.checked_mul(per_extent).is_none_or(|n| n > d.len().saturating_sub(p)) {
            return bad("an item lists more extents than the table holds");
        }
        for _ in 0..ext_count {
            if ver > 0 && index_size > 0 {
                p += index_size;
            }
            let off = be_n(d, p, offset_size)? as usize;
            p += offset_size;
            let len = be_n(d, p, length_size)? as usize;
            p += length_size;
            if dref != 0 {
                return Err(ScrubError::Unsupported("an item's data is stored in another file".into()));
            }
            let at = match method {
                0 => base.checked_add(off),
                1 => idat.and_then(|i| i.body().checked_add(base)?.checked_add(off)),
                _ => return Err(ScrubError::Unsupported("an item is stored by reference to another item".into())),
            };
            let at = at.ok_or_else(|| ScrubError::Malformed("an item location is out of range".into()))?;
            out.push(Extent { item: id, at, len });
        }
    }
    Ok(out)
}

fn zeros(n: usize) -> Vec<u8> {
    vec![0u8; n]
}

/// The tiff bytes of an Exif item (4-byte offset to the tiff header first).
fn exif_findings(payload: &[u8]) -> Vec<Finding> {
    if payload.len() < 8 {
        return vec![Finding::new("Exif data", "empty", true)];
    }
    let skip = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    let tiff = payload.get(4 + skip..).unwrap_or(&[]);
    let tiff = if tiff.starts_with(b"Exif\0\0") { &tiff[6..] } else { tiff };
    let mut v: Vec<Finding> = crate::image::exif_block(tiff).0.into_iter().filter(|f| f.remove).collect();
    if v.is_empty() {
        v.push(Finding::new("Exif data", format!("{} bytes", payload.len()), true));
    }
    v
}

fn plan(d: &[u8], opts: &Options) -> Result<(Vec<Patch>, Vec<Finding>, Vec<String>)> {
    let _ = opts;
    let top = boxes(d, 0, d.len())?;
    let mut patches: Vec<Patch> = Vec::new();
    let mut found = Vec::new();
    let mut notes = Vec::new();
    let mut meta: Option<B> = None;
    // boxes that are not on the list of what a reader needs to show the picture: emptied and renamed to `free` below
    let mut odd: Vec<B> = Vec::new();
    for b in &top {
        match &b.ty {
            b"meta" => {
                if meta.is_some() {
                    return bad("two meta boxes");
                }
                meta = Some(*b);
            }
            b"moov" => return Err(ScrubError::Unsupported("the file is an image sequence or has a movie part; only single pictures are cleaned".into())),
            b"udta" => {
                found.push(Finding::new("Tags and device information", format!("{} bytes", b.end - b.off), true));
                free_box(*b, &mut patches, d);
            }
            b"uuid" if b.end - b.body() >= 16 && d[b.body()..b.body() + 16] == XMP_UUID => {
                found.push(Finding::new("XMP", format!("{} bytes", b.end - b.off), true));
                free_box(*b, &mut patches, d);
            }
            b"free" | b"skip" => {
                if d[b.body()..b.end].iter().any(|c| *c != 0) {
                    found.push(Finding::new("Leftover data in padding", format!("{} bytes", b.end - b.body()), true));
                    patches.push((b.body(), zeros(b.end - b.body())));
                }
            }
            b"ftyp" | b"mdat" => {}
            _ => odd.push(*b),
        }
    }
    let meta = meta.ok_or_else(|| ScrubError::Malformed("no meta box: not a HEIF / AVIF picture".into()))?;
    if meta.end - meta.body() < 4 {
        return bad("the meta box is cut off");
    }
    let kids = boxes(d, meta.body() + 4, meta.end)?;
    let find = |t: &[u8; 4]| kids.iter().find(|k| &k.ty == t).copied();
    let iinf = find(b"iinf").ok_or_else(|| ScrubError::Malformed("no item list".into()))?;
    let iloc = find(b"iloc").ok_or_else(|| ScrubError::Malformed("no item location table".into()))?;
    let items = parse_iinf(d, iinf)?;
    let extents = parse_iloc(d, iloc, find(b"idat"))?;
    for k in &kids {
        match &k.ty {
            b"udta" => {
                found.push(Finding::new("Tags and device information", format!("{} bytes", k.end - k.off), true));
                free_box(*k, &mut patches, d);
            }
            b"free" | b"skip" if d[k.body()..k.end].iter().any(|c| *c != 0) => {
                found.push(Finding::new("Leftover data in padding", format!("{} bytes", k.end - k.body()), true));
                patches.push((k.body(), zeros(k.end - k.body())));
            }
            b"hdlr" | b"pitm" | b"iloc" | b"iinf" | b"iref" | b"iprp" | b"idat" | b"dinf" | b"grpl" | b"free" | b"skip" | b"ipro" => {}
            // `xml ` / `bxml` (XML metadata of the file) and any box leaf does not know
            _ => odd.push(*k),
        }
    }
    for it in items.iter().filter(|i| i.goes()) {
        let mine: Vec<&Extent> = extents.iter().filter(|e| e.item == it.id).collect();
        let mut payload = Vec::new();
        for e in &mine {
            if e.len == 0 {
                return Err(ScrubError::Unsupported("an item has an extent of unknown length".into()));
            }
            let end = e.at.checked_add(e.len).filter(|n| *n <= d.len()).ok_or_else(|| ScrubError::Malformed("an item's data runs past the end of the file".into()))?;
            if let Some(m) = top.iter().find(|b| &b.ty == b"ftyp") {
                if e.at < m.end {
                    return bad("an item's data overlaps the file header");
                }
            }
            payload.extend_from_slice(&d[e.at..end]);
            patches.push((e.at, zeros(e.len)));
        }
        match it.label() {
            "Exif" => found.extend(exif_findings(&payload)),
            l => found.push(Finding::new(l, format!("{} bytes", payload.len()), true)),
        }
        patches.push((it.type_off, b"free".to_vec()));
        for (s, e) in &it.strings {
            patches.push((*s, zeros(e - s)));
        }
    }
    // a payload that is shared with a picture would be zeroed together with it: refuse
    let keep_ranges: Vec<(usize, usize)> =
        extents.iter().filter(|e| !items.iter().any(|i| i.id == e.item && i.goes())).map(|e| (e.at, e.at.saturating_add(e.len))).collect();
    for e in extents.iter().filter(|e| items.iter().any(|i| i.id == e.item && i.goes())) {
        let (a, b) = (e.at, e.at + e.len);
        if keep_ranges.iter().any(|(s, t)| a < *t && *s < b) {
            return Err(ScrubError::Unsupported("the metadata shares bytes with the picture data, it cannot be erased without damaging the picture".into()));
        }
    }
    for b in odd {
        if keep_ranges.iter().any(|(s, t)| b.off < *t && *s < b.end) {
            return Err(ScrubError::Unsupported(format!("a box leaf does not know ({}) holds picture data", String::from_utf8_lossy(&b.ty))));
        }
        let label = match &b.ty {
            b"xml " | b"bxml" => "Embedded XML metadata".to_string(),
            b"uuid" => "Vendor data box (uuid: can hold the camera model and serial)".to_string(),
            t => format!("Box leaf does not know ({})", String::from_utf8_lossy(t).trim()),
        };
        found.push(Finding::new(label, format!("{} bytes", b.end - b.off), true));
        free_box(b, &mut patches, d);
    }
    notes.push("The coded picture is not touched. Rotation and mirroring stay. The metadata items were overwritten with zeros of the same length and marked as unused: they are not deleted from the item table, because deleting would move the picture data.".into());
    Ok((patches, found, notes))
}

fn free_box(b: B, patches: &mut Vec<Patch>, d: &[u8]) {
    if d[b.body()..b.end].iter().any(|c| *c != 0) {
        patches.push((b.body(), zeros(b.end - b.body())));
    }
    patches.push((b.off + 4, b"free".to_vec()));
}

pub(crate) fn inspect(path: &Path, name: &'static str, opts: &Options) -> Result<Inspection> {
    let d = crate::image::read_all(path)?;
    let (_, findings, notes) = plan(&d, opts)?;
    // only the pending part is listed: a cleaned file lists nothing
    Ok(Inspection { format: name.into(), findings, notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let mut d = crate::image::read_all(src)?;
    let (patches, _, _) = plan(&d, opts)?;
    let len = d.len();
    for (off, bytes) in &patches {
        d[*off..*off + bytes.len()].copy_from_slice(bytes);
    }
    debug_assert_eq!(d.len(), len);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, &d)?;
    f.sync_all()?;
    drop(f);
    // the same walk over the result: nothing left to clean
    let (left, found, _) = plan(&d, opts)?;
    let pending = left.iter().any(|(_, b)| b.iter().any(|c| *c != 0)) || !found.is_empty();
    if pending {
        return Err(ScrubError::Verify("metadata items are still in the file".into()));
    }
    Ok(())
}
