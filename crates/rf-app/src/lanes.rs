//! Lanes (PLAN F1, D6): the lane records and what the rest of the app asks about them. A lane is a git worktree on a branch of
//! its own, made by "New lane" for one piece of work; its record (name, repository, worktree folder, branch, base, colour) is
//! kept in `lanes.json` in leaf's data folder. The git side is `leaf_dev::lanes`; the dialogs and the sidebar section are
//! `workspace/lanes.rs`.
//!
//! Behind the agent layer's master switch (`Settings::ai_layer`): off, no lane is shown, no colour is drawn and nothing is read.
//! The progress of each lane (ahead/behind, changed files) is read off the UI thread at most every 15 s while the sidebar shows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::*;
use leaf_agents::folder_key;
use leaf_dev::lanes::Progress;
use serde::{Deserialize, Serialize};

const STALE: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub name: String,
    /// The repository's main worktree.
    pub repo: PathBuf,
    /// The lane's worktree folder.
    pub path: PathBuf,
    pub branch: String,
    /// The branch (or commit) it was made from, and is merged into.
    pub base: String,
    /// Index into [`color`].
    #[serde(default)]
    pub color: u8,
    #[serde(default)]
    pub created_ms: i64,
}

#[derive(Default)]
struct LanesState {
    lanes: Vec<Lane>,
    /// Worktree folder -> its progress (`None`: the folder is gone or git did not answer).
    progress: HashMap<PathBuf, Option<Progress>>,
    at: Option<Instant>,
    pending: bool,
}

impl Global for LanesState {}

fn file() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("lanes.json"))
}

pub fn init(cx: &mut App) {
    let lanes = file()
        .and_then(|f| match rf_core::read_config(&f) {
            Ok(text) => serde_json::from_str::<Vec<Lane>>(&text).map_err(|e| log::warn!("lanes: {} cannot be read, ignored: {e}", f.display())).ok(),
            Err(_) => None,
        })
        .unwrap_or_default()
        .into_iter()
        // a branch or base that git could read as an option (`--output=...`) or that is no ref name at all: the record is left out
        .filter(|l: &Lane| {
            let ok = leaf_dev::lanes::plausible_ref(&l.branch) && leaf_dev::lanes::plausible_ref(&l.base) && l.path.is_absolute() && l.repo.is_absolute();
            if !ok {
                log::warn!("lanes: the record {:?} is not valid, left out", l.name);
            }
            ok
        })
        .collect();
    cx.set_global(LanesState { lanes, ..Default::default() });
}

fn save(cx: &App) {
    let Some(f) = file() else { return };
    let text = match serde_json::to_string_pretty(&cx.global::<LanesState>().lanes) {
        Ok(t) => t,
        Err(_) => return,
    };
    // (a small file, written in place of the old one at once: two saves in a row never race)
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = f.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &f)) {
        log::warn!("lanes: {} not saved: {e}", f.display());
    }
}

/// The lane layer is on (the agent layer's master switch).
pub fn on(cx: &App) -> bool {
    cx.try_global::<crate::settings::Settings>().is_some_and(|s| s.ai_layer)
}

/// Every lane, oldest first; empty while the layer is off.
pub fn list(cx: &App) -> Vec<Lane> {
    if !on(cx) {
        return Vec::new();
    }
    cx.try_global::<LanesState>().map(|s| s.lanes.clone()).unwrap_or_default()
}

pub fn add(lane: Lane, cx: &mut App) {
    cx.update_global::<LanesState, _>(|s, _| {
        s.lanes.retain(|l| folder_key(&l.path) != folder_key(&lane.path));
        s.lanes.push(lane);
        s.at = None;
    });
    save(cx);
    cx.refresh_windows();
}

/// Forgets the lane whose worktree is `path` (after it was removed or discarded).
pub fn forget(path: &Path, cx: &mut App) {
    let key = folder_key(path);
    cx.update_global::<LanesState, _>(|s, _| {
        s.lanes.retain(|l| folder_key(&l.path) != key);
        s.progress.retain(|p, _| folder_key(p) != key);
    });
    save(cx);
    cx.refresh_windows();
}

/// The colour for a new lane: the first one no lane wears, else the next in turn.
pub fn next_color(cx: &App) -> u8 {
    let used: Vec<u8> = cx.try_global::<LanesState>().map(|s| s.lanes.iter().map(|l| l.color % COLORS).collect()).unwrap_or_default();
    (0..COLORS).find(|c| !used.contains(c)).unwrap_or((used.len() as u8) % COLORS)
}

const COLORS: u8 = 6;

/// A lane's colour (D6), from the theme's own colours only.
pub fn color(ix: u8, p: &crate::theme::Palette) -> Hsla {
    match ix % COLORS {
        0 => p.accent,
        1 => p.success,
        2 => p.warning,
        3 => p.compare[0],
        4 => p.compare[1],
        _ => p.compare[3],
    }
}

/// The lane `dir` is in (its worktree folder or a folder inside it).
pub fn of_dir(cx: &App, dir: &Path) -> Option<Lane> {
    if !on(cx) {
        return None;
    }
    let s = cx.try_global::<LanesState>()?;
    if s.lanes.is_empty() {
        return None;
    }
    let key = folder_key(dir);
    s.lanes
        .iter()
        .filter(|l| {
            let k = folder_key(&l.path);
            key == k || key.strip_prefix(k.as_str()).is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|l| l.path.as_os_str().len())
        .cloned()
}

/// The colour of the lane `dir` is in, for its tab and badges.
pub fn color_of_dir(cx: &App, dir: &Path, p: &crate::theme::Palette) -> Option<Hsla> {
    of_dir(cx, dir).map(|l| color(l.color, p))
}

/// The last progress read for the lane at `path`: outer `None` before the first read.
pub fn progress(cx: &App, path: &Path) -> Option<Option<Progress>> {
    cx.try_global::<LanesState>()?.progress.get(path).copied()
}

/// Reads every lane's progress again when the last read is older than 15 s (off the UI thread).
pub fn poll(cx: &mut App) {
    // (minimised or in the tray: the sidebar is not seen, so no git; the next draw after the window is back reads again)
    if !on(cx) || cx.try_global::<LanesState>().is_none() || leaf_sys::window::app_hidden() {
        return;
    }
    let s = cx.global::<LanesState>();
    if s.pending || s.lanes.is_empty() || s.at.is_some_and(|t| t.elapsed() < STALE) {
        return;
    }
    let lanes = s.lanes.clone();
    cx.update_global::<LanesState, _>(|s, _| s.pending = true);
    cx.spawn(async move |cx| {
        let got = cx
            .background_executor()
            .spawn(async move { lanes.into_iter().map(|l| (l.path.clone(), leaf_dev::lanes::progress(&l.path, &l.base))).collect::<HashMap<_, _>>() })
            .await;
        let _ = cx.update(|cx| {
            let changed = cx.update_global::<LanesState, _>(|s, _| {
                s.pending = false;
                s.at = Some(Instant::now());
                let changed = s.progress != got;
                s.progress = got;
                changed
            });
            if changed {
                cx.refresh_windows();
            }
        });
    })
    .detach();
}

/// Reads the progress again at the next draw (after a commit, a merge, a new lane).
pub fn refresh(cx: &mut App) {
    if cx.try_global::<LanesState>().is_some() {
        cx.update_global::<LanesState, _>(|s, _| s.at = None);
        cx.refresh_windows();
    }
}

/// Why `name` cannot be a lane's name, `None` when it can. Only letters, digits and `. _ / -`: the name is the branch and the
/// folder, and the folder goes on a terminal's command line (Windows Terminal splits it at `;` even inside quotes). No device
/// name of Windows (`CON`, `NUL`, `COM1`, also with an extension) and no `.` at an end.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.starts_with(['-', '.', '/']) || name.ends_with(['.', '/']) {
        return Some(crate::t!("A lane name starts and ends with a letter or a digit"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')) {
        return Some(crate::t!("A lane name has only letters, digits and . _ / -"));
    }
    let device = |part: &str| {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit())
    };
    if device(&folder_name(name)) || name.split('/').any(device) {
        return Some(crate::t!("This name is reserved by Windows for a device"));
    }
    None
}

/// The lane's name made safe as one folder name (`/` and the characters Windows refuses become `-`).
pub fn folder_name(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect()
}

/// The folder of a new lane from the pattern in Settings: `<repo>` the repository's folder, `<name>` the lane's name; a relative
/// result is taken from the repository's parent folder.
pub fn folder_for(pattern: &str, repo: &Path, name: &str) -> PathBuf {
    let pattern = if pattern.trim().is_empty() || !pattern.contains("<name>") { "<repo>-wt/<name>" } else { pattern.trim() };
    let repo_s = repo.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    let text = pattern.replace("<repo>", &repo_s).replace("<name>", &folder_name(name));
    let text = if cfg!(windows) { text.replace('/', "\\") } else { text };
    let p = PathBuf::from(text);
    if p.is_absolute() { p } else { repo.parent().unwrap_or(repo).join(p) }
}

