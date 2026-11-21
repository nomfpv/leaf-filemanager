//! What both sides looked like after the last run of a job: per path (the folded key) the size and time on the left and on the
//! right, and how many files each side had. Kept in leaf's data folder (`<state dir>/<job id>.state`), never in the synced folders.
//!
//! The file is lines of tab-separated fields; a path cannot hold a tab or a line end (names with control characters are refused by
//! the scan), so no quoting is needed. A file that cannot be read is no state: the next run is a first run (two-way then lists
//! differing files as conflicts instead of guessing).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    pub dir: bool,
    pub l_size: u64,
    pub l_mtime: i64,
    pub r_size: u64,
    pub r_mtime: i64,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub left_files: u64,
    pub right_files: u64,
    pub entries: BTreeMap<String, Seen>,
}

pub fn path_of(dir: &Path, job_id: &str) -> PathBuf {
    let id: String = job_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(64).collect();
    dir.join(format!("{}.state", if id.is_empty() { "job" } else { &id }))
}

impl State {
    pub fn load(dir: &Path, job_id: &str) -> State {
        let Ok(text) = std::fs::read_to_string(path_of(dir, job_id)) else { return State::default() };
        let mut s = State::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["#files", l, r] => {
                    s.left_files = l.parse().unwrap_or(0);
                    s.right_files = r.parse().unwrap_or(0);
                }
                [kind, ls, lm, rs, rm, key] => {
                    let (Ok(l_size), Ok(l_mtime), Ok(r_size), Ok(r_mtime)) = (ls.parse(), lm.parse(), rs.parse(), rm.parse()) else { continue };
                    s.entries.insert(key.to_string(), Seen { dir: *kind == "d", l_size, l_mtime, r_size, r_mtime });
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self, dir: &Path, job_id: &str) -> std::io::Result<()> {
        use std::fmt::Write as _;
        let mut out = format!("#files\t{}\t{}\n", self.left_files, self.right_files);
        for (k, e) in &self.entries {
            let _ = writeln!(out, "{}\t{}\t{}\t{}\t{}\t{k}", if e.dir { "d" } else { "f" }, e.l_size, e.l_mtime, e.r_size, e.r_mtime);
        }
        std::fs::create_dir_all(dir)?;
        let path = path_of(dir, job_id);
        let tmp = path.with_extension("state.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &path)
    }

    /// Forgets `key` and everything under it.
    pub fn forget(&mut self, key: &str) {
        self.entries.remove(key);
        let lo = format!("{key}/");
        let hi = format!("{key}0");
        let under: Vec<String> = self.entries.range(lo..hi).map(|(k, _)| k.clone()).collect();
        for k in under {
            self.entries.remove(&k);
        }
    }
}
