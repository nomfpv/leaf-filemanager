//! Runs one request: the syntax checks of `leaf_common::admin::check_request`, then the per-system opening checks of [`crate::os`],
//! then the operation with leaf's own engines (the same `leaf_sys::fastcopy`, `fastdelete` and `fileops` code leaf runs without
//! the helper, so there is no second copy or delete implementation to drift apart).
//!
//! What every operation does with its paths before acting:
//! - every folder on the way to a named item is opened and HELD for the whole operation ([`crate::os::hold_parents`]): it is not a
//!   link or a junction, it is where its name says, and (Windows) it cannot be renamed, deleted or replaced while it is held, so a
//!   junction cannot be put in its place half way;
//! - the named item itself is opened without following a link ([`crate::os::check_item`]) and must be where its name says.
//!
//! CHECK-THEN-ACT, for the whole module: the engines take paths, not handles. What is held cannot change; what is not held can:
//! - the named item itself between [`crate::os::check_item`] and the engine's own open (it can only be swapped by someone who may
//!   write to its folder, so not in a protected folder, which is what the helper is for);
//! - anything INSIDE a folder that is copied, moved or deleted: the engines walk it by path. They do not follow links inside (a
//!   link is copied or removed as a link), but the walk and the act on each entry are separate steps;
//! - on Linux, the folders on the way too: a descriptor held open does not keep a folder from being renamed, so for a new folder,
//!   a rename and everything that goes through the engines, the path is looked up again after the walk that checked it (the listing,
//!   stat, read and the editor's write work through the descriptors and have no such gap).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leaf_common::admin::{Choice, Clash, MAX_FILE, Refusal, Reply, Request, check_request};
use leaf_sys::fileops::{Tracker, Transfer};

use crate::Fail;
use crate::os;

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
            Ok(Reply::Listing { items: os::list(dir)? })
        }
        Request::Stat { path } => {
            let _held = os::hold_parents(path)?;
            Ok(Reply::Stat { item: os::stat(path)? })
        }
        Request::CreateFolder { parent, name } => {
            let _held = os::hold_folder(parent)?;
            // (the one path the helper joins: a held folder and a name `check_name` accepted, one plain part)
            let p = parent.join(name);
            std::fs::create_dir(&p)?;
            Ok(Reply::Created { path: p })
        }
        Request::Rename { path, new_name } => {
            let _held = os::hold_parents(path)?;
            os::check_item(path)?;
            let to = path.with_file_name(new_name);
            // CHECK-THEN-ACT: `path` itself between `check_item` and the rename. A rename moves the name it finds (a link stays a link,
            // nothing is followed), and the folder it is in is held.
            leaf_sys::fileops::rename_no_replace(path, &to)?;
            Ok(Reply::Done { ok: true, pairs: vec![(path.clone(), to)] })
        }
        Request::Copy { sources, dest } => transfer(Transfer::Copy, sources, dest, job),
        Request::Move { sources, dest } => transfer(Transfer::Move, sources, dest, job),
        Request::Recycle { paths } => {
            if !os::HAS_RECYCLE {
                return Err(Refusal::WrongKind.into());
            }
            let _held = hold_items(paths, true)?;
            let t = tracker(job);
            let r = leaf_sys::fileops::delete_tracked(paths, false, t.clone());
            engine_done(r, &t)
        }
        Request::DeleteForGood { paths, .. } => {
            // (`check_request` refused it already when leaf did not say it asked the user)
            let _held = hold_items(paths, true)?;
            let t = tracker(job);
            let r = os::delete_for_good(paths, t.clone());
            engine_done(r, &t)
        }
        Request::WriteFromPart { part, target } => {
            os::write_from_part(part, target)?;
            Ok(Reply::Done { ok: true, pairs: Vec::new() })
        }
        Request::ReadFile { path } => {
            let bytes = os::read_file(path)?;
            if bytes.len() as u64 > MAX_FILE {
                return Err(Refusal::TooBig.into());
            }
            if !(job.send)(&Reply::Reading { len: bytes.len() as u64 }) {
                return Err(Fail::Failed { denied: false, detail: "leaf went away".into() });
            }
            for chunk in bytes.chunks(leaf_common::admin::DATA_CHUNK) {
                if !(job.send_data)(chunk) {
                    return Err(Fail::Failed { denied: false, detail: "leaf went away".into() });
                }
            }
            Ok(Reply::Done { ok: true, pairs: Vec::new() })
        }
        Request::Hello { .. } | Request::Answer { .. } | Request::Cancel | Request::Bye => Err(Refusal::Unexpected.into()),
    }
}

/// Holds the folders on the way to each item and checks each item; `links_ok`: the item may itself be a link (it is then removed
/// or moved as the link, which the engines do).
fn hold_items(paths: &[PathBuf], links_ok: bool) -> Result<Vec<os::Held>, Fail> {
    let mut held = Vec::with_capacity(paths.len());
    for p in paths {
        held.push(os::hold_parents(p)?);
        let facts = os::check_item(p)?;
        if facts.link && !links_ok {
            return Err(Refusal::Link.into());
        }
    }
    Ok(held)
}

fn transfer(kind: Transfer, sources: &[PathBuf], dest: &Path, job: &Job) -> Result<Reply, Fail> {
    // a source that is a link is refused: copying it as administrator would read wherever it points
    let _held_sources = hold_items(sources, false)?;
    let _held_dest = os::hold_folder(dest)?;
    let t = tracker(job);
    // the same engine as leaf's own copy (with the system's engine for what it leaves); no job journal is written: the helper
    // never writes into leaf's settings folder
    let r = leaf_sys::fastcopy::transfer(kind, sources, dest, t.clone());
    engine_done(r, &t)
}

fn engine_done(r: anyhow::Result<bool>, t: &Tracker) -> Result<Reply, Fail> {
    let pairs = t.results.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match r {
        Ok(ok) => Ok(Reply::Done { ok, pairs }),
        Err(e) => Err(Fail::Failed { denied: os::is_denied(&e), detail: format!("{e:#}") }),
    }
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
}
