//! SVG: the `metadata` element, comments, processing instructions, and every element and attribute of an editor namespace
//! (inkscape, sodipodi, Dublin Core, Creative Commons, RDF, Adobe, Sketch, Affinity) are cut out of the text; the drawing
//! elements are not touched. Pictures embedded as data are not rewritten: their own metadata is REPORTED.

use crate::{Family, Finding, Inspection, Options, Result, ScrubError};
use std::fs;
use std::ops::Range;
use std::path::Path;

const EDITOR_NS: [&str; 9] = [
    "inkscape.org",
    "sodipodi.sourceforge.net",
    "creativecommons.org/ns",
    "purl.org/dc/",
    "w3.org/1999/02/22-rdf-syntax-ns",
    "ns.adobe.com",
    "bohemiancoding.com/sketch",
    "serif.com",
    "ns.nsc",
];
pub(crate) const SVG_NS: &str = "http://www.w3.org/2000/svg";

pub(crate) fn is_editor(uri: Option<&str>) -> bool {
    uri.is_some_and(|u| EDITOR_NS.iter().any(|e| u.contains(e)))
}

fn load(d: &[u8]) -> Result<String> {
    let s = std::str::from_utf8(d).map_err(|_| ScrubError::Unsupported("the SVG is not UTF-8 text".into()))?;
    Ok(s.trim_start_matches('\u{feff}').to_string())
}

fn base64(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

struct Found {
    /// what is cut out (empty replacement) or replaced
    ranges: Vec<(Range<usize>, String)>,
    findings: Vec<Finding>,
    notes: Vec<String>,
    decls: Vec<(String, String)>,
}

fn scan(xml: &str, opts: &Options) -> Result<Found> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| ScrubError::Malformed(format!("the SVG could not be read: {e}")))?;
    let mut f = Found { ranges: vec![], findings: vec![], notes: vec![], decls: vec![] };
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err(ScrubError::Malformed("the file is not an SVG drawing".into()));
    }
    let mut embedded = 0;
    for n in doc.descendants() {
        if n.is_comment() {
            f.findings.push(Finding::new("Comment", n.text().unwrap_or(""), true));
            f.ranges.push((n.range(), String::new()));
        } else if n.is_pi() {
            f.findings.push(Finding::new("Processing instruction", n.pi().map(|p| p.target).unwrap_or(""), true));
            f.ranges.push((n.range(), String::new()));
        } else if n.is_element() {
            let ns = n.tag_name().namespace();
            if is_editor(ns) || (ns == Some(SVG_NS) && n.tag_name().name() == "metadata") {
                let label = if n.tag_name().name() == "metadata" { "Metadata element".to_string() } else { format!("Editor data ({})", n.tag_name().name()) };
                f.findings.push(Finding::new(label, "", true));
                f.ranges.push((n.range(), String::new()));
                continue;
            }
            for a in n.attributes() {
                if is_editor(a.namespace()) {
                    f.findings.push(Finding::new(format!("Editor attribute ({})", a.name()), a.value(), true));
                    let r = a.range();
                    let mut s = r.start;
                    while s > 0 && xml.as_bytes()[s - 1].is_ascii_whitespace() {
                        s -= 1;
                    }
                    f.ranges.push((s..r.end, String::new()));
                }
            }
            // a link to a file on this computer names the folder (and the user): reduced to the file name
            for a in n.attributes().filter(|a| a.name() == "href" && !is_editor(a.namespace())) {
                if local_path(a.value()) {
                    let file = a.value().rsplit(['/', '\\']).next().unwrap_or("").to_string();
                    f.findings.push(Finding::new("Local file path in a link (reduced to the file name)", a.value(), true));
                    f.ranges.push((a.range_value(), file.replace('&', "&amp;").replace('"', "&quot;")));
                }
            }
            if n.tag_name().name() == "image" {
                if let Some(h) = n.attributes().find(|a| a.name() == "href").map(|a| a.value()) {
                    if let Some(rest) = h.strip_prefix("data:image/") {
                        embedded += 1;
                        if let Some((_, b64)) = rest.split_once("base64,") {
                            let bytes = base64(b64);
                            let fam = if bytes.starts_with(&[0xFF, 0xD8]) {
                                Some(Family::Jpeg)
                            } else if bytes.starts_with(b"\x89PNG") {
                                Some(Family::Png)
                            } else if bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
                                Some(Family::Webp)
                            } else {
                                None
                            };
                            if let Some(fam) = fam {
                                if let Ok((_, inner)) = crate::image::process(&bytes, fam, opts) {
                                    let what: Vec<String> = inner.iter().filter(|x| x.remove).take(4).map(|x| x.label.clone()).collect();
                                    if !what.is_empty() {
                                        f.notes.push(format!("Still inside: a picture embedded in the drawing carries its own metadata ({}). It is not rewritten; clean the picture before embedding it.", what.join(", ")));
                                    }
                                }
                            }
                        }
                    } else if !h.starts_with('#') {
                        f.notes.push("The drawing links to a picture file: that file keeps its own metadata.".into());
                    }
                }
            }
        }
    }
    if embedded > 0 {
        f.notes.push(format!("{embedded} picture(s) are embedded in the drawing."));
    }
    for ns in root.namespaces() {
        if let (Some(p), true) = (ns.name(), is_editor(Some(ns.uri()))) {
            f.decls.push((p.to_string(), ns.uri().to_string()));
        }
    }
    if doc.descendants().any(|n| n.is_element() && matches!(n.tag_name().name(), "title" | "desc") && n.tag_name().namespace() == Some(SVG_NS)) {
        f.notes.push("Title and description texts are part of the drawing (shown as tooltips and read aloud) and stay.".into());
    }
    Ok(f)
}

/// A path on this computer or a share: `file:///C:/...`, `C:\...`, `\server\...`, `/home/...`, `/Users/...`.
pub(crate) fn local_path(t: &str) -> bool {
    let b = t.as_bytes();
    t.to_ascii_lowercase().starts_with("file:")
        || t.starts_with("\\\\")
        || t.starts_with("/home/")
        || t.starts_with("/Users/")
        || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
}

fn cut(xml: &str, mut ranges: Vec<(Range<usize>, String)>) -> String {
    ranges.sort_by_key(|(r, _)| (r.start, std::cmp::Reverse(r.end)));
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for (r, with) in ranges {
        if r.start < at {
            if r.end > at {
                at = r.end;
            }
            continue;
        }
        out.push_str(&xml[at..r.start]);
        out.push_str(&with);
        at = r.end;
    }
    out.push_str(&xml[at..]);
    out
}

fn clean(xml: &str, opts: &Options) -> Result<(String, Vec<Finding>, Vec<String>)> {
    let f = scan(xml, opts)?;
    let mut out = cut(xml, f.ranges.clone());
    let mut findings = f.findings;
    // the declarations of the editor prefixes, when nothing uses them any more
    for (p, uri) in &f.decls {
        for q in ['"', '\''] {
            let decl = format!("xmlns:{p}={q}{uri}{q}");
            if let Some(at) = out.find(&decl) {
                let mut s = at;
                while s > 0 && out.as_bytes()[s - 1].is_ascii_whitespace() {
                    s -= 1;
                }
                let tried = format!("{}{}", &out[..s], &out[at + decl.len()..]);
                if roxmltree::Document::parse(&tried).is_ok() {
                    out = tried;
                    findings.push(Finding::new(format!("Editor namespace declaration ({p})"), "", true));
                }
            }
        }
    }
    Ok((out, findings, f.notes))
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    if fs::metadata(path)?.len() > 256 << 20 {
        return Err(ScrubError::Unsupported("the SVG is bigger than 256 MB".into()));
    }
    let xml = load(&fs::read(path)?)?;
    let f = scan(&xml, opts)?;
    let mut findings = f.findings;
    for (p, _) in f.decls {
        findings.push(Finding::new(format!("Editor namespace declaration ({p})"), "", true));
    }
    Ok(Inspection { format: "SVG".into(), findings, notes: f.notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    if fs::metadata(src)?.len() > 256 << 20 {
        return Err(ScrubError::Unsupported("the SVG is bigger than 256 MB".into()));
    }
    let xml = load(&fs::read(src)?)?;
    let (out, _, _) = clean(&xml, opts)?;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, out.as_bytes())?;
    f.sync_all()?;
    Ok(())
}
