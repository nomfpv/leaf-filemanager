//! Office (docx, xlsx, pptx) and OpenDocument (odt, ods, odp) files: zips of XML parts.
//! The document property parts are blanked (the files stay valid), the thumbnail, printer settings and template path are removed with
//! the relationships that point at them, comment and tracked-change authors become "Author" and their dates go, editing session
//! numbers (rsid), document variables and custom XML parts go, pictures in the media folders lose their own metadata, and every
//! entry is rewritten with neutral zip times. The text of comments and changes is never touched.

use crate::{Family, Finding, Inspection, Options, Result, ScrubError, MAX_IN_MEMORY};
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
    (&mut e).take(MAX_XML + 1).read_to_string(&mut s).ok()?;
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


const NEUTRAL_DATE: &str = "1970-01-01T00:00:00";
const WORD_NS: &str = "wordprocessingml/2006/main";

const CAT_AUTHOR: &str = "Author names (comments, tracked changes)";
const CAT_DATE: &str = "Dates (comments, tracked changes)";
const CAT_RSID: &str = "Editing session numbers (rsid)";
const CAT_VARS: &str = "Document variables";
const CAT_TEMPLATE: &str = "Attached template path";
const CAT_PRINTER: &str = "Printer settings";
const CAT_PATH: &str = "Folder path where the workbook was saved";
const CAT_SHARING: &str = "Name of the user who reserved the workbook";
const CAT_IDS: &str = "Save and co-authoring ids (revisionPtr, uid)";

enum Act {
    Set(&'static str),
    Remove,
}

fn attr_rule(part: &str, local: &str, ns: Option<&str>, value: &str) -> Option<(Act, &'static str)> {
    let word = part.starts_with("word/") && part.ends_with(".xml");
    let rule = if word {
        let main = ns.is_some_and(|n| n.contains(WORD_NS));
        match local {
            "author" => Some((Act::Set("Author"), CAT_AUTHOR)),
            "initials" => Some((Act::Set("A"), CAT_AUTHOR)),
            "userId" => Some((Act::Set("Author"), CAT_AUTHOR)),
            "providerId" => Some((Act::Set("None"), CAT_AUTHOR)),
            "date" if main => Some((Act::Remove, CAT_DATE)),
            l if main && l.starts_with("rsid") => Some((Act::Remove, CAT_RSID)),
            _ => None,
        }
    } else if part.starts_with("xl/persons/") || part.starts_with("xl/threadedComments/") {
        match local {
            "displayName" | "userId" => Some((Act::Set("Author"), CAT_AUTHOR)),
            "providerId" => Some((Act::Set("None"), CAT_AUTHOR)),
            "dT" => Some((Act::Set("1970-01-01T00:00:00.00"), CAT_DATE)),
            _ => None,
        }
    } else if part == "xl/workbook.xml" {
        match local {
            "userName" => Some((Act::Remove, CAT_SHARING)),
            "uid" if ns.is_some_and(|n| n.contains("spreadsheetml/2014/revision")) => Some((Act::Remove, CAT_IDS)),
            _ => None,
        }
    } else if part.starts_with("xl/") && local == "uid" && ns.is_some_and(|n| n.contains("spreadsheetml/2014/revision")) {
        Some((Act::Remove, CAT_IDS))
    } else if part == "ppt/commentAuthors.xml" {
        match local {
            "name" => Some((Act::Set("Author"), CAT_AUTHOR)),
            "initials" => Some((Act::Set("A"), CAT_AUTHOR)),
            _ => None,
        }
    } else if part == "ppt/authors.xml" {
        match local {
            "name" | "userId" => Some((Act::Set("Author"), CAT_AUTHOR)),
            "providerId" => Some((Act::Set("None"), CAT_AUTHOR)),
            _ => None,
        }
    } else if part.starts_with("ppt/comments/") && part.ends_with(".xml") {
        match local {
            "dt" => Some((Act::Remove, CAT_DATE)),
            "created" => Some((Act::Set("1970-01-01T00:00:00.000"), CAT_DATE)),
            _ => None,
        }
    } else {
        None
    };
    match rule {
        Some((Act::Set(v), _)) if value == v => None,
        r => r,
    }
}

/// `Some(text)`: the text content of this element is replaced; `None` with `remove`: the element goes.
enum ElemAct {
    Text(&'static str),
    Remove,
}

fn elem_rule(part: &str, odf: bool, local: &str, parent: &str, config_name: Option<&str>) -> Option<(ElemAct, &'static str)> {
    if odf {
        let in_content = part == "content.xml" || part == "styles.xml";
        if in_content && matches!(parent, "annotation" | "change-info" | "annotation-end") {
            return match local {
                "creator" => Some((ElemAct::Text("Author"), CAT_AUTHOR)),
                "creator-initials" => Some((ElemAct::Text("A"), CAT_AUTHOR)),
                "date" => Some((ElemAct::Text(NEUTRAL_DATE), CAT_DATE)),
                "date-string" => Some((ElemAct::Text(""), CAT_DATE)),
                _ => None,
            };
        }
        if part == "settings.xml" && local == "config-item" && matches!(config_name, Some("PrinterName") | Some("PrinterSetup")) {
            return Some((ElemAct::Text(""), CAT_PRINTER));
        }
        return None;
    }
    if part.starts_with("word/") && part.ends_with(".xml") {
        return match local {
            "rsids" | "rsidRoot" | "rsid" => Some((ElemAct::Remove, CAT_RSID)),
            "docVars" => Some((ElemAct::Remove, CAT_VARS)),
            "attachedTemplate" => Some((ElemAct::Remove, CAT_TEMPLATE)),
            _ => None,
        };
    }
    if part.starts_with("xl/comments") && part.ends_with(".xml") && local == "author" && parent == "authors" {
        return Some((ElemAct::Text("Author"), CAT_AUTHOR));
    }
    None
}

#[derive(Default)]
struct Counts {
    /// category -> (places, distinct original values)
    by: std::collections::BTreeMap<&'static str, (usize, std::collections::BTreeSet<String>)>,
}

impl Counts {
    fn add(&mut self, cat: &'static str, value: &str) {
        let e = self.by.entry(cat).or_default();
        e.0 += 1;
        if !value.is_empty() && e.1.len() < 4 && matches!(cat, CAT_AUTHOR | CAT_PATH | CAT_SHARING) {
            e.1.insert(value.trim().to_string());
        }
    }
    fn findings(self) -> Vec<Finding> {
        self.by
            .into_iter()
            .map(|(cat, (n, who))| {
                let names = if who.is_empty() { String::new() } else { format!(": {}", who.into_iter().collect::<Vec<_>>().join(", ")) };
                Finding::new(cat, format!("{n} places{names}"), true)
            })
            .collect()
    }
}

fn apply_edits(xml: &str, mut edits: Vec<(std::ops::Range<usize>, String)>) -> String {
    edits.sort_by_key(|(r, _)| (r.start, std::cmp::Reverse(r.end)));
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for (r, with) in edits {
        if r.start < at {
            continue;
        }
        out.push_str(&xml[at..r.start]);
        out.push_str(&with);
        at = r.end;
    }
    out.push_str(&xml[at..]);
    out
}

/// The rewritten text of an XML part when the rules change anything in it, with what was found. `None`: nothing to do.
fn rewrite_part(part: &str, odf: bool, xml: &str, counts: &mut Counts) -> Result<Option<String>> {
    let relevant = if odf {
        matches!(part, "content.xml" | "styles.xml" | "settings.xml")
    } else {
        part.ends_with(".xml")
            && (part.starts_with("word/") || part.starts_with("xl/") || part.starts_with("xl/comments") || part.starts_with("xl/persons/") || part.starts_with("xl/threadedComments/") || part.starts_with("ppt/comments/") || part == "ppt/commentAuthors.xml" || part == "ppt/authors.xml")
    };
    if !relevant {
        return Ok(None);
    }
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
        .map_err(|e| ScrubError::Malformed(format!("{part} is damaged: {e}")))?;
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        let local = n.tag_name().name();
        if !odf && part == "xl/workbook.xml" {
            // the folder the workbook was saved in (holds the Windows user name): `x15ac:absPath`, normally inside an
            // `mc:AlternateContent` wrapper that goes with it; the save / co-authoring ids of `xr:revisionPtr`
            let abs_url = |m: roxmltree::Node| m.descendants().find(|d| d.is_element() && d.tag_name().name() == "absPath").map(|a| a.attribute("url").unwrap_or("").to_string());
            if local == "AlternateContent" {
                if let Some(url) = abs_url(n) {
                    counts.add(CAT_PATH, &url);
                    edits.push((n.range(), String::new()));
                    continue;
                }
            }
            if local == "absPath" && !n.ancestors().any(|a| a.tag_name().name() == "AlternateContent") {
                counts.add(CAT_PATH, n.attribute("url").unwrap_or(""));
                edits.push((n.range(), String::new()));
                continue;
            }
            if local == "revisionPtr" {
                counts.add(CAT_IDS, "");
                edits.push((n.range(), String::new()));
                continue;
            }
        }
        let parent = n.parent_element().map(|p| p.tag_name().name()).unwrap_or("");
        let cfg = n.attributes().find(|a| a.name() == "name").map(|a| a.value());
        if let Some((act, cat)) = elem_rule(part, odf, local, parent, cfg) {
            match act {
                ElemAct::Remove => {
                    counts.add(cat, "");
                    edits.push((n.range(), String::new()));
                    continue;
                }
                ElemAct::Text(t) => {
                    if let Some(tn) = n.children().find(|c| c.is_text()) {
                        if tn.text().unwrap_or("") != t {
                            counts.add(cat, tn.text().unwrap_or(""));
                            edits.push((tn.range(), t.to_string()));
                        }
                    }
                }
            }
        }
        for a in n.attributes() {
            if let Some((act, cat)) = attr_rule(part, a.name(), a.namespace(), a.value()) {
                counts.add(cat, a.value());
                let r = a.range();
                match act {
                    Act::Remove => {
                        let mut s = r.start;
                        while s > 0 && xml.as_bytes()[s - 1].is_ascii_whitespace() {
                            s -= 1;
                        }
                        edits.push((s..r.end, String::new()));
                    }
                    Act::Set(v) => {
                        let rv = a.range_value();
                        edits.push((r.start..r.end, format!("{}{}{}", &xml[r.start..rv.start], v, &xml[rv.end..r.end])));
                    }
                }
            }
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }
    Ok(Some(apply_edits(xml, edits)))
}

enum Fate {
    Keep,
    Drop,
    Replace(Vec<u8>),
}

struct Plan {
    fates: Vec<Fate>,
    findings: Vec<Finding>,
    notes: Vec<String>,
    /// The document carries digital signatures.
    signed: bool,
}

/// The most any one entry may unpack to, and the most it may grow while unpacking: a zip bomb is refused, never unpacked.
const MAX_RATIO: u64 = 200;
/// The most all entries together may unpack to.
const MAX_TOTAL: u64 = 8 << 30;

fn bomb_check(e: &zip::read::ZipFile<'_, fs::File>, name: &str) -> Result<()> {
    let (size, packed) = (e.size(), e.compressed_size().max(1));
    if size > 1 << 20 && size / packed > MAX_RATIO {
        return Err(ScrubError::Unsupported(format!("{name} unpacks to more than {MAX_RATIO} times its packed size, leaf does not unpack it")));
    }
    Ok(())
}

/// The bytes of an entry, never more than `cap` and never more than the size the zip declares for it.
fn read_capped(e: &mut zip::read::ZipFile<'_, fs::File>, cap: u64) -> Result<Vec<u8>> {
    let name = ename(e);
    bomb_check(e, &name)?;
    let limit = e.size().min(cap);
    let mut v = Vec::with_capacity(limit.min(1 << 20) as usize);
    (&mut *e).take(limit + 1).read_to_end(&mut v).map_err(|e| ScrubError::Malformed(format!("an entry is damaged: {e}")))?;
    if v.len() as u64 > limit {
        return Err(ScrubError::Unsupported(format!("{name} is bigger than it says or than leaf reads")));
    }
    Ok(v)
}

fn read_bytes(e: &mut zip::read::ZipFile<'_, fs::File>) -> Result<Vec<u8>> {
    read_capped(e, MAX_IN_MEMORY)
}

/// Reads an entry through to its end without keeping it (a damaged one is refused here, not half-cleaned).
fn read_through(e: &mut zip::read::ZipFile<'_, fs::File>) -> Result<()> {
    let name = ename(e);
    bomb_check(e, &name)?;
    let size = e.size();
    let n = io::copy(&mut (&mut *e).take(size + 1), &mut io::sink()).map_err(|e| ScrubError::Malformed(format!("an entry is damaged: {e}")))?;
    if n > size {
        return Err(ScrubError::Malformed(format!("{name} is bigger than it says")));
    }
    Ok(())
}

/// Relationship types whose relationship goes with the part it points at.
fn is_rel_dropped(t: &str) -> bool {
    t.ends_with("/thumbnail")
        || t.ends_with("/printerSettings")
        || t.ends_with("/attachedTemplate")
        || t.contains("/digital-signature/")
}

pub(crate) const R_NS: [&str; 2] = ["http://schemas.openxmlformats.org/officeDocument/2006/relationships", "http://purl.oclc.org/ooxml/officeDocument/relationships"];

/// The part a `.rels` file describes: `word/_rels/document.xml.rels` -> `word/document.xml`, `_rels/.rels` -> `` (the package).
pub(crate) fn rels_source(rels: &str) -> Option<String> {
    let (dir, file) = rels.rsplit_once("_rels/")?;
    let file = file.strip_suffix(".rels")?;
    Some(format!("{dir}{file}"))
}

/// The part name a relationship target means, seen from `source` (relative to the source's folder; `/x` from the root).
pub(crate) fn resolve(source: &str, target: &str) -> String {
    let target = target.split('#').next().unwrap_or("");
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut v: Vec<&str> = source.split('/').collect();
        v.pop();
        v
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Signature parts of a document.
fn is_signature_part(name: &str, odf: bool) -> bool {
    if odf {
        name.starts_with("META-INF/") && name.to_ascii_lowercase().contains("signatures")
    } else {
        name.starts_with("_xmlsignatures/")
    }
}

/// Who signed and when, from the XML of a signature part (`X509SubjectName`, `SigningTime`, `SignatureComments`).
fn signature_details(xml: &str) -> String {
    let Ok(doc) = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }) else { return String::new() };
    let mut v: Vec<String> = Vec::new();
    for want in ["X509SubjectName", "SigningTime", "SignatureComments", "SignerRole", "ClaimedRole"] {
        if let Some(t) = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == want).and_then(|n| n.text()) {
            if !t.trim().is_empty() {
                v.push(t.trim().to_string());
            }
        }
    }
    v.join(", ")
}

/// `xml` without the attributes of the relationships namespace whose value is one of `ids` (an element that is nothing but such a
/// reference goes whole).
fn drop_rel_refs(part: &str, xml: &str, ids: &std::collections::BTreeSet<String>) -> Result<String> {
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
        .map_err(|e| ScrubError::Malformed(format!("{part} is damaged: {e}")))?;
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        for a in n.attributes() {
            if a.namespace().is_some_and(|ns| R_NS.contains(&ns)) && ids.contains(a.value()) {
                if matches!(n.tag_name().name(), "attachedTemplate" | "printerSettings") {
                    edits.push((n.range(), String::new()));
                } else {
                    let r = a.range();
                    let mut s = r.start;
                    while s > 0 && xml.as_bytes()[s - 1].is_ascii_whitespace() {
                        s -= 1;
                    }
                    edits.push((s..r.end, String::new()));
                }
            }
        }
    }
    Ok(apply_edits(xml, edits))
}

/// `[Content_Types].xml` without the overrides of the parts that go and without the defaults no remaining part uses.
fn drop_content_types(xml: &str, dropped: &std::collections::BTreeSet<String>, remaining: &[String]) -> Result<String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| ScrubError::Malformed(format!("[Content_Types].xml is damaged: {e}")))?;
    let used: std::collections::BTreeSet<String> = remaining.iter().filter_map(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())).collect();
    let low: std::collections::BTreeSet<String> = dropped.iter().map(|d| d.to_ascii_lowercase()).collect();
    let mut edits = Vec::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "Override" => {
                let part = n.attribute("PartName").unwrap_or("").trim_start_matches('/').to_ascii_lowercase();
                if low.contains(&part) {
                    edits.push((n.range(), String::new()));
                }
            }
            "Default" => {
                let ext = n.attribute("Extension").unwrap_or("").to_ascii_lowercase();
                if !matches!(ext.as_str(), "rels" | "xml") && !used.contains(&ext) {
                    edits.push((n.range(), String::new()));
                }
            }
            _ => {}
        }
    }
    Ok(apply_edits(xml, edits))
}

/// The broken references of a package: relationship targets that are not there, relationship ids an XML part uses that its
/// `.rels` does not define, content-type overrides of missing parts (OpenDocument: manifest entries of missing files).
pub(crate) fn broken_references(path: &Path) -> Result<std::collections::BTreeSet<String>> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut z = open(path)?;
    let odf = is_odf(&mut z);
    let names: Vec<String> = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|e| ename(&e))).collect();
    let have: BTreeSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut issues = BTreeSet::new();
    let text = |z: &mut ZipArchive<fs::File>, n: &str| -> Result<Option<String>> {
        let mut e = z.by_name(n).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))?;
        let b = read_capped(&mut e, MAX_XML)?;
        Ok(String::from_utf8(b).ok())
    };
    let popts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    if odf {
        if let Some(x) = text(&mut z, "META-INF/manifest.xml")? {
            let doc = roxmltree::Document::parse_with_options(&x, popts).map_err(|e| ScrubError::Malformed(format!("manifest.xml is damaged: {e}")))?;
            for n in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "file-entry") {
                let p = n.attributes().find(|a| a.name() == "full-path").map(|a| a.value()).unwrap_or("");
                if p != "/" && !p.ends_with('/') && !p.is_empty() && !have.contains(&p.to_ascii_lowercase()) {
                    issues.insert(format!("the manifest lists {p}, which is not in the file"));
                }
            }
        }
        return Ok(issues);
    }
    let mut ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in names.iter().filter(|n| n.ends_with(".rels")) {
        let Some(source) = rels_source(r) else { continue };
        let Some(x) = text(&mut z, r)? else { continue };
        let doc = roxmltree::Document::parse_with_options(&x, popts).map_err(|e| ScrubError::Malformed(format!("{r} is damaged: {e}")))?;
        let set = ids.entry(source.clone()).or_default();
        for n in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Relationship") {
            set.insert(n.attribute("Id").unwrap_or("").to_string());
            let external = n.attribute("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
            let target = n.attribute("Target").unwrap_or("");
            if !external && !target.is_empty() && !have.contains(&resolve(&source, target).to_ascii_lowercase()) {
                issues.insert(format!("{r} points at {target}, which is not in the file"));
            }
        }
    }
    for n in names.iter().filter(|n| {
        let l = n.to_ascii_lowercase();
        l.ends_with(".xml") || l.ends_with(".vml")
    }) {
        let Some(x) = text(&mut z, n)? else { continue };
        if n == "[Content_Types].xml" {
            let doc = roxmltree::Document::parse(&x).map_err(|e| ScrubError::Malformed(format!("[Content_Types].xml is damaged: {e}")))?;
            for o in doc.descendants().filter(|o| o.is_element() && o.tag_name().name() == "Override") {
                let p = o.attribute("PartName").unwrap_or("").trim_start_matches('/');
                if !have.contains(&p.to_ascii_lowercase()) {
                    issues.insert(format!("[Content_Types].xml names {p}, which is not in the file"));
                }
            }
            continue;
        }
        let Ok(doc) = roxmltree::Document::parse_with_options(&x, popts) else { continue };
        let empty = BTreeSet::new();
        let mine = ids.get(n.as_str()).unwrap_or(&empty);
        for e in doc.descendants().filter(|e| e.is_element()) {
            for a in e.attributes() {
                if a.namespace().is_some_and(|ns| R_NS.contains(&ns)) && !a.value().is_empty() && !mine.contains(a.value()) {
                    issues.insert(format!("{n} uses the relationship {}, which is not defined", a.value()));
                }
            }
        }
    }
    Ok(issues)
}

fn plan(path: &Path, opts: &Options) -> Result<Plan> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut z = open(path)?;
    let odf = is_odf(&mut z);
    let mut p = Plan { fates: Vec::new(), findings: Vec::new(), notes: Vec::new(), signed: false };
    let mut counts = Counts::default();
    let (mut thumbs, mut printers, mut customs, mut unreadable, mut macros, mut embedded) = (0, 0, 0, 0, false, false);
    let mut pics: Vec<Finding> = Vec::new();
    // 1. the names, and the parts that go whole
    let mut names: Vec<String> = Vec::with_capacity(z.len());
    let mut total = 0u64;
    for i in 0..z.len() {
        let e = z.by_index_raw(i).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))?;
        if e.encrypted() {
            return Err(ScrubError::Unsupported("the document is password protected".into()));
        }
        total = total.saturating_add(e.size());
        names.push(ename(&e));
    }
    if total > MAX_TOTAL {
        return Err(ScrubError::Unsupported("the document unpacks to more than 8 GB".into()));
    }
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    let mut signatures: Vec<String> = Vec::new();
    for n in &names {
        if n.ends_with('/') {
            continue;
        }
        let is_thumb = if odf { n.starts_with("Thumbnails/") } else { n.starts_with("docProps/thumbnail") };
        let is_printer = !odf && n.contains("/printerSettings/");
        if is_signature_part(n, odf) {
            signatures.push(n.clone());
            p.signed = true;
            if opts.remove_signatures {
                dropped.insert(n.clone());
            }
        } else if is_thumb || is_printer {
            dropped.insert(n.clone());
        }
    }
    // 2. the relationships that go: those of a dropped type, and every one that points at a part that goes
    let mut drop_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let name_set: BTreeSet<String> = names.iter().cloned().collect();
    if !odf {
        let rels: Vec<String> = names.iter().filter(|n| n.ends_with(".rels") && !dropped.contains(*n)).cloned().collect();
        for r in &rels {
            let Some(source) = rels_source(r) else { continue };
            let mut e = z.by_name(r).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))?;
            let x = String::from_utf8(read_capped(&mut e, MAX_XML)?).map_err(|_| ScrubError::Unsupported(format!("{r} is not UTF-8 text, leaf cannot rewrite it")))?;
            let doc = roxmltree::Document::parse(&x).map_err(|e| ScrubError::Malformed(format!("{r} is damaged: {e}")))?;
            for n in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "Relationship") {
                let ty = n.attribute("Type").unwrap_or("");
                let external = n.attribute("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
                let to = resolve(&source, n.attribute("Target").unwrap_or(""));
                let sig = ty.contains("/digital-signature/");
                if (sig && opts.remove_signatures) || (!sig && is_rel_dropped(ty)) || (!external && dropped.contains(&to)) {
                    drop_ids.entry(source.clone()).or_default().insert(n.attribute("Id").unwrap_or("").to_string());
                    if !external && !to.is_empty() && !sig && is_rel_dropped(ty) && name_set.contains(&to) {
                        dropped.insert(to);
                    }
                }
            }
        }
    }
    let remaining: Vec<String> = names.iter().filter(|n| !dropped.contains(*n)).cloned().collect();
    // 3. every entry
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| ScrubError::Malformed(format!("the zip structure could not be read: {e}")))?;
        let name = ename(&e);
        if name.contains("..") && !e.is_dir() {
            return Err(ScrubError::Malformed("the zip has an entry with a path that leaves the folder".into()));
        }
        if e.is_dir() {
            p.fates.push(Fate::Keep);
            continue;
        }
        if signatures.contains(&name) {
            let b = read_capped(&mut e, MAX_XML)?;
            let who = String::from_utf8(b).map(|x| signature_details(&x)).unwrap_or_default();
            p.findings.push(Finding::new("Digital signature (signer, time, certificate)", who, true));
            p.fates.push(if dropped.contains(&name) { Fate::Drop } else { Fate::Keep });
            continue;
        }
        if dropped.contains(&name) {
            read_through(&mut e)?;
            if name.contains("/printerSettings/") {
                printers += 1;
            } else if name.starts_with("Thumbnails/") || name.starts_with("docProps/thumbnail") {
                thumbs += 1;
            }
            p.fates.push(Fate::Drop);
            continue;
        }
        if name.ends_with("vbaProject.bin") {
            macros = true;
        }
        if name.contains("/embeddings/") || name.starts_with("Object ") {
            embedded = true;
        }
        let low = name.to_ascii_lowercase();
        let is_media = (name.contains("/media/") || name.starts_with("Pictures/")) && !name.ends_with('/');
        let xml_like = low.ends_with(".xml") || low.ends_with(".rels");
        if e.size() > MAX_XML.max(if is_media { MAX_IN_MEMORY } else { 0 }) && (xml_like || is_media) {
            if is_media {
                unreadable += 1;
            } else {
                return Err(ScrubError::Unsupported(format!("{name} is bigger than 64 MB")));
            }
            read_through(&mut e)?;
            p.fates.push(Fate::Keep);
            continue;
        }
        if is_media {
            let data = read_bytes(&mut e)?;
            let fam = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
                Some(Family::Jpeg)
            } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
                Some(Family::Png)
            } else if data.len() > 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
                Some(Family::Webp)
            } else {
                None
            };
            let mut fate = Fate::Keep;
            if let Some(fam) = fam {
                match crate::image::process(&data, fam, opts) {
                    Ok((out, found)) => {
                        let what: Vec<String> = found.iter().filter(|f| f.remove).take(3).map(|f| f.label.clone()).collect();
                        if !what.is_empty() && out != data {
                            pics.push(Finding::new(format!("Picture metadata ({})", name.rsplit('/').next().unwrap_or(&name)), what.join(", "), true));
                            fate = Fate::Replace(out);
                        }
                    }
                    Err(_) => unreadable += 1,
                }
            }
            p.fates.push(fate);
            continue;
        }
        if !xml_like && !name.starts_with("customXml/") {
            read_through(&mut e)?;
            p.fates.push(Fate::Keep);
            continue;
        }
        let bytes = read_capped(&mut e, MAX_XML)?;
        let xml = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                // a UTF-16 or damaged XML part: not rewritten, never silently kept
                if xml_like {
                    return Err(ScrubError::Unsupported(format!("{name} is not UTF-8 text, leaf cannot rewrite it")));
                }
                p.fates.push(Fate::Keep);
                continue;
            }
        };
        let blank: Option<&str> = match (odf, name.as_str()) {
            (false, "docProps/core.xml") => Some(CORE_BLANK),
            (false, "docProps/app.xml") => Some(APP_BLANK),
            (false, "docProps/custom.xml") => Some(CUSTOM_BLANK),
            _ => None,
        };
        let new: Option<String> = if let Some(b) = blank {
            roxmltree::Document::parse(&xml).map_err(|e| ScrubError::Malformed(format!("{name} is damaged: {e}")))?;
            for (l, v) in props(&xml, false) {
                p.findings.push(Finding::new(l, v, true));
            }
            (xml != b).then(|| b.to_string())
        } else if odf && name == "meta.xml" {
            roxmltree::Document::parse(&xml).map_err(|e| ScrubError::Malformed(format!("meta.xml is damaged: {e}")))?;
            for (l, v) in props(&xml, true) {
                p.findings.push(Finding::new(l, v, true));
            }
            let b = odf_meta_blank(&xml);
            (xml != b).then_some(b)
        } else if name.starts_with("customXml/") && low.ends_with(".xml") && !name.contains("itemProps") {
            if xml.trim() != CUSTOM_XML_BLANK {
                customs += 1;
                Some(CUSTOM_XML_BLANK.to_string())
            } else {
                None
            }
        } else if low.ends_with(".rels") {
            let ids = rels_source(&name).and_then(|s| drop_ids.get(&s)).cloned().unwrap_or_default();
            let changed = drop_elements(&xml, "Relationship", "Id", |v| ids.contains(v)).ok_or_else(|| ScrubError::Malformed(format!("{name} is damaged")))?;
            (changed != xml).then_some(changed)
        } else if !odf && name == "[Content_Types].xml" {
            let changed = drop_content_types(&xml, &dropped, &remaining)?;
            (changed != xml).then_some(changed)
        } else if odf && name == "META-INF/manifest.xml" {
            let changed = drop_elements(&xml, "file-entry", "full-path", |p| dropped.contains(p) || p.starts_with("Thumbnails/")).ok_or_else(|| ScrubError::Malformed("manifest.xml is damaged".into()))?;
            (changed != xml).then_some(changed)
        } else {
            let rewritten = rewrite_part(&name, odf, &xml, &mut counts)?;
            match drop_ids.get(&name) {
                Some(ids) if !ids.is_empty() => {
                    let base = rewritten.as_deref().unwrap_or(&xml);
                    let changed = drop_rel_refs(&name, base, ids)?;
                    (changed != xml).then_some(changed)
                }
                _ => rewritten,
            }
        };
        p.fates.push(match new {
            Some(n) => Fate::Replace(n.into_bytes()),
            None => Fate::Keep,
        });
    }
    if thumbs > 0 {
        p.findings.push(Finding::new("Thumbnail picture", "", true));
    }
    if printers > 0 {
        p.findings.push(Finding::new(CAT_PRINTER, format!("{printers} files"), true));
    }
    if customs > 0 {
        p.findings.push(Finding::new("Custom XML data", format!("{customs} parts"), true));
    }
    p.findings.extend(counts.findings());
    p.findings.extend(pics);
    p.notes.push("The text of comments and tracked changes and everything you can see in the document stays: only who wrote them, when, and the program's own numbers are removed.".into());
    if p.signed && opts.remove_signatures {
        p.notes.push("The digital signatures are removed completely (signer, time, certificate): the cleaned document is no longer signed.".into());
    }
    if unreadable > 0 {
        p.notes.push(format!("Still inside: {unreadable} embedded picture(s) leaf could not read, their own metadata stays."));
    }
    if macros {
        p.notes.push("Still inside: macros (vbaProject.bin) are not touched and can carry names.".into());
    }
    if embedded {
        p.notes.push("Still inside: embedded objects keep their own metadata.".into());
    }
    Ok(p)
}

/// The refusal of a signed document when the signatures are not to be removed.
pub(crate) const SIGNED_TEXT: &str = "this document is signed: removing metadata breaks the signature. Tick “Remove digital signatures too” to clean it without them";

const CUSTOM_XML_BLANK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<root/>";

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let mut z = open(path)?;
    let odf = is_odf(&mut z);
    if odf {
        let meta = read_entry(&mut z, "meta.xml").ok_or_else(|| ScrubError::Malformed("no meta.xml".into()))?;
        roxmltree::Document::parse(&meta).map_err(|e| ScrubError::Malformed(format!("meta.xml is damaged: {e}")))?;
    }
    let format = match sniff(path) {
        Some(Kind::Ooxml(n)) | Some(Kind::Odf(n)) => n,
        None => "Office document",
    };
    let p = plan(path, opts)?;
    let refuse = p.signed && !opts.remove_signatures;
    Ok(Inspection { format: format.into(), findings: p.findings, notes: p.notes, can_clean: !refuse, refusal: refuse.then(|| SIGNED_TEXT.to_string()) })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    // refuses a damaged or encrypted zip before anything is written
    let p = plan(src, opts)?;
    if p.signed && !opts.remove_signatures {
        return Err(ScrubError::Unsupported(SIGNED_TEXT.into()));
    }
    let mut z = open(src)?;
    let out = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    let mut w = ZipWriter::new(io::BufWriter::new(out));
    let zerr = |e: zip::result::ZipError| ScrubError::Io(io::Error::other(e.to_string()));
    for (i, fate) in p.fates.iter().enumerate() {
        let mut e = z.by_index(i).map_err(zerr)?;
        let name = ename(&e);
        if matches!(fate, Fate::Drop) {
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
        w.start_file(name, opts).map_err(zerr)?;
        match fate {
            Fate::Replace(bytes) => w.write_all(bytes)?,
            _ => {
                io::copy(&mut e, &mut w)?;
            }
        }
    }
    let mut inner = w.finish().map_err(zerr)?;
    inner.flush()?;
    inner.get_ref().sync_all()?;
    drop(inner);
    // every reference of the cleaned package must still resolve: a removed part that something still points at is a document
    // Office "repairs" or refuses to open. What was already broken in the original is not leaf's doing and does not count.
    let before = broken_references(src)?;
    let after = broken_references(dst)?;
    if let Some(new) = after.difference(&before).next() {
        return Err(ScrubError::Verify(format!("the cleaned document would be inconsistent: {new}")));
    }
    Ok(())
}
