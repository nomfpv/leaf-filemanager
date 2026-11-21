//! The plan: both sides read, every path compared, a row for each one a run would touch. Nothing here writes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use leaf_vfs::{Fs, FsError, Kind, VPath, copy::MAX_DEPTH, names};

use crate::filter::{Filter, GitStack};
use crate::state::State;
use crate::{CompareBy, Error, Job, Mode, TIME_SLACK_MS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Copy (or, for a folder, make) from the left to the right.
    ToRight,
    ToLeft,
    /// Remove on the right (to the Trash / `.leaf-sync-trash`, never for good).
    DeleteRight,
    DeleteLeft,
    /// Changed on both sides: nothing is done, both stay.
    Conflict,
    Skip,
}

impl Action {
    /// What the list shows.
    pub fn text(self) -> &'static str {
        match self {
            Action::ToRight => "Copy \u{2192}",
            Action::ToLeft => "Copy \u{2190}",
            Action::DeleteRight => "Remove on the right",
            Action::DeleteLeft => "Remove on the left",
            Action::Conflict => "Conflict",
            Action::Skip => "Skip",
        }
    }
}

/// What one side has at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    pub mtime_ms: i64,
}

#[derive(Debug, Clone)]
pub struct Row {
    /// The path folded the way the job compares names (case folded when a side ignores case).
    pub key: String,
    /// The path as shown (the left side's spelling when it has one).
    pub rel: String,
    /// The path on each side, in that side's spelling (made from the other side's for what is not there yet).
    pub left_rel: String,
    pub right_rel: String,
    pub left: Option<Info>,
    pub right: Option<Info>,
    /// What the plan chose, and what the run will do (the user may change it: [`Plan::set_action`]).
    pub proposed: Action,
    pub action: Action,
    pub why: &'static str,
    /// For a removed folder taken as a whole: the files in it and their bytes.
    pub files: u64,
    pub bytes: u64,
    /// A row that can only be skipped (a link, a name clash, a folder against a file).
    pub fixed: bool,
    /// A removed folder taken as a whole (everything in it goes with it, in one move to the Trash).
    pub whole: bool,
}

impl Row {
    pub fn is_dir(&self) -> bool {
        self.left.or(self.right).is_some_and(|i| i.dir)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Files copied each way, and folders made.
    pub to_right: u64,
    pub to_left: u64,
    pub folders: u64,
    /// Files removed on each side (a folder taken as a whole counts its files).
    pub delete_right: u64,
    pub delete_left: u64,
    pub conflicts: u64,
    pub skipped: u64,
    /// Bytes copied.
    pub bytes: u64,
}

pub struct Plan {
    pub job: Job,
    pub left_root: VPath,
    pub right_root: VPath,
    pub rows: Vec<Row>,
    /// Paths that are the same on both sides (for the state of the next run): key, left, right.
    pub(crate) same: Vec<(String, Info, Info)>,
    /// Files seen on each side (inside the filter).
    pub left_files: u64,
    pub right_files: u64,
    /// Names that were left out because they cannot be used here, or folders that could not be read: "where: why".
    pub refused: Vec<String>,
    pub left_missing: bool,
    pub right_missing: bool,
    pub made_ms: i64,
    pub(crate) state: State,
    /// Every path either side has inside the filter (the state keeps only these).
    pub(crate) seen: BTreeSet<String>,
}

impl Plan {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.rows {
            let dir = r.is_dir();
            match r.action {
                Action::ToRight | Action::ToLeft if dir => c.folders += 1,
                Action::ToRight => {
                    c.to_right += 1;
                    c.bytes += r.left.map_or(0, |i| i.size);
                }
                Action::ToLeft => {
                    c.to_left += 1;
                    c.bytes += r.right.map_or(0, |i| i.size);
                }
                Action::DeleteRight => c.delete_right += if dir { r.files } else { 1 },
                Action::DeleteLeft => c.delete_left += if dir { r.files } else { 1 },
                Action::Conflict => c.conflicts += 1,
                Action::Skip => c.skipped += 1,
            }
        }
        c
    }

    /// The largest share of one side's files the run removes.
    pub fn delete_share(&self) -> f64 {
        let c = self.counts();
        let l = c.delete_left as f64 / self.left_files.max(1) as f64;
        let r = c.delete_right as f64 / self.right_files.max(1) as f64;
        l.max(r)
    }

    /// Removes so much that the app asks a second time.
    pub fn needs_second_ask(&self) -> bool {
        let c = self.counts();
        c.delete_left + c.delete_right > 0 && self.delete_share() > crate::ASK_SHARE
    }

    /// Nothing to do.
    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|r| matches!(r.action, Action::Skip))
    }

    /// What a row may be turned into: what was planned, skip, and the other direction where both are possible.
    pub fn choices(&self, i: usize) -> Vec<Action> {
        let Some(r) = self.rows.get(i) else { return Vec::new() };
        if r.fixed {
            return vec![Action::Skip];
        }
        let mut v = vec![r.proposed];
        let usable = |s: Option<Info>| s.is_some_and(|s| !s.link);
        if !r.is_dir() {
            if usable(r.left) && !v.contains(&Action::ToRight) {
                v.push(Action::ToRight);
            }
            if usable(r.right) && !v.contains(&Action::ToLeft) {
                v.push(Action::ToLeft);
            }
        }
        if !v.contains(&Action::Skip) {
            v.push(Action::Skip);
        }
        v
    }

    /// Sets what the run does with row `i`; false when that is not one of its [`choices`](Plan::choices).
    pub fn set_action(&mut self, i: usize, a: Action) -> bool {
        if !self.choices(i).contains(&a) {
            return false;
        }
        self.rows[i].action = a;
        true
    }
}

// ------------------------------------------------------------------------------------------------------------------------ scan

struct Item {
    rel: String,
    info: Info,
}

#[derive(Default)]
struct Scan {
    items: BTreeMap<String, Item>,
    /// Left out (filters, unreadable folders): with everything under them, on both sides.
    out: BTreeSet<String>,
    /// Two names on this side that are one name to the other side (case).
    dup: BTreeSet<String>,
    refused: Vec<String>,
    files: u64,
    missing: bool,
}

pub(crate) fn key_of(rel: &str, fold: bool) -> String {
    if fold { rel.to_lowercase() } else { rel.to_string() }
}

/// Does the comparison ignore case? When either side is a local folder on a system that does.
pub(crate) fn folds(l: &VPath, r: &VPath) -> bool {
    cfg!(any(windows, target_os = "macos")) && (l.scheme() == "file" || r.scheme() == "file")
}

fn is_part(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".leaf-part")
}

/// The path of `rel` (`/` between parts) under `root`, every part checked again.
pub(crate) fn join(root: &VPath, rel: &str) -> Result<VPath, &'static str> {
    let mut p = root.clone();
    for part in rel.split('/') {
        p = names::vchild(&p, part)?;
    }
    Ok(p)
}

fn scan(which: &str, fs: &dyn Fs, root: &VPath, filter: &Filter, fold: bool, cancelled: &dyn Fn() -> bool) -> Result<Scan, Error> {
    let mut s = Scan::default();
    match fs.stat(root) {
        Ok(m) if m.kind == Kind::Dir => {}
        Ok(m) if m.kind == Kind::Link => {
            return Err(Error::new(format!("The {which} folder is a link; leaf does not sync through links"), root.to_string()));
        }
        Ok(_) => return Err(Error::new(format!("The {which} side is a file, not a folder"), root.to_string())),
        Err(FsError::NotFound) => {
            s.missing = true;
            return Ok(s);
        }
        Err(FsError::Cancelled) => return Err(cancelled_error()),
        Err(e) => return Err(Error::new(format!("The {which} folder could not be read"), e.to_string())),
    }
    let mut git = GitStack::default();
    let mut stack: Vec<(VPath, String, usize)> = vec![(root.clone(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if cancelled() {
            return Err(cancelled_error());
        }
        let mut entries = Vec::new();
        if let Err(e) = fs.list(&dir, 1024, cancelled, &mut |b| entries.extend(b)) {
            if e == FsError::Cancelled {
                return Err(cancelled_error());
            }
            if rel.is_empty() {
                return Err(Error::new(format!("The {which} folder could not be read"), e.to_string()));
            }
            // never "empty": a folder that cannot be read is left out on both sides (its twin is not removed)
            s.out.insert(key_of(&rel, fold));
            s.refused.push(format!("{which}: {rel}: could not be read ({e})"));
            continue;
        }
        if filter.gitignore {
            git.leave_to(&rel);
            if let Some(e) = entries.iter().find(|e| &*e.name == ".gitignore" && !e.is_dir() && !e.link)
                && e.size <= 1 << 20
                && let Ok(p) = names::vchild(&dir, ".gitignore")
                && let Ok(mut r) = fs.open_read(&p, 0)
            {
                let mut text = String::new();
                if r.by_ref().take(1 << 20).read_to_string(&mut text).is_ok() {
                    git.push(&rel, &text);
                }
            }
        }
        for e in entries {
            let name = e.name.to_string();
            if rel.is_empty() && name == crate::TRASH_DIR || is_part(&name) {
                continue;
            }
            let child = match names::vchild(&dir, &name) {
                Ok(c) => c,
                Err(why) => {
                    s.refused.push(format!("{which}: {}{name}: {why}", if rel.is_empty() { String::new() } else { format!("{rel}/") }));
                    // the folder holding it is never taken as a whole (the name would be lost)
                    if !rel.is_empty() {
                        s.out.insert(format!("{}\u{0}", key_of(&rel, fold)));
                    }
                    continue;
                }
            };
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let link = e.link || e.is_symlink();
            let info = Info { dir: e.is_dir() && !link, link, size: if e.is_dir() { 0 } else { e.size }, mtime_ms: e.modified_ms };
            let key = key_of(&child_rel, fold);
            if filter.leaves_out(&child_rel, &name, info.dir, e.is_hidden()) || filter.gitignore && git.ignores(&child_rel, info.dir) {
                s.out.insert(key);
                continue;
            }
            if s.items.contains_key(&key) {
                s.dup.insert(key);
                continue;
            }
            if !info.dir && !info.link {
                s.files += 1;
            }
            if info.dir {
                if depth + 1 >= MAX_DEPTH {
                    return Err(Error::new(format!("The {which} folder goes deeper than {MAX_DEPTH} levels"), child_rel));
                }
                stack.push((child, child_rel.clone(), depth + 1));
            }
            s.items.insert(key, Item { rel: child_rel, info });
        }
    }
    Ok(s)
}

fn cancelled_error() -> Error {
    Error::new("Cancelled", "")
}

/// `key` itself or one of the folders above it is in `set`.
fn under(key: &str, set: &BTreeSet<String>) -> bool {
    if set.contains(key) {
        return true;
    }
    key.char_indices().filter(|(_, c)| *c == '/').any(|(i, _)| set.contains(&key[..i]))
}

/// Something in `set` lies strictly below `key` (or is the marker of a refused name in it).
fn has_below(key: &str, set: &BTreeSet<String>) -> bool {
    set.contains(&format!("{key}\u{0}")) || set.range(format!("{key}/")..format!("{key}0")).next().is_some()
}

fn meta_eq(a: &Info, size: u64, mtime: i64) -> bool {
    a.size == size && (a.mtime_ms == 0 || mtime == 0 || (a.mtime_ms - mtime).abs() <= TIME_SLACK_MS)
}

pub(crate) fn hash(fs: &dyn Fs, p: &VPath, cancelled: &dyn Fn() -> bool) -> Result<[u8; 32], FsError> {
    use sha2::{Digest, Sha256};
    let mut r = fs.open_read(p, 0)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(h.finalize().into())
}

// ------------------------------------------------------------------------------------------------------------------------ plan

/// Reads both sides of `job` and says what a run would do. `state_dir`: where the job's state is kept (leaf's data folder).
pub fn plan(job: &Job, left_fs: &dyn Fs, right_fs: &dyn Fs, state_dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<Plan, Error> {
    plan_with(job, left_fs, right_fs, state_dir, cancelled, false)
}

/// Like [`plan`], but a side that is missing or empty (the refusal with `side_gone`) is taken as meant: for one manual run, after
/// the user said so. Never for a scheduled run, which uses [`plan_scheduled`].
pub fn plan_anyway(job: &Job, left_fs: &dyn Fs, right_fs: &dyn Fs, state_dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<Plan, Error> {
    plan_with(job, left_fs, right_fs, state_dir, cancelled, true)
}

/// The plan of a scheduled run, which never asks anything: a refusal stays a refusal, and a plan that would need the second
/// confirmation (it removes a large share of a side) is an error too, so the caller leaves a notice instead of running it.
pub fn plan_scheduled(job: &Job, left_fs: &dyn Fs, right_fs: &dyn Fs, state_dir: &Path, cancelled: &dyn Fn() -> bool) -> Result<Plan, Error> {
    let p = plan_with(job, left_fs, right_fs, state_dir, cancelled, false)?;
    if p.needs_second_ask() {
        let share = (p.delete_share() * 100.0).round() as u32;
        return Err(Error::new(
            format!("The scheduled run was not started: it would remove {share}% of the files of one side. Run the job by hand to look at the preview"),
            String::new(),
        ));
    }
    Ok(p)
}

fn plan_with(job: &Job, left_fs: &dyn Fs, right_fs: &dyn Fs, state_dir: &Path, cancelled: &dyn Fn() -> bool, anyway: bool) -> Result<Plan, Error> {
    let (lroot, rroot) = job.sides()?;
    let fold = folds(&lroot, &rroot);
    let filter = Filter::of(job);
    let state = State::load(state_dir, &job.id);
    let l = scan("left", left_fs, &lroot, &filter, fold, cancelled)?;
    let r = scan("right", right_fs, &rroot, &filter, fold, cancelled)?;

    // a missing or empty side that had files at the last run: the drive is not there, the share is not mounted...
    let gone = |which: &str, s: &Scan, before: u64| -> Result<(), Error> {
        if before > 0 && (s.missing || s.files == 0) && !anyway {
            let mut e = Error::new(
                format!(
                    "The {which} folder is {} now, but the last run found {before} files there. The drive may not be connected; nothing was changed",
                    if s.missing { "not there" } else { "empty" }
                ),
                String::new(),
            );
            e.side_gone = true;
            return Err(e);
        }
        Ok(())
    };
    gone("left", &l, state.left_files)?;
    gone("right", &r, state.right_files)?;
    if l.missing && job.mode != Mode::TwoWay && !anyway {
        let mut e = Error::new("The left folder is not there. The drive may not be connected; nothing was changed", lroot.to_string());
        e.side_gone = true;
        return Err(e);
    }

    let out: BTreeSet<String> = l.out.union(&r.out).cloned().collect();
    let mut blocked: BTreeSet<String> = l.dup.union(&r.dup).cloned().collect();
    let keys: BTreeSet<&String> = l.items.keys().chain(r.items.keys()).collect();
    let seen_keys: BTreeSet<String> = keys.iter().filter(|k| !under(k, &out)).map(|k| (*k).clone()).collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut same = Vec::new();
    // each path's spelling on the left and on the right (parents come before their children)
    let mut spell: BTreeMap<String, (String, String)> = BTreeMap::new();
    let two_way = job.mode == Mode::TwoWay;

    for key in keys {
        if under(key, &out) {
            continue;
        }
        let (li, ri) = (l.items.get(key), r.items.get(key));
        let (lrel, rrel) = {
            let name_of = |rel: &str| rel.rsplit('/').next().unwrap_or(rel).to_string();
            let parent = key.rsplit_once('/').map(|(p, _)| p);
            let pre = |left: bool| -> String {
                match parent.and_then(|p| spell.get(p)) {
                    Some((pl, pr)) => format!("{}/", if left { pl } else { pr }),
                    None => parent.map_or(String::new(), |_| {
                        let other = li.or(ri).map_or(String::new(), |i| i.rel.clone());
                        other.rsplit_once('/').map_or(String::new(), |(p, _)| format!("{p}/"))
                    }),
                }
            };
            let other_name = name_of(&li.or(ri).map_or(key.clone(), |i| i.rel.clone()));
            (
                li.map_or_else(|| format!("{}{other_name}", pre(true)), |i| i.rel.clone()),
                ri.map_or_else(|| format!("{}{other_name}", pre(false)), |i| i.rel.clone()),
            )
        };
        spell.insert(key.clone(), (lrel.clone(), rrel.clone()));
        let seen = state.entries.get(key);
        let mut row = |action: Action, why: &'static str, fixed: bool| {
            let (lf, rf) = (li.map(|i| i.info), ri.map(|i| i.info));
            let size = lf.or(rf).filter(|i| !i.dir).map_or(0, |i| i.size);
            rows.push(Row {
                key: key.clone(),
                rel: li.map_or(rrel.clone(), |i| i.rel.clone()),
                left_rel: lrel.clone(),
                right_rel: rrel.clone(),
                left: lf,
                right: rf,
                proposed: action,
                action,
                why,
                files: if lf.or(rf).is_some_and(|i| i.dir) { 0 } else { 1 },
                bytes: size,
                fixed,
                whole: false,
            });
        };
        if under(key, &blocked) && !blocked.contains(key) {
            row(Action::Skip, "inside a link, or a folder that is a file on the other side", true);
            continue;
        }
        if blocked.contains(key) {
            row(Action::Skip, "two names that differ only in case", true);
            continue;
        }
        let (lx, rx) = (li.map(|i| i.info), ri.map(|i| i.info));
        if lx.is_some_and(|i| i.link) || rx.is_some_and(|i| i.link) {
            blocked.insert(key.clone());
            row(Action::Skip, "a link (never followed)", true);
            continue;
        }
        match (lx, rx) {
            (Some(a), Some(b)) if a.dir != b.dir => {
                blocked.insert(key.clone());
                row(Action::Skip, "a folder on one side, a file on the other", true);
            }
            (Some(a), Some(b)) if a.dir => same.push((key.clone(), a, b)),
            (Some(a), Some(b)) => {
                let seen_file = seen.filter(|s| !s.dir);
                let unchanged_l = seen_file.is_some_and(|s| meta_eq(&a, s.l_size, s.l_mtime));
                let unchanged_r = seen_file.is_some_and(|s| meta_eq(&b, s.r_size, s.r_mtime));
                let equal = (unchanged_l && unchanged_r)
                    || match job.compare {
                        CompareBy::Time => meta_eq(&a, b.size, b.mtime_ms),
                        CompareBy::Hash => {
                            a.size == b.size && {
                                let lp = join(&lroot, &lrel).map_err(|w| Error::new("A name cannot be used", w))?;
                                let rp = join(&rroot, &rrel).map_err(|w| Error::new("A name cannot be used", w))?;
                                let hl = hash(left_fs, &lp, cancelled);
                                let hr = hash(right_fs, &rp, cancelled);
                                match (hl, hr) {
                                    (Ok(x), Ok(y)) => x == y,
                                    (Err(FsError::Cancelled), _) | (_, Err(FsError::Cancelled)) => return Err(cancelled_error()),
                                    _ => false,
                                }
                            }
                        }
                    };
                if equal {
                    same.push((key.clone(), a, b));
                } else if !two_way {
                    row(Action::ToRight, if b.mtime_ms > a.mtime_ms + TIME_SLACK_MS { "the right one is newer; it is replaced" } else { "different" }, false);
                } else if seen_file.is_none() {
                    row(Action::Conflict, "different on both sides, and no earlier run tells which one changed", false);
                } else {
                    match (unchanged_l, unchanged_r) {
                        (false, true) => row(Action::ToRight, "changed on the left", false),
                        (true, false) => row(Action::ToLeft, "changed on the right", false),
                        _ => row(Action::Conflict, "changed on both sides", false),
                    }
                }
            }
            (Some(a), None) => {
                let seen_kind = seen.filter(|s| s.dir == a.dir);
                if !two_way || seen_kind.is_none() {
                    row(Action::ToRight, if a.dir { "new folder" } else { "only on the left" }, false);
                } else if a.dir || seen_kind.is_some_and(|s| meta_eq(&a, s.l_size, s.l_mtime)) {
                    row(Action::DeleteLeft, "removed on the right", false);
                } else {
                    row(Action::ToRight, "changed on the left, removed on the right", false);
                }
            }
            (None, Some(b)) => {
                let seen_kind = seen.filter(|s| s.dir == b.dir);
                match job.mode {
                    Mode::Mirror => row(Action::DeleteRight, "not on the left", false),
                    Mode::Backup if b.dir => {}
                    Mode::Backup => row(Action::Skip, "only on the right (a backup never removes)", false),
                    Mode::TwoWay if seen_kind.is_none() => row(Action::ToLeft, if b.dir { "new folder" } else { "only on the right" }, false),
                    Mode::TwoWay if b.dir || seen_kind.is_some_and(|s| meta_eq(&b, s.r_size, s.r_mtime)) => {
                        row(Action::DeleteRight, "removed on the left", false)
                    }
                    Mode::TwoWay => row(Action::ToLeft, "changed on the right, removed on the left", false),
                }
            }
            (None, None) => {}
        }
    }

    // a folder removed with everything in it is one row (and one move to the Trash), unless something in it stays
    let by_key: BTreeMap<String, usize> = rows.iter().enumerate().map(|(i, r)| (r.key.clone(), i)).collect();
    let mut gone_rows = vec![false; rows.len()];
    for i in 0..rows.len() {
        let r = &rows[i];
        if gone_rows[i] || !r.is_dir() || !matches!(r.proposed, Action::DeleteLeft | Action::DeleteRight) {
            continue;
        }
        let kind = r.proposed;
        if has_below(&r.key, &out) || has_below(&r.key, &blocked) {
            continue;
        }
        let kids: Vec<usize> = by_key.range(format!("{}/", r.key)..format!("{}0", r.key)).map(|(_, &j)| j).collect();
        if kids.iter().any(|&j| rows[j].proposed != kind) {
            continue;
        }
        let (mut files, mut bytes) = (0, 0);
        for &j in &kids {
            gone_rows[j] = true;
            if !rows[j].is_dir() {
                files += 1;
                bytes += rows[j].bytes;
            }
        }
        rows[i].files = files;
        rows[i].bytes = bytes;
        rows[i].whole = true;
    }
    let rows: Vec<Row> = rows.into_iter().zip(gone_rows).filter(|(_, g)| !g).map(|(r, _)| r).collect();

    let mut refused = l.refused;
    refused.extend(r.refused);
    Ok(Plan {
        job: job.clone(),
        left_root: lroot,
        right_root: rroot,
        rows,
        same,
        left_files: l.files,
        right_files: r.files,
        refused,
        left_missing: l.missing,
        right_missing: r.missing,
        made_ms: crate::now_ms(),
        state,
        seen: seen_keys,
    })
}

/// Forgets what the last run saw (the next plan is a first run): after the user said a side is empty on purpose.
pub fn forget_state(state_dir: &Path, job_id: &str) {
    let _ = std::fs::remove_file(crate::state::path_of(state_dir, job_id));
}
