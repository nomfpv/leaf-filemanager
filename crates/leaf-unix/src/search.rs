//! Linux backend of rf_win::search. Status: REAL (a plain walk, no index)
//!
//! Walks `root` on the calling thread (call it off the UI thread, as on Windows), matching names: a case-folded substring, or a
//! glob when the pattern holds `*` / `?` (the same matcher and folding as Windows, so "sarki" finds "Şarkı"). Hits go to `on_batch`
//! 200 at a time; `cancelled` is polled between folders and every few hundred entries. Hidden (dot) entries are skipped, and hidden
//! folders not entered, unless `include_hidden`; symbolic links are listed but never followed; folders that cannot be read count in
//! `skipped_dirs`.

use std::path::PathBuf;

use rf_core::{Entry, attrs};

// TYPE-COPY(rf-win search.rs:28)
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub root: PathBuf,
    pub pattern: String,
    pub include_hidden: bool,
    pub max_results: usize,
}

// TYPE-COPY(rf-win search.rs:37)
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub entry: Entry,
}

// TYPE-COPY(rf-win search.rs:43)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub dirs_scanned: usize,
    pub entries_scanned: usize,
    pub hits: usize,
    pub skipped_dirs: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

const BATCH: usize = 200;

/// Case-folded for matching: dotless i counts as i, the other Turkish letters as their plain ones.
fn fold(s: &str) -> Vec<char> {
    s.to_lowercase()
        .chars()
        .filter(|&c| c != '\u{307}')
        .map(|c| match c {
            'ı' | 'î' => 'i',
            'ş' => 's',
            'ğ' => 'g',
            'ü' | 'û' => 'u',
            'ö' => 'o',
            'ç' => 'c',
            'â' => 'a',
            c => c,
        })
        .collect()
}

enum Matcher {
    All,
    Substr(Vec<char>),
    Glob(Vec<char>),
}

impl Matcher {
    fn new(pattern: &str) -> Self {
        let p = fold(pattern.trim());
        if p.is_empty() {
            Matcher::All
        } else if p.iter().any(|&c| c == '*' || c == '?') {
            Matcher::Glob(p)
        } else {
            Matcher::Substr(p)
        }
    }

    fn is_match(&self, name: &str) -> bool {
        match self {
            Matcher::All => true,
            Matcher::Substr(p) => {
                let n = fold(name);
                n.len() >= p.len() && n.windows(p.len()).any(|w| w == p.as_slice())
            }
            Matcher::Glob(p) => glob_match(p, &fold(name)),
        }
    }
}

/// `*` = any run (including empty), `?` = exactly one char.
fn glob_match(p: &[char], n: &[char]) -> bool {
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (usize::MAX, 0);
    while ni < n.len() {
        if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ni;
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Searches `q.root` recursively. `on_batch` receives up to 200 hits at a time, on the calling thread; so is `cancelled` polled.
pub fn search(q: &SearchQuery, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<SearchHit>)) -> anyhow::Result<SearchStats> {
    anyhow::ensure!(q.root.is_dir(), "Folder not found: {}", q.root.display());
    let matcher = Matcher::new(&q.pattern);
    let mut st = SearchStats::default();
    let mut batch: Vec<SearchHit> = Vec::new();
    let mut todo = vec![q.root.clone()];
    let mut n = 0u32;
    'walk: while let Some(dir) = todo.pop() {
        if cancelled() {
            st.cancelled = true;
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            st.skipped_dirs += 1;
            continue;
        };
        st.dirs_scanned += 1;
        for item in rd.flatten() {
            n = n.wrapping_add(1);
            if n % 256 == 0 && cancelled() {
                st.cancelled = true;
                break 'walk;
            }
            let Some(entry) = crate::listing::from_dir_entry(&dir, &item) else { continue };
            st.entries_scanned += 1;
            if entry.attrs & attrs::HIDDEN != 0 && !q.include_hidden {
                continue;
            }
            let path = dir.join(&*entry.name);
            if entry.is_dir() && !entry.link {
                todo.push(path.clone());
            }
            if matcher.is_match(&entry.name) {
                if st.hits >= q.max_results {
                    st.truncated = true;
                    break 'walk;
                }
                st.hits += 1;
                batch.push(SearchHit { path, entry });
                if batch.len() >= BATCH {
                    on_batch(std::mem::take(&mut batch));
                }
            }
        }
    }
    if !batch.is_empty() && !st.cancelled {
        on_batch(batch);
    }
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_names_by_substring_and_glob_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("leaf-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join(".hid")).unwrap();
        for f in ["Report.TXT", "a/report2.txt", "a/b/notes.md", ".hid/report3.txt", "Şarkı.mp3"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let run = |pat: &str, hidden: bool| {
            let mut got = Vec::new();
            let q = SearchQuery { root: root.clone(), pattern: pat.into(), include_hidden: hidden, max_results: 100 };
            let st = search(&q, &|| false, &mut |b| got.extend(b.into_iter().map(|h| h.entry.name.to_string()))).unwrap();
            got.sort();
            (got, st)
        };
        assert_eq!(run("report", false).0, ["Report.TXT", "report2.txt"]);
        assert_eq!(run("report", true).0.len(), 3);
        assert_eq!(run("*.md", false).0, ["notes.md"]);
        assert_eq!(run("sarki", false).0, ["Şarkı.mp3"]);
        let q = SearchQuery { root: root.clone(), pattern: String::new(), include_hidden: false, max_results: 2 };
        assert!(search(&q, &|| false, &mut |_| {}).unwrap().truncated);
        let _ = std::fs::remove_dir_all(&root);
    }
}
