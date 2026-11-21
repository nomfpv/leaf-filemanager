//! Which names in a folder git ignores: `.gitignore` files from the repository root down to the folder, `.git/info/exclude` and the
//! global excludes file, with the usual precedence (a deeper file wins over a shallower one, `!pattern` brings a name back, and
//! everything below an ignored folder is ignored). Pure file reading; the matchers are cached per folder until one of the files
//! changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// The top of the repository `dir` is in: the nearest folder above it holding `.git` (a folder, or a file for worktrees).
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// What decides for one folder of a repository.
pub struct Matcher {
    root: PathBuf,
    /// Lowest priority first: global excludes, `info/exclude`, then the `.gitignore` of the root down to the folder.
    layers: Vec<Gitignore>,
}

type Stamp = Vec<(PathBuf, Option<SystemTime>)>;

fn files_of(root: &Path, dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![root.join(".git").join("info").join("exclude")];
    let mut chain: Vec<&Path> = dir.ancestors().take_while(|a| a.starts_with(root)).collect();
    chain.reverse();
    v.extend(chain.into_iter().map(|d| d.join(".gitignore")));
    v
}

fn stamp_of(files: &[PathBuf]) -> Stamp {
    files.iter().map(|f| (f.clone(), std::fs::metadata(f).and_then(|m| m.modified()).ok())).collect()
}

fn build(root: &Path, files: &[PathBuf]) -> Matcher {
    let mut layers = Vec::new();
    layers.push(GitignoreBuilder::new(root).build_global().0);
    for f in files {
        if !f.is_file() {
            continue;
        }
        // `info/exclude` patterns are relative to the repository root, a `.gitignore` to its own folder
        let base = if f.file_name().is_some_and(|n| n == "exclude") { root } else { f.parent().unwrap_or(root) };
        let mut b = GitignoreBuilder::new(base);
        let _ = b.add(f);
        if let Ok(g) = b.build() {
            layers.push(g);
        }
    }
    Matcher { root: root.to_path_buf(), layers }
}

static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Stamp, Arc<Matcher>)>>> = OnceLock::new();

/// The matcher for `dir`, or `None` outside a git repository. Rebuilt only when one of the ignore files changed since.
pub fn matcher_for(dir: &Path) -> Option<Arc<Matcher>> {
    let root = repo_root(dir)?;
    let files = files_of(&root, dir);
    let stamp = stamp_of(&files);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((s, m)) = cache.lock().ok()?.get(dir)
        && *s == stamp
    {
        return Some(m.clone());
    }
    let m = Arc::new(build(&root, &files));
    let mut c = cache.lock().ok()?;
    if c.len() > 64 {
        c.clear();
    }
    c.insert(dir.to_path_buf(), (stamp, m.clone()));
    Some(m)
}

impl Matcher {
    fn layered(&self, path: &Path, is_dir: bool) -> bool {
        for g in self.layers.iter().rev() {
            match g.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }

    /// Is `path` ignored? A name under an ignored folder is. `.git` itself never is (the hidden-files rule has it).
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        if rel.file_name().is_some_and(|n| n == ".git") {
            return false;
        }
        let parts: Vec<_> = rel.components().collect();
        let mut cur = self.root.clone();
        for c in parts.iter().take(parts.len().saturating_sub(1)) {
            cur.push(c);
            if self.layered(&cur, true) {
                return true;
            }
        }
        self.layered(path, is_dir)
    }
}

/// Of the `(name, is_folder)` items directly in `dir`, the names git ignores. `None` outside a repository.
pub fn ignored_among(dir: &Path, items: &[(String, bool)]) -> Option<Vec<String>> {
    let m = matcher_for(dir)?;
    Some(items.iter().filter(|(n, d)| m.is_ignored(&dir.join(n), *d)).map(|(n, _)| n.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_matching() {
        let tmp = std::env::temp_dir().join(format!("leaf-dev-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".git").join("info")).unwrap();
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(tmp.join(".git").join("info").join("exclude"), "scratch.txt\n").unwrap();
        std::fs::write(tmp.join("sub").join(".gitignore"), "!keep.log\n*.tmp\n").unwrap();
        let item = |n: &str, d: bool| (n.to_string(), d);
        let top = ignored_among(&tmp, &[item("target", true), item("a.log", false), item("scratch.txt", false), item("src", true), item(".git", true)]).unwrap();
        assert_eq!(top, vec!["target", "a.log", "scratch.txt"]);
        let sub = ignored_among(&tmp.join("sub"), &[item("keep.log", false), item("b.log", false), item("c.tmp", false)]).unwrap();
        assert_eq!(sub, vec!["b.log", "c.tmp"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
