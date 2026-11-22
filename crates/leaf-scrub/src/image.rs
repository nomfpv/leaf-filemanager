//! JPEG, PNG and WebP: segment / chunk level removal. The pixel data is copied byte for byte, never decoded or re-encoded.

use crate::{Family, Finding, Inspection, Options, Result, ScrubError, MAX_IN_MEMORY};
use std::fs;
use std::path::Path;

pub(crate) fn read_all(path: &Path) -> Result<Vec<u8>> {
    let len = fs::metadata(path)?.len();
    if len > MAX_IN_MEMORY {
        return Err(ScrubError::Unsupported("the picture is bigger than 1 GB".into()));
    }
    Ok(fs::read(path)?)
}

pub(crate) fn inspect(path: &Path, fam: Family, opts: &Options) -> Result<Inspection> {
    let data = read_all(path)?;
    let (_, findings) = process(&data, fam, opts)?;
    Ok(Inspection {
        format: match fam {
            Family::Jpeg => "JPEG",
            Family::Png => "PNG",
            _ => "WebP",
        }
        .into(),
        findings,
        notes: vec!["The picture itself is copied byte for byte, so its quality does not change.".into()],
        can_clean: true,
        refusal: None,
    })
}

/// TIFF / HEIC / AVIF / GIF: what the EXIF reader finds is listed; a run refuses them.
pub(crate) fn inspect_unwritable(path: &Path, kind: &'static str) -> Inspection {
    let mut findings = Vec::new();
    if let Ok(f) = fs::File::open(path) {
        let mut r = std::io::BufReader::new(f);
        if let Ok(ex) = exif::Reader::new().read_from_container(&mut r) {
            findings = exif_findings(&ex, true);
        }
    }
    Inspection {
        format: kind.into(),
        findings,
        notes: vec![],
        can_clean: false,
        refusal: Some(match kind {
            "camera RAW" => crate::tiff::RAW_TEXT.to_string(),
            "Matroska / WebM" => "Matroska / WebM files cannot be rewritten without re-encoding or a risk of damaging them: they are refused and left untouched; their tags, title and writing program stay inside".to_string(),
            "AVI" => "AVI files cannot be rewritten without re-encoding or a risk of damaging them: they are refused and left untouched; their tags and writing program stay inside".to_string(),
            "old Office" => "old Office files (.doc, .xls, .ppt) and password-protected Office files carry author names leaf cannot rewrite: they are refused and left untouched; save them as .docx, .xlsx or .pptx without a password first".to_string(),
            _ => format!("not supported: {kind} files carry metadata leaf has no remover for, so the file is refused and left untouched"),
        }),
    }
}

pub(crate) fn produce(src: &Path, dst: &Path, fam: Family, opts: &Options) -> Result<()> {
    let data = read_all(src)?;
    let (out, _) = process(&data, fam, opts)?;
    // The written file must be the same picture: same length of picture data is checked by the callers' read-back;
    // here a cleaned file that is bigger than the original is a bug, never written.
    if out.len() > data.len() + 64 {
        return Err(ScrubError::Verify("the cleaned file came out bigger than the original".into()));
    }
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, &out)?;
    f.sync_all()?;
    Ok(())
}

pub(crate) fn process(data: &[u8], fam: Family, opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    match fam {
        Family::Jpeg => jpeg(data, opts),
        Family::Png => png(data, opts),
        _ => webp(data, opts),
    }
}

fn bad<T>(what: &str) -> Result<T> {
    Err(ScrubError::Malformed(what.into()))
}

// ---------------------------------------------------------------- EXIF

/// The TIFF bytes of an Exif block that holds only the orientation (written when the picture is not upright, so it still shows right).
fn orientation_tiff(o: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"MM\0*");
    v.extend_from_slice(&8u32.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&0x0112u16.to_be_bytes());
    v.extend_from_slice(&3u16.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&o.to_be_bytes());
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(&0u32.to_be_bytes());
    v
}

fn orientation_of(ex: &exif::Exif) -> Option<u16> {
    let f = ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    let o = f.value.get_uint(0)? as u16;
    (2..=8).contains(&o).then_some(o)
}

fn exif_findings(ex: &exif::Exif, all_remove: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut thumb = false;
    for f in ex.fields() {
        if f.ifd_num == exif::In::THUMBNAIL {
            thumb = true;
            continue;
        }
        if f.tag == exif::Tag::Orientation && !all_remove {
            continue;
        }
        let tag = f.tag;
        let desc = tag.description().map(|s| s.to_string()).unwrap_or_else(|| format!("tag 0x{:04x}", tag.number()));
        let label = match tag.context() {
            exif::Context::Gps => format!("GPS {desc}"),
            _ if tag == exif::Tag::MakerNote => "Maker note".to_string(),
            _ => desc,
        };
        let value = if tag == exif::Tag::MakerNote || matches!(f.value, exif::Value::Undefined(..)) {
            match &f.value {
                exif::Value::Undefined(b, _) => format!("{} bytes", b.len()),
                _ => String::new(),
            }
        } else {
            f.display_value().with_unit(ex).to_string()
        };
        out.push(Finding::new(label, value, true));
    }
    if thumb {
        out.push(Finding::new("Embedded thumbnail picture", "", true));
    }
    out
}

/// Findings of an Exif block, and the orientation to keep (if the picture is not upright).
pub(crate) fn exif_block(tiff: &[u8]) -> (Vec<Finding>, Option<u16>) {
    match exif::Reader::new().read_raw(tiff.to_vec()) {
        Ok(ex) => {
            let o = orientation_of(&ex);
            let mut v = exif_findings(&ex, false);
            if let Some(o) = o {
                v.push(Finding::new("Orientation", format!("{o} (kept so the picture shows the right way up)"), false));
            } else if let Some(f) = ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
                // "normal" orientation: nothing to keep, nothing worth listing
                let _ = f;
            }
            (v, o)
        }
        Err(_) => {
            // the reader gave up (a damaged maker note, an odd value type): the orientation is read straight from the first
            // directory, so a picture that is not upright does not come out rotated
            let o = raw_orientation(tiff);
            let mut v = vec![Finding::new("Exif data", "could not be read, removed whole", true)];
            if let Some(o) = o {
                v.push(Finding::new("Orientation", format!("{o} (kept so the picture shows the right way up)"), false));
            }
            (v, o)
        }
    }
}

/// Tag 0x0112 of the first directory of a TIFF / Exif block, read without the Exif reader (2 to 8; 1 means upright).
fn raw_orientation(t: &[u8]) -> Option<u16> {
    let le = match t.get(0..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |p: usize| t.get(p..p + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let off = t.get(4..8).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) })? as usize;
    let n = u16_at(off)? as usize;
    (0..n).find_map(|i| {
        let at = off + 2 + 12 * i;
        (u16_at(at)? == 0x0112 && u16_at(at + 2)? == 3).then(|| u16_at(at + 8)).flatten()
    })
    .filter(|o| (2..=8).contains(o))
}

// ---------------------------------------------------------------- JPEG

fn jpeg(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return bad("no JPEG start marker");
    }
    let mut out = vec![0xFF, 0xD8];
    let mut found = Vec::new();
    let mut orient: Option<u16> = None;
    let mut pos = 2usize;
    let mut exif_at: Option<usize> = None;
    let mut saw_sof = false;
    loop {
        // marker
        if pos < d.len() && d[pos] != 0xFF {
            return bad("data where a marker should be");
        }
        while pos < d.len() && d[pos] == 0xFF {
            pos += 1;
        }
        if pos >= d.len() {
            return bad("no end of image marker");
        }
        let m = d[pos];
        pos += 1;
        match m {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                if !saw_sof {
                    return bad("no picture data");
                }
                if pos < d.len() {
                    found.push(Finding::new("Data after the end of the picture", format!("{} bytes", d.len() - pos), true));
                }
                break;
            }
            0x00 | 0xD8 => return bad("unexpected marker"),
            0x01 | 0xD0..=0xD7 => {
                out.extend_from_slice(&[0xFF, m]);
                continue;
            }
            _ => {}
        }
        if pos + 2 > d.len() {
            return bad("cut off inside a segment header");
        }
        let len = u16::from_be_bytes([d[pos], d[pos + 1]]) as usize;
        if len < 2 || pos + len > d.len() {
            return bad("a segment runs past the end of the file");
        }
        let payload = &d[pos + 2..pos + len];
        let seg_end = pos + len;
        let keep_raw = |out: &mut Vec<u8>| {
            out.extend_from_slice(&[0xFF, m]);
            out.extend_from_slice(&d[pos..seg_end]);
        };
        match m {
            0xE0 => {
                if payload.starts_with(b"JFIF\0") && payload.len() >= 14 {
                    let (xt, yt) = (payload[12] as usize, payload[13] as usize);
                    if xt * yt > 0 {
                        found.push(Finding::new("Thumbnail picture in the JFIF header", "", true));
                    }
                    out.extend_from_slice(&[0xFF, 0xE0, 0, 16]);
                    let mut h = payload[..14].to_vec();
                    h[12] = 0;
                    h[13] = 0;
                    out.extend_from_slice(&h);
                } else {
                    found.push(Finding::new("Extension segment (APP0)", "", true));
                }
            }
            0xE1 => {
                if payload.starts_with(b"Exif\0\0") {
                    let (f, o) = exif_block(&payload[6..]);
                    found.extend(f);
                    if let Some(o) = o {
                        orient = Some(o);
                    }
                    if exif_at.is_none() {
                        exif_at = Some(out.len());
                    }
                } else if payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                    found.push(Finding::new("XMP", format!("{} bytes", payload.len()), true));
                } else if payload.starts_with(b"http://ns.adobe.com/xmp/extension/\0") {
                    found.push(Finding::new("XMP (extended part)", format!("{} bytes", payload.len()), true));
                } else {
                    found.push(Finding::new("Other application data (APP1)", "", true));
                }
            }
            0xE2 => {
                if payload.starts_with(b"ICC_PROFILE\0") {
                    found.push(Finding::new("Colour profile", "", !opts.keep_color_profile));
                    if opts.keep_color_profile {
                        keep_raw(&mut out);
                    }
                } else if payload.starts_with(b"MPF\0") {
                    found.push(Finding::new("Multi-picture data (extra embedded pictures)", "", true));
                } else {
                    found.push(Finding::new("Other application data (APP2)", "", true));
                }
            }
            0xED => found.push(Finding::new("IPTC / Photoshop data", format!("{} bytes", payload.len()), true)),
            0xEE if payload.starts_with(b"Adobe") => keep_raw(&mut out),
            0xE3..=0xEF => found.push(Finding::new(format!("Application data (APP{})", m - 0xE0), format!("{} bytes", payload.len()), true)),
            0xFE => found.push(Finding::new("Comment", String::from_utf8_lossy(payload), true)),
            0xC0..=0xCF if m != 0xC4 && m != 0xC8 && m != 0xCC => {
                saw_sof = true;
                keep_raw(&mut out);
            }
            0xDA => {
                keep_raw(&mut out);
                // entropy-coded data up to the next real marker
                let mut p = seg_end;
                loop {
                    if p >= d.len() {
                        return bad("the picture data is cut off");
                    }
                    if d[p] != 0xFF {
                        p += 1;
                        continue;
                    }
                    let Some(&b) = d.get(p + 1) else {
                        return bad("the picture data is cut off");
                    };
                    if b == 0 || (0xD0..=0xD7).contains(&b) {
                        p += 2;
                    } else if b == 0xFF {
                        p += 1;
                    } else {
                        break;
                    }
                }
                out.extend_from_slice(&d[seg_end..p]);
                pos = p;
                continue;
            }
            _ => keep_raw(&mut out),
        }
        pos = seg_end;
    }
    if let Some(o) = orient {
        // right after the start marker (and the JFIF header if it is first): Exif goes first among the segments
        let tiff = orientation_tiff(o);
        let mut seg = vec![0xFF, 0xE1];
        seg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&tiff);
        let at = exif_at.unwrap_or(2).max(2);
        // exif_at was taken while `out` was shorter or equal; it is a valid segment boundary of `out`
        let at = at.min(out.len());
        out.splice(at..at, seg);
    }
    Ok((out, found))
}

// ---------------------------------------------------------------- PNG

pub(crate) const PNG_KEEP: [&[u8; 4]; 17] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"sBIT", b"bKGD", b"hIST", b"pHYs", b"sPLT", b"acTL",
    b"fcTL", b"fdAT", b"cICP",
];

fn png(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    if !d.starts_with(b"\x89PNG\r\n\x1a\n") {
        return bad("no PNG signature");
    }
    let mut out = d[..8].to_vec();
    let mut found = Vec::new();
    let mut pos = 8usize;
    let mut orient: Option<u16> = None;
    let mut ended = false;
    let mut first = true;
    let mut idat_at: Option<usize> = None;
    while pos < d.len() {
        if pos + 12 > d.len() {
            return bad("a chunk is cut off");
        }
        let len = u32::from_be_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        let ty: [u8; 4] = [d[pos + 4], d[pos + 5], d[pos + 6], d[pos + 7]];
        if len > d.len() || pos + 12 + len > d.len() {
            return bad("a chunk runs past the end of the file");
        }
        let body = &d[pos + 8..pos + 8 + len];
        let crc = u32::from_be_bytes([d[pos + 8 + len], d[pos + 9 + len], d[pos + 10 + len], d[pos + 11 + len]]);
        if crc32fast::hash(&d[pos + 4..pos + 8 + len]) != crc {
            return bad("a chunk fails its checksum");
        }
        if first && &ty != b"IHDR" {
            return bad("the first chunk is not the header");
        }
        first = false;
        let raw = &d[pos..pos + 12 + len];
        let name = String::from_utf8_lossy(&ty).into_owned();
        if PNG_KEEP.iter().any(|k| **k == ty) {
            if &ty == b"IDAT" || &ty == b"fdAT" {
                idat_at.get_or_insert(out.len());
            }
            out.extend_from_slice(raw);
            if &ty == b"IEND" {
                ended = true;
                pos += 12 + len;
                break;
            }
        } else {
            match &ty {
                b"iCCP" => {
                    found.push(Finding::new("Colour profile", "", !opts.keep_color_profile));
                    if opts.keep_color_profile {
                        out.extend_from_slice(raw);
                    }
                }
                b"tEXt" => {
                    let (k, v) = split_nul(body);
                    found.push(Finding::new(format!("Text: {}", latin1(k)), latin1(v), true));
                }
                b"zTXt" => {
                    let (k, _) = split_nul(body);
                    found.push(Finding::new(format!("Text: {}", latin1(k)), "compressed", true));
                }
                b"iTXt" => {
                    let (k, rest) = split_nul(body);
                    let key = latin1(k);
                    if key == "XML:com.adobe.xmp" {
                        found.push(Finding::new("XMP", format!("{} bytes", rest.len()), true));
                    } else {
                        // compression flag, method, language\0, translated\0, text
                        let text = rest
                            .get(2..)
                            .map(|r| {
                                let (_, r) = split_nul(r);
                                let (_, r) = split_nul(r);
                                if rest[0] == 0 { String::from_utf8_lossy(r).into_owned() } else { "compressed".into() }
                            })
                            .unwrap_or_default();
                        found.push(Finding::new(format!("Text: {key}"), text, true));
                    }
                }
                b"eXIf" => {
                    let (f, o) = exif_block(body);
                    found.extend(f);
                    if o.is_some() {
                        orient = o;
                    }
                }
                b"tIME" if len == 7 => {
                    let y = u16::from_be_bytes([body[0], body[1]]);
                    found.push(Finding::new("Last modified", format!("{y:04}-{:02}-{:02} {:02}:{:02}:{:02}", body[2], body[3], body[4], body[5], body[6]), true));
                }
                _ => {
                    if ty[0] & 0x20 == 0 {
                        return Err(ScrubError::Unsupported(format!("the PNG has a chunk ({name}) leaf does not know and cannot drop safely")));
                    }
                    found.push(Finding::new(format!("Extra chunk ({name})"), format!("{len} bytes"), true));
                }
            }
        }
        pos += 12 + len;
    }
    if !ended {
        return bad("no end chunk");
    }
    if pos < d.len() {
        found.push(Finding::new("Data after the end of the picture", format!("{} bytes", d.len() - pos), true));
    }
    if let Some(o) = orient {
        let tiff = orientation_tiff(o);
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"eXIf");
        chunk.extend_from_slice(&tiff);
        let crc = crc32fast::hash(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        let at = idat_at.unwrap_or(out.len());
        out.splice(at..at, chunk);
    }
    Ok((out, found))
}

fn split_nul(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|c| *c == 0) {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[]),
    }
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|c| *c as char).collect()
}

// ---------------------------------------------------------------- WebP

fn webp(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    if d.len() < 20 || &d[0..4] != b"RIFF" || &d[8..12] != b"WEBP" {
        return bad("no WebP header");
    }
    let riff = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize + 8;
    if riff > d.len() || riff < 20 {
        return bad("the file is shorter than its header says");
    }
    let mut found = Vec::new();
    let mut chunks: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let mut pos = 12usize;
    let mut has_vp8x = false;
    let mut orient: Option<u16> = None;
    while pos < riff {
        if pos + 8 > riff {
            return bad("a chunk is cut off");
        }
        let ty: [u8; 4] = [d[pos], d[pos + 1], d[pos + 2], d[pos + 3]];
        let len = u32::from_le_bytes([d[pos + 4], d[pos + 5], d[pos + 6], d[pos + 7]]) as usize;
        let end = pos + 8 + len;
        if end > riff {
            return bad("a chunk runs past the end of the file");
        }
        let body = &d[pos + 8..end];
        match &ty {
            b"VP8X" => {
                has_vp8x = true;
                chunks.push((ty, body.to_vec()));
            }
            b"VP8 " | b"VP8L" | b"ALPH" | b"ANIM" | b"ANMF" => chunks.push((ty, body.to_vec())),
            b"ICCP" => {
                found.push(Finding::new("Colour profile", "", !opts.keep_color_profile));
                if opts.keep_color_profile {
                    chunks.push((ty, body.to_vec()));
                }
            }
            b"EXIF" => {
                let tiff = if body.starts_with(b"Exif\0\0") { &body[6..] } else { body };
                let (f, o) = exif_block(tiff);
                found.extend(f);
                if o.is_some() {
                    orient = o;
                }
            }
            b"XMP " => found.push(Finding::new("XMP", format!("{len} bytes"), true)),
            _ => found.push(Finding::new(format!("Extra chunk ({})", String::from_utf8_lossy(&ty)), format!("{len} bytes"), true)),
        }
        pos = end + (len & 1);
    }
    if !chunks.iter().any(|(t, _)| matches!(&t[..], b"VP8 " | b"VP8L" | b"ANMF")) {
        return bad("no picture data");
    }
    if riff < d.len() {
        found.push(Finding::new("Data after the end of the picture", format!("{} bytes", d.len() - riff), true));
    }
    if let (Some(o), true) = (orient, has_vp8x) {
        let mut b = b"Exif\0\0".to_vec();
        b.truncate(0);
        b.extend_from_slice(&orientation_tiff(o));
        chunks.push((*b"EXIF", b));
    } else if orient.is_some() {
        orient = None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF\0\0\0\0WEBP");
    for (t, body) in &mut chunks {
        if t == b"VP8X" && !body.is_empty() {
            body[0] &= !0x04; // XMP
            if orient.is_some() {
                body[0] |= 0x08;
            } else {
                body[0] &= !0x08;
            }
        }
    }
    let has_icc = chunks.iter().any(|(t, _)| t == b"ICCP");
    for (t, body) in &mut chunks {
        if t == b"VP8X" && !body.is_empty() {
            if has_icc {
                body[0] |= 0x20;
            } else {
                body[0] &= !0x20;
            }
        }
    }
    for (t, body) in &chunks {
        out.extend_from_slice(t);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() & 1 == 1 {
            out.push(0);
        }
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok((out, found))
}
