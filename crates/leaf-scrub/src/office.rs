//! Office (docx, xlsx, pptx) and OpenDocument (odt, ods, odp) files: zips of XML parts.
//! The document property parts are blanked (the files stay valid), the thumbnail is removed with the relationship that points
//! at it, and every entry is rewritten with neutral zip times. Comments and tracked changes are REPORTED, not rewritten.

use crate::{Finding, Inspection, Result, ScrubError};
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub(crate) enum Kind {
    Ooxml(&'static str),
    Odf(&'static str),
}

const MAX_XML: u64 = 64 << 20;

fn open(path: &Path) -> Result<ZipArchive<fs::File>> {
    let f = fs::File::open(path)?;
    ZipArchive::new(f).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))
}

pub(crate) fn sniff(path: &Path) -> Option<Kind> {
    let mut z = ZipArchive::new(fs::File::open(path).ok()?).ok()?;
    if let Ok(mut m) = z.by_name("mimetype") {
        let mut s = String::new();
        let _ = (&mut m).take(200).read_to_string(&mut s);
        let name = match s.trim() {
            "application/vnd.oasis.opendocument.text" => "OpenDocument text",
            "application/vnd.oasis.opendocument.spreadsheet" => "OpenDocument spreadsheet",
            "application/vnd.oasis.opendocument.presentation" => "OpenDocument presentation",
            m if m.starts_with("application/vnd.oasis.opendocument.") => "OpenDocument file",
            _ => return None,
        };
        return Some(Kind::Odf(name));
    }
    if z.by_name("[Content_Types].xml").is_ok() {
        let name = if z.by_name("word/document.xml").is_ok() {
            "Word document"
        } else if z.by_name("xl/workbook.xml").is_ok() {
            "Excel workbook"
        } else if z.by_name("ppt/presentation.xml").is_ok() {
            "PowerPoint presentation"
        } else if z.by_name("docProps/core.xml").is_ok() {
            "Office document"
        } else {
            return None;
        };
        return Some(Kind::Ooxml(name));
    }
    None
}

fn read_entry(z: &mut ZipArchive<fs::File>, name: &str) -> Option<String> {
    let mut e = z.by_name(name).ok()?;
    if e.size() > MAX_XML || e.encrypted() {
        return None;
    }
    let mut s = String::new();
    e.read_to_string(&mut s).ok()?;
    Some(s)
}

fn ename<R: Read + io::Seek>(e: &zip::read::ZipFile<'_, R>) -> String {
    e.name().map(|c| c.into_owned()).unwrap_or_default()
}

fn is_odf(z: &mut ZipArchive<fs::File>) -> bool {
    z.by_name("mimetype").is_ok()
}

/// (label, value) of the leaf elements directly under the root (or under `office:meta`).
fn props(xml: &str, odf: bool) -> Vec<(String, String)> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return vec![] };
    let root = doc.root_element();
    let parent = if odf {
        match root.children().find(|n| n.is_element() && n.tag_name().name() == "meta") {
            Some(m) => m,
            None => return vec![],
        }
    } else {
        root
    };
    let mut out = Vec::new();
    for n in parent.children().filter(|n| n.is_element()) {
        let name = n.tag_name().name();
        let value: String = n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>().trim().to_string();
        if odf && name == "document-statistic" {
            continue;
        }
        if name == "property" {
            // custom.xml: <property name="X"><vt:lpwstr>value</vt:lpwstr></property>
            out.push((format!("Custom property: {}", n.attribute("name").unwrap_or("")), value));
            continue;
        }
        if odf && name == "user-defined" {
            out.push((format!("Custom property: {}", n.attribute("name").unwrap_or("")), value));
            continue;
        }
        if value.is_empty() {
            continue;
        }
        if matches!(name, "HeadingPairs" | "TitlesOfParts" | "HLinks") {
            continue;
        }
        out.push((friendly(name).to_string(), value));
    }
    out
}

fn friendly(n: &str) -> &str {
    match n {
        "creator" => "Author",
        "initial-creator" => "First author",
        "lastModifiedBy" => "Last modified by",
        "title" => "Title",
        "subject" => "Subject",
        "keywords" | "keyword" => "Keywords",
        "description" => "Comments",
        "category" => "Category",
        "created" | "creation-date" => "Created",
        "modified" | "date" => "Last modified",
        "lastPrinted" | "print-date" => "Last printed",
        "revision" => "Revision",
        "Company" => "Company",
        "Manager" => "Manager",
        "Template" => "Template",
        "TotalTime" | "editing-duration" => "Total editing time",
        "editing-cycles" => "Editing cycles",
        "Application" => "Application",
        "AppVersion" => "Application version",
        "generator" => "Generator",
        "language" => "Language",
        "contentStatus" => "Status",
        other => other,
    }
}

fn count_attr_authors(xml: &str, elem: &[&str], attr: &str) -> (usize, std::collections::BTreeSet<String>) {
    let mut n = 0;
    let mut who = std::collections::BTreeSet::new();
    if let Ok(doc) = roxmltree::Document::parse(xml) {
        for e in doc.descendants().filter(|e| e.is_element() && elem.contains(&e.tag_name().name())) {
            n += 1;
            if let Some(a) = e.attributes().find(|a| a.name() == attr) {
                if !a.value().is_empty() {
                    who.insert(a.value().to_string());
                }
            }
        }
    }
    (n, who)
}

pub(crate) fn inspect(path: &Path) -> Result<Inspection> {
    let mut z = open(path)?;
    let odf = is_odf(&mut z);
    let mut findings = Vec::new();
    let mut notes = Vec::new();
    // every entry must be readable: a damaged zip is refused here
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))?;
        if e.encrypted() {
            return Err(ScrubError::Unsupported("the document is password protected".into()));
        }
        let name = ename(&e);
        if name.contains("..") && !e.is_dir() {
            return Err(ScrubError::Malformed("the zip has an entry with a path that leaves the folder".into()));
        }
        io::copy(&mut e, &mut io::sink()).map_err(|e| ScrubError::Malformed(format!("an entry is damaged: {e}")))?;
    }
    let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index(i).ok().map(|e| ename(&e))).collect();
    let format = match sniff(path) {
        Some(Kind::Ooxml(n)) | Some(Kind::Odf(n)) => n,
        None => "Office document",
    };
    if odf {
        let meta = read_entry(&mut z, "meta.xml").ok_or_else(|| ScrubError::Malformed("no meta.xml".into()))?;
        roxmltree::Document::parse(&meta).map_err(|e| ScrubError::Malformed(format!("meta.xml is damaged: {e}")))?;
        for (l, v) in props(&meta, true) {
            findings.push(Finding::new(l, v, true));
        }
        if names.iter().any(|n| n.starts_with("Thumbnails/") && !n.ends_with('/')) {
            findings.push(Finding::new("Thumbnail picture", "", true));
        }
        if let Some(c) = read_entry(&mut z, "content.xml") {
            let (n, who) = count_attr_authors(&c, &["annotation"], "creator");
            let (ann, ann_who) = {
                // dc:creator is a child element of office:annotation, not an attribute
                let mut k = 0;
                let mut w = std::collections::BTreeSet::new();
                if let Ok(d) = roxmltree::Document::parse(&c) {
                    for a in d.descendants().filter(|e| e.is_element() && e.tag_name().name() == "annotation") {
                        k += 1;
                        for cr in a.children().filter(|e| e.is_element() && e.tag_name().name() == "creator") {
                            if let Some(t) = cr.text() {
                                w.insert(t.trim().to_string());
                            }
                        }
                    }
                }
                (k, w)
            };
            let _ = (n, who);
            if ann > 0 {
                notes.push(format!("Still inside: {ann} comments by {} named authors. They are not rewritten.", ann_who.len()));
            }
            if c.contains("tracked-changes") && c.contains("changed-region") {
                notes.push("Still inside: tracked changes (with author names and dates) are not rewritten.".into());
            }
        }
    } else {
        for (part, tag) in [("docProps/core.xml", false), ("docProps/app.xml", false), ("docProps/custom.xml", false)] {
            if let Some(x) = read_entry(&mut z, part) {
                roxmltree::Document::parse(&x).map_err(|e| ScrubError::Malformed(format!("{part} is damaged: {e}")))?;
                let _ = tag;
                for (l, v) in props(&x, false) {
                    findings.push(Finding::new(l, v, true));
                }
            }
        }
        if names.iter().any(|n| n.starts_with("docProps/thumbnail")) {
            findings.push(Finding::new("Thumbnail picture", "", true));
        }
        // comments and tracked changes
        let mut comments = 0;
        let mut who = std::collections::BTreeSet::new();
        for n in &names {
            let is_c = n == "word/comments.xml" || (n.starts_with("xl/comments") && n.ends_with(".xml")) || (n.starts_with("ppt/comments/") && n.ends_with(".xml"));
            if is_c {
                if let Some(x) = read_entry(&mut z, n) {
                    let (k, w) = count_attr_authors(&x, &["comment", "cm"], "author");
                    comments += k;
                    who.extend(w);
                    // xlsx lists authors as elements
                    if let Ok(d) = roxmltree::Document::parse(&x) {
                        for a in d.descendants().filter(|e| e.is_element() && e.tag_name().name() == "author") {
                            if let Some(t) = a.text() {
                                who.insert(t.trim().to_string());
                            }
                        }
                    }
                }
            }
            if n == "ppt/commentAuthors.xml" {
                if let Some(x) = read_entry(&mut z, n) {
                    let (_, w) = count_attr_authors(&x, &["cmAuthor"], "name");
                    who.extend(w);
                }
            }
        }
        if comments > 0 || !who.is_empty() {
            notes.push(format!("Still inside: {comments} comments by {} named authors. They are not rewritten.", who.len()));
        }
        if let Some(x) = read_entry(&mut z, "word/document.xml") {
            let (ins, _) = count_attr_authors(&x, &["ins", "del", "moveFrom", "moveTo"], "author");
            if ins > 0 {
                notes.push(format!("Still inside: {ins} tracked changes (with author names and dates). They are not rewritten."));
            }
            if x.contains("w:rsid") {
                notes.push("Still inside: Word's editing session numbers (rsid) are not removed.".into());
            }
        }
    }
    let media = names.iter().filter(|n| n.contains("/media/") || n.starts_with("Pictures/")).count();
    if media > 0 {
        notes.push(format!("Still inside: {media} pictures or other files embedded in the document keep their own metadata."));
    }
    Ok(Inspection { format: format.into(), findings, notes, can_clean: true, refusal: None })
}

const CORE_BLANK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"/>";
const APP_BLANK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"/>";
const CUSTOM_BLANK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"/>";

/// `xml` without the elements of `tag` whose attribute `attr` satisfies `drop` (text-level removal by element range).
fn drop_elements(xml: &str, tag: &str, attr: &str, drop: impl Fn(&str) -> bool) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let mut ranges: Vec<std::ops::Range<usize>> = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag && n.attributes().find(|a| a.name() == attr).map(|a| drop(a.value())).unwrap_or(false))
        .map(|n| n.range())
        .collect();
    ranges.sort_by_key(|r| r.start);
    let mut out = String::new();
    let mut at = 0;
    for r in ranges {
        out.push_str(&xml[at..r.start]);
        at = r.end;
    }
    out.push_str(&xml[at..]);
    Some(out)
}

fn odf_meta_blank(old: &str) -> String {
    let version = roxmltree::Document::parse(old)
        .ok()
        .and_then(|d| d.root_element().attributes().find(|a| a.name() == "version").map(|a| a.value().to_string()))
        .unwrap_or_else(|| "1.2".into());
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-meta xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" office:version=\"{version}\"><office:meta/></office:document-meta>")
}

pub(crate) fn produce(src: &Path, dst: &Path) -> Result<()> {
    // refuses a damaged or encrypted zip before anything is written
    let before = inspect(src)?;
    let _ = before;
    let mut z = open(src)?;
    let odf = is_odf(&mut z);
    let out = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    let mut w = ZipWriter::new(io::BufWriter::new(out));
    let zerr = |e: zip::result::ZipError| ScrubError::Io(io::Error::other(e.to_string()));
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(zerr)?;
        let name = ename(&e);
        let is_thumb = if odf { name.starts_with("Thumbnails/") } else { name.starts_with("docProps/thumbnail") };
        if is_thumb {
            continue;
        }
        let method = if name == "mimetype" || e.compression() == CompressionMethod::Stored { CompressionMethod::Stored } else { CompressionMethod::Deflated };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(zip::DateTime::default())
            .large_file(e.size() >= u32::MAX as u64);
        if e.is_dir() {
            w.add_directory(name, opts).map_err(zerr)?;
            continue;
        }
        let blank: Option<String> = match (odf, name.as_str()) {
            (false, "docProps/core.xml") => Some(CORE_BLANK.into()),
            (false, "docProps/app.xml") => Some(APP_BLANK.into()),
            (false, "docProps/custom.xml") => Some(CUSTOM_BLANK.into()),
            (true, "meta.xml") => {
                let mut s = String::new();
                e.read_to_string(&mut s)?;
                Some(odf_meta_blank(&s))
            }
            (false, "_rels/.rels") => {
                let mut s = String::new();
                e.read_to_string(&mut s)?;
                Some(drop_elements(&s, "Relationship", "Type", |t| t.ends_with("/thumbnail")).ok_or_else(|| ScrubError::Malformed("_rels/.rels is damaged".into()))?)
            }
            (true, "META-INF/manifest.xml") => {
                let mut s = String::new();
                e.read_to_string(&mut s)?;
                Some(drop_elements(&s, "file-entry", "full-path", |p| p.starts_with("Thumbnails/")).ok_or_else(|| ScrubError::Malformed("manifest.xml is damaged".into()))?)
            }
            _ => None,
        };
        w.start_file(name, opts).map_err(zerr)?;
        match blank {
            Some(s) => w.write_all(s.as_bytes())?,
            None => {
                io::copy(&mut e, &mut w)?;
            }
        }
    }
    let mut inner = w.finish().map_err(zerr)?;
    inner.flush()?;
    inner.get_ref().sync_all()?;
    Ok(())
}
