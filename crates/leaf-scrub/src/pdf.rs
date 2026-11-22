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

fn is_markup(d: &Dictionary) -> bool {
    matches!(d.get(b"Subtype"), Ok(Object::Name(n)) if MARKUP.contains(&n.as_slice())) && d.has(b"Rect")
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
    notes.push("Pictures inside the pages keep their own metadata.".into());
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
    let drawn: Vec<Vec<u8>> = doc.get_pages().values().map(|id| doc.get_page_content(*id)).collect();
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
    let drawn_again: Vec<Vec<u8>> = again.get_pages().values().map(|id| again.get_page_content(*id)).collect();
    if drawn != drawn_again {
        return Err(ScrubError::Verify("the rewritten PDF draws its pages differently".into()));
    }
    Ok(())
}
