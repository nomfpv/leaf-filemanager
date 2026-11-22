//! The NZB file: an XML list of the files of a post, each with its newsgroups and the message ids of its articles
//! (segments). The password of the archives comes from `<meta type="password">` or from `{{password}}` in the NZB's own
//! file name.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// 1-based, the order of the parts in the file.
    pub number: u32,
    /// The article's encoded size (what the server sends, about 1.03 times the part).
    pub bytes: u64,
    /// Without the angle brackets.
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NzbFile {
    pub subject: String,
    pub poster: String,
    /// Unix seconds of the post (0: unknown): against a server's retention.
    pub date: u64,
    pub groups: Vec<String>,
    /// Sorted by number, duplicates dropped. A segment whose message id cannot be asked for safely has an empty one: it
    /// counts as missing.
    pub segments: Vec<Segment>,
    /// The name the subject gives (the quoted part, else the word that looks like a file name); may be obfuscated.
    pub name: String,
    /// Segments without a number, with one number twice, or numbers with gaps: the parts may not cover the file, so it is
    /// never taken as good without a full par2 verify.
    #[serde(default)]
    pub irregular: bool,
}

impl NzbFile {
    /// What the articles weigh on the wire.
    pub fn bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.bytes).sum()
    }

    pub fn is_par2(&self) -> bool {
        self.name.to_ascii_lowercase().ends_with(".par2")
    }

    /// A par2 recovery volume (`name.vol03+04.par2`): the number of recovery blocks it holds. The index file (`name.par2`)
    /// gives `None`.
    pub fn par2_blocks(&self) -> Option<u32> {
        par2_blocks(&self.name)
    }
}

/// `x.vol03+04.par2` → 4.
pub fn par2_blocks(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".par2")?;
    let i = stem.rfind(".vol")?;
    let (_, count) = stem[i + 4..].split_once(['+', '-'])?;
    count.parse().ok()
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nzb {
    /// `<meta type="title">`, else the NZB's file name without `{{password}}` and `.nzb`.
    pub title: String,
    /// The archives' password, from the meta tag or the file name. Never logged.
    #[serde(skip)]
    pub password: Option<String>,
    /// `<meta type="category">`.
    pub category: Option<String>,
    pub files: Vec<NzbFile>,
}

impl Nzb {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(NzbFile::bytes).fold(0, u64::saturating_add)
    }

    /// What tells this NZB from another one with the same title (its files' names and message ids; FNV-1a, stable across
    /// versions of leaf). Never 0.
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for x in b.iter().chain([0xffu8].iter()) {
                h ^= u64::from(*x);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        };
        for f in &self.files {
            eat(f.name.as_bytes());
            for s in &f.segments {
                eat(&s.number.to_le_bytes());
                eat(s.message_id.as_bytes());
            }
        }
        h.max(1)
    }
}

// (the password is never in a Debug text)
impl std::fmt::Debug for Nzb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nzb").field("title", &self.title).field("password", &self.password.as_ref().map(|_| "***")).field("category", &self.category).field("files", &self.files).finish()
    }
}

/// The largest NZB file read (a real one of a very large post is a few MB).
pub const MAX_NZB_BYTES: u64 = 128 << 20;
/// The most segments one NZB may list over all its files.
pub const MAX_SEGMENTS: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not XML, or XML that is not an NZB.
    NotNzb(String),
    /// An NZB without a single segment.
    Empty,
    /// More than [`MAX_NZB_BYTES`] or [`MAX_SEGMENTS`].
    TooLarge,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NotNzb(d) => write!(f, "not an NZB file: {d}"),
            ParseError::Empty => f.write_str("the NZB lists no articles"),
            ParseError::TooLarge => f.write_str("the NZB lists too many articles"),
        }
    }
}

/// `{{password}}` in an NZB's file name: the name without it, and the password.
pub fn split_password(file_name: &str) -> (String, Option<String>) {
    if let Some(a) = file_name.find("{{")
        && let Some(b) = file_name[a + 2..].find("}}")
    {
        let pw = file_name[a + 2..a + 2 + b].to_string();
        let rest = format!("{}{}", &file_name[..a], &file_name[a + 4 + b..]);
        return (rest.trim().to_string(), (!pw.is_empty()).then_some(pw));
    }
    (file_name.to_string(), None)
}

/// Parses an NZB. `file_name` is the NZB's own name (for the title and a `{{password}}` in it).
pub fn parse(xml: &str, file_name: &str) -> Result<Nzb, ParseError> {
    if xml.len() as u64 > MAX_NZB_BYTES {
        return Err(ParseError::TooLarge);
    }
    // (a DOCTYPE line is usual in NZB files: roxmltree refuses DTDs unless allowed. An NZB never defines entities of its
    // own; one that does is refused, so a nested-entity "billion laughs" file cannot blow up in memory: roxmltree limits the
    // nesting, not the number of top-level references)
    if xml.contains("<!ENTITY") {
        return Err(ParseError::NotNzb("the file defines XML entities".into()));
    }
    let opts = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: (MAX_SEGMENTS * 4) as u32 };
    let doc = roxmltree::Document::parse_with_options(xml, opts).map_err(|e| ParseError::NotNzb(e.to_string()))?;
    let root = doc.root_element();
    if !root.tag_name().name().eq_ignore_ascii_case("nzb") {
        return Err(ParseError::NotNzb(format!("the root element is <{}>", root.tag_name().name())));
    }
    let (stem, name_pw) = split_password(file_name);
    let stem = stem.strip_suffix(".nzb").or_else(|| stem.strip_suffix(".NZB")).unwrap_or(&stem).trim().to_string();
    let mut nzb = Nzb { title: stem, password: name_pw, ..Default::default() };
    let mut segments_total = 0usize;
    for el in root.children().filter(|n| n.is_element()) {
        match el.tag_name().name() {
            "head" => {
                for m in el.children().filter(|n| n.has_tag_name("meta")) {
                    let text = m.text().unwrap_or_default().trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    match m.attribute("type").unwrap_or_default().to_ascii_lowercase().as_str() {
                        "title" | "name" => nzb.title = text,
                        "password" => nzb.password = Some(text),
                        "category" => nzb.category = Some(text),
                        _ => {}
                    }
                }
            }
            "file" => {
                let subject = el.attribute("subject").unwrap_or_default().to_string();
                let mut f = NzbFile {
                    name: name_from_subject(&subject),
                    subject,
                    poster: el.attribute("poster").unwrap_or_default().to_string(),
                    date: el.attribute("date").and_then(|d| d.parse().ok()).unwrap_or(0),
                    groups: Vec::new(),
                    segments: Vec::new(),
                    irregular: false,
                };
                for part in el.children().filter(|n| n.is_element()) {
                    match part.tag_name().name() {
                        "groups" => f.groups.extend(part.children().filter(|n| n.has_tag_name("group")).filter_map(|g| g.text()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty())),
                        "segments" => {
                            for s in part.children().filter(|n| n.has_tag_name("segment")) {
                                segments_total += 1;
                                if segments_total > MAX_SEGMENTS {
                                    return Err(ParseError::TooLarge);
                                }
                                let raw = s.text().unwrap_or_default().trim();
                                let raw = raw.strip_prefix('<').unwrap_or(raw);
                                let raw = raw.strip_suffix('>').unwrap_or(raw);
                                // (an id with a line break, a space, a bracket or a control character could carry further NNTP
                                // commands: never asked for, the segment counts as missing)
                                let id = if crate::nntp::valid_id(raw) { raw.to_string() } else { String::new() };
                                let number = s.attribute("number").and_then(|n| n.parse().ok()).unwrap_or(0);
                                let bytes = s.attribute("bytes").and_then(|n| n.parse().ok()).unwrap_or(0);
                                f.segments.push(Segment { number, bytes, message_id: id });
                            }
                        }
                        _ => {}
                    }
                }
                f.segments.sort_by_key(|s| s.number);
                let listed = f.segments.len();
                // (unnumbered segments are all kept: each may be a different part, the yEnc header places it)
                f.segments.dedup_by(|b, a| a.number != 0 && a.number == b.number);
                let numbered = f.segments.iter().filter(|s| s.number != 0).count();
                let max = f.segments.last().map_or(0, |s| s.number as usize);
                f.irregular = f.segments.len() != listed || numbered != f.segments.len() || max != numbered;
                if !f.segments.is_empty() {
                    nzb.files.push(f);
                }
            }
            _ => {}
        }
    }
    if nzb.files.is_empty() {
        return Err(ParseError::Empty);
    }
    // two files with one name (a poster's mistake, or obfuscation): numbered so neither overwrites the other
    let mut seen = std::collections::HashSet::new();
    for (i, f) in nzb.files.iter_mut().enumerate() {
        if f.name.is_empty() || !seen.insert(f.name.to_lowercase()) {
            f.name = match f.name.rsplit_once('.') {
                _ if f.name.is_empty() => format!("file{:03}", i + 1),
                Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({}).{ext}", i + 1),
                _ => format!("{} ({})", f.name, i + 1),
            };
            seen.insert(f.name.to_lowercase());
        }
    }
    Ok(nzb)
}

/// The file name in a subject: `[01/10] - "name.ext" yEnc (1/50)` → `name.ext`; without quotes, the word before `yEnc`
/// or the part counter that has a dot in it.
pub fn name_from_subject(subject: &str) -> String {
    let quoted = subject.split('"').nth(1).filter(|q| !q.trim().is_empty() && subject.matches('"').count() >= 2);
    let raw = match quoted {
        Some(q) => q.trim().to_string(),
        None => {
            let cut = subject.find(" yEnc").unwrap_or(subject.len());
            let head = &subject[..cut];
            let head = strip_counter(head.trim_end());
            head.split_whitespace().rev().find(|w| w.contains('.') && !w.starts_with('.')).unwrap_or(head.split_whitespace().last().unwrap_or("")).to_string()
        }
    };
    safe_name(&raw)
}

/// `name (1/50)` / `name [01/10]` → `name`.
fn strip_counter(s: &str) -> &str {
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(rest) = s.strip_suffix(close)
            && let Some(i) = rest.rfind(open)
            && rest[i + 1..].chars().all(|c| c.is_ascii_digit() || c == '/')
        {
            return s[..i].trim_end();
        }
    }
    s
}

/// A name that is one file in one folder: no path, no characters Windows refuses, no `..`.
pub fn safe_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return String::new();
    }
    if is_reserved(&cleaned) { format!("_{cleaned}") } else { cleaned }
}

/// A name Windows keeps for a device (`CON`, `NUL`, `COM1`, `LPT¹`, also with an extension: `nul.txt`).
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        s => {
            let mut c = s.chars();
            let head: String = c.by_ref().take(3).collect();
            let rest: String = c.collect();
            (head == "COM" || head == "LPT") && matches!(rest.as_str(), "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
        }
    }
}

/// A name that says nothing of its file: no extension, or a long run of hex / random letters before it.
pub fn looks_obfuscated(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else { return true };
    if ext.is_empty() || ext.len() > 5 {
        return true;
    }
    stem.len() >= 24 && !stem.contains([' ', '.', '-', '_']) && stem.chars().all(|c| c.is_ascii_alphanumeric())
}
