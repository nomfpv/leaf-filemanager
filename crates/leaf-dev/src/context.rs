//! Copy as context: one text block from a selection (or a folder) for pasting into a chat or a prompt.

use crate::secrets;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
pub struct Options {
    /// A tree of the included paths before the files.
    pub tree: bool,
    pub line_numbers: bool,
    /// Files whose content (not name) was flagged are included anyway.
    pub include_anyway: bool,
    /// Files larger than this are left out.
    pub max_file_bytes: u64,
    /// The whole text stops here.
    pub max_total_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { tree: true, line_numbers: false, include_anyway: false, max_file_bytes: 256 * 1024, max_total_bytes: 2 * 1024 * 1024 }
    }
}

#[derive(Debug, Clone)]
pub struct LeftOut {
    pub rel: String,
    pub size: u64,
    pub reason: String,
    /// True when only "include anyway" would bring the file in (content findings).
    pub overridable: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Built {
    pub text: String,
    pub included: usize,
    pub included_bytes: u64,
    pub left_out: Vec<LeftOut>,
    /// Set when the hard cap stopped the build.
    pub capped: Option<String>,
}

impl Built {
    pub fn tokens(&self) -> u64 {
        estimate_tokens(self.text.len() as u64)
    }
}

/// bytes / 4, rounded up.
pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// "about 12,400 tokens (estimate)"
pub fn tokens_phrase(tokens: u64) -> String {
    let r = if tokens >= 1000 { (tokens + 50) / 100 * 100 } else { tokens };
    let s = r.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("about {out} tokens (estimate)")
}

struct Cand {
    rel: String,
    abs: PathBuf,
}

fn rel_of(base: &Path, p: &Path) -> String {
    let r = p.strip_prefix(base).unwrap_or(p);
    r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

fn collect(base: &Path, roots: &[PathBuf], cancel: &AtomicBool) -> Option<Vec<Cand>> {
    let mut out: Vec<Cand> = Vec::new();
    for root in roots {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if root.is_dir() {
            let walk = ignore::WalkBuilder::new(root)
                .hidden(false)
                .require_git(false)
                .parents(true)
                .git_global(false)
                .filter_entry(|e| e.file_name() != ".git")
                .sort_by_file_path(|a, b| a.cmp(b))
                .build();
            for ent in walk.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if ent.file_type().is_some_and(|t| t.is_file()) {
                    out.push(Cand { rel: rel_of(base, ent.path()), abs: ent.path().to_path_buf() });
                }
            }
        } else if root.is_file() {
            out.push(Cand { rel: rel_of(base, root), abs: root.clone() });
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.rel.clone()));
    Some(out)
}

fn tree_text(paths: &[&str]) -> String {
    #[derive(Default)]
    struct Node(BTreeMap<String, Node>);
    let mut root = Node::default();
    for p in paths {
        let mut n = &mut root;
        for part in p.split('/') {
            n = n.0.entry(part.to_string()).or_default();
        }
    }
    fn walk(n: &Node, depth: usize, out: &mut String) {
        // folders first, then files
        let mut keys: Vec<&String> = n.0.keys().collect();
        keys.sort_by_key(|k| (n.0[*k].0.is_empty(), k.to_ascii_lowercase()));
        for k in keys {
            let child = &n.0[k];
            out.push_str(&"  ".repeat(depth));
            out.push_str(k);
            if !child.0.is_empty() {
                out.push('/');
            }
            out.push('\n');
            walk(child, depth + 1, out);
        }
    }
    let mut s = String::new();
    walk(&root, 0, &mut s);
    s
}

fn lang_of(rel: &str) -> &'static str {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "json" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "md" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "sh" => "bash",
        "ps1" => "powershell",
        "sql" => "sql",
        "xml" => "xml",
        _ => "",
    }
}

fn size_text(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / 1048576.0)
    } else if b >= 1024 {
        format!("{} KB", b.div_ceil(1024))
    } else {
        format!("{b} bytes")
    }
}

/// One readable text file that passed the name, size and binary checks.
pub struct PFile {
    pub rel: String,
    pub size: u64,
    pub text: String,
    /// A content finding (kind label, line, redacted excerpt): the file is out unless "include anyway".
    pub flagged: Option<(String, usize, String)>,
}

/// What was read from the disk: independent of the display toggles, so a dialog can change them without reading again.
#[derive(Default)]
pub struct Prepared {
    pub files: Vec<PFile>,
    /// Left out whatever the toggles say (secret by name, too large, binary, unreadable).
    pub fixed_out: Vec<LeftOut>,
    pub capped: Option<String>,
}

/// Reads the candidates. `base` is the folder the paths are shown relative to. Returns `None` when cancelled.
pub fn prepare(base: &Path, roots: &[PathBuf], max_file_bytes: u64, max_total_bytes: usize, cancel: &AtomicBool) -> Option<Prepared> {
    let cands = collect(base, roots, cancel)?;
    let mut p = Prepared::default();
    let mut total = 0usize;
    for c in &cands {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let size = std::fs::metadata(&c.abs).map(|m| m.len()).unwrap_or(0);
        let name = c.rel.rsplit('/').next().unwrap_or(&c.rel);
        let mut out = |reason: String| p.fixed_out.push(LeftOut { rel: c.rel.clone(), size, reason, overridable: false });
        if let Some(k) = secrets::looks_secret(name) {
            out(format!("secret by name ({})", k.label()));
            continue;
        }
        if size > max_file_bytes {
            out(format!("over the size limit ({})", size_text(max_file_bytes)));
            continue;
        }
        let Ok(bytes) = std::fs::read(&c.abs) else {
            out("could not be read".into());
            continue;
        };
        let binary = bytes[..bytes.len().min(8192)].contains(&0);
        let Ok(text) = (if binary { Err(()) } else { String::from_utf8(bytes).map_err(|_| ()) }) else {
            out("binary".into());
            continue;
        };
        let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        if total + text.len() > max_total_bytes {
            p.capped = Some(format!(
                "Stopped at {}: the text would pass the cap of {}. Select fewer files or raise the cap in Settings.",
                c.rel,
                size_text(max_total_bytes as u64)
            ));
            break;
        }
        total += text.len();
        let flagged = secrets::scan(&text).into_iter().next().map(|f| (f.kind.label().to_string(), f.line, f.excerpt));
        p.files.push(PFile { rel: c.rel.clone(), size, text, flagged });
    }
    Some(p)
}

impl Prepared {
    /// The files and the left-out list for these toggles.
    fn split(&self, opt: &Options) -> (Vec<&PFile>, Vec<LeftOut>) {
        let mut left = self.fixed_out.clone();
        let mut inc = Vec::new();
        for f in &self.files {
            match &f.flagged {
                Some((kind, line, ex)) if !opt.include_anyway => {
                    left.push(LeftOut { rel: f.rel.clone(), size: f.size, reason: format!("possible {kind} on line {line} ({ex})"), overridable: true })
                }
                _ => inc.push(f),
            }
        }
        (inc, left)
    }

    /// Counts for the dialog, without the text.
    pub fn summary(&self, opt: &Options) -> (usize, u64, Vec<LeftOut>) {
        let (inc, left) = self.split(opt);
        (inc.len(), inc.iter().map(|f| f.size).sum(), left)
    }

    /// Whether any file is held back by a content finding (the "include anyway" toggle matters).
    pub fn has_flagged(&self) -> bool {
        self.files.iter().any(|f| f.flagged.is_some())
    }

    /// The text block for these toggles.
    pub fn assemble(&self, opt: &Options) -> Built {
        let (inc, left) = self.split(opt);
        let mut b = Built { included: inc.len(), included_bytes: inc.iter().map(|f| f.size).sum(), left_out: left, capped: self.capped.clone(), ..Built::default() };
        let mut out = String::new();
        if opt.tree && !inc.is_empty() {
            let paths: Vec<&str> = inc.iter().map(|f| f.rel.as_str()).collect();
            out.push_str("Files:\n");
            out.push_str(&tree_text(&paths));
            out.push('\n');
        }
        for f in &inc {
            let (rel, text) = (&f.rel, &f.text);
            let mut run = 0usize;
            let mut longest = 0usize;
            for ch in text.chars() {
                if ch == '`' {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
            let fence = "`".repeat((longest + 1).max(3));
            out.push_str(&format!("{rel}\n{fence}{}\n", lang_of(rel)));
            if opt.line_numbers {
                let width = text.lines().count().to_string().len();
                for (i, l) in text.lines().enumerate() {
                    out.push_str(&format!("{:>w$} | {l}\n", i + 1, w = width));
                }
            } else {
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            out.push_str(&fence);
            out.push_str("\n\n");
        }
        if !b.left_out.is_empty() {
            out.push_str("Left out:\n");
            for l in &b.left_out {
                out.push_str(&format!("- {} ({}): {}\n", l.rel, size_text(l.size), l.reason));
            }
        }
        if let Some(c) = &b.capped {
            out.push_str(&format!("\n{c}\n"));
        }
        b.text = out;
        b
    }
}

/// Builds the block in one go. Returns `None` when cancelled.
pub fn build(base: &Path, roots: &[PathBuf], opt: &Options, cancel: &AtomicBool) -> Option<Built> {
    Some(prepare(base, roots, opt.max_file_bytes, opt.max_total_bytes, cancel)?.assemble(opt))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_from_a_temp_tree() {
        let base = std::env::temp_dir().join(format!("leaf-dev-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::create_dir_all(proj.join("target")).unwrap();
        std::fs::write(proj.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(proj.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(proj.join("target/out.txt"), "ignored").unwrap();
        std::fs::write(proj.join(".env"), "A=1\n").unwrap();
        std::fs::write(proj.join("pic.bin"), [0u8, 1, 2, 3]).unwrap();
        let key = format!("{}{}", "AKIA", "Z".repeat(16));
        std::fs::write(proj.join("notes.txt"), format!("key {key}\n")).unwrap();
        let cancel = AtomicBool::new(false);
        let roots = vec![proj.clone()];
        let o = Options::default();
        let b = build(&base, &roots, &o, &cancel).unwrap();
        assert_eq!(b.included, 2, "{}", b.text); // .gitignore and main.rs
        assert!(b.text.contains("proj/src/main.rs\n```rust"));
        assert!(!b.text.contains("ignored"));
        assert!(!b.text.contains("A=1"));
        assert!(!b.text.contains(&key));
        let why = |n: &str| b.left_out.iter().find(|l| l.rel.ends_with(n)).map(|l| l.reason.clone()).unwrap_or_default();
        assert!(why(".env").starts_with("secret by name"));
        assert_eq!(why("pic.bin"), "binary");
        assert!(why("notes.txt").starts_with("possible"));
        // include anyway brings the content finding in but never the name match
        let b2 = build(&base, &roots, &Options { include_anyway: true, ..o }, &cancel).unwrap();
        assert!(b2.text.contains(&key) && !b2.text.contains("A=1"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
