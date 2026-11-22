//! A session's transcript (PLAN F4), read from the agent's own file, read only. Files are tens of MB: a page of the transcript is a
//! chunk of bytes ([`read_back`], [`read_forward`]) cut at line boundaries, never the whole file; the scans ([`files_touched`],
//! [`wrote`], [`search`]) read line by line and keep only what they report. Every call blocks: run it off the UI thread.
//!
//! The Claude Code format is not documented: a line that does not parse is skipped. What is read: `user` records with a plain prompt,
//! `assistant` records with text parts and `tool_use` parts (the tool's name and, of its input, the file path, the notebook path, the
//! path, the pattern or the command). Nothing from a file is copied out of memory except into the page that shows it.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

/// A text longer than this many characters is cut (the page shows a transcript, not a dump).
const TEXT_MAX: usize = 6000;
/// A chunk that has no line end in it grows this many times before it is given up.
const GROW: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// What the person wrote.
    Prompt,
    /// What the agent said.
    Text,
    /// A tool call, folded to one line: `text` is the tool's name, `target` its file path (or what it works on).
    Tool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Byte offset of the file line it comes from (items of one line share it).
    pub offset: u64,
    pub kind: Kind,
    pub text: String,
    pub target: String,
    /// The tool call writes this file (Edit, Write, MultiEdit, NotebookEdit).
    pub writes: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Chunk {
    pub items: Vec<Item>,
    /// The file bytes `[start, end)` were read (both at line boundaries).
    pub start: u64,
    pub end: u64,
}

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    message: Option<MsgOrOther>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MsgOrOther {
    Msg(Msg),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Msg {
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<Part>),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    name: Option<String>,
    input: Option<Input>,
}

#[derive(Deserialize)]
struct Input {
    file_path: Option<String>,
    notebook_path: Option<String>,
    path: Option<String>,
    pattern: Option<String>,
    command: Option<String>,
}

fn cut(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn is_writer(tool: &str) -> bool {
    matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// The items one file line holds (offset 0; the reader sets it). Empty for a line that does not parse or holds nothing to show.
pub fn items_of(line: &str) -> Vec<Item> {
    let Ok(r) = serde_json::from_str::<Rec>(line) else { return Vec::new() };
    if r.is_meta == Some(true) || r.is_sidechain == Some(true) {
        return Vec::new();
    }
    let Some(MsgOrOther::Msg(Msg { content: Some(content) })) = r.message else { return Vec::new() };
    let item = |kind, text: String, target: String, writes| Item { offset: 0, kind, text, target, writes };
    let mut out = Vec::new();
    match (r.kind.as_deref(), content) {
        (Some("user"), Content::Text(t)) => {
            let t = t.trim();
            if !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:") {
                out.push(item(Kind::Prompt, cut(t, TEXT_MAX), String::new(), None));
            }
        }
        (Some("user"), Content::Parts(ps)) => {
            for p in ps.iter().filter(|p| p.kind.as_deref() == Some("text")) {
                let t = p.text.as_deref().unwrap_or("").trim();
                if !t.is_empty() && !t.starts_with('<') && !t.starts_with("Caveat:") {
                    out.push(item(Kind::Prompt, cut(t, TEXT_MAX), String::new(), None));
                }
            }
        }
        (Some("assistant"), Content::Parts(ps)) => {
            for p in ps {
                match p.kind.as_deref() {
                    Some("text") => {
                        let t = p.text.as_deref().unwrap_or("").trim();
                        if !t.is_empty() {
                            out.push(item(Kind::Text, cut(t, TEXT_MAX), String::new(), None));
                        }
                    }
                    Some("tool_use") => {
                        let name = p.name.clone().unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        let inp = p.input.as_ref();
                        let file = inp.and_then(|i| i.file_path.clone().or_else(|| i.notebook_path.clone())).filter(|f| !f.is_empty());
                        let target = file
                            .clone()
                            .or_else(|| inp.and_then(|i| i.path.clone().or_else(|| i.pattern.clone())).filter(|f| !f.is_empty()))
                            .or_else(|| inp.and_then(|i| i.command.as_deref().map(|c| cut(c.lines().next().unwrap_or(""), 120))))
                            .unwrap_or_default();
                        let writes = file.filter(|_| is_writer(&name));
                        out.push(item(Kind::Tool, name, target, writes));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

fn items_at(buf: &[u8], base: u64, into: &mut Vec<Item>) {
    let mut at = 0usize;
    for line in buf.split(|b| *b == b'\n') {
        let off = base + at as u64;
        at += line.len() + 1;
        if line.len() < 2 {
            continue;
        }
        let s = String::from_utf8_lossy(line);
        for mut it in items_of(&s) {
            it.offset = off;
            into.push(it);
        }
    }
}

/// The chunk of about `max` bytes that ends at byte `end` (a line boundary: the file's length, or a chunk's `start`). Its first line is
/// the first whole one; `start` says where it begins (0: the file's beginning is reached).
pub fn read_back(path: &Path, end: u64, max: u64) -> std::io::Result<Chunk> {
    let mut f = std::fs::File::open(path)?;
    let end = end.min(f.metadata()?.len());
    let mut size = max.max(4096);
    for _ in 0..=GROW {
        let from = end.saturating_sub(size);
        f.seek(SeekFrom::Start(from))?;
        let mut buf = vec![0u8; (end - from) as usize];
        f.read_exact(&mut buf)?;
        let skip = if from == 0 {
            0
        } else {
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => i + 1,
                None => {
                    size *= 4;
                    continue;
                }
            }
        };
        let start = from + skip as u64;
        let mut items = Vec::new();
        items_at(&buf[skip..], start, &mut items);
        return Ok(Chunk { items, start, end });
    }
    Ok(Chunk { items: Vec::new(), start: 0, end })
}

/// The chunk of about `max` bytes that begins at byte `from` (a line boundary). `end` is where the next one begins (the file's length:
/// the end is reached).
pub fn read_forward(path: &Path, from: u64, max: u64) -> std::io::Result<Chunk> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let from = from.min(len);
    let mut size = max.max(4096);
    for _ in 0..=GROW {
        let to = (from + size).min(len);
        f.seek(SeekFrom::Start(from))?;
        let mut buf = vec![0u8; (to - from) as usize];
        f.read_exact(&mut buf)?;
        let keep = if to == len {
            buf.len()
        } else {
            match buf.iter().rposition(|b| *b == b'\n') {
                Some(i) => i + 1,
                None => {
                    size *= 4;
                    continue;
                }
            }
        };
        let mut items = Vec::new();
        items_at(&buf[..keep], from, &mut items);
        return Ok(Chunk { items, start: from, end: from + keep as u64 });
    }
    Ok(Chunk { items: Vec::new(), start: from, end: len })
}

/// Calls `f(offset, line)` for every line of the file, reading one line at a time; stops when `cancel` is set or `f` says so.
pub(crate) fn each_line(path: &Path, cancel: &AtomicBool, mut f: impl FnMut(u64, &str) -> bool) {
    let Ok(file) = std::fs::File::open(path) else { return };
    let mut r = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    let mut off = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if !f(off, &String::from_utf8_lossy(&buf)) {
                    return;
                }
                off += n as u64;
            }
        }
    }
}

/// The files the session's edit and write tool calls named, the most recently touched first, each once.
pub fn files_touched(path: &Path, cancel: &AtomicBool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    each_line(path, cancel, |_, line| {
        if line.contains("\"tool_use\"") {
            for it in items_of(line) {
                if let Some(w) = it.writes {
                    out.retain(|o| *o != w);
                    out.push(w);
                }
            }
        }
        true
    });
    out.reverse();
    out
}

/// Whether a tool call of the session wrote the file whose key ([`crate::folder_key`]) is `key`.
pub fn wrote(path: &Path, key: &str, cancel: &AtomicBool) -> bool {
    let mut found = false;
    each_line(path, cancel, |_, line| {
        if line.contains("\"tool_use\"") && items_of(line).iter().any(|it| it.writes.as_deref().is_some_and(|w| crate::folder_key(Path::new(w)) == key)) {
            found = true;
        }
        !found
    });
    found
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub offset: u64,
    pub kind: Kind,
    /// A little text around the match.
    pub snippet: String,
}

/// Case-blind plain-text search of the session's prompts, texts and tool lines; `f` gets each hit (one per item); stops after `max`.
pub fn search(path: &Path, query: &str, max: usize, cancel: &AtomicBool, mut f: impl FnMut(Hit)) {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return;
    }
    // on the raw line only when the query has nothing the JSON would write differently
    let plain = !q.chars().any(|c| matches!(c, '"' | '\\') || c.is_control() || !c.is_ascii());
    let mut n = 0;
    each_line(path, cancel, |off, line| {
        if plain && !line.to_ascii_lowercase().contains(&q) {
            return true;
        }
        for it in items_of(line) {
            let hay = if it.kind == Kind::Tool { format!("{} {}", it.text, it.target) } else { it.text.clone() };
            let low = hay.to_lowercase();
            if let Some(i) = low.find(&q) {
                // `i` is in the lower-cased text; the same position in the original when the case change kept the lengths
                let src = if low.len() == hay.len() { &hay } else { &low };
                f(Hit { offset: off, kind: it.kind, snippet: snippet(src, i, q.len()) });
                n += 1;
                if n >= max {
                    return false;
                }
            }
        }
        true
    });
}

fn snippet(s: &str, at: usize, len: usize) -> String {
    let mut a = at.saturating_sub(40);
    while !s.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (at + len + 80).min(s.len());
    while !s.is_char_boundary(b) {
        b += 1;
    }
    let body: String = s[a..b].split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}{}{}", if a > 0 { "\u{2026}" } else { "" }, body, if b < s.len() { "\u{2026}" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // a made-up sample in the shape of the format (no real content)
    const SAMPLE: &str = concat!(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Add a sample feature\"}}\n",
        "garbage line\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Sure, editing now.\"},{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"C:\\\\proj\\\\app\\\\a.rs\",\"old_string\":\"x\",\"new_string\":\"y\"}}]}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"C:\\\\proj\\\\app\\\\r.rs\"}}]}}\n",
        "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Write\",\"input\":{\"file_path\":\"C:\\\\proj\\\\app\\\\side.rs\"}}]}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Write\",\"input\":{\"file_path\":\"C:\\\\proj\\\\app\\\\b.rs\",\"content\":\"hello\"}},{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"C:\\\\proj\\\\app\\\\a.rs\"}}]}}\n",
        "{\"type\":\"future\",\"x\":1}\n",
    );

    #[test]
    fn files_touched_are_the_edit_and_write_calls_newest_first_once_each_and_the_rest_is_skipped() {
        let dir = std::env::temp_dir().join(format!("leaf-transcript-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.jsonl");
        std::fs::File::create(&p).unwrap().write_all(SAMPLE.as_bytes()).unwrap();
        let no = AtomicBool::new(false);
        assert_eq!(files_touched(&p, &no), ["C:\\proj\\app\\a.rs", "C:\\proj\\app\\b.rs"]);
        assert!(wrote(&p, &crate::folder_key(Path::new("C:\\proj\\app\\B.rs")), &no) == cfg!(windows));
        assert!(!wrote(&p, &crate::folder_key(Path::new("C:\\proj\\app\\r.rs")), &no));
        // the chunk read from the end holds the same items, in order, with the tool folded to name + path
        let c = read_back(&p, std::fs::metadata(&p).unwrap().len(), 1 << 20).unwrap();
        assert_eq!(c.start, 0);
        assert_eq!(c.items[0].kind, Kind::Prompt);
        assert_eq!((c.items[2].text.as_str(), c.items[2].target.as_str()), ("Edit", "C:\\proj\\app\\a.rs"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
