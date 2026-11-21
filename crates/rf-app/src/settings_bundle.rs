//! Settings export and import (B5): one TOML file holding settings, your actions, hosts, themes (and the tags when asked for).
//! A host entry never holds a password (see `rf_core::hosts`) and the settings hold no secret, so nothing is filtered out beyond
//! what is not on the list of names below: anything else in an imported file is ignored. Importing first shows what changes and
//! keeps the old files in `<config>/backup/settings-<time>/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FORMAT: &str = "leaf-settings-bundle-1";
const TAGS: &str = "tags.json";
/// Entries of the settings file that belong to this PC and are kept on import.
const LOCAL_KEYS: [&str; 3] = ["window", "session_tabs", "session_active"];

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    New,
    Changed,
    Same,
}

/// Only these names are read from or written to the config folder.
fn allowed(name: &str, with_tags: bool) -> bool {
    match name {
        "settings.toml" | "actions.toml" | "hosts.toml" => true,
        TAGS => with_tags,
        n => n.strip_prefix("themes/").is_some_and(|f| f.ends_with(".toml") && !f.contains(['/', '\\']) && !f.starts_with('.')),
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok().map(|t| t.trim_start_matches('\u{feff}').to_string())
}

/// Collects the files of the config folder; `settings` is the live settings text (the file may lag behind).
pub fn build(dir: &Path, settings: String, with_tags: bool) -> Bundle {
    let mut b = Bundle { format: FORMAT.into(), files: BTreeMap::new() };
    b.files.insert("settings.toml".into(), settings);
    for n in ["actions.toml", "hosts.toml"] {
        if let Some(t) = read(dir, n) {
            b.files.insert(n.into(), t);
        }
    }
    if with_tags {
        if let Some(t) = read(dir, TAGS) {
            b.files.insert(TAGS.into(), t);
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("themes")) {
        for e in rd.flatten() {
            let n = format!("themes/{}", e.file_name().to_string_lossy());
            if allowed(&n, false) {
                if let Some(t) = read(dir, &n) {
                    b.files.insert(n, t);
                }
            }
        }
    }
    b
}

pub fn to_text(b: &Bundle) -> Result<String, String> {
    toml::to_string_pretty(b).map_err(|e| e.to_string())
}

pub fn parse(text: &str) -> Result<Bundle, String> {
    let mut b: Bundle = toml::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    if b.format != FORMAT {
        return Err("not a leaf settings bundle".into());
    }
    b.files.retain(|n, _| allowed(n, true));
    Ok(b)
}

/// The settings text as it will be written: the incoming one with this PC's window and session entries kept.
fn merged_settings(dir: &Path, incoming: &str) -> String {
    let Ok(mut new) = incoming.parse::<toml::Table>() else { return incoming.to_string() };
    let old = read(dir, "settings.toml").and_then(|t| t.parse::<toml::Table>().ok()).unwrap_or_default();
    for k in LOCAL_KEYS {
        match old.get(k) {
            Some(v) => {
                new.insert(k.into(), v.clone());
            }
            None => {
                new.remove(k);
            }
        }
    }
    toml::to_string_pretty(&new).unwrap_or_else(|_| incoming.to_string())
}

fn final_text(dir: &Path, name: &str, text: &str) -> String {
    if name == "settings.toml" { merged_settings(dir, text) } else { text.to_string() }
}

/// What importing would do to each file.
pub fn plan(dir: &Path, b: &Bundle, with_tags: bool) -> Vec<(String, Change)> {
    b.files
        .iter()
        .filter(|(n, _)| with_tags || n.as_str() != TAGS)
        .map(|(n, t)| {
            let c = match read(dir, n) {
                None => Change::New,
                Some(old) if old.trim() == final_text(dir, n, t).trim() => Change::Same,
                Some(_) => Change::Changed,
            };
            (n.clone(), c)
        })
        .collect()
}

/// Writes the changed files after copying their old versions to a backup folder, which is returned.
pub fn apply(dir: &Path, b: &Bundle, with_tags: bool, stamp: u128) -> std::io::Result<PathBuf> {
    let backup = dir.join("backup").join(format!("settings-{stamp}"));
    let todo: Vec<_> = plan(dir, b, with_tags).into_iter().filter(|(_, c)| *c != Change::Same).collect();
    for (n, _) in &todo {
        let from = dir.join(n);
        if from.is_file() {
            let to = backup.join(n);
            std::fs::create_dir_all(to.parent().unwrap_or(&backup))?;
            std::fs::copy(&from, &to)?;
        }
    }
    for (n, _) in &todo {
        let to = dir.join(n);
        std::fs::create_dir_all(to.parent().unwrap_or(dir))?;
        let tmp = to.with_extension("import.tmp");
        std::fs::write(&tmp, final_text(dir, n, &b.files[n]))?;
        std::fs::rename(&tmp, &to)?;
    }
    Ok(backup)
}

pub fn summary(p: &[(String, Change)]) -> String {
    p.iter()
        .map(|(n, c)| {
            let w = match c {
                Change::New => crate::t!("new"),
                Change::Changed => crate::t!("changes"),
                Change::Same => crate::t!("same"),
            };
            format!("{n}: {w}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_keeps_a_backup_of_what_it_overwrites() {
        let dir = std::env::temp_dir().join(format!("leaf-bundle-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.toml"), "zoom = 1\nsession_active = 3\n").unwrap();
        std::fs::write(dir.join("hosts.toml"), "old hosts").unwrap();
        let mut b = Bundle { format: FORMAT.into(), files: BTreeMap::new() };
        b.files.insert("settings.toml".into(), "zoom = 2\nsession_active = 9\n".into());
        b.files.insert("hosts.toml".into(), "old hosts".into());
        b.files.insert("evil/x.toml".into(), "x".into());
        let b = parse(&to_text(&b).unwrap()).unwrap();
        assert!(!b.files.contains_key("evil/x.toml"));
        let p = plan(&dir, &b, false);
        assert_eq!(p, vec![("hosts.toml".to_string(), Change::Same), ("settings.toml".to_string(), Change::Changed)]);
        let backup = apply(&dir, &b, false, 1).unwrap();
        assert_eq!(std::fs::read_to_string(backup.join("settings.toml")).unwrap(), "zoom = 1\nsession_active = 3\n");
        let now: toml::Table = std::fs::read_to_string(dir.join("settings.toml")).unwrap().parse().unwrap();
        assert_eq!(now["zoom"].as_integer(), Some(2));
        assert_eq!(now["session_active"].as_integer(), Some(3), "this PC's session stays");
        assert!(!backup.join("hosts.toml").exists(), "unchanged files are not copied");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
