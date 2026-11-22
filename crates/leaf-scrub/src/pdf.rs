//! PDF: the Info dictionary, XMP streams, document ids, page thumbnails and application private data.
//! The file is rewritten as one clean revision (earlier saved versions inside the file go with it).
//! Comments, form fields, layers and attachments inside the pages are NOT touched, and the notes say so.

use crate::{Finding, Inspection, Result, ScrubError, MAX_IN_MEMORY};
use lopdf::{Dictionary, Document, Object};
use std::fs;
use std::path::Path;

const DROP_KEYS: [&[u8]; 4] = [b"Metadata", b"Thumb", b"PieceInfo", b"LastModified"];

fn load(path: &Path) -> Result<(Document, Vec<u8>)> {
    if fs::metadata(path)?.len() > MAX_IN_MEMORY {
        return Err(ScrubError::Unsupported("the PDF is bigger than 1 GB".into()));
    }
    let raw = fs::read(path)?;
    let tail = &raw[raw.len().saturating_sub(2048)..];
    if !tail.windows(5).any(|w| w == b"%%EOF") {
        return Err(ScrubError::Malformed("the PDF has no end marker, it is cut off".into()));
    }
    let doc = Document::load_mem(&raw).map_err(|e| ScrubError::Malformed(format!("the PDF could not be read: {e}")))?;
    if doc.trailer.has(b"Encrypt") || doc.is_encrypted() {
        return Err(ScrubError::Unsupported("the PDF is encrypted".into()));
    }
    if doc.get_pages().is_empty() {
        return Err(ScrubError::Malformed("the PDF has no pages".into()));
    }
    Ok((doc, raw))
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
    let eofs = raw.windows(5).filter(|w| *w == b"%%EOF").count();
    if eofs > 1 {
        f.push(Finding::new("Earlier saved versions inside the file", format!("{} older", eofs - 1), true));
    }
    // what stays
    let mut annots = 0;
    let mut authors = 0;
    for d in dicts(doc) {
        if let Ok(Object::Name(st)) = d.get(b"Subtype") {
            let st = st.as_slice();
            let markup = [
                &b"Text"[..], b"FreeText", b"Highlight", b"Underline", b"StrikeOut", b"Squiggly", b"Stamp", b"Caret", b"Ink", b"Line",
                b"Square", b"Circle", b"Polygon", b"PolyLine", b"FileAttachment",
            ];
            if markup.contains(&st) && d.has(b"Rect") {
                annots += 1;
                if d.has(b"T") {
                    authors += 1;
                }
            }
        }
    }
    if annots > 0 {
        notes.push(format!("Still inside: {annots} comments or markings in the pages ({authors} with an author name). They are not touched."));
    }
    if doc.catalog().map(|c| c.has(b"AcroForm")).unwrap_or(false) {
        notes.push("Still inside: form fields and what was typed into them are not touched.".into());
    }
    if doc.catalog().map(|c| c.has(b"OCProperties")).unwrap_or(false) {
        notes.push("Still inside: hidden and visible layers are not touched.".into());
    }
    if doc.catalog().ok().and_then(|c| c.get(b"Names").ok()).map(|n| match n {
        Object::Dictionary(d) => d.has(b"EmbeddedFiles"),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok()).map(|d| d.has(b"EmbeddedFiles")).unwrap_or(false),
        _ => false,
    }).unwrap_or(false)
    {
        notes.push("Still inside: attached files are not touched (they carry their own metadata).".into());
    }
    notes.push("Pictures inside the pages keep their own metadata.".into());
    (f, notes)
}

pub(crate) fn inspect(path: &Path) -> Result<Inspection> {
    let (doc, raw) = load(path)?;
    let (findings, notes) = analyse(&doc, &raw);
    Ok(Inspection { format: "PDF".into(), findings, notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path) -> Result<()> {
    let (mut doc, _) = load(src)?;
    let pages_before = doc.get_pages().len();
    // what every page draws, as it is now: the rewrite must give the same back
    let drawn: Vec<Vec<u8>> = doc.get_pages().values().map(|id| doc.get_page_content(*id)).collect();
    if let Some(Object::Reference(id)) = doc.trailer.get(b"Info").ok().cloned() {
        doc.delete_object(id);
    }
    doc.trailer.remove(b"Info");
    doc.trailer.remove(b"ID");
    for o in doc.objects.values_mut() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        for k in DROP_KEYS {
            d.remove(k);
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
    let drawn_again: Vec<Vec<u8>> = again.get_pages().values().map(|id| again.get_page_content(*id)).collect();
    if drawn != drawn_again {
        return Err(ScrubError::Verify("the rewritten PDF draws its pages differently".into()));
    }
    Ok(())
}
