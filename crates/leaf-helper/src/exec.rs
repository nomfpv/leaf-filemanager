//! Runs one request: the syntax checks of `leaf_common::admin::check_request`, then the per-system opening checks of [`crate::os`],
//! then the operation.
//!
//! What every operation does with its paths before acting: every folder on the way to a named item is opened and HELD for the
//! whole operation ([`crate::os::hold_parents`]): it is not a link or a junction, it is where its name says, and (Windows) it cannot
//! be renamed, deleted or replaced while it is held. From the held parent on, everything that changes files works RELATIVE to open
//! handles, never by a path again: delete, copy and move through the helper's own walk ([`crate::tree`]), a new folder and a rename
//! through [`crate::node`]. leaf's own engines (`leaf_sys::fastcopy`, `fastdelete`, the shell's engine) are NOT used here: they
//! work by path and would follow a junction swapped into a tree.
//!
//! What still goes by path: the listing and the stat (read only, inside a held folder: `leaf_sys::list_dir`, `stat_entry`). On
//! Linux, where a descriptor does not keep a folder from being renamed, removals are by name in the held folder's descriptor
//! (`unlinkat`, see `node_unix.rs`).
//!
//! The Recycle Bin is not used as administrator ([`Refusal::NoRecycle`]): the shell's engine that does it works by path, and as
//! another administrator account (over-the-shoulder) it would be that account's bin. leaf offers a permanent delete instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leaf_common::admin::{Choice, Clash, MAX_FILE, PAGE_ITEMS, Refusal, Reply, Request, check_request};
use leaf_sys::fileops::{Tracker, Transfer};

use crate::Fail;
use crate::{node, os, tree};

/// How a running operation talks back: replies (progress, questions) and raw bytes, and the answers to its questions.
pub struct Job {
    pub cancel: Arc<AtomicBool>,
    /// Sends one reply; `false` when leaf's end is gone.
    pub send: Arc<dyn Fn(&Reply) -> bool + Send + Sync>,
    /// Sends one frame of raw bytes.
    pub send_data: Arc<dyn Fn(&[u8]) -> bool + Send + Sync>,
    pub answers: Arc<Mutex<Receiver<Option<Vec<Choice>>>>>,
}

/// Runs `req` and gives its final reply.
pub fn run(req: &Request, job: &Job) -> Reply {
    if let Err(why) = check_request(req) {
        return Reply::Refused { why };
    }
    match act(req, job) {
        Ok(r) => r,
        Err(f) => f.reply(),
    }
}

fn act(req: &Request, job: &Job) -> Result<Reply, Fail> {
    match req {
        Request::List { dir } => {
            let _held = os::hold_folder(dir)?;
            let mut items = os::list(dir)?;
            // (in pages: one reply stays far below the reply limit)
            while items.len() > PAGE_ITEMS {
                let rest = items.split_off(PAGE_ITEMS);
                if !(job.send)(&Reply::Page { items }) {
                    return Err(gone());
                }
                items = rest;
            }
            Ok(Reply::Listing { items })
        }
        Request::Stat { path } => {
            let _held = os::hold_parents(path)?;
            Ok(Reply::Stat { item: os::stat(path)? })
        }
        Request::CreateFolder { parent, name } => {
            let held = os::hold_folder(parent)?;
            let dir = node::held_node(&held, true)?;
            node::make_dir(&dir, std::ffi::OsStr::new(name))?;
            Ok(Reply::Created { path: parent.join(name) })
        }
        Request::Rename { path, new_name } => {
            let held = os::hold_parents(path)?;
            let dir = node::held_node(&held, false)?;
            let name = path.file_name().ok_or(Refusal::WrongKind)?;
            node::rename_in(&dir, name, std::ffi::OsStr::new(new_name))?;
            Ok(Reply::Done { ok: true, pairs: vec![(path.clone(), path.with_file_name(new_name))], missing: 0, kept: 0, partial: 0 })
        }
        Request::Copy { sources, dest } => done(tree::transfer(Transfer::Copy, sources, dest, &tracker(job))?),
        Request::Move { sources, dest } => done(tree::transfer(Transfer::Move, sources, dest, &tracker(job))?),
        Request::Recycle { .. } => Err(Refusal::NoRecycle.into()),
        // (`check_request` refused it already when leaf did not say it asked the user)
        Request::DeleteForGood { paths, .. } => done(tree::delete(paths, &tracker(job))?),
        Request::WriteFromPart { part, target } => {
            os::write_from_part(part, target)?;
            Ok(Reply::Done { ok: true, pairs: Vec::new(), missing: 0, kept: 0, partial: 0 })
        }
        Request::ReadFile { path } => {
            let bytes = os::read_file(path)?;
            if bytes.len() as u64 > MAX_FILE {
                return Err(Refusal::TooBig.into());
            }
            if !(job.send)(&Reply::Reading { len: bytes.len() as u64 }) {
                return Err(gone());
            }
            for chunk in bytes.chunks(leaf_common::admin::DATA_CHUNK) {
                if !(job.send_data)(chunk) {
                    return Err(gone());
                }
            }
            Ok(Reply::Done { ok: true, pairs: Vec::new(), missing: 0, kept: 0, partial: 0 })
        }
        // READ ONLY: the storage API's numbers for the disks, nothing is written and no command goes to a drive
        Request::DriveHealth { disk } => Ok(Reply::Health { disks: leaf_sys::drives::disk_health_admin(*disk) }),
        Request::Hello { .. } | Request::Answer { .. } | Request::Cancel | Request::Bye => Err(Refusal::Unexpected.into()),
    }
}

fn gone() -> Fail {
    Fail::Failed { denied: false, detail: "leaf went away".into() }
}

fn done(o: tree::Outcome) -> Result<Reply, Fail> {
    Ok(Reply::Done { ok: o.ok, pairs: o.pairs, missing: o.missing, kept: o.kept, partial: o.partial })
}

/// A tracker like the Status Center's: progress goes back to leaf at most 20 times a second, a question about names that exist
/// goes to leaf and waits for its answer (a cancel or leaf going away answers "cancel").
fn tracker(job: &Job) -> Arc<Tracker> {
    let send = job.send.clone();
    let bytes = Arc::new(leaf_sys::fileops::ByteProgress::default());
    let b = bytes.clone();
    let last = Mutex::new(Instant::now() - Duration::from_secs(1));
    let mut t = Tracker::new(
        Box::new(move |total, done, current| {
            let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
            if current.is_some() || l.elapsed() >= Duration::from_millis(50) || done >= total {
                *l = Instant::now();
                let (bytes_done, bytes_total) = b.get();
                send(&Reply::Progress { total, done, current, bytes_done, bytes_total });
            }
        }),
        job.cancel.clone(),
    );
    t.bytes = bytes;
    let (send, answers, cancel) = (job.send.clone(), job.answers.clone(), job.cancel.clone());
    t.ask = Some(Box::new(move |list| {
        let clashes: Vec<Clash> = list.iter().map(Clash::from).collect();
        if !send(&Reply::Ask { clashes }) {
            return None;
        }
        let rx = answers.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                // an answer of another length is no answer
                Ok(Some(c)) if c.len() == list.len() => return Some(c.into_iter().map(Into::into).collect()),
                Ok(_) => return None,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !cancel.load(Ordering::Relaxed) => {}
                Err(_) => return None,
            }
        }
    }));
    Arc::new(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn job() -> (Job, std::sync::mpsc::Sender<Option<Vec<Choice>>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = Job {
            cancel: Arc::new(AtomicBool::new(false)),
            send: Arc::new(|_| true),
            send_data: Arc::new(|_| true),
            answers: Arc::new(Mutex::new(rx)),
        };
        (job, tx)
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("leaf-helper-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // the checks compare with the folder's real name (no 8.3 short form in a temp path)
        leaf_canonical(&d)
    }

    fn leaf_canonical(p: &Path) -> PathBuf {
        let c = std::fs::canonicalize(p).unwrap();
        let s = c.to_string_lossy().into_owned();
        PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
    }

    #[test]
    fn admin_path_validation_refuses_relative_dots_and_swapped_link() {
        let (job, _tx) = job();
        let refused = |r: &Request| matches!(run(r, &job), Reply::Refused { .. });
        assert!(refused(&Request::Stat { path: PathBuf::from("relative.txt") }));
        let d = temp("paths");
        std::fs::create_dir(d.join("a")).unwrap();
        std::fs::write(d.join("a").join("f.txt"), b"x").unwrap();
        let dots = PathBuf::from(format!("{}{}a{}..{}a", d.display(), std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR));
        assert!(refused(&Request::List { dir: dots }));
        // the plain path works
        assert!(matches!(run(&Request::List { dir: d.join("a") }, &job), Reply::Listing { items } if items.len() == 1));
        // a link (Windows: a junction, which needs no privilege; Linux: a symlink) where a folder was: the same text now leads
        // elsewhere, and is refused
        let target = d.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("secret.txt"), b"s").unwrap();
        std::fs::rename(d.join("a"), d.join("a-old")).unwrap();
        os::make_dir_link(&d.join("a"), &target).unwrap();
        assert!(refused(&Request::List { dir: d.join("a") }), "listing through a swapped-in link");
        assert!(refused(&Request::ReadFile { path: d.join("a").join("secret.txt") }), "reading through a swapped-in link");
        assert!(refused(&Request::DeleteForGood { paths: vec![d.join("a").join("secret.txt")], asked_user: true }));
        assert!(target.join("secret.txt").exists(), "nothing behind the link was touched");
        let _ = std::fs::remove_dir(d.join("a"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn admin_delete_and_move_touch_exactly_the_named_item() {
        let (job, _tx) = job();
        let d = temp("exact");
        let src = d.join("src");
        let dst = d.join("dst");
        std::fs::create_dir_all(src.join("keep-dir")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        for n in ["one.txt", "two.txt", "one.txt.bak"] {
            std::fs::write(src.join(n), n.as_bytes()).unwrap();
        }
        std::fs::write(src.join("keep-dir").join("inner.txt"), b"i").unwrap();
        // delete for good: only one.txt goes
        let r = run(&Request::DeleteForGood { paths: vec![src.join("one.txt")], asked_user: true }, &job);
        assert!(matches!(r, Reply::Done { ok: true, .. }), "{r:?}");
        assert!(!src.join("one.txt").exists());
        assert!(src.join("one.txt.bak").exists() && src.join("two.txt").exists() && src.join("keep-dir").join("inner.txt").exists());
        // move: only two.txt goes, into dst
        let r = run(&Request::Move { sources: vec![src.join("two.txt")], dest: dst.clone() }, &job);
        assert!(matches!(r, Reply::Done { ok: true, .. }), "{r:?}");
        assert!(!src.join("two.txt").exists());
        assert_eq!(std::fs::read(dst.join("two.txt")).unwrap(), b"two.txt");
        assert!(src.join("one.txt.bak").exists() && src.join("keep-dir").join("inner.txt").exists());
        let left: Vec<_> = std::fs::read_dir(&dst).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "nothing else arrived: {left:?}");
        // a permanent delete leaf did not ask the user about is refused, and leaves the item
        let r = run(&Request::DeleteForGood { paths: vec![src.join("one.txt.bak")], asked_user: false }, &job);
        assert!(matches!(r, Reply::Refused { why: Refusal::NotAsked }));
        assert!(src.join("one.txt.bak").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn admin_delete_does_not_follow_a_junction_swapped_in_after_the_check() {
        let d = temp("swap");
        let x = d.join("x");
        std::fs::create_dir_all(x.join("sub")).unwrap();
        std::fs::write(x.join("sub").join("own.txt"), b"o").unwrap();
        // (removed first: the walk takes the names from the end, and "z.txt" comes after "sub")
        std::fs::write(x.join("z.txt"), b"z").unwrap();
        let protected = d.join("protected");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("system.dll"), b"keep").unwrap();
        // the swap happens at the first progress report, after the request was checked and the tree surveyed: `sub` becomes a
        // junction to the protected folder
        let swapped = Arc::new(AtomicBool::new(false));
        let (sw, xx, pp) = (swapped.clone(), x.clone(), protected.clone());
        let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<Choice>>>();
        let job = Job {
            cancel: Arc::new(AtomicBool::new(false)),
            send: Arc::new(move |r| {
                if matches!(r, Reply::Progress { .. }) && !sw.swap(true, Ordering::Relaxed) {
                    std::fs::rename(xx.join("sub"), xx.with_file_name("sub-moved-away")).unwrap();
                    os::make_dir_link(&xx.join("sub"), &pp).unwrap();
                }
                true
            }),
            send_data: Arc::new(|_| true),
            answers: Arc::new(Mutex::new(rx)),
        };
        let r = run(&Request::DeleteForGood { paths: vec![x.clone()], asked_user: true }, &job);
        drop(tx);
        assert!(swapped.load(Ordering::Relaxed), "the swap happened during the delete");
        assert_eq!(std::fs::read(protected.join("system.dll")).unwrap(), b"keep", "nothing behind the link was deleted");
        // Windows removes the junction as itself and finishes. Linux removes by name, sees that the name no longer holds the folder
        // it opened, keeps it and says so: nothing behind the link is touched either way.
        #[cfg(windows)]
        {
            assert!(matches!(r, Reply::Done { .. }), "{r:?}");
            assert!(!x.exists(), "the junction itself and the folder went");
        }
        #[cfg(not(windows))]
        assert!(matches!(r, Reply::Done { .. } | Reply::Failed { denied: false, .. }), "{r:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn admin_copy_of_a_tree_with_a_junction_is_refused_and_writes_nothing() {
        let (job, _tx) = job();
        let d = temp("copylink");
        let src = d.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("inner").join("a.txt"), b"a").unwrap();
        let elsewhere = d.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("secret.txt"), b"s").unwrap();
        os::make_dir_link(&src.join("inner").join("j"), &elsewhere).unwrap();
        let dst = d.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        for req in [Request::Copy { sources: vec![src.clone()], dest: dst.clone() }, Request::Move { sources: vec![src.clone()], dest: dst.clone() }] {
            let r = run(&req, &job);
            assert!(matches!(r, Reply::Refused { why: Refusal::Link }), "{r:?}");
            assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 0, "nothing was written");
        }
        assert!(src.join("inner").join("a.txt").exists() && elsewhere.join("secret.txt").exists());
        let _ = std::fs::remove_dir(src.join("inner").join("j"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A job whose questions about existing names are all answered "Replace".
    fn job_replacing() -> Job {
        let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<Choice>>>();
        let tx = Mutex::new(tx);
        Job {
            cancel: Arc::new(AtomicBool::new(false)),
            send: Arc::new(move |r| {
                if let Reply::Ask { clashes } = r {
                    let _ = tx.lock().unwrap().send(Some(vec![Choice::Replace; clashes.len()]));
                }
                true
            }),
            send_data: Arc::new(|_| true),
            answers: Arc::new(Mutex::new(rx)),
        }
    }

    #[test]
    fn admin_replace_onto_itself_leaves_everything_intact() {
        let job = job_replacing();
        let d = temp("itself");
        let x = d.join("x");
        std::fs::create_dir_all(x.join("folder")).unwrap();
        std::fs::write(x.join("f.txt"), b"file").unwrap();
        std::fs::write(x.join("folder").join("inner.txt"), b"inner").unwrap();
        let intact = || {
            assert_eq!(std::fs::read(x.join("f.txt")).unwrap(), b"file");
            assert_eq!(std::fs::read(x.join("folder").join("inner.txt")).unwrap(), b"inner");
            let names: Vec<_> = std::fs::read_dir(&x).unwrap().flatten().map(|e| e.file_name()).collect();
            assert_eq!(names.len(), 2, "nothing added or left behind: {names:?}");
        };
        // a file moved and copied onto itself (into the folder it is in), "Replace" answered
        for req in [Request::Move { sources: vec![x.join("f.txt")], dest: x.clone() }, Request::Copy { sources: vec![x.join("f.txt")], dest: x.clone() }] {
            let r = run(&req, &job);
            assert!(matches!(r, Reply::Done { .. }), "{r:?}");
            intact();
        }
        // a folder moved into its own parent
        let r = run(&Request::Move { sources: vec![x.join("folder")], dest: x.clone() }, &job);
        assert!(matches!(r, Reply::Done { .. }), "{r:?}");
        intact();
        // a folder into itself, and a folder into a folder inside it (the second in other letter case: compared by identity)
        let inside = PathBuf::from(x.join("folder").to_string_lossy().to_uppercase());
        for req in [
            Request::Move { sources: vec![x.join("folder")], dest: x.join("folder") },
            Request::Copy { sources: vec![x.clone()], dest: if cfg!(windows) { inside.clone() } else { x.join("folder") } },
        ] {
            let r = run(&req, &job);
            assert!(matches!(r, Reply::Refused { .. }), "{r:?}");
            intact();
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn admin_move_source_that_changed_during_the_copy_is_kept() {
        let (job, _tx) = job();
        let d = temp("grow");
        let (src, dst) = (d.join("src"), d.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("log.txt"), b"first part").unwrap();
        // the source "grows" by 5 bytes while it is copied (as seen by the size check after the copy)
        crate::tree::GROW.with(|g| g.set(5));
        let r = run(&Request::Move { sources: vec![src.join("log.txt")], dest: dst.clone() }, &job);
        crate::tree::GROW.with(|g| g.set(0));
        assert!(matches!(r, Reply::Done { ok: false, kept: 1, .. }), "{r:?}");
        assert_eq!(std::fs::read(src.join("log.txt")).unwrap(), b"first part", "the source stays");
        assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 0, "no copy and no temp file left at the destination");
        let _ = std::fs::remove_dir_all(&d);
    }
}
