//! Duplicate finder, the pure part: how candidates narrow down to groups of identical files, and the rules for which copies of a
//! group to tick and whether a set of ticked files may be deleted. The walk and the hashing are `leaf_common::dupes`.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};

/// A file met by the walk. `id` is the file's identity on its drive (volume, index) where the system tells it: two paths with the
/// same id are one file (hard links), not duplicates.
#[derive(Debug, Clone)]
pub struct Cand {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: i64,
    pub id: Option<(u64, u64)>,
}

/// One file of a finished group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupFile {
    pub path: PathBuf,
    pub modified_ms: i64,
}

/// Files with the same content (all `size` bytes long).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupGroup {
    pub size: u64,
    pub files: Vec<DupFile>,
}

impl DupGroup {
    /// What deleting all but one would free.
    pub fn wasted(&self) -> u64 {
        self.size * self.files.len().saturating_sub(1) as u64
    }
}

impl From<Vec<Cand>> for DupGroup {
    fn from(c: Vec<Cand>) -> Self {
        let size = c.first().map_or(0, |f| f.size);
        DupGroup { size, files: c.into_iter().map(|f| DupFile { path: f.path, modified_ms: f.modified_ms }).collect() }
    }
}

/// Step one: candidates grouped by size. Empty files, hard links of one file (counted once) and sizes seen only once drop out.
pub fn group_by_size(cands: Vec<Cand>) -> Vec<Vec<Cand>> {
    let mut by: HashMap<u64, Vec<Cand>> = HashMap::new();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    for c in cands {
        if c.size == 0 {
            continue;
        }
        if let Some(id) = c.id
            && !seen.insert(id)
        {
            continue;
        }
        by.entry(c.size).or_default().push(c);
    }
    by.into_values().filter(|g| g.len() > 1).collect()
}

/// Splits every group by `key` (a hash); files whose key is `None` (unreadable) leave, and groups left with one file go.
pub fn split_by<K: Hash + Eq>(groups: Vec<Vec<Cand>>, key: impl Fn(&Cand) -> Option<K>) -> Vec<Vec<Cand>> {
    let mut out = Vec::new();
    for g in groups {
        let mut by: HashMap<K, Vec<Cand>> = HashMap::new();
        for c in g {
            if let Some(k) = key(&c) {
                by.entry(k).or_default().push(c);
            }
        }
        out.extend(by.into_values().filter(|g| g.len() > 1));
    }
    out
}

/// Groups in the order the results page lists them: the most wasted space first (ties: bigger files, then the first path).
pub fn sort_groups(groups: &mut [DupGroup]) {
    for g in groups.iter_mut() {
        g.files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then(b.size.cmp(&a.size)).then_with(|| a.files[0].path.cmp(&b.files[0].path)));
}

/// "412 files in 97 groups": (files, groups, bytes that could be freed).
pub fn totals(groups: &[DupGroup]) -> (usize, usize, u64) {
    (groups.iter().map(|g| g.files.len()).sum(), groups.len(), groups.iter().map(DupGroup::wasted).sum())
}

/// Which copy of a group stays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    Newest,
    Oldest,
    /// The copy inside this folder (or below it); another group member is ticked only when one is found there.
    InFolder(PathBuf),
}

/// Indexes of the files of `g` to tick so that exactly one stays by `keep`. With `InFolder` and no copy in that folder nothing is
/// ticked (the group has no copy to prefer). Ties go to the first path.
pub fn ticks(g: &DupGroup, keep: &Keep) -> Vec<usize> {
    let kept = match keep {
        Keep::Newest => g.files.iter().enumerate().max_by(|a, b| a.1.modified_ms.cmp(&b.1.modified_ms).then(b.0.cmp(&a.0))).map(|f| f.0),
        Keep::Oldest => g.files.iter().enumerate().min_by(|a, b| a.1.modified_ms.cmp(&b.1.modified_ms).then(a.0.cmp(&b.0))).map(|f| f.0),
        Keep::InFolder(dir) => g.files.iter().position(|f| f.path.starts_with(dir)),
    };
    match kept {
        Some(k) => (0..g.files.len()).filter(|&i| i != k).collect(),
        None => Vec::new(),
    }
}

/// The distinct folders holding copies of `g`, for the "keep the one in" choices.
pub fn folders_of(g: &DupGroup) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for f in &g.files {
        if let Some(p) = f.path.parent()
            && !v.iter().any(|d| d == p)
        {
            v.push(p.to_path_buf());
        }
    }
    v
}

/// Outcome of [`plan_delete`].
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Go ahead with `delete`; `skipped` changed since they were hashed and stay.
    Go { delete: Vec<PathBuf>, skipped: Vec<PathBuf> },
    /// Every file of this group (index) is ticked: nothing is deleted.
    LastCopy(usize),
}

/// What may go: refuses when every file of some group is ticked, else re-checks each ticked file with `stat` (size, modified ms; `None`
/// = gone) against what it was when hashed, and leaves out what changed.
pub fn plan_delete(groups: &[DupGroup], ticked: &HashSet<PathBuf>, stat: impl Fn(&Path) -> Option<(u64, i64)>) -> Plan {
    for (i, g) in groups.iter().enumerate() {
        if !g.files.is_empty() && g.files.iter().all(|f| ticked.contains(&f.path)) {
            return Plan::LastCopy(i);
        }
    }
    let (mut delete, mut skipped) = (Vec::new(), Vec::new());
    for g in groups {
        for f in g.files.iter().filter(|f| ticked.contains(&f.path)) {
            if stat(&f.path) == Some((g.size, f.modified_ms)) {
                delete.push(f.path.clone());
            } else {
                skipped.push(f.path.clone());
            }
        }
    }
    Plan::Go { delete, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(p: &str, size: u64, id: Option<(u64, u64)>) -> Cand {
        Cand { path: PathBuf::from(p), size, modified_ms: 0, id }
    }

    #[test]
    fn grouping() {
        let cands = vec![
            cand("/d/a", 10, None),
            cand("/d/b", 10, None),
            cand("/d/c", 20, None),
            cand("/d/empty1", 0, None),
            cand("/d/empty2", 0, None),
            cand("/d/h1", 30, Some((1, 7))),
            cand("/d/h2", 30, Some((1, 7))),
            cand("/d/x", 40, Some((1, 8))),
            cand("/d/y", 40, Some((1, 9))),
        ];
        let mut g: Vec<Vec<String>> =
            group_by_size(cands).into_iter().map(|g| { let mut v: Vec<String> = g.iter().map(|c| c.path.display().to_string()).collect(); v.sort(); v }).collect();
        g.sort();
        // 20 alone, empties and the hard-linked pair (one file) drop out
        assert_eq!(g, vec![vec!["/d/a", "/d/b"], vec!["/d/x", "/d/y"]]);
        let by_name = split_by(group_by_size(vec![cand("/d/a", 5, None), cand("/d/b", 5, None), cand("/d/c", 5, None)]), |c| {
            (c.path.to_str() != Some("/d/c")).then_some(1)
        });
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].len(), 2);
    }

    #[test]
    fn delete_guards() {
        // throwaway files: a/b are a group, c/d another
        let dir = std::env::temp_dir().join(format!("leaf-dupes-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |n: &str| {
            let p = dir.join(n);
            std::fs::write(&p, b"12345").unwrap();
            let m = std::fs::metadata(&p).unwrap();
            let ms = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
            DupFile { path: p, modified_ms: ms }
        };
        let groups = vec![
            DupGroup { size: 5, files: vec![mk("a"), mk("b")] },
            DupGroup { size: 5, files: vec![mk("c"), mk("d")] },
        ];
        let stat = |p: &Path| {
            let m = std::fs::metadata(p).ok()?;
            Some((m.len(), m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64))
        };
        // c changes after it was hashed: it is skipped, a goes
        std::fs::write(dir.join("c"), b"changed!").unwrap();
        let ticked: HashSet<PathBuf> = [dir.join("a"), dir.join("c")].into_iter().collect();
        assert_eq!(plan_delete(&groups, &ticked, stat), Plan::Go { delete: vec![dir.join("a")], skipped: vec![dir.join("c")] });
        // every file of a group ticked: refused
        let all: HashSet<PathBuf> = [dir.join("c"), dir.join("d")].into_iter().collect();
        assert_eq!(plan_delete(&groups, &all, stat), Plan::LastCopy(1));
        assert_eq!(ticks(&groups[0], &Keep::Newest).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
