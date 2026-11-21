//! The run: the rows of an accepted plan, in a safe order (folders made, files copied, then removals, empty folders last), each
//! checked again just before it is done. Nothing is deleted for good: see the crate docs.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use leaf_vfs::{Fs, FsError, Kind, LocalFs, MetaChange, VPath, WriteOpts, copy::part_name, names};

use crate::plan::{Action, Info, Plan, Row, hash};
use crate::state::Seen;
use crate::{Error, TIME_SLACK_MS};

/// The folder inside a server side's root that removed and overwritten files are moved into, one folder per run.
pub const TRASH_DIR: &str = ".leaf-sync-trash";

/// How the caller follows and steers a run.
pub struct Control<'a> {
    pub cancelled: &'a dyn Fn() -> bool,
    /// The run waits while this is true.
    pub paused: &'a dyn Fn() -> bool,
    pub progress: &'a mut dyn FnMut(&Progress),
    /// Moves local files to the Trash (or the Recycle Bin); blocks. Never a permanent delete.
    pub trash_local: &'a dyn Fn(&[PathBuf]) -> Result<(), String>,
}

#[derive(Debug, Clone)]
pub struct Progress<'a> {
    pub rows_done: u64,
    pub rows_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Every row that was done: its key and what was done.
    pub done: Vec<(String, Action)>,
    /// Files copied (both ways), folders made, files removed (a folder taken as a whole counts its files).
    pub copied: u64,
    pub folders: u64,
    pub removed: u64,
    pub bytes: u64,
    /// Copies read back and found equal.
    pub verified: u64,
    pub conflicts: Vec<String>,
    pub skipped: u64,
    /// Folders not removed because something in them stays.
    pub kept: Vec<String>,
    /// What failed: the path, and why in leaf's words (what the system said in brackets).
    pub failed: Vec<(String, String)>,
    pub cancelled: bool,
    /// The run's folder under `.leaf-sync-trash` on server sides (`2026-11-21_031522`).
    pub trash_stamp: String,
}

impl Report {
    /// One line for the job list and the Status Center card.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} copied", self.copied)];
        if self.removed > 0 {
            parts.push(format!("{} removed", self.removed));
        }
        if !self.conflicts.is_empty() {
            parts.push(format!("{} conflicts", self.conflicts.len()));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        if self.cancelled {
            parts.push("cancelled".into());
        }
        parts.join(", ")
    }
}

struct Side<'a> {
    fs: &'a dyn Fs,
    root: &'a VPath,
    local: bool,
}

/// `YYYY-MM-DD_HHMMSS` (UTC) of a Unix time in ms.
fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}_{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

fn same_file(m: &leaf_vfs::Meta, i: &Info) -> bool {
    m.kind == Kind::File && m.size == i.size && (m.modified_ms - i.mtime_ms).abs() <= TIME_SLACK_MS
}

fn err_text(what: &str, e: &FsError) -> String {
    format!("{what} ({e})")
}

/// Walks from the root to the folder `rel` goes in: every part must be a real folder (a link there would carry the write out of
/// the job); missing folders are made when `make`. Returns the folder.
fn parent_of(side: &Side, rel: &str, make: bool) -> Result<VPath, String> {
    let mut cur = side.root.clone();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        cur = names::vchild(&cur, part).map_err(|w| format!("the name cannot be used ({w})"))?;
        match side.fs.stat(&cur) {
            Ok(m) if m.kind == Kind::Dir => {}
            Ok(m) if m.kind == Kind::Link => return Err(format!("{cur} is a link now: nothing is written through it")),
            Ok(_) => return Err(format!("a file is in the way at {cur}")),
            Err(FsError::NotFound) if make => match side.fs.mkdir(&cur) {
                Ok(()) => {}
                Err(FsError::Exists) if side.fs.stat(&cur).is_ok_and(|m| m.kind == Kind::Dir) => {}
                Err(e) => return Err(err_text("the folder could not be made", &e)),
            },
            Err(FsError::NotFound) => return Ok(cur),
            Err(e) => return Err(err_text("the folder could not be read", &e)),
        }
    }
    Ok(cur)
}

/// The path of `rel`, after checking that no link is on the way to it.
fn checked(side: &Side, rel: &str) -> Result<VPath, String> {
    let parent = parent_of(side, rel, false)?;
    let name = rel.rsplit('/').next().unwrap_or(rel);
    names::vchild(&parent, name).map_err(|w| format!("the name cannot be used ({w})"))
}

/// Makes the root (and the folders above it) when it is not there yet.
fn ensure_root(side: &Side) -> Result<(), String> {
    match side.fs.stat(side.root) {
        Ok(m) if m.kind == Kind::Dir => return Ok(()),
        Ok(m) if m.kind == Kind::Link => return Err(format!("{} is a link: leaf does not sync through links", side.root)),
        Ok(_) => return Err(format!("{} is a file, not a folder", side.root)),
        Err(FsError::NotFound) => {}
        Err(e) => return Err(err_text("the folder could not be read", &e)),
    }
    let mut chain = vec![side.root.clone()];
    while let Some(p) = chain.last().and_then(|p| p.parent()) {
        if side.fs.stat(&p).is_ok() || p.is_root() {
            break;
        }
        chain.push(p);
    }
    for p in chain.iter().rev() {
        match side.fs.mkdir(p) {
            Ok(()) | Err(FsError::Exists) => {}
            Err(e) => return Err(err_text("the folder could not be made", &e)),
        }
    }
    Ok(())
}

fn root_ok(side: &Side) -> bool {
    side.fs.stat(side.root).is_ok_and(|m| m.kind == Kind::Dir)
}

struct Runner<'a, 'c> {
    plan: &'a Plan,
    ctl: &'a mut Control<'c>,
    left: Side<'a>,
    right: Side<'a>,
    stamp: String,
    report: Report,
    rows_done: u64,
    rows_total: u64,
    bytes_total: u64,
    /// Rows done, for the state: key and (after the run) what each side has.
    seen: Vec<(String, Action, Option<Seen>)>,
}

impl Runner<'_, '_> {
    fn wait(&self) -> bool {
        while (self.ctl.paused)() && !(self.ctl.cancelled)() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        (self.ctl.cancelled)()
    }

    fn tick(&mut self, current: &str) {
        let p = Progress { rows_done: self.rows_done, rows_total: self.rows_total, bytes_done: self.report.bytes, bytes_total: self.bytes_total, current };
        (self.ctl.progress)(&p);
    }

    fn sides(&self, to_right: bool) -> (&Side<'_>, &Side<'_>) {
        if to_right { (&self.left, &self.right) } else { (&self.right, &self.left) }
    }

    /// Moves `p` (at `rel` on `side`) out of the way: to the Trash locally, into this run's `.leaf-sync-trash` folder on a server.
    fn trash(&self, left: bool, p: &VPath, rel: &str) -> Result<(), String> {
        let side = if left { &self.left } else { &self.right };
        if side.local {
            (self.ctl.trash_local)(&[LocalFs::native(p)])?;
            return match side.fs.stat(p) {
                Err(FsError::NotFound) => Ok(()),
                _ => Err("it is still there after the move to the Trash".into()),
            };
        }
        let in_trash = format!("{TRASH_DIR}/{}/{rel}", self.stamp);
        let parent = parent_of(side, &in_trash, true)?;
        let name = rel.rsplit('/').next().unwrap_or(rel);
        for n in 1..1000 {
            let cand = if n == 1 { name.to_string() } else { format!("{name} ({n})") };
            let to = names::vchild(&parent, &cand).map_err(|w| format!("the name cannot be used ({w})"))?;
            match side.fs.rename(p, &to, false) {
                Ok(()) => return Ok(()),
                Err(FsError::Exists) => continue,
                Err(e) => return Err(err_text(&format!("it could not be moved into {TRASH_DIR}"), &e)),
            }
        }
        Err(format!("it could not be moved into {TRASH_DIR}"))
    }

    fn make_dir(&mut self, row: &Row, to_right: bool) -> Result<Option<Seen>, String> {
        let rel = if to_right { &row.right_rel } else { &row.left_rel };
        let (_, to) = self.sides(to_right);
        let p = parent_of(to, &format!("{rel}/x"), true)?;
        match to.fs.stat(&p) {
            Ok(m) if m.kind == Kind::Dir => Ok(Some(Seen { dir: true, l_size: 0, l_mtime: 0, r_size: 0, r_mtime: 0 })),
            _ => Err("the folder could not be made".into()),
        }
    }

    fn copy(&mut self, row: &Row, to_right: bool) -> Result<Option<Seen>, String> {
        let (from_rel, to_rel) = if to_right { (&row.left_rel, &row.right_rel) } else { (&row.right_rel, &row.left_rel) };
        let (src_info, dst_info) = if to_right { (row.left, row.right) } else { (row.right, row.left) };
        let src_info = src_info.ok_or("there is nothing to copy")?;
        let keep_old = self.plan.job.keep_overwritten;
        let verify = self.plan.job.verify;
        let (from, to) = self.sides(to_right);
        let src = checked(from, from_rel)?;
        match from.fs.stat(&src) {
            Ok(m) if same_file(&m, &src_info) => {}
            Ok(m) if m.kind == Kind::Link => return Err("the original is a link now: it is not followed".into()),
            Ok(_) => return Err("the original changed since the preview; preview again".into()),
            Err(e) => return Err(err_text("the original could not be read", &e)),
        }
        let parent = parent_of(to, to_rel, true)?;
        let name = to_rel.rsplit('/').next().unwrap_or(to_rel);
        let target = names::vchild(&parent, name).map_err(|w| format!("the name cannot be used ({w})"))?;
        let exists = match to.fs.stat(&target) {
            Ok(m) if m.kind == Kind::Link => return Err("the target is a link now: nothing is written through it".into()),
            Ok(m) if m.kind == Kind::Dir => return Err("a folder is in the way".into()),
            Ok(m) => match dst_info {
                Some(d) if same_file(&m, &d) => true,
                _ => return Err("the target changed since the preview; preview again".into()),
            },
            Err(FsError::NotFound) => false,
            Err(e) => return Err(err_text("the target could not be read", &e)),
        };
        let part = names::vchild(&parent, &part_name(name)).map_err(|w| format!("the name cannot be used ({w})"))?;
        let r = self.write_part(to_right, &src, &part, src_info).and_then(|_| {
            let (_, to) = self.sides(to_right);
            if to.fs.caps().set_times && src_info.mtime_ms > 0 {
                to.fs.set_meta(&part, &MetaChange { modified_ms: Some(src_info.mtime_ms), mode: None }).map_err(|e| err_text("the time could not be set", &e))?;
            }
            if exists && keep_old {
                self.trash(!to_right, &target, to_rel).map_err(|e| format!("the old file could not be kept: {e}"))?;
            }
            let (_, to) = self.sides(to_right);
            to.fs.rename(&part, &target, exists && !keep_old).map_err(|e| err_text("the copy could not take its name", &e))
        });
        let (from, to) = self.sides(to_right);
        if let Err(e) = r {
            let _ = to.fs.remove(&part, false);
            return Err(e);
        }
        if verify {
            let no = || false;
            match (hash(from.fs, &src, &no), hash(to.fs, &target, &no)) {
                (Ok(a), Ok(b)) if a == b => self.report.verified += 1,
                (Ok(_), Ok(_)) => return Err("the copy does not match the original (checked by hash)".into()),
                (Err(e), _) | (_, Err(e)) => return Err(err_text("the copy could not be checked", &e)),
            }
        }
        let (from, to) = self.sides(to_right);
        let (a, b) = (from.fs.stat(&src).ok(), to.fs.stat(&target).ok());
        Ok(a.zip(b).map(|(a, b)| {
            let (l, r) = if to_right { (a, b) } else { (b, a) };
            Seen { dir: false, l_size: l.size, l_mtime: l.modified_ms, r_size: r.size, r_mtime: r.modified_ms }
        }))
    }

    fn write_part(&mut self, to_right: bool, src: &VPath, part: &VPath, info: Info) -> Result<(), String> {
        let (from, to) = self.sides(to_right);
        let mut input = from.fs.open_read(src, 0).map_err(|e| err_text("the original could not be read", &e))?;
        let mut out = to.fs.open_write(part, WriteOpts::CREATE_NEW).map_err(|e| err_text("the copy could not be written", &e))?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            if self.wait() {
                return Err("cancelled".into());
            }
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(err_text("the original could not be read", &e.into())),
            };
            out.write_all(&buf[..n]).map_err(|e| err_text("the copy could not be written", &e.into()))?;
            done += n as u64;
            self.report.bytes += n as u64;
            if done > info.size.saturating_add(64 << 20) {
                return Err("the original grew while it was copied; preview again".into());
            }
            let rel = src.name().to_string();
            self.tick(&rel);
        }
        out.finish().map_err(|e| err_text("the copy could not be written", &e))
    }

    fn delete(&mut self, row: &Row, on_left: bool) -> Result<(), String> {
        let rel = if on_left { &row.left_rel } else { &row.right_rel };
        let info = if on_left { row.left } else { row.right };
        let info = info.ok_or("there is nothing to remove")?;
        let side = if on_left { &self.left } else { &self.right };
        let p = checked(side, rel)?;
        match side.fs.stat(&p) {
            Ok(m) if m.kind == Kind::Link => return Err("it is a link now; it is left alone".into()),
            Ok(m) if info.dir && m.kind == Kind::Dir => {}
            Ok(m) if !info.dir && same_file(&m, &info) => {}
            Ok(_) => return Err("it changed since the preview; preview again".into()),
            Err(FsError::NotFound) => return Ok(()),
            Err(e) => return Err(err_text("it could not be read", &e)),
        }
        if info.dir && !row.whole {
            // only an empty folder goes: what stays in it keeps it
            return match side.fs.remove(&p, false) {
                Ok(()) => Ok(()),
                Err(FsError::NotEmpty) => Err(String::new()),
                Err(e) => Err(err_text("the folder could not be removed", &e)),
            };
        }
        self.trash(on_left, &p, rel)
    }

    fn one(&mut self, row: &Row) {
        if self.wait() {
            self.report.cancelled = true;
            return;
        }
        self.tick(&row.rel);
        let dir = row.is_dir();
        let r = match row.action {
            Action::ToRight | Action::ToLeft => {
                let to_right = row.action == Action::ToRight;
                let r = if dir { self.make_dir(row, to_right) } else { self.copy(row, to_right) };
                r.map(|s| {
                    if dir { self.report.folders += 1 } else { self.report.copied += 1 }
                    s
                })
            }
            Action::DeleteRight | Action::DeleteLeft => match self.delete(row, row.action == Action::DeleteLeft) {
                Ok(()) => {
                    self.report.removed += if dir { row.files } else { 1 };
                    Ok(None)
                }
                Err(e) if e.is_empty() => {
                    self.report.kept.push(row.rel.clone());
                    self.rows_done += 1;
                    return;
                }
                Err(e) => Err(e),
            },
            Action::Conflict => {
                self.report.conflicts.push(row.rel.clone());
                self.rows_done += 1;
                return;
            }
            Action::Skip => {
                self.report.skipped += 1;
                self.rows_done += 1;
                return;
            }
        };
        self.rows_done += 1;
        match r {
            Ok(s) => {
                self.report.done.push((row.key.clone(), row.action));
                self.seen.push((row.key.clone(), row.action, s));
            }
            Err(e) if (self.ctl.cancelled)() => {
                self.report.cancelled = true;
                log::debug!("sync: {} stopped: {e}", row.rel);
            }
            Err(e) => self.report.failed.push((row.rel.clone(), e)),
        }
    }
}

/// Does the rows of `plan` (as the user left them) and nothing else, then saves what both sides look like for the next run.
/// `Err` only when the run could not start (a root went missing); what fails on the way is in the report.
pub fn run(plan: &Plan, left_fs: &dyn Fs, right_fs: &dyn Fs, state_dir: &Path, ctl: &mut Control) -> Result<Report, Error> {
    let left = Side { fs: left_fs, root: &plan.left_root, local: plan.left_root.scheme() == "file" };
    let right = Side { fs: right_fs, root: &plan.right_root, local: plan.right_root.scheme() == "file" };
    let writes = |to_right: bool| plan.rows.iter().any(|r| r.action == if to_right { Action::ToRight } else { Action::ToLeft });
    for (side, missing, which, w) in [(&left, plan.left_missing, "left", writes(false)), (&right, plan.right_missing, "right", writes(true))] {
        if missing && w {
            ensure_root(side).map_err(|e| Error::new(format!("The {which} folder could not be made"), e))?;
        } else if !missing && !root_ok(side) {
            return Err(Error::new(format!("The {which} folder is not there any more. The drive may not be connected; nothing was changed"), side.root.to_string()));
        }
    }
    let bytes_total = plan.rows.iter().filter(|r| matches!(r.action, Action::ToRight | Action::ToLeft) && !r.is_dir()).map(|r| r.bytes).sum();
    let mut rn = Runner {
        plan,
        ctl,
        left,
        right,
        stamp: stamp(crate::now_ms()),
        report: Report::default(),
        rows_done: 0,
        rows_total: plan.rows.len() as u64,
        bytes_total,
        seen: Vec::new(),
    };
    rn.report.trash_stamp = rn.stamp.clone();
    let rows = &plan.rows;
    let is_copy = |r: &Row| matches!(r.action, Action::ToRight | Action::ToLeft);
    let is_del = |r: &Row| matches!(r.action, Action::DeleteRight | Action::DeleteLeft);
    // folders made, then files copied, then the rest but removals, then removals (empty folders last, deepest first)
    let mut order: Vec<&Row> = rows.iter().filter(|r| is_copy(r) && r.is_dir()).collect();
    order.extend(rows.iter().filter(|r| is_copy(r) && !r.is_dir()));
    order.extend(rows.iter().filter(|r| !is_copy(r) && !is_del(r)));
    let deletes: Vec<&Row> = rows.iter().filter(|r| is_del(r) && (r.whole || !r.is_dir())).collect();
    let mut empty_dirs: Vec<&Row> = rows.iter().filter(|r| is_del(r) && r.is_dir() && !r.whole).collect();
    empty_dirs.reverse();
    for row in order {
        rn.one(row);
        if rn.report.cancelled {
            break;
        }
    }
    if !rn.report.cancelled && !(deletes.is_empty() && empty_dirs.is_empty()) {
        // a side that went away during the copies removes nothing on the other
        if !root_ok(&rn.left) || !root_ok(&rn.right) {
            for r in deletes.iter().chain(&empty_dirs) {
                rn.report.failed.push((r.rel.clone(), "not removed: one of the two folders is not there any more".into()));
            }
        } else {
            for row in deletes.into_iter().chain(empty_dirs) {
                rn.one(row);
                if rn.report.cancelled {
                    break;
                }
            }
        }
    }
    let report = std::mem::take(&mut rn.report);
    save_state(plan, &rn.seen, &report, state_dir);
    Ok(report)
}

fn save_state(plan: &Plan, seen: &[(String, Action, Option<Seen>)], report: &Report, state_dir: &Path) {
    let mut st = plan.state.clone();
    st.entries.retain(|k, _| plan.seen.contains(k));
    for (k, l, r) in &plan.same {
        st.entries.insert(k.clone(), Seen { dir: l.dir, l_size: l.size, l_mtime: l.mtime_ms, r_size: r.size, r_mtime: r.mtime_ms });
    }
    let (mut lf, mut rf) = (plan.left_files as i64, plan.right_files as i64);
    for (k, a, s) in seen {
        let row = plan.rows.iter().find(|r| &r.key == k);
        let n = row.map_or(1, |r| if r.is_dir() { r.files as i64 } else { 1 });
        match a {
            Action::DeleteLeft | Action::DeleteRight => {
                st.forget(k);
                if *a == Action::DeleteLeft { lf -= n } else { rf -= n }
            }
            _ => {
                match s {
                    Some(s) => {
                        st.entries.insert(k.clone(), *s);
                    }
                    None => {
                        st.entries.remove(k);
                    }
                }
                if row.is_some_and(|r| !r.is_dir() && r.left.is_none()) {
                    lf += 1;
                }
                if row.is_some_and(|r| !r.is_dir() && r.right.is_none()) {
                    rf += 1;
                }
            }
        }
    }
    st.left_files = lf.max(0) as u64;
    st.right_files = rf.max(0) as u64;
    if let Err(e) = st.save(state_dir, &plan.job.id) {
        log::warn!("sync: the state of job {} could not be saved: {e} ({} rows done)", plan.job.id, report.done.len());
    }
}

/// Empties `.leaf-sync-trash` of a server side for good (the user's "Purge"). Ok(false): there was none.
pub fn purge(fs: &dyn Fs, root: &VPath) -> Result<bool, Error> {
    let p = names::vchild(root, TRASH_DIR).map_err(|w| Error::new("The folder cannot be used", w))?;
    match fs.stat(&p) {
        Err(FsError::NotFound) => Ok(false),
        Ok(m) if m.kind == Kind::Dir => fs.remove(&p, true).map(|_| true).map_err(|e| Error::new(format!("{TRASH_DIR} could not be emptied"), e.to_string())),
        Ok(_) => Err(Error::new(format!("{TRASH_DIR} is not a folder (a link?); it is left alone"), p.to_string())),
        Err(e) => Err(Error::new(format!("{TRASH_DIR} could not be read"), e.to_string())),
    }
}

/// Puts a file a run moved into `.leaf-sync-trash/<stamp>/` back where it was (never over something that is there now).
pub fn restore_from_trash(fs: &dyn Fs, root: &VPath, stamp: &str, rel: &str) -> Result<(), Error> {
    let side = Side { fs, root, local: false };
    let bad = |w: String| Error::new("It could not be put back", w);
    let from = checked(&side, &format!("{TRASH_DIR}/{stamp}/{rel}")).map_err(bad)?;
    let parent = parent_of(&side, rel, true).map_err(bad)?;
    let to = names::vchild(&parent, rel.rsplit('/').next().unwrap_or(rel)).map_err(|w| bad(w.into()))?;
    fs.rename(&from, &to, false).map_err(|e| Error::new("It could not be put back", e.to_string()))
}
