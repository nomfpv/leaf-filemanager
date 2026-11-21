//! Context baskets: named sets of files kept for a project (a repository root, or a folder), to hand to an assistant later as one
//! block or as `@path` mentions. Plain data in `baskets.json`; the app reads and writes it around each action.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_NAME: &str = "Basket";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Basket {
    pub name: String,
    /// The project folder the basket belongs to (compared by path key).
    pub project: PathBuf,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Baskets {
    #[serde(default)]
    pub baskets: Vec<Basket>,
    /// The basket each project adds to: (project key, name).
    #[serde(default)]
    pub active: Vec<(String, String)>,
}

fn key(p: &Path) -> String {
    crate::pathkey::path_key(p)
}

/// The project of a path: its repository root, else its own folder (the path itself when it is one).
pub fn project_of(p: &Path) -> PathBuf {
    if let Some(r) = crate::copy_path::repo_root(p) {
        return r;
    }
    if p.is_dir() { p.to_path_buf() } else { p.parent().map(Path::to_path_buf).unwrap_or_else(|| p.to_path_buf()) }
}

impl Baskets {
    pub fn load(file: &Path) -> Self {
        match crate::read_config(file) {
            Ok(t) => serde_json::from_str(t.trim_start_matches('\u{feff}')).unwrap_or_else(|_| {
                crate::backup_corrupt(file);
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, file: &Path) {
        if let (Some(d), Ok(text)) = (file.parent(), serde_json::to_string_pretty(self)) {
            let _ = std::fs::create_dir_all(d);
            let tmp = file.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, file);
            }
        }
    }

    /// The names of the project's baskets.
    pub fn names(&self, project: &Path) -> Vec<String> {
        let k = key(project);
        self.baskets.iter().filter(|b| key(&b.project) == k).map(|b| b.name.clone()).collect()
    }

    /// The name the project adds to: the chosen one, else its first, else the default.
    pub fn active_name(&self, project: &Path) -> String {
        let k = key(project);
        let names = self.names(project);
        self.active.iter().find(|(p, n)| *p == k && names.contains(n)).map(|(_, n)| n.clone()).or_else(|| names.into_iter().next()).unwrap_or_else(|| DEFAULT_NAME.to_string())
    }

    pub fn set_active(&mut self, project: &Path, name: &str) {
        let k = key(project);
        self.active.retain(|(p, _)| *p != k);
        self.active.push((k, name.to_string()));
    }

    pub fn get(&self, project: &Path, name: &str) -> Option<&Basket> {
        let k = key(project);
        self.baskets.iter().find(|b| key(&b.project) == k && b.name == name)
    }

    /// Adds paths (no duplicates) to the named basket, making it when needed; returns how many were new.
    pub fn add(&mut self, project: &Path, name: &str, paths: &[PathBuf]) -> usize {
        let k = key(project);
        let ix = match self.baskets.iter().position(|b| key(&b.project) == k && b.name == name) {
            Some(i) => i,
            None => {
                self.baskets.push(Basket { name: name.to_string(), project: project.to_path_buf(), paths: Vec::new() });
                self.baskets.len() - 1
            }
        };
        let b = &mut self.baskets[ix];
        let mut added = 0;
        for p in paths {
            let pk = key(p);
            if !b.paths.iter().any(|x| key(x) == pk) {
                b.paths.push(p.clone());
                added += 1;
            }
        }
        added
    }

    /// Takes paths out of the named basket; an emptied basket stays (it keeps its name).
    pub fn remove(&mut self, project: &Path, name: &str, paths: &[PathBuf]) -> usize {
        let k = key(project);
        let Some(b) = self.baskets.iter_mut().find(|b| key(&b.project) == k && b.name == name) else { return 0 };
        let drop: Vec<String> = paths.iter().map(|p| key(p)).collect();
        let before = b.paths.len();
        b.paths.retain(|p| !drop.contains(&key(p)));
        before - b.paths.len()
    }

    pub fn delete(&mut self, project: &Path, name: &str) {
        let k = key(project);
        self.baskets.retain(|b| !(key(&b.project) == k && b.name == name));
    }

    /// Follows renames and moves (old path, new path).
    pub fn moved(&mut self, pairs: &[(PathBuf, PathBuf)]) {
        for b in &mut self.baskets {
            for p in &mut b.paths {
                for (from, to) in pairs {
                    if let Ok(rest) = p.strip_prefix(from) {
                        *p = to.join(rest);
                        break;
                    }
                }
            }
        }
    }
}

impl Basket {
    /// One `@path` mention per line, relative to the project with forward slashes.
    pub fn mentions(&self) -> String {
        let items: Vec<(String, Option<String>)> = self.paths.iter().map(|p| (p.display().to_string(), Some(self.project.display().to_string()))).collect();
        crate::copy_path::format_all(crate::copy_path::CopyPathKind::At, &items, "\n").unwrap_or_default()
    }
}
