//! Linux backend of rf_win::nameindex. Status: REAL (a simple name index: per folder, its children's names)
//!
//! An [`Index`] holds, for a root, every folder it walked and the names inside it (name + is-folder). The walk is cancellable,
//! stays on the root's device, does not follow links, leaves out the folders in [`default_excluded_names`], the user's
//! exclusion list and any folder holding a `CACHEDIR.TAG` (the folder itself is listed, its inside is not). The index saves to a
//! plain file and loads back; [`Index::refresh_dir`] lists one folder again (what inotify reported).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// Folders left out of the index (their names lowercase): caches, version-control internals, build and package stores. They hold
/// most of a home folder's entries and are rarely searched; a search started inside one walks the disk.
const SKIP_NAMES: &[&str] = &[".cache", "node_modules", ".git", "__pycache__", ".cargo", ".rustup", ".gradle", ".npm", "lost+found", "proc", "sys"];

/// Folders the user left out (Settings, Search), as given.
static USER_SKIP: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// Sets the folders the user leaves out of the index.
pub fn set_excluded(dirs: &[PathBuf]) {
    let v = dirs.iter().map(|d| PathBuf::from(d.to_string_lossy().trim_end_matches('/'))).collect();
    if let Ok(mut g) = USER_SKIP.write() {
        *g = v;
    }
}

/// The built-in exclusions, for showing them.
pub fn default_excluded_names() -> &'static [&'static str] {
    SKIP_NAMES
}

/// Whether a folder's inside stays out of the index.
pub fn skipped(dir: &Path) -> bool {
    if let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_lowercase())
        && SKIP_NAMES.contains(&name.as_str())
    {
        return true;
    }
    if USER_SKIP.read().map(|u| u.iter().any(|x| x == dir)).unwrap_or(false) {
        return true;
    }
    // a cache folder says so: https://bford.info/cachedir/
    dir.join("CACHEDIR.TAG").exists()
}

/// One child of a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub name: OsString,
    pub dir: bool,
}

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    dirs: BTreeMap<PathBuf, Vec<Child>>,
}

fn list(dir: &Path) -> Option<Vec<Child>> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut v = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        if name.as_bytes().contains(&b'\n') {
            continue;
        }
        // file_type does not follow links: a link to a folder is a plain entry
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        v.push(Child { name, dir: is_dir });
    }
    Some(v)
}

impl Index {
    /// Walks `root`; `None` when cancelled or the root cannot be read.
    pub fn build(root: &Path, cancel: &AtomicBool) -> Option<Index> {
        let dev = std::fs::metadata(root).ok()?.dev();
        let mut ix = Index { root: root.to_path_buf(), dirs: BTreeMap::new() };
        let mut todo = vec![root.to_path_buf()];
        let mut n = 0u32;
        while let Some(d) = todo.pop() {
            n = n.wrapping_add(1);
            if n % 64 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Some(kids) = list(&d) else {
                if d == root {
                    return None;
                }
                continue;
            };
            for k in kids.iter().filter(|k| k.dir) {
                let p = d.join(&k.name);
                if !skipped(&p) && std::fs::symlink_metadata(&p).map(|m| m.dev() == dev).unwrap_or(false) {
                    todo.push(p);
                }
            }
            ix.dirs.insert(d, kids);
        }
        Some(ix)
    }

    /// Lists one folder again (after a change); new sub-folders are walked, gone ones dropped.
    pub fn refresh_dir(&mut self, dir: &Path, cancel: &AtomicBool) {
        if !self.covers(dir) {
            return;
        }
        let Some(kids) = list(dir) else {
            self.drop_tree(dir);
            return;
        };
        let dev = std::fs::metadata(&self.root).map(|m| m.dev()).unwrap_or(0);
        // sub-folders that were known and are gone
        if let Some(old) = self.dirs.get(dir) {
            let gone: Vec<PathBuf> = old.iter().filter(|o| o.dir && !kids.iter().any(|k| k.dir && k.name == o.name)).map(|o| dir.join(&o.name)).collect();
            for g in gone {
                self.drop_tree(&g);
            }
        }
        let fresh: Vec<PathBuf> = kids.iter().filter(|k| k.dir).map(|k| dir.join(&k.name)).filter(|p| !self.dirs.contains_key(p)).collect();
        self.dirs.insert(dir.to_path_buf(), kids);
        for p in fresh {
            if !skipped(&p) && std::fs::symlink_metadata(&p).map(|m| m.dev() == dev).unwrap_or(false) {
                if let Some(sub) = Index::build(&p, cancel) {
                    self.dirs.extend(sub.dirs);
                }
            }
        }
    }

    fn drop_tree(&mut self, dir: &Path) {
        let keys: Vec<PathBuf> = self.dirs.range(dir.to_path_buf()..).take_while(|(k, _)| k.starts_with(dir)).map(|(k, _)| k.clone()).collect();
        for k in keys {
            self.dirs.remove(&k);
        }
    }

    /// Entries (files and folders) held.
    pub fn live(&self) -> usize {
        self.dirs.values().map(Vec::len).sum()
    }

    /// Rough bytes in memory.
    pub fn memory(&self) -> usize {
        self.dirs.iter().map(|(k, v)| k.as_os_str().len() + 48 + v.iter().map(|c| c.name.len() + 40).sum::<usize>()).sum()
    }

    /// Whether `under` is inside a folder that is left out (the index cannot answer for it).
    pub fn covers(&self, under: &Path) -> bool {
        if !under.starts_with(&self.root) {
            return false;
        }
        !under.ancestors().take_while(|a| a.starts_with(&self.root) && *a != self.root.as_path()).any(skipped)
    }

    /// Every entry under `under` (the root when `None`) whose name `ok` accepts; hidden (dot) names and what is inside hidden
    /// folders only with `include_hidden` (judged below `under`).
    pub fn matches(&self, under: Option<&Path>, include_hidden: bool, ok: &mut dyn FnMut(&str) -> bool, out: &mut dyn FnMut(PathBuf, &str) -> bool) {
        let under = under.unwrap_or(&self.root);
        for (dir, kids) in self.dirs.range(under.to_path_buf()..) {
            if !dir.starts_with(under) {
                break;
            }
            if !include_hidden
                && dir.strip_prefix(under).map(|r| r.components().any(|c| c.as_os_str().as_bytes().first() == Some(&b'.'))).unwrap_or(false)
            {
                continue;
            }
            for k in kids {
                if !include_hidden && k.name.as_bytes().first() == Some(&b'.') {
                    continue;
                }
                let name = k.name.to_string_lossy();
                if ok(&name) && !out(dir.join(&k.name), &name) {
                    return;
                }
            }
        }
    }

    /// Saves to `file` (written beside it, then renamed).
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let mut b: Vec<u8> = Vec::with_capacity(self.memory());
        b.extend_from_slice(b"leafidx 1\n");
        b.extend_from_slice(self.root.as_os_str().as_bytes());
        b.push(b'\n');
        for (d, kids) in &self.dirs {
            b.push(b'D');
            b.extend_from_slice(d.as_os_str().as_bytes());
            b.push(b'\n');
            for k in kids {
                b.push(if k.dir { b'd' } else { b'f' });
                b.extend_from_slice(k.name.as_bytes());
                b.push(b'\n');
            }
        }
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, b)?;
        std::fs::rename(&tmp, file)
    }

    /// Loads a saved index of `root`; `None` when the file is missing, of another version or of another root.
    pub fn load(file: &Path, root: &Path) -> Option<Index> {
        let data = std::fs::read(file).ok()?;
        let mut lines = data.split(|&c| c == b'\n');
        if lines.next()? != b"leafidx 1" || lines.next()? != root.as_os_str().as_bytes() {
            return None;
        }
        let mut ix = Index { root: root.to_path_buf(), dirs: BTreeMap::new() };
        let mut cur: Option<PathBuf> = None;
        let mut kids: Vec<Child> = Vec::new();
        for l in lines {
            let Some((&tag, rest)) = l.split_first() else { continue };
            match tag {
                b'D' => {
                    if let Some(c) = cur.take() {
                        ix.dirs.insert(c, std::mem::take(&mut kids));
                    }
                    cur = Some(PathBuf::from(OsString::from_vec(rest.to_vec())));
                }
                b'd' | b'f' => kids.push(Child { name: OsString::from_vec(rest.to_vec()), dir: tag == b'd' }),
                _ => return None,
            }
        }
        if let Some(c) = cur {
            ix.dirs.insert(c, kids);
        }
        Some(ix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_saves_loads_and_refreshes() {
        let root = std::env::temp_dir().join(format!("leaf-nameindex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join(".hid")).unwrap();
        for f in ["one.txt", "a/two.txt", "a/b/three.md", "node_modules/x/skip.txt", ".hid/h.txt"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        let cancel = AtomicBool::new(false);
        let mut ix = Index::build(&root, &cancel).unwrap();
        let find = |ix: &Index, hidden: bool, want: &str| {
            let mut got = Vec::new();
            ix.matches(None, hidden, &mut |n| n.contains(want), &mut |p, _| {
                got.push(p);
                true
            });
            got
        };
        assert_eq!(find(&ix, false, ".txt").len(), 2);
        assert_eq!(find(&ix, true, ".txt").len(), 3);
        // the excluded folder is listed itself, its inside is not
        assert_eq!(find(&ix, false, "node_modules").len(), 1);
        assert!(find(&ix, false, "skip").is_empty());
        let file = root.join("idx.leafidx");
        ix.save(&file).unwrap();
        let back = Index::load(&file, &root).unwrap();
        assert_eq!(back.live(), ix.live());
        assert!(Index::load(&file, Path::new("/elsewhere")).is_none());
        std::fs::write(root.join("a/new.txt"), b"x").unwrap();
        std::fs::remove_dir_all(root.join("a/b")).unwrap();
        ix.refresh_dir(&root.join("a"), &cancel);
        assert_eq!(find(&ix, false, ".txt").len(), 3);
        assert!(find(&ix, false, "three").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
