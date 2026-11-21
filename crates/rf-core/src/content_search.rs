//! Search in file contents: `content:text` (plain text, any case) or `regex:pattern` (the ripgrep engine) in the folder search box.
//! Pure and blocking: the caller runs it on a worker and hands the hits on as they come.

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use std::path::Path;

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
pub fn matcher(query: &str) -> Option<Result<RegexMatcher, String>> {
    let (mode, text) = mode_of(query);
    if mode == Mode::Names || text.is_empty() {
        return None;
    }
    Some(RegexMatcherBuilder::new().case_insensitive(true).fixed_strings(mode == Mode::Text).size_limit(10 << 20).build(text).map_err(|e| e.to_string()))
}

/// Walks `root` and calls `hit(path, size, line number, line)` for the first matching line of each text file. Binary files (a NUL
/// byte), files over [`MAX_FILE`], links and (unless `hidden`) dot-named items are skipped; UTF-16 files (byte order mark) are read
/// as text; with `git_ignore` the names `.gitignore` and `.git/info/exclude` list are left out. Stops at `max_hits` or when
/// `cancelled` says so. Returns the number of hits.
pub fn search(
    root: &Path,
    re: &RegexMatcher,
    hidden: bool,
    git_ignore: bool,
    max_hits: usize,
    cancelled: &dyn Fn() -> bool,
    hit: &mut dyn FnMut(&Path, u64, usize, &str),
) -> usize {
    let mut found = 0usize;
    let walk = WalkBuilder::new(root)
        .hidden(!hidden)
        .ignore(false)
        .git_ignore(git_ignore)
        .git_exclude(git_ignore)
        .git_global(false)
        .require_git(false)
        .parents(git_ignore)
        .follow_links(false)
        .max_filesize(Some(MAX_FILE))
        .build();
    let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
    for e in walk.flatten() {
        if cancelled() || found >= max_hits {
            break;
        }
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        let mut first: Option<(usize, String)> = None;
        let _ = searcher.search_path(
            re,
            e.path(),
            Lossy(|n, line: &str| {
                let shown: String = line.trim().chars().take(MAX_LINE).collect();
                first = Some((n as usize, shown));
                Ok(false)
            }),
        );
        if let Some((n, line)) = first {
            found += 1;
            hit(e.path(), m.len(), n, &line);
        }
    }
    found
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
        use grep_matcher::Matcher;
        assert!(matcher("content:a.b").unwrap().unwrap().is_match(b"A.B").unwrap());
        assert!(!matcher("content:a.b").unwrap().unwrap().is_match(b"axb").unwrap());
        assert!(matcher("regex:(").unwrap().is_err());
    }
}
