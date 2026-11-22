//! The second look at a cleaned file. It knows nothing of what the remover planned to do; it checks the result two ways:
//! - the STRUCTURE: it walks the cleaned file and reports anything that is not on the format's allow-list (what is needed to show
//!   or play the content), whatever the remover thought of it;
//! - the BYTES: it scans the raw file (and the unpacked parts of zip containers and PDF streams) for the actual values the lister
//!   found in the ORIGINAL (author names, serials, GPS numbers, paths, device names). Any hit means the file failed.
//!
//! Short values (under 6 bytes, a three-letter name for example) are not searched for in the bytes: they would match by chance.

use crate::{Family, Inspection, Options, Result, ScrubError};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MIN_NEEDLE: usize = 6;
const MAX_UNPACK: u64 = 256 << 20;

struct Needle {
    show: String,
    forms: Vec<Vec<u8>>,
}

/// The check run after every clean, before the file is moved into place.
pub(crate) fn second_look(src: &Path, out: &Path, before: &Inspection, fam: Family, opts: &Options) -> Result<()> {
    if let Some(i) = structure(out, fam, opts)?.into_iter().next() {
        return Err(ScrubError::Verify(format!("the second check found {i}")));
    }
    let needles = needles(src, before, fam);
    if let Some(hit) = scan(out, fam, &needles)? {
        return Err(ScrubError::Verify(format!("the second check found “{hit}” from the original still in the cleaned file")));
    }
    Ok(())
}

/// What the structural walk finds in a file that is not on its format's allow-list (empty: nothing). For "Verify".
pub fn leftovers(path: &Path, opts: &Options) -> Result<Vec<String>> {
    let (fam, _) = crate::family_of(path)?;
    structure(path, fam, opts)
}

// ---------------------------------------------------------------- the values to look for

/// Labels of findings whose values say who, with what or where (titles and the like can be part of the visible content).
const IDENTITY: [&str; 21] = [
    "author", "creator", "modified by", "company", "manager", "artist", "owner", "serial", "camera", "model", "lens", "user", "path",
    "host", "copyright", "signature", "producer", "software", "created with", "encoded", "reserved",
];

fn forms(s: &str) -> Vec<Vec<u8>> {
    let mut v = vec![s.as_bytes().to_vec()];
    let le: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let be: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
    v.push(le);
    v.push(be);
    let esc = s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;");
    if esc != s {
        v.push(esc.into_bytes());
    }
    v
}

fn push_text(out: &mut Vec<Needle>, s: &str) {
    let s = s.trim_end_matches('…').trim().trim_matches('\0');
    if s.len() >= MIN_NEEDLE && s.chars().any(|c| c.is_alphanumeric()) && !out.iter().any(|n| n.show == s) {
        out.push(Needle { show: s.to_string(), forms: forms(s) });
    }
}

fn needles(src: &Path, before: &Inspection, fam: Family) -> Vec<Needle> {
    let mut out = Vec::new();
    for f in before.to_remove() {
        let l = f.label.to_ascii_lowercase();
        if !IDENTITY.iter().any(|k| l.contains(k)) {
            continue;
        }
        // "3 places: Name, Other Name" (Office) lists the values after the count
        let v = f.value.split_once(" places: ").map(|(_, r)| r).unwrap_or(&f.value);
        if v.contains(", ") {
            for p in v.split(", ") {
                push_text(&mut out, p);
            }
        }
        push_text(&mut out, v);
    }
    if matches!(fam, Family::Jpeg | Family::Png | Family::Webp | Family::Tiff | Family::Heif(_)) {
        if let Ok(f) = fs::File::open(src) {
            if let Ok(ex) = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)) {
                for fld in ex.fields() {
                    match &fld.value {
                        exif::Value::Ascii(vs) => {
                            for v in vs {
                                push_text(&mut out, &String::from_utf8_lossy(v));
                            }
                        }
                        exif::Value::Rational(rs) if fld.tag.context() == exif::Context::Gps && !rs.is_empty() => {
                            let be: Vec<u8> = rs.iter().flat_map(|r| [r.num.to_be_bytes(), r.denom.to_be_bytes()].concat()).collect();
                            let le: Vec<u8> = rs.iter().flat_map(|r| [r.num.to_le_bytes(), r.denom.to_le_bytes()].concat()).collect();
                            out.push(Needle { show: format!("GPS {}", fld.tag), forms: vec![be, le] });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- the byte scan

fn hit_in(hay: &[u8], needles: &[Needle]) -> Option<String> {
    needles.iter().find(|n| n.forms.iter().any(|f| memchr::memmem::find(hay, f).is_some())).map(|n| n.show.clone())
}

/// The raw file in blocks (with an overlap so a value across two blocks is found), then the unpacked parts.
fn scan(out: &Path, fam: Family, needles: &[Needle]) -> Result<Option<String>> {
    if needles.is_empty() {
        return Ok(None);
    }
    let longest = needles.iter().flat_map(|n| n.forms.iter().map(|f| f.len())).max().unwrap_or(0);
    let mut f = fs::File::open(out)?;
    let block = (8 << 20).max(longest * 2);
    let mut buf = vec![0u8; block + longest];
    let mut keep = 0usize;
    loop {
        let n = f.read(&mut buf[keep..])?;
        if n == 0 {
            break;
        }
        let end = keep + n;
        if let Some(h) = hit_in(&buf[..end], needles) {
            return Ok(Some(h));
        }
        keep = longest.min(end);
        buf.copy_within(end - keep..end, 0);
    }
    match fam {
        Family::Ooxml | Family::Odf => {
            let mut z = zip::ZipArchive::new(fs::File::open(out)?).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
            for i in 0..z.len() {
                let mut e = z.by_index(i).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
                let mut b = Vec::new();
                (&mut e).take(MAX_UNPACK).read_to_end(&mut b).map_err(|e| ScrubError::Verify(format!("an entry of the cleaned file is damaged: {e}")))?;
                if let Some(h) = hit_in(&b, needles) {
                    return Ok(Some(h));
                }
            }
        }
        Family::Pdf => {
            let doc = lopdf::Document::load(out).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
            for o in doc.objects.values() {
                if let lopdf::Object::Stream(s) = o {
                    let flate = match s.dict.get(b"Filter") {
                        Ok(lopdf::Object::Name(n)) => n == b"FlateDecode",
                        Ok(lopdf::Object::Array(a)) => a.first().is_some_and(|x| matches!(x, lopdf::Object::Name(n) if n == b"FlateDecode")),
                        _ => false,
                    };
                    if flate {
                        let mut b = Vec::new();
                        let _ = flate2::read::ZlibDecoder::new(&s.content[..]).take(MAX_UNPACK).read_to_end(&mut b);
                        if let Some(h) = hit_in(&b, needles) {
                            return Ok(Some(h));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(None)
}

// ---------------------------------------------------------------- the structural walk

fn structure(path: &Path, fam: Family, opts: &Options) -> Result<Vec<String>> {
    let mut v = match fam {
        Family::Jpeg => jpeg(&crate::image::read_all(path)?, opts),
        Family::Png => png(&crate::image::read_all(path)?, opts),
        Family::Webp => webp(&crate::image::read_all(path)?, opts),
        Family::Gif => gif(&crate::image::read_all(path)?, opts),
        Family::Tiff => tiff(&crate::image::read_all(path)?, opts),
        Family::Heif(_) => heif(&crate::image::read_all(path)?),
        Family::Mp4 => mp4(path, opts)?,
        Family::Pdf => pdf(path)?,
        Family::Ooxml | Family::Odf => office(path)?,
        Family::Svg => svg(path)?,
        Family::Tagged => tagged(path, opts),
        _ => Vec::new(),
    };
    for s in crate::streams::list_streams(path) {
        v.push(s.label);
    }
    Ok(v)
}

/// An Exif block that holds nothing but the orientation (what the remover writes back for a picture that is not upright).
fn orientation_only(tiff: &[u8]) -> bool {
    match exif::Reader::new().read_raw(tiff.to_vec()) {
        Ok(ex) => ex.fields().all(|f| f.tag == exif::Tag::Orientation && f.ifd_num == exif::In::PRIMARY),
        Err(_) => false,
    }
}

fn jpeg(d: &[u8], opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    let mut pos = 2usize;
    loop {
        while pos < d.len() && d[pos] == 0xFF {
            pos += 1;
        }
        let Some(&m) = d.get(pos) else {
            v.push("a JPEG without its end".into());
            break;
        };
        pos += 1;
        if m == 0xD9 {
            if pos < d.len() {
                v.push(format!("{} bytes after the end of the picture", d.len() - pos));
            }
            break;
        }
        if matches!(m, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        let Some(len) = d.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize) else { break };
        let end = pos + len;
        let p = d.get(pos + 2..end).unwrap_or(&[]);
        match m {
            0xE0 if p.starts_with(b"JFIF\0") && p.len() >= 14 && p[12] == 0 && p[13] == 0 => {}
            0xE1 if p.starts_with(b"Exif\0\0") && orientation_only(&p[6..]) => {}
            0xE2 if p.starts_with(b"ICC_PROFILE\0") && opts.keep_color_profile => {}
            0xEE if p.starts_with(b"Adobe") => {}
            0xE0..=0xEF => v.push(format!("an application segment (APP{})", m - 0xE0)),
            0xFE => v.push("a comment".into()),
            0xDA => {
                let mut q = end;
                while q + 1 < d.len() && !(d[q] == 0xFF && d[q + 1] != 0 && !(0xD0..=0xD7).contains(&d[q + 1]) && d[q + 1] != 0xFF) {
                    q += 1;
                }
                pos = q;
                continue;
            }
            _ => {}
        }
        pos = end;
    }
    v
}

fn png(d: &[u8], opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    let mut pos = 8usize;
    while pos + 12 <= d.len() {
        let len = u32::from_be_bytes([d[pos], d[pos + 1], d[pos + 2], d[pos + 3]]) as usize;
        let ty: [u8; 4] = d[pos + 4..pos + 8].try_into().unwrap();
        let body = d.get(pos + 8..pos + 8 + len).unwrap_or(&[]);
        let ok = crate::image::PNG_KEEP.iter().any(|k| **k == ty) || (&ty == b"iCCP" && opts.keep_color_profile) || (&ty == b"eXIf" && orientation_only(body));
        if !ok {
            v.push(format!("a {} chunk", String::from_utf8_lossy(&ty)));
        }
        pos += 12 + len;
        if &ty == b"IEND" {
            break;
        }
    }
    if pos < d.len() {
        v.push(format!("{} bytes after the end of the picture", d.len() - pos));
    }
    v
}

fn webp(d: &[u8], opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    if d.len() < 12 {
        return v;
    }
    let riff = (u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize + 8).min(d.len());
    let mut pos = 12usize;
    while pos + 8 <= riff {
        let ty: [u8; 4] = d[pos..pos + 4].try_into().unwrap();
        let len = u32::from_le_bytes([d[pos + 4], d[pos + 5], d[pos + 6], d[pos + 7]]) as usize;
        let body = d.get(pos + 8..pos + 8 + len).unwrap_or(&[]);
        let ok = matches!(&ty, b"VP8X" | b"VP8 " | b"VP8L" | b"ALPH" | b"ANIM" | b"ANMF") || (&ty == b"ICCP" && opts.keep_color_profile) || (&ty == b"EXIF" && orientation_only(body.strip_prefix(b"Exif\0\0").unwrap_or(body)));
        if !ok {
            v.push(format!("a {} chunk", String::from_utf8_lossy(&ty)));
        }
        pos += 8 + len + (len & 1);
    }
    if riff < d.len() {
        v.push(format!("{} bytes after the end of the picture", d.len() - riff));
    }
    v
}

fn gif(d: &[u8], opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    if d.len() < 13 {
        return v;
    }
    let mut pos = 13usize;
    if d[10] & 0x80 != 0 {
        pos += 3 << ((d[10] & 7) + 1);
    }
    let skip_blocks = |mut p: usize| -> (usize, Vec<u8>) {
        let mut data = Vec::new();
        while let Some(&n) = d.get(p) {
            p += 1;
            if n == 0 {
                break;
            }
            data.extend_from_slice(d.get(p..p + n as usize).unwrap_or(&[]));
            p += n as usize;
        }
        (p, data)
    };
    while let Some(&b) = d.get(pos) {
        match b {
            0x3B => {
                pos += 1;
                break;
            }
            0x2C => {
                let packed = d.get(pos + 9).copied().unwrap_or(0);
                pos += 10;
                if packed & 0x80 != 0 {
                    pos += 3 << ((packed & 7) + 1);
                }
                pos = skip_blocks(pos + 1).0;
            }
            0x21 => {
                let label = d.get(pos + 1).copied().unwrap_or(0);
                let (end, data) = skip_blocks(pos + 2);
                let id = &data[..data.len().min(11)];
                let ok = matches!(label, 0xF9 | 0x01) || (label == 0xFF && (id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0" || (id == b"ICCRGBG1012" && opts.keep_color_profile)));
                if !ok {
                    v.push(format!("an extension block (0x{label:02X})"));
                }
                pos = end;
            }
            _ => break,
        }
    }
    if pos < d.len() {
        v.push(format!("{} bytes after the end of the picture", d.len() - pos));
    }
    v
}

fn tiff(d: &[u8], opts: &Options) -> Vec<String> {
    let mut v = Vec::new();
    let le = d.starts_with(b"II");
    let u16_at = |p: usize| d.get(p..p + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |p: usize| d.get(p..p + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } as usize);
    let mut off = u32_at(4).unwrap_or(0);
    let mut seen = 0;
    while off != 0 && seen < 4096 {
        seen += 1;
        let Some(n) = u16_at(off) else { break };
        for i in 0..n as usize {
            let Some(tag) = u16_at(off + 2 + 12 * i) else { break };
            if !(crate::tiff::KEEP.contains(&tag) || (tag == 34675 && opts.keep_color_profile)) {
                v.push(format!("the TIFF tag {tag}"));
            }
        }
        off = u32_at(off + 2 + 12 * n as usize).unwrap_or(0);
    }
    v
}

/// (type, start of body, end) of the boxes in `d[start..end]`.
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut v = Vec::new();
    let mut p = start;
    while p + 8 <= end {
        let s = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
        let ty: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (hdr, size) = match s {
            0 => (8, end - p),
            1 if p + 16 <= end => (16, u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize),
            n => (8, n),
        };
        if size < hdr || size > end - p {
            break;
        }
        v.push((ty, p + hdr, p + size));
        p += size;
    }
    v
}

fn heif(d: &[u8]) -> Vec<String> {
    let mut v = Vec::new();
    let zero = |a: usize, b: usize| d[a..b].iter().all(|c| *c == 0);
    for (ty, s, e) in boxes(d, 0, d.len()) {
        match &ty {
            b"ftyp" | b"mdat" => {}
            b"free" | b"skip" if zero(s, e) => {}
            b"meta" => {
                for (k, ks, ke) in boxes(d, s + 4, e) {
                    match &k {
                        b"hdlr" | b"pitm" | b"iloc" | b"iref" | b"iprp" | b"idat" | b"dinf" | b"grpl" | b"ipro" => {}
                        b"free" | b"skip" if zero(ks, ke) => {}
                        b"iinf" => {
                            let skip = if d.get(ks) == Some(&0) { 6 } else { 8 };
                            for (inf, is, ie) in boxes(d, ks + skip, ke) {
                                let at = if d.get(is) == Some(&3) { is + 10 } else { is + 8 };
                                if &inf == b"infe" && ie >= at + 4 && matches!(&d[at..at + 4], b"Exif" | b"mime" | b"xml ") {
                                    v.push(format!("a {} item", String::from_utf8_lossy(&d[at..at + 4]).trim()));
                                }
                            }
                        }
                        other => v.push(format!("a {} box in the item list", String::from_utf8_lossy(other).trim())),
                    }
                }
            }
            other => v.push(format!("a {} box", String::from_utf8_lossy(other).trim())),
        }
    }
    v
}

fn mp4(path: &Path, opts: &Options) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut pos = 0u64;
    let nonzero = |f: &mut fs::File, a: u64, n: u64| -> Result<bool> {
        f.seek(SeekFrom::Start(a))?;
        let mut left = n;
        let mut buf = vec![0u8; n.min(1 << 20) as usize];
        while left > 0 {
            let k = left.min(buf.len() as u64) as usize;
            f.read_exact(&mut buf[..k])?;
            if buf[..k].iter().any(|b| *b != 0) {
                return Ok(true);
            }
            left -= k as u64;
        }
        Ok(false)
    };
    while pos + 8 <= len {
        let mut h = [0u8; 16];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut h[..(len - pos).min(16) as usize])?;
        let s = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        let ty: [u8; 4] = h[4..8].try_into().unwrap();
        let (hdr, size) = match s {
            0 => (8, len - pos),
            1 => (16, u64::from_be_bytes(h[8..16].try_into().unwrap())),
            n => (8, n),
        };
        if size < hdr || pos + size > len {
            break;
        }
        match &ty {
            b"mdat" | b"ftyp" | b"wide" | b"moof" | b"mfra" | b"sidx" | b"ssix" | b"styp" | b"pdin" | b"emsg" => {}
            b"free" | b"skip" => {
                if nonzero(&mut f, pos + hdr, size - hdr)? {
                    v.push("data in a padding box".into());
                }
            }
            b"moov" if size - hdr <= 512 << 20 => {
                let mut body = vec![0u8; (size - hdr) as usize];
                f.seek(SeekFrom::Start(pos + hdr))?;
                f.read_exact(&mut body)?;
                moov(&body, opts, &mut v);
            }
            other => v.push(format!("a {} box", String::from_utf8_lossy(other).trim())),
        }
        pos += size;
    }
    Ok(v)
}

fn moov(b: &[u8], opts: &Options, v: &mut Vec<String>) {
    let zero = |a: usize, e: usize| b[a..e].iter().all(|c| *c == 0);
    // version 0: 4 bytes each, version 1: 8 bytes each, right after version / flags
    let times_zero = |s: usize, e: usize| {
        let n = if b.get(s) == Some(&1) { 16 } else { 8 };
        e >= s + 4 + n && zero(s + 4, s + 4 + n)
    };
    for (ty, s, e) in boxes(b, 0, b.len()) {
        match &ty {
            b"mvhd" if !times_zero(s, e) => v.push("the movie's creation time".into()),
            b"mvhd" | b"mvex" | b"iods" => {}
            b"free" | b"skip" if zero(s, e) => {}
            b"trak" => {
                let (mut handler, mut enabled) = ([0u8; 4], false);
                for (t, ts, te) in boxes(b, s, e) {
                    match &t {
                        b"tkhd" => {
                            enabled = b.get(ts + 3).is_some_and(|f| f & 1 != 0);
                            if !times_zero(ts, te) {
                                v.push("a track's creation time".into());
                            }
                        }
                        b"mdia" => {
                            for (m, ms, me) in boxes(b, ts, te) {
                                match &m {
                                    b"mdhd" if !times_zero(ms, me) => v.push("a track's media time".into()),
                                    b"hdlr" if me >= ms + 12 => {
                                        handler.copy_from_slice(&b[ms + 8..ms + 12]);
                                        if me > ms + 24 && !zero(ms + 24, me) {
                                            v.push("a handler name".into());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        b"edts" | b"tref" | b"tapt" | b"load" | b"imap" | b"txas" | b"clip" | b"matt" | b"kmat" => {}
                        b"free" | b"skip" if zero(ts, te) => {}
                        other => v.push(format!("a {} box in a track", String::from_utf8_lossy(other).trim())),
                    }
                }
                let subtitle = matches!(&handler, b"sbtl" | b"subt" | b"text" | b"clcp");
                let allowed = &handler == b"vide" || &handler == b"soun" || (subtitle && opts.keep_subtitles);
                if !allowed && enabled {
                    v.push(format!("a {} track that is still enabled", String::from_utf8_lossy(&handler).trim()));
                }
            }
            other => v.push(format!("a {} box in the movie header", String::from_utf8_lossy(other).trim())),
        }
    }
}

fn pdf(path: &Path) -> Result<Vec<String>> {
    use lopdf::Object;
    let mut v = Vec::new();
    let doc = lopdf::Document::load(path).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
    if doc.trailer.has(b"Info") {
        v.push("a document information dictionary".into());
    }
    if doc.trailer.has(b"ID") {
        v.push("a document ID".into());
    }
    if doc.catalog().is_ok_and(|c| c.has(b"Perms") || c.has(b"DSS")) {
        v.push("signature data".into());
    }
    for o in doc.objects.values() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        for k in [b"Metadata".as_slice(), b"Thumb", b"PieceInfo", b"LastModified"] {
            if d.has(k) {
                v.push(format!("a /{} entry", String::from_utf8_lossy(k)));
            }
        }
        let markup = matches!(d.get(b"Subtype"), Ok(Object::Name(n)) if n != b"Widget" && n != b"Link" && n != b"Popup") && d.has(b"Rect");
        if markup && (d.has(b"T") || d.has(b"M") || d.has(b"CreationDate")) {
            v.push("a comment with its author or date".into());
        }
        if matches!(d.get(b"Type"), Ok(Object::Name(n)) if n == b"Sig" || n == b"DocTimeStamp") {
            v.push("a signature".into());
        }
    }
    Ok(v)
}

fn office(path: &Path) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let mut z = zip::ZipArchive::new(fs::File::open(path)?).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
    let names: Vec<String> = z.file_names().filter_map(|s| s.ok().map(|c| c.into_owned())).collect();
    for n in &names {
        if n.starts_with("docProps/thumbnail") || n.starts_with("Thumbnails/") || n.contains("/printerSettings/") || n.starts_with("_xmlsignatures/") || (n.starts_with("META-INF/") && n.to_ascii_lowercase().contains("signatures")) {
            v.push(format!("the part {n}"));
        }
    }
    let mut text = |n: &str| -> Option<String> {
        let mut e = z.by_name(n).ok()?;
        let mut s = String::new();
        (&mut e).take(64 << 20).read_to_string(&mut s).ok()?;
        Some(s)
    };
    for n in ["docProps/core.xml", "docProps/app.xml", "docProps/custom.xml"] {
        if let Some(x) = text(n) {
            if roxmltree::Document::parse(&x).map(|d| d.root_element().children().any(|c| c.is_element())).unwrap_or(true) {
                v.push(format!("document properties in {n}"));
            }
        }
    }
    if let Some(x) = text("meta.xml") {
        let filled = roxmltree::Document::parse(&x)
            .map(|d| d.descendants().any(|n| n.is_element() && n.tag_name().name() == "meta" && n.children().any(|c| c.is_element())))
            .unwrap_or(true);
        if filled {
            v.push("document properties in meta.xml".into());
        }
    }
    if let Some(x) = text("xl/workbook.xml") {
        if x.contains("absPath") || x.contains("revisionPtr") || x.contains("userName=") {
            v.push("the saved folder path or user name in the workbook".into());
        }
    }
    Ok(v)
}

fn svg(path: &Path) -> Result<Vec<String>> {
    let mut v = Vec::new();
    let s = fs::read_to_string(path)?;
    let doc = roxmltree::Document::parse(s.trim_start_matches('\u{feff}')).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
    for n in doc.descendants() {
        if n.is_comment() {
            v.push("a comment".into());
        } else if n.is_pi() {
            v.push("a processing instruction".into());
        } else if n.is_element() {
            let ns = n.tag_name().namespace();
            if crate::svg::is_editor(ns) || (ns == Some(crate::svg::SVG_NS) && n.tag_name().name() == "metadata") {
                v.push(format!("the element {}", n.tag_name().name()));
            }
            if n.attributes().any(|a| crate::svg::is_editor(a.namespace())) {
                v.push("an editor attribute".into());
            }
        }
    }
    Ok(v)
}

fn tagged(path: &Path, opts: &Options) -> Vec<String> {
    use lofty::file::TaggedFileExt;
    let mut v = Vec::new();
    let read = lofty::probe::Probe::open(path).ok().and_then(|p| p.guess_file_type().ok()).and_then(|p| p.read().ok());
    if let Some(t) = read {
        for tag in t.tags() {
            if tag.item_count() > 0 {
                v.push(format!("{} tag fields", tag.item_count()));
            }
            if tag.picture_count() > 0 && !opts.keep_cover_art {
                v.push("cover art".into());
            }
        }
    }
    if let Ok(more) = crate::tags::extra(path, false) {
        v.extend(more.into_iter().map(|f| f.label));
    }
    v
}
