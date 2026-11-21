//! Per-folder layout memory (Settings → Layout → "Sync layout across folders" off): each
//! folder remembers its view, sort and grouping, like Files. Stored in
//! `%APPDATA%\leaf\layouts.json`, written in the background after a change.

use std::collections::HashMap;
use std::path::Path;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// In memory a layout holds any sort key. On disk (`FolderLayoutFile`) the keys added later (tag,
/// path, sync) are written as `sort = "Name"` plus `sort_x = "tag"` etc., so that an OLDER build,
/// which has no such variants, still parses the whole file and sorts that folder by name; a
/// layout file written before `sort_x` existed has none and reads as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "FolderLayoutFile", into = "FolderLayoutFile")]
pub struct FolderLayout {
    /// 0 details, 1 grid, 2 columns
    pub view: u8,
    pub sort: rf_core::SortKey,
    pub desc: bool,
    pub dirs_first: bool,
    #[serde(default)]
    pub files_first: bool,
    /// "Size" slider: Details row height / grid tile size (indices).
    #[serde(default = "default_row")]
    pub row_size: u8,
    #[serde(default = "default_grid")]
    pub grid_size: u8,
    /// 0 none, 1 name, 2 type, 3 modified, 4 size
    pub group: u8,
}

/// The shape in the file. `sort` is always a key every build knows; `sort_x` names a newer one.
#[derive(Serialize, Deserialize)]
struct FolderLayoutFile {
    view: u8,
    sort: rf_core::SortKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort_x: Option<String>,
    desc: bool,
    dirs_first: bool,
    #[serde(default)]
    files_first: bool,
    #[serde(default = "default_row")]
    row_size: u8,
    #[serde(default = "default_grid")]
    grid_size: u8,
    group: u8,
}

impl From<FolderLayout> for FolderLayoutFile {
    fn from(l: FolderLayout) -> Self {
        let (sort, sort_x) = l.sort.stored();
        FolderLayoutFile {
            view: l.view,
            sort,
            sort_x: sort_x.map(str::to_string),
            desc: l.desc,
            dirs_first: l.dirs_first,
            files_first: l.files_first,
            row_size: l.row_size,
            grid_size: l.grid_size,
            group: l.group,
        }
    }
}

impl From<FolderLayoutFile> for FolderLayout {
    fn from(f: FolderLayoutFile) -> Self {
        FolderLayout {
            view: f.view,
            sort: rf_core::SortKey::from_stored(f.sort, f.sort_x.as_deref()),
            desc: f.desc,
            dirs_first: f.dirs_first,
            files_first: f.files_first,
            row_size: f.row_size,
            grid_size: f.grid_size,
            group: f.group,
        }
    }
}

fn default_row() -> u8 {
    1
}
fn default_grid() -> u8 {
    2
}

/// A layout as stored: the layout itself plus when the folder was last used (ms since the epoch;
/// files from before the stamp existed read 0, i.e. oldest).
#[derive(Serialize, Deserialize, Clone, Copy)]
struct Stored {
    #[serde(flatten)]
    layout: FolderLayout,
    #[serde(default, skip_serializing_if = "is_zero")]
    used: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Default)]
struct Layouts {
    map: HashMap<String, Stored>,
    loaded: bool,
    dirty_gen: u64,
}
impl Global for Layouts {}

const MAX: usize = 4000;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn file() -> Option<std::path::PathBuf> {
    crate::settings::app_dir().map(|d| d.join("layouts.json"))
}

/// The map key of a folder: case-insensitive on Windows, case-sensitive elsewhere; no trailing separator.
fn key_str(p: &str) -> String {
    #[cfg(windows)]
    {
        p.trim_end_matches(['\\', '/']).to_lowercase()
    }
    #[cfg(not(windows))]
    {
        let t = p.trim_end_matches('/');
        if t.is_empty() && !p.is_empty() { "/".to_string() } else { t.to_string() }
    }
}

fn key(p: &Path) -> String {
    key_str(&p.to_string_lossy())
}

/// Drop the least recently used folders until `keep` remain.
fn evict(map: &mut HashMap<String, Stored>, keep: usize) {
    if map.len() <= keep {
        return;
    }
    let mut by_age: Vec<(u64, String)> = map.iter().map(|(k, v)| (v.used, k.clone())).collect();
    by_age.sort();
    for (_, k) in by_age.into_iter().take(map.len() - keep) {
        map.remove(&k);
    }
}

/// Re-key the layouts of renamed/moved folders (and of every folder below them).
fn rekey(map: &mut HashMap<String, Stored>, pairs: &[(String, String)]) {
    if map.is_empty() {
        return;
    }
    for (from, to) in pairs {
        let (from, to) = (key_str(from), key_str(to));
        if from == to {
            continue;
        }
        let below = |k: &str| k.len() > from.len() && k.starts_with(&from) && matches!(k.as_bytes()[from.len()], b'\\' | b'/');
        let moving: Vec<String> = map.keys().filter(|k| **k == from || below(k)).cloned().collect();
        let moved: Vec<(String, Stored)> =
            moving.into_iter().filter_map(|k| map.remove(&k).map(|v| (format!("{to}{}", &k[from.len()..]), v))).collect();
        map.extend(moved);
    }
}

fn store(cx: &mut App) -> &mut Layouts {
    if !cx.has_global::<Layouts>() {
        cx.set_global(Layouts::default());
    }
    let l = cx.global_mut::<Layouts>();
    if !l.loaded {
        l.loaded = true;
        if let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) {
            l.map = serde_json::from_str(&text).unwrap_or_default();
        }
    }
    l
}

pub fn get(path: &Path, cx: &mut App) -> Option<FolderLayout> {
    let k = key(path);
    let e = store(cx).map.get_mut(&k)?;
    e.used = now_ms(); // in memory; reaches the disk with the next write
    Some(e.layout)
}

/// Folders renamed or moved inside leaf take their saved layout along.
pub fn follow(cx: &mut App, pairs: &[(std::path::PathBuf, std::path::PathBuf)]) {
    let s = store(cx);
    if pairs.is_empty() || s.map.is_empty() {
        return;
    }
    let pairs: Vec<(String, String)> =
        pairs.iter().map(|(a, b)| (a.to_string_lossy().into_owned(), b.to_string_lossy().into_owned())).collect();
    rekey(&mut s.map, &pairs);
    save_soon(cx);
}

pub fn set(path: &Path, layout: FolderLayout, cx: &mut App) {
    let s = store(cx);
    let k = key(path);
    if s.map.get(&k).map(|e| e.layout) == Some(layout) {
        return;
    }
    if s.map.len() >= MAX && !s.map.contains_key(&k) {
        // forget the least recently used tenth; layouts are a convenience, not data
        evict(&mut s.map, MAX - MAX / 10);
    }
    s.map.insert(k, Stored { layout, used: now_ms() });
    save_soon(cx);
}

fn save_soon(cx: &mut App) {
    let s = store(cx);
    s.dirty_gen += 1;
    let gen_ = s.dirty_gen;
    // coalesce bursts (sort clicks) into one write
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(800)).await;
        let _ = cx.update(|cx| {
            let s = store(cx);
            if s.dirty_gen != gen_ {
                return;
            }
            let Ok(text) = serde_json::to_string(&s.map) else { return };
            if let Some(f) = file() {
                std::thread::spawn(move || {
                    if let Some(d) = f.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let tmp = f.with_extension("json.tmp");
                    if std::fs::write(&tmp, text).is_ok() {
                        let _ = std::fs::rename(&tmp, &f);
                    }
                });
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rf_core::SortKey;
    use serde::Deserialize;

    // (not `use super::*`: that brings gpui's `test` attribute macro into scope and #[test] then loops)
    use super::{FolderLayout, Stored, default_grid, default_row, evict, key_str, rekey};

    fn st(used: u64) -> Stored {
        Stored { layout: layout(SortKey::Size), used }
    }

    #[test]
    fn the_least_recently_used_folders_are_dropped_first() {
        let mut m: HashMap<String, Stored> = HashMap::new();
        for (k, u) in [("a", 50), ("b", 10), ("c", 0), ("d", 99), ("e", 30)] {
            m.insert(k.to_string(), st(u));
        }
        evict(&mut m, 3);
        let mut left: Vec<_> = m.keys().cloned().collect();
        left.sort();
        assert_eq!(left, ["a", "d", "e"]);
    }

    #[test]
    fn a_rename_takes_the_layout_of_the_folder_and_those_below_it() {
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let p = |s: &str| key_str(&s.replace('/', &sep.to_string()));
        let mut m: HashMap<String, Stored> = HashMap::new();
        for (k, u) in [("/x/old", 1), ("/x/old/sub", 2), ("/x/older", 3), ("/y", 4)] {
            m.insert(p(k), st(u));
        }
        rekey(&mut m, &[(p("/x/old"), p("/x/new"))]);
        assert_eq!(m[&p("/x/new")].used, 1);
        assert_eq!(m[&p("/x/new/sub")].used, 2);
        assert_eq!(m[&p("/x/older")].used, 3, "a folder that only shares the name prefix stays");
        assert!(!m.contains_key(&p("/x/old")) && !m.contains_key(&p("/x/old/sub")));
        assert_eq!(m.len(), 4);
    }

    fn layout(sort: SortKey) -> FolderLayout {
        FolderLayout { view: 0, sort, desc: true, dirs_first: true, files_first: false, row_size: 1, grid_size: 2, group: 0 }
    }

    /// What an older build's layout looked like: five sort keys, no `sort_x`, unknown fields ignored.
    #[derive(Deserialize, Debug, PartialEq)]
    enum OldKey {
        Name,
        Modified,
        Created,
        Size,
        Type,
    }
    #[derive(Deserialize, Debug)]
    #[allow(dead_code)]
    struct OldLayout {
        view: u8,
        sort: OldKey,
        desc: bool,
        dirs_first: bool,
        #[serde(default)]
        files_first: bool,
        #[serde(default = "default_row")]
        row_size: u8,
        #[serde(default = "default_grid")]
        grid_size: u8,
        group: u8,
    }

    #[test]
    fn a_layout_file_written_before_the_new_keys_still_loads() {
        let old = r#"{"c:\\a":{"view":0,"sort":"Size","desc":true,"dirs_first":true,"group":3},
                      "c:\\b":{"view":1,"sort":"Type","desc":false,"dirs_first":false,"files_first":true,"row_size":2,"grid_size":3,"group":0}}"#;
        let map: HashMap<String, FolderLayout> = serde_json::from_str(old).expect("old file parses");
        assert_eq!(map["c:\\a"].sort, SortKey::Size);
        assert_eq!(map["c:\\a"].row_size, 1, "fields missing in the old file take their defaults");
        assert!(map["c:\\b"].files_first);
        assert_eq!(map["c:\\b"].sort, SortKey::Type);
    }

    #[test]
    fn a_new_key_survives_a_round_trip() {
        for k in [SortKey::Tag, SortKey::Path, SortKey::Sync, SortKey::Modified] {
            let text = serde_json::to_string(&layout(k)).unwrap();
            let back: FolderLayout = serde_json::from_str(&text).unwrap();
            assert_eq!(back, layout(k), "{text}");
        }
    }

    #[test]
    fn a_new_key_is_written_as_name_so_that_an_older_build_reads_the_file() {
        let mut map = HashMap::new();
        map.insert("c:\\t".to_string(), layout(SortKey::Tag));
        map.insert("c:\\s".to_string(), layout(SortKey::Size));
        let text = serde_json::to_string(&map).unwrap();
        assert!(text.contains(r#""sort_x":"tag""#), "{text}");
        // an older build: every folder's layout reads, the new key's folder sorts by name
        let old: HashMap<String, OldLayout> = serde_json::from_str(&text).expect("an older build parses the file");
        assert_eq!(old["c:\\t"].sort, OldKey::Name);
        assert!(old["c:\\t"].desc);
        assert_eq!(old["c:\\s"].sort, OldKey::Size);
        // and the file of an older key carries no new field at all
        assert!(!serde_json::to_string(&layout(SortKey::Size)).unwrap().contains("sort_x"));
    }

    #[test]
    fn a_key_name_from_a_later_build_leaves_the_key_an_older_build_would_read() {
        let text = r#"{"view":0,"sort":"Modified","sort_x":"owner","desc":false,"dirs_first":true,"group":0}"#;
        let l: FolderLayout = serde_json::from_str(text).unwrap();
        assert_eq!(l.sort, SortKey::Modified);
    }
}
