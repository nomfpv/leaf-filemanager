//! Search in file contents: `content:text` (plain text, any case) or `regex:pattern` (the `regex` crate) in the folder search box.
//! Pure and blocking: the caller runs it on a worker and hands the hits on as they come.

use regex::{Regex, RegexBuilder};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// Files bigger than this are not read.
pub const MAX_FILE: u64 = 8 << 20;
/// A line is cut to this many characters in the result.
const MAX_LINE: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Names,
    Text,
    Regex,
}

/// The mode a query is in and the text after its prefix.
pub fn mode_of(query: &str) -> (Mode, &str) {
    let q = query.trim_start();
    for (pre, mode) in [("content:", Mode::Text), ("regex:", Mode::Regex)] {
        if let Some(head) = q.get(..pre.len())
            && head.eq_ignore_ascii_case(pre)
        {
            return (mode, q[pre.len()..].trim_start());
        }
    }
    (Mode::Names, query)
}

/// The query written in `mode` (the old prefix taken off first).
pub fn with_mode(query: &str, mode: Mode) -> String {
    let (_, text) = mode_of(query);
    match mode {
        Mode::Names => text.to_string(),
        Mode::Text => format!("content:{text}"),
        Mode::Regex => format!("regex:{text}"),
    }
}

/// What the lines are matched with; `None` for a name search or an empty pattern. Case does not matter unless the regex says so
/// (`(?-i)`).
pub fn matcher(query: &str) -> Option<Result<Regex, String>> {
    let (mode, text) = mode_of(query);
    if mode == Mode::Names || text.is_empty() {
        return None;
    }
    let src = if mode == Mode::Text { regex::escape(text) } else { text.to_string() };
    Some(RegexBuilder::new(&src).case_insensitive(true).size_limit(10 << 20).build().map_err(|e| e.to_string()))
}

/// Walks `root` and calls `hit(path, size, line number, line)` for the first matching line of each text file. Binary files (a NUL
/// in the first 8 KB), files over [`MAX_FILE`], links and (unless `hidden`) dot-named items are skipped. Stops at `max_hits`
/// or when `cancelled` says so. Returns the number of hits.
pub fn search(root: &Path, re: &Regex, hidden: bool, max_hits: usize, cancelled: &dyn Fn() -> bool, hit: &mut dyn FnMut(&Path, u64, usize, &str)) -> usize {
    let mut found = 0usize;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if cancelled() || found >= max_hits {
                return found;
            }
            if !hidden && e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                let Ok(m) = e.metadata() else { continue };
                if m.len() > MAX_FILE {
                    continue;
                }
                let path = e.path();
                if let Some((n, line)) = first_match(&path, re, cancelled) {
                    found += 1;
                    hit(&path, m.len(), n, &line);
                }
            }
        }
    }
    found
}

fn first_match(path: &Path, re: &Regex, cancelled: &dyn Fn() -> bool) -> Option<(usize, String)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 8192];
    let n = f.read(&mut head).ok()?;
    if head[..n].contains(&0) {
        return None;
    }
    let mut rd = BufReader::new(std::io::Cursor::new(head[..n].to_vec()).chain(f));
    let mut buf = Vec::new();
    let mut no = 0usize;
    loop {
        buf.clear();
        if rd.read_until(b'\n', &mut buf).ok()? == 0 {
            return None;
        }
        no += 1;
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if re.is_match(line) {
            let shown: String = line.trim().chars().take(MAX_LINE).collect();
            return Some((no, shown));
        }
        if no % 4096 == 0 && cancelled() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_and_matching() {
        assert_eq!(mode_of("content: foo"), (Mode::Text, "foo"));
        assert_eq!(mode_of("Regex:a+b"), (Mode::Regex, "a+b"));
        assert_eq!(mode_of("plain"), (Mode::Names, "plain"));
        assert_eq!(with_mode("content:x", Mode::Regex), "regex:x");
        assert!(matcher("a.b").is_none());
        assert!(matcher("content:a.b").unwrap().unwrap().is_match("A.B"));
        assert!(!matcher("content:a.b").unwrap().unwrap().is_match("axb"));
        assert!(matcher("regex:(").unwrap().is_err());
    }
}
