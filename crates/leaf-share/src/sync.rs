//! One sync of a share, on the PC that shared it: leaf-sync's scheduled plan (it never asks: a refusal pauses the share with its
//! reason) and run, the friend's folder as a remote `Fs`. A file changed on both sides keeps both: the older one is renamed on its
//! side to `name (conflict from <device> <date>).ext` and the next round copies each to the other side.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use leaf_sync::{Action, Control, Job, Mode, Plan, Progress};
use leaf_vfs::{Fs as _, FsError, LocalFs, MetaChange, names};

use crate::remote::{RemoteFs, SCHEME};
use crate::service::Inner;
use crate::store::ShareRec;
use crate::transport::Link;

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a share takes and leaves out: its own globs, the lines of `.leafignore` in its folder, and the parts of transfers.
fn excludes(rec: &ShareRec) -> Vec<String> {
    let mut v = rec.exclude.clone();
    if let Ok(text) = std::fs::read_to_string(rec.folder.join(".leafignore")) {
        v.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_string));
    }
    v.push(format!("*{}", rf_nearby::PART));
    v
}

pub(crate) fn job_of(rec: &ShareRec) -> Job {
    let left = LocalFs::address(&rec.folder).to_string();
    let mut job = Job::new(&rec.name, &left, &format!("{SCHEME}://{}/", rec.id));
    job.id = rec.id.clone();
    job.mode = if rec.friend_can_write { Mode::TwoWay } else { Mode::Mirror };
    job.exclude = excludes(rec);
    job.keep_overwritten = true;
    job
}

/// A refusal of a run that never asks (a side missing or emptied, too much removed, no record): the share waits for the user.
fn is_refusal(e: &leaf_sync::Error) -> bool {
    e.side_gone || e.message.starts_with("The scheduled run was not started")
}

fn set_syncing(inner: &Inner, id: &str, n: Option<u64>) {
    if let Some(l) = lock(&inner.live).get_mut(id) {
        l.syncing = n;
    }
    inner.listener.changed();
}

fn finish(inner: &Inner, id: &str, ok: bool, result: String) {
    {
        let mut store = lock(&inner.store);
        if let Some(s) = store.get_mut(id) {
            if ok {
                s.last_sync_ms = crate::now_ms();
            }
            s.last_result = result;
            store.save();
        }
    }
    set_syncing(inner, id, None);
}

pub(crate) fn sync_share(inner: &Arc<Inner>, id: &str, link: Arc<dyn Link>) {
    let Some(rec) = lock(&inner.store).get(id).cloned() else { return };
    let Some(stop) = lock(&inner.live).get(id).map(|l| l.stop.clone()) else { return };
    let remote = RemoteFs { link: link.clone(), share: id.to_string(), rt: inner.rt.clone() };
    let state_dir = inner.cfg.state_dir.join("state");
    let job = job_of(&rec);
    let cancelled = || stop.load(Ordering::Relaxed) || link.is_closed();
    set_syncing(inner, id, Some(0));
    let mut summary = String::new();
    for _round in 0..3 {
        let plan = match leaf_sync::plan_scheduled(&job, &LocalFs, &remote, &state_dir, &cancelled) {
            Ok(p) => p,
            Err(e) if is_refusal(&e) => {
                finish(inner, id, false, e.message.clone());
                inner.pause(id, &e.message);
                return;
            }
            Err(e) => {
                finish(inner, id, false, e.to_string());
                return;
            }
        };
        let c = plan.counts();
        set_syncing(inner, id, Some(c.to_right + c.to_left + c.delete_left + c.delete_right));
        if !plan.is_empty() {
            let mut last_note = Instant::now();
            let mut progress = |p: &Progress| {
                if last_note.elapsed().as_millis() >= 500 {
                    last_note = Instant::now();
                    if let Some(l) = lock(&inner.live).get_mut(id) {
                        l.syncing = Some(p.rows_total.saturating_sub(p.rows_done));
                    }
                    inner.listener.changed();
                }
            };
            let trash = |p: &[PathBuf]| (inner.cfg.trash_local)(p);
            let bin = |p: &Path| (inner.cfg.bin_takes)(p);
            let never = || false;
            let mut ctl = Control { cancelled: &cancelled, paused: &never, progress: &mut progress, trash_local: &trash, bin_takes: &bin };
            match leaf_sync::run(&plan, &LocalFs, &remote, &state_dir, &mut ctl) {
                Ok(report) => {
                    summary = if let Some((path, why)) = report.failed.first() {
                        format!("{}; {path}: {why}", report.summary())
                    } else {
                        report.summary()
                    };
                    if report.cancelled {
                        finish(inner, id, false, summary);
                        return;
                    }
                }
                Err(e) if is_refusal(&e) => {
                    finish(inner, id, false, e.message.clone());
                    inner.pause(id, &e.message);
                    return;
                }
                Err(e) => {
                    finish(inner, id, false, e.to_string());
                    return;
                }
            }
        }
        if resolve_conflicts(inner, &rec, &plan, &remote) == 0 {
            break;
        }
    }
    finish(inner, id, true, if summary.is_empty() { "In sync".into() } else { summary });
}

fn hash_local(p: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(p)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().to_vec())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `report.docx` -> `report (conflict from Neo's PC 2026-11-21).docx` (a number after the date when that is taken).
pub fn conflict_name(name: &str, device: &str, date: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let device = names::safe_local_name(device);
    let tail = if n > 1 { format!(" {n}") } else { String::new() };
    names::safe_local_name(&format!("{stem} (conflict from {device} {date}{tail}){ext}"))
}

fn today() -> String {
    let (y, m, d) = crate::folder::civil(crate::now_ms().div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Files changed on both sides: the same bytes (only the times differ) are taken as one; otherwise the older one gets the conflict
/// name on its side. Returns how many were handled (another round copies them across).
fn resolve_conflicts(inner: &Inner, rec: &ShareRec, plan: &Plan, remote: &RemoteFs) -> u64 {
    let me = inner.cfg.name.clone();
    let peer = rec.peer.as_ref().map_or_else(|| "the other PC".to_string(), |p| p.name.clone());
    let date = today();
    let mut handled = 0;
    for row in plan.rows.iter().filter(|r| r.action == Action::Conflict && !r.is_dir()) {
        let (Some(l), Some(r)) = (row.left, row.right) else { continue };
        if l.link || r.link {
            continue;
        }
        let mut lp = rec.folder.clone();
        let mut bad = false;
        for part in row.left_rel.split('/') {
            match names::local_child(&lp, part) {
                Ok(p) => lp = p,
                Err(_) => bad = true,
            }
        }
        let rp = row.right_rel.split('/').try_fold(plan.right_root.clone(), |p, part| names::vchild(&p, part));
        let (false, Ok(rp)) = (bad, rp) else { continue };
        if l.size == r.size
            && let (Ok(a), Ok(b)) = (hash_local(&lp), remote.hash(&rp))
            && hex(&a) == b
        {
            if remote.set_meta(&rp, &MetaChange { modified_ms: Some(l.mtime_ms), mode: None }).is_ok() {
                handled += 1;
            }
            continue;
        }
        // the older one gives way (the same time: the friend's)
        let right_loses = r.mtime_ms <= l.mtime_ms;
        let name = row.rel.rsplit('/').next().unwrap_or(&row.rel).to_string();
        for n in 1..100 {
            let r = if right_loses {
                let Some(parent) = rp.parent() else { break };
                match names::vchild(&parent, &conflict_name(&name, &peer, &date, n)) {
                    Ok(to) => remote.rename(&rp, &to, false),
                    Err(_) => break,
                }
            } else {
                let Some(parent) = lp.parent() else { break };
                match names::local_child(parent, &conflict_name(&name, &me, &date, n)) {
                    Ok(to) => LocalFs.rename(&LocalFs::address(&lp), &LocalFs::address(&to), false),
                    Err(_) => break,
                }
            };
            match r {
                Ok(()) => {
                    handled += 1;
                    break;
                }
                Err(FsError::Exists) => continue,
                Err(e) => {
                    log::warn!("share: the conflict at {} could not be kept apart: {e}", row.rel);
                    break;
                }
            }
        }
    }
    handled
}
