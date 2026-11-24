//! PDF: the Info dictionary, XMP streams, document ids, page thumbnails and application private data.
//! The file is rewritten as one clean revision (earlier saved versions inside the file go with it).
//! Comment author names and dates and attached files' dates are removed; the comments' text, form fields, JavaScript, layers
//! and the attachments themselves are NOT touched, and the notes say so.

use crate::{Finding, Inspection, Options, Result, ScrubError, MAX_IN_MEMORY};
use lopdf::{Dictionary, Document, Object};
use std::fs;
use std::path::Path;

const DROP_KEYS: [&[u8]; 4] = [b"Metadata", b"Thumb", b"PieceInfo", b"LastModified"];
/// Who wrote a comment and when (annotation dictionaries). `T` of a form field is its NAME, so widgets are never touched.
const ANNOT_KEYS: [&[u8]; 3] = [b"T", b"M", b"CreationDate"];
const MARKUP: [&[u8]; 15] = [
    b"Text", b"FreeText", b"Highlight", b"Underline", b"StrikeOut", b"Squiggly", b"Stamp", b"Caret", b"Ink", b"Line", b"Square", b"Circle",
    b"Polygon", b"PolyLine", b"FileAttachment",
];
/// Dates of an attached file, in the `Params` of its stream.
const PARAM_KEYS: [&[u8]; 2] = [b"CreationDate", b"ModDate"];

/// An annotation whose `T` is its author's name: every kind except form fields (`Widget`, where `T` is the field's name), links
/// and pop-ups (the known markup kinds of [`MARKUP`], and the rest: redactions, sounds, movies, screens, watermarks ...).
fn is_markup(d: &Dictionary) -> bool {
    match d.get(b"Subtype") {
        Ok(Object::Name(n)) => d.has(b"Rect") && (MARKUP.contains(&n.as_slice()) || !matches!(n.as_slice(), b"Widget" | b"Link" | b"Popup")),
        _ => false,
    }
}

/// The ids of dictionaries used as `Params` of a stream by reference (attached files' dates), and whether a direct one holds dates.
fn params_ids(doc: &Document) -> Vec<lopdf::ObjectId> {
    doc.objects
        .values()
        .filter_map(|o| match o {
            Object::Stream(s) => match s.dict.get(b"Params") {
                Ok(Object::Reference(id)) => Some(*id),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn load(path: &Path) -> Result<(Document, Vec<u8>)> {
    if fs::metadata(path)?.len() > MAX_IN_MEMORY {
        return Err(ScrubError::Unsupported("the PDF is bigger than 1 GB".into()));
    }
    let raw = fs::read(path)?;
    let tail_from = raw.len().saturating_sub(2048);
    let ends = end_markers(&raw);
    match ends.iter().filter(|(_, real)| *real).map(|(end, _)| *end).max() {
        // the last real end marker must close the file: one further up with a `%%EOF` near the end that is not real is a file
        // cut off inside an attached file, or one leaf cannot tell from it
        Some(last) if last < tail_from => {
            return Err(ScrubError::Malformed("the PDF does not end with its own end marker, it may be cut off".into()));
        }
        Some(_) => {}
        // no end marker leads to this file's cross-reference data (offsets all off, the reader rebuilds them): only the
        // marker's presence is checked
        None if !raw[tail_from..].windows(5).any(|w| w == b"%%EOF") => {
            return Err(ScrubError::Malformed("the PDF has no end marker, it is cut off".into()));
        }
        None => {}
    }
    guard(&raw)?;
    let doc = Document::load_mem(&raw).map_err(|e| ScrubError::Malformed(format!("the PDF could not be read: {e}")))?;
    if doc.trailer.has(b"Encrypt") || doc.is_encrypted() {
        return Err(ScrubError::Unsupported("the PDF is encrypted".into()));
    }
    if doc.get_pages().is_empty() {
        return Err(ScrubError::Malformed("the PDF has no pages".into()));
    }
    Ok((doc, raw))
}

/// The most a compressed object stream may unpack to, and how deep dictionaries and arrays may nest: a PDF built to exhaust
/// memory or the stack is refused before the reader sees it.
const MAX_UNPACKED: u64 = 256 << 20;
const MAX_DEPTH: usize = 100;

/// Checks what the PDF reader would otherwise do without a limit: unpack the object streams and cross-reference streams it reads
/// whole, and follow nesting.
fn guard(raw: &[u8]) -> Result<()> {
    use std::io::Read;
    // nesting of << >> and [ ] outside strings, comments and stream data
    let (mut depth, mut i) = (0usize, 0usize);
    while i < raw.len() {
        match raw[i] {
            b'(' => {
                let mut level = 1;
                i += 1;
                while i < raw.len() && level > 0 {
                    match raw[i] {
                        b'\\' => i += 1,
                        b'(' => level += 1,
                        b')' => level -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                continue;
            }
            b'%' => {
                while i < raw.len() && raw[i] != b'\n' && raw[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' if raw.get(i + 1) == Some(&b'<') => {
                depth += 1;
                i += 1;
            }
            b'>' if raw.get(i + 1) == Some(&b'>') => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b's' if raw[i..].starts_with(b"stream") && matches!(raw.get(i + 6), Some(b'\r' | b'\n')) => {
                let start = i + 6;
                match find(&raw[start..], b"endstream") {
                    Some(k) => {
                        let data = &raw[start..start + k];
                        // the dictionary before it: an object stream or cross-reference stream is unpacked by the reader itself
                        let dict_from = raw[..i].windows(3).rposition(|w| w == b"obj").unwrap_or(0);
                        let dict = &raw[dict_from..i];
                        if (find(dict, b"/ObjStm").is_some() || find(dict, b"/XRef").is_some()) && find(dict, b"/FlateDecode").is_some() {
                            let body = data.strip_prefix(b"\r\n").or_else(|| data.strip_prefix(b"\n")).unwrap_or(data);
                            let mut z = flate2::read::ZlibDecoder::new(body).take(MAX_UNPACKED + 1);
                            let n = std::io::copy(&mut z, &mut std::io::sink()).unwrap_or(0);
                            if n > MAX_UNPACKED || (n > 1 << 20 && n / (body.len() as u64).max(1) > 1000) {
                                return Err(ScrubError::Unsupported("the PDF has a compressed part that unpacks to far more than its size, leaf does not unpack it".into()));
                            }
                        }
                        i = start + k + 9;
                        continue;
                    }
                    None => break,
                }
            }
            _ => {}
        }
        if depth > MAX_DEPTH {
            return Err(ScrubError::Unsupported("the PDF nests its objects deeper than leaf reads".into()));
        }
        i += 1;
    }
    Ok(())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\r' | b'\n' | b'\t' | b'\x0c' | b'\0')
}

/// Does `off` lead to cross-reference data of this file: a table (`xref`) or a cross-reference stream object?
fn xref_at(raw: &[u8], off: usize) -> bool {
    let Some(rest) = raw.get(off..) else { return false };
    let rest = &rest[rest.iter().take(64).take_while(|c| is_space(**c)).count()..];
    if rest.starts_with(b"xref") {
        return true;
    }
    // `12 0 obj << /Type /XRef ... >> stream`
    let mut p = 0;
    for _ in 0..2 {
        let n = rest[p..].iter().take(10).take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        p += n;
        p += rest[p..].iter().take(16).take_while(|c| is_space(**c)).count();
    }
    if !rest[p..].starts_with(b"obj") {
        return false;
    }
    let head = &rest[p..rest.len().min(p + 4096)];
    let head = &head[..find(head, b"stream").unwrap_or(head.len())];
    head.windows(6).any(|w| w.starts_with(b"/XRef") && !w[5].is_ascii_alphanumeric())
}

/// Every `startxref <offset> %%EOF` of the file: where the marker ends, and whether it is REAL, its offset leading to this
/// file's cross-reference data. An attached PDF stored unpacked brings its own markers, with offsets that mean nothing here; a
/// bare `%%EOF` inside a stream is not counted at all.
fn end_markers(raw: &[u8]) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    for at in memchr::memmem::find_iter(raw, b"startxref") {
        let mut p = at + 9;
        p += raw[p..].iter().take(64).take_while(|c| is_space(**c)).count();
        let n = raw[p..].iter().take(20).take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            continue;
        }
        let off = std::str::from_utf8(&raw[p..p + n]).ok().and_then(|s| s.parse::<usize>().ok());
        p += n;
        p += raw[p..].iter().take(64).take_while(|c| is_space(**c)).count();
        if raw[p..].starts_with(b"%%EOF") {
            out.push((p + 5, off.is_some_and(|o| xref_at(raw, o))));
        }
    }
    out
}

/// How many saved versions the file holds: its real end markers, or, when none can be checked (offsets all off), every
/// `startxref ... %%EOF`.
pub(crate) fn revisions(raw: &[u8]) -> usize {
    let ends = end_markers(raw);
    match ends.iter().filter(|(_, real)| *real).count() {
        0 => ends.len(),
        n => n,
    }
}

/// The raw (still compressed) bytes of every content stream of every page: the rewrite must give the same back. Nothing is
/// unpacked for this.
fn page_streams(doc: &Document) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for id in doc.get_pages().values() {
        for c in doc.get_page_contents(*id) {
            if let Ok(Object::Stream(s)) = doc.get_object(c) {
                out.push(s.content.clone());
            }
        }
    }
    out
}

fn text(o: &Object, doc: &Document) -> String {
    match o {
        Object::String(b, _) => decode_text(b),
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        Object::Integer(i) => i.to_string(),
        Object::Real(r) => r.to_string(),
        Object::Boolean(b) => b.to_string(),
        Object::Reference(id) => doc.get_object(*id).map(|o| text(o, doc)).unwrap_or_default(),
        _ => String::new(),
    }
}

fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        b.iter().map(|c| *c as char).collect()
    }
}

/// A signature dictionary (or a document time stamp): the one that holds the signer's name, time, place, reason, contact and the
/// signed certificate data.
fn is_sig_dict(d: &Dictionary) -> bool {
    matches!(d.get(b"Type"), Ok(Object::Name(n)) if n == b"Sig" || n == b"DocTimeStamp") || (d.has(b"ByteRange") && d.has(b"Contents") && d.has(b"Filter"))
}

fn is_sig_field(d: &Dictionary) -> bool {
    matches!(d.get(b"FT"), Ok(Object::Name(n)) if n == b"Sig")
}

/// The AcroForm dictionary, wherever it is (direct in the catalog or an object of its own).
fn acroform_id(doc: &Document) -> Option<lopdf::ObjectId> {
    match doc.catalog().ok()?.get(b"AcroForm").ok()? {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

/// The signatures of the document: (who, when, where, why) per signature, and whether it carries any signature data at all
/// (signature dictionaries, permissions of a certified document, the document security store, the "signed" flag of the form).
fn signatures(doc: &Document) -> (Vec<String>, bool) {
    let mut who = Vec::new();
    for d in dicts(doc).filter(|d| is_sig_dict(d)) {
        let parts: Vec<String> = [b"Name".as_slice(), b"M", b"Location", b"Reason", b"ContactInfo"]
            .iter()
            .filter_map(|k| d.get(k).ok().map(|v| text(v, doc)))
            .filter(|t| !t.trim().is_empty())
            .collect();
        who.push(parts.join(", "));
    }
    let cat = doc.catalog().ok();
    let flags = |d: &Dictionary| d.has(b"SigFlags");
    let form_flag = cat.and_then(|c| c.get(b"AcroForm").ok()).is_some_and(|f| match f {
        Object::Dictionary(d) => flags(d),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok()).is_some_and(flags),
        _ => false,
    });
    let any = !who.is_empty() || cat.is_some_and(|c| c.has(b"Perms") || c.has(b"DSS")) || form_flag || dicts(doc).any(|d| is_sig_field(d) && d.has(b"V"));
    (who, any)
}

/// The refusal of a signed PDF when the signatures are not to be removed.
const SIGNED_TEXT: &str = "this PDF is signed: removing metadata breaks the signature. Tick “Remove digital signatures too” to clean it without them";

/// The JPEG pictures of the document (streams whose only filter is DCTDecode), with their bytes.
fn dct_pictures(doc: &Document) -> impl Iterator<Item = (lopdf::ObjectId, &[u8])> {
    doc.objects.iter().filter_map(|(id, o)| match o {
        Object::Stream(s) if is_dct(&s.dict) && s.content.starts_with(&[0xFF, 0xD8]) => Some((*id, s.content.as_slice())),
        _ => None,
    })
}

fn is_dct(d: &Dictionary) -> bool {
    match d.get(b"Filter") {
        Ok(Object::Name(n)) => n == b"DCTDecode",
        Ok(Object::Array(a)) => a.len() == 1 && matches!(&a[0], Object::Name(n) if n == b"DCTDecode"),
        _ => false,
    }
}

/// Does a JPEG carry Exif, XMP, IPTC, comments (anything the picture remover takes out except the colour profile)?
fn jpeg_metadata(c: &[u8]) -> bool {
    let keep = Options { keep_color_profile: true, ..Default::default() };
    crate::image::process(c, crate::Family::Jpeg, &keep).is_ok_and(|(_, f)| f.iter().any(|x| x.remove))
}

fn info_dict<'a>(doc: &'a Document) -> Option<&'a Dictionary> {
    match doc.trailer.get(b"Info").ok()? {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_dict().ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

fn dicts(doc: &Document) -> impl Iterator<Item = &Dictionary> {
    doc.objects.values().filter_map(|o| match o {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    })
}

fn analyse(doc: &Document, raw: &[u8]) -> (Vec<Finding>, Vec<String>) {
    let mut f = Vec::new();
    let mut notes = Vec::new();
    if let Some(info) = info_dict(doc) {
        for (k, v) in info.iter() {
            let name = String::from_utf8_lossy(k).into_owned();
            let label = match name.as_str() {
                "Author" => "Author".to_string(),
                "Title" => "Title".into(),
                "Subject" => "Subject".into(),
                "Keywords" => "Keywords".into(),
                "Creator" => "Created with".into(),
                "Producer" => "PDF producer".into(),
                "CreationDate" => "Created".into(),
                "ModDate" => "Last modified".into(),
                other => format!("Info: {other}"),
            };
            f.push(Finding::new(label, text(v, doc), true));
        }
    }
    let mut xmp = 0;
    let mut thumbs = 0;
    let mut piece = 0;
    for d in dicts(doc) {
        if d.has(b"Metadata") {
            xmp += 1;
        }
        if d.has(b"Thumb") {
            thumbs += 1;
        }
        if d.has(b"PieceInfo") {
            piece += 1;
        }
    }
    let pdfa = doc.objects.values().any(|o| match o {
        Object::Stream(s) if matches!(s.dict.get(b"Subtype"), Ok(Object::Name(n)) if n == b"XML") => {
            let mut b = Vec::new();
            let raw: &[u8] = if s.dict.has(b"Filter") {
                use std::io::Read;
                let _ = flate2::read::ZlibDecoder::new(&s.content[..]).take(16 << 20).read_to_end(&mut b);
                &b
            } else {
                &s.content
            };
            raw.windows(6).any(|w| w == b"pdfaid")
        }
        _ => false,
    });
    if pdfa {
        notes.push("This is a PDF/A file: its archive conformance mark lives in the XMP metadata, so the cleaned file no longer says it is PDF/A.".into());
    }
    if xmp > 0 {
        f.push(Finding::new("XMP metadata", if xmp == 1 { "1 stream".to_string() } else { format!("{xmp} streams") }, true));
    }
    if doc.trailer.has(b"ID") {
        f.push(Finding::new("Document ID", "", true));
    }
    if thumbs > 0 {
        f.push(Finding::new("Page thumbnails", thumbs.to_string(), true));
    }
    if piece > 0 {
        f.push(Finding::new("Application private data", format!("{piece} places"), true));
    }
    let eofs = revisions(raw);
    if eofs > 1 {
        f.push(Finding::new("Earlier saved versions inside the file", format!("{} older", eofs - 1), true));
    }
    let mut annots = 0;
    let mut authors = 0;
    let mut dates = 0;
    for d in dicts(doc) {
        if is_markup(d) {
            annots += 1;
            if d.has(b"T") {
                authors += 1;
            }
            if d.has(b"M") || d.has(b"CreationDate") {
                dates += 1;
            }
        }
    }
    if authors > 0 {
        f.push(Finding::new("Comment author names", format!("{authors} comments"), true));
    }
    if dates > 0 {
        f.push(Finding::new("Comment dates", format!("{dates} comments"), true));
    }
    let ids = params_ids(doc);
    let mut attach_dates = 0;
    for o in doc.objects.values() {
        if let Object::Stream(s) = o {
            if let Ok(Object::Dictionary(p)) = s.dict.get(b"Params") {
                if PARAM_KEYS.iter().any(|k| p.has(k)) {
                    attach_dates += 1;
                }
            }
        }
    }
    for id in &ids {
        if doc.get_object(*id).ok().and_then(|o| o.as_dict().ok()).is_some_and(|p| PARAM_KEYS.iter().any(|k| p.has(k))) {
            attach_dates += 1;
        }
    }
    if attach_dates > 0 {
        f.push(Finding::new("Dates of attached files", format!("{attach_dates} files"), true));
    }
    let (signers, signed) = signatures(doc);
    if signed {
        if signers.is_empty() {
            f.push(Finding::new("Digital signature data", "", true));
        }
        for w in signers {
            f.push(Finding::new("Digital signature (signer, time, place, reason, certificate)", w, true));
        }
    }
    // what stays
    if annots > 0 {
        notes.push(format!("Still inside: the text of {annots} comments or markings in the pages. Their author names and dates are removed, what they say is not."));
    }
    let attached: Vec<String> = dicts(doc)
        .filter(|d| matches!(d.get(b"Type"), Ok(Object::Name(n)) if n == b"Filespec") && d.has(b"EF"))
        .filter_map(|d| d.get(b"UF").or_else(|_| d.get(b"F")).ok().map(|n| text(n, doc)))
        .take(5)
        .collect();
    if !attached.is_empty() {
        notes.push(format!("Still inside: attached files ({}). The file names stay, their dates are removed; the files keep their own metadata.", attached.join(", ")));
    }
    if dicts(doc).any(|d| d.has(b"JS") || d.has(b"JavaScript")) {
        notes.push("Still inside: JavaScript in the document (reported, not removed).".into());
    }
    if doc.catalog().map(|c| c.has(b"AcroForm")).unwrap_or(false) {
        notes.push("Still inside: form fields and what was typed into them are not touched.".into());
    }
    if doc.catalog().map(|c| c.has(b"OCProperties")).unwrap_or(false) {
        notes.push("Still inside: hidden and visible layers are not touched.".into());
    }
    let pics = dct_pictures(doc).filter(|(_, c)| jpeg_metadata(c)).count();
    if pics > 0 {
        f.push(Finding::new("Picture metadata inside the pages (camera, GPS, XMP of JPEG pictures)", format!("{pics} pictures"), true));
    }
    if doc.objects.values().any(|o| matches!(o, Object::Stream(s) if matches!(s.dict.get(b"Filter"), Ok(Object::Name(n)) if n == b"JPXDecode"))) {
        notes.push("Still inside: JPEG 2000 pictures in the pages keep their own metadata.".into());
    }
    (f, notes)
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let (doc, raw) = load(path)?;
    let (findings, mut notes) = analyse(&doc, &raw);
    let signed = signatures(&doc).1;
    if signed && opts.remove_signatures {
        notes.push("The digital signatures are removed completely (signer, time, place, reason, certificate, the drawn signature): the cleaned PDF is no longer signed.".into());
    }
    let refuse = signed && !opts.remove_signatures;
    Ok(Inspection { format: "PDF".into(), findings, notes, can_clean: !refuse, refusal: refuse.then(|| SIGNED_TEXT.to_string()) })
}

/// Removes every signature: the signature dictionaries (with the signed data and the certificates), the value and the drawn
/// appearance of signature fields, the permissions of a certified document, the document security store and the form's flag.
fn remove_signatures(doc: &mut Document) {
    let sig_ids: Vec<lopdf::ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, o)| match o {
            Object::Dictionary(d) => is_sig_dict(d),
            Object::Stream(s) => is_sig_dict(&s.dict),
            _ => false,
        })
        .map(|(id, _)| *id)
        .collect();
    for id in sig_ids {
        doc.objects.remove(&id);
    }
    for o in doc.objects.values_mut() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        if is_sig_field(d) {
            d.remove(b"V");
            d.remove(b"AP");
        }
        // a direct signature dictionary as a field value
        if matches!(d.get(b"V"), Ok(Object::Dictionary(v)) if is_sig_dict(v)) {
            d.remove(b"V");
        }
    }
    if let Ok(c) = doc.catalog_mut() {
        c.remove(b"Perms");
        c.remove(b"DSS");
        if let Ok(Object::Dictionary(f)) = c.get_mut(b"AcroForm") {
            f.remove(b"SigFlags");
        }
    }
    if let Some(id) = acroform_id(doc) {
        if let Some(Object::Dictionary(f)) = doc.objects.get_mut(&id) {
            f.remove(b"SigFlags");
        }
    }
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let (mut doc, _) = load(src)?;
    if signatures(&doc).1 {
        if !opts.remove_signatures {
            return Err(ScrubError::Unsupported(SIGNED_TEXT.into()));
        }
        remove_signatures(&mut doc);
    }
    let pages_before = doc.get_pages().len();
    // what every page draws, as it is now: the rewrite must give the same back
    let drawn = page_streams(&doc);
    if let Some(Object::Reference(id)) = doc.trailer.get(b"Info").ok().cloned() {
        doc.delete_object(id);
    }
    doc.trailer.remove(b"Info");
    doc.trailer.remove(b"ID");
    let pids = params_ids(&doc);
    for id in pids {
        if let Some(Object::Dictionary(p)) = doc.objects.get_mut(&id) {
            for k in PARAM_KEYS {
                p.remove(k);
            }
        }
    }
    // the JPEG pictures in the pages: their own Exif / XMP / IPTC go, the picture data is copied byte for byte (the colour
    // profile inside stays: the page's colour space decides how it looks)
    let keep = Options { keep_color_profile: true, ..opts.clone() };
    let pics: Vec<lopdf::ObjectId> = dct_pictures(&doc).filter(|(_, c)| jpeg_metadata(c)).map(|(id, _)| id).collect();
    for id in pics {
        if let Some(Object::Stream(st)) = doc.objects.get_mut(&id) {
            match crate::image::process(&st.content, crate::Family::Jpeg, &keep) {
                Ok((clean, _)) => st.set_content(clean),
                Err(_) => return Err(ScrubError::Unsupported("a picture inside the PDF is damaged, leaf cannot clean it".into())),
            }
        }
    }
    for o in doc.objects.values_mut() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        for k in DROP_KEYS {
            d.remove(k);
        }
        if is_markup(d) {
            for k in ANNOT_KEYS {
                d.remove(k);
            }
        }
        if let Ok(Object::Dictionary(p)) = d.get_mut(b"Params") {
            for k in PARAM_KEYS {
                p.remove(k);
            }
        }
    }
    doc.prune_objects();
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    doc.save_to(&mut f).map_err(|e| ScrubError::Io(e))?;
    f.sync_all()?;
    drop(f);
    // the rewrite must open again with the same pages
    let (again, _) = load(dst)?;
    if again.get_pages().len() != pages_before {
        return Err(ScrubError::Verify("the rewritten PDF has a different number of pages".into()));
    }
    let drawn_again = page_streams(&again);
    if drawn != drawn_again {
        return Err(ScrubError::Verify("the rewritten PDF draws its pages differently".into()));
    }
    Ok(())
}
