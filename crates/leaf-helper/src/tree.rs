//! The helper's own delete, copy and move: an iterative walk where every item is opened RELATIVE to its parent's open handle
//! ([`crate::node`]), never by a path again, its kind read from that handle. leaf's own engines (rf-win's `fastcopy`, `fastdelete`
//! and the shell's engine behind them) work by path and are not used by the helper.
//!
//! - Delete: a link, a junction or any other reparse point met in the tree is removed AS ITSELF and never entered; a folder is
//!   removed through its own handle once it is empty; an item that is already gone counts as done.
//! - Copy and move: the whole tree is looked at first; a link anywhere in it refuses the operation before anything is written
//!   ([`Refusal::Link`]). While copying, each item is opened relative to its parent again and a link met then (one swapped in after
//!   the look) stops the operation. Every file and folder at the destination is created NEW relative to the destination folder's
//!   handle (never an existing name, never through a link); "Replace" first removes the existing plain file (or a link as itself).
//!   A move copies each file and then removes that source file through its handle; a source folder is removed when it is empty
//!   (what appeared in it meanwhile stays). So a moved item always gets the destination's inherited permissions.
//!   Not copied: alternate data streams, the file's own permissions (the copy gets the destination's), attributes other than
//!   the modification time (and on Linux the permission bits).
//!
//! No recursion: the walk keeps its own stack (a deep tree cannot overflow the thread's stack).

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use leaf_common::admin::Refusal;
use leaf_sys::fastcopy::{Conflict, Resolve};
use leaf_sys::fileops::{Tracker, Transfer};

use crate::Fail;
use crate::node::{self, Kind, Node, Want};
use crate::os;

fn is_missing(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(3)
}

/// Progress in bytes (copy) or items (delete), as the Status Center reads it.
struct Progress<'a> {
    t: &'a Tracker,
    total: u64,
    done: u64,
}

impl Progress<'_> {
    fn add(&mut self, n: u64, name: Option<&OsStr>) {
        self.done = (self.done + n).min(self.total.max(1));
        let total = self.total.max(1);
        self.t.bytes.set(self.done, total);
        (self.t.on_progress)(1000, (u128::from(self.done) * 1000 / u128::from(total)) as u32, name.map(|n| n.to_string_lossy().into_owned()));
    }
    fn cancelled(&self) -> bool {
        self.t.cancel.load(Ordering::Relaxed)
    }
}

/// Counts what is under an open folder: (items, bytes). `deny_links`: a link anywhere refuses (copy, move).
fn survey(dir: Node, deny_links: bool) -> Result<(u64, u64), Fail> {
    let (mut items, mut bytes) = (0u64, 0u64);
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for name in node::children(&d)? {
            let c = match node::open_child(&d, &name, Want::Read) {
                Ok(c) => c,
                Err(e) if is_missing(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            items += 1;
            match node::kind(&c)? {
                Kind::Link if deny_links => return Err(Refusal::Link.into()),
                Kind::Link => {}
                Kind::File(n) => bytes += n,
                Kind::Dir => stack.push(c),
            }
        }
    }
    Ok((items, bytes))
}

/// The parent folder of each path, held and opened, with the item's name.
struct Top {
    _held: os::Held,
    parent: Node,
    name: OsString,
    path: PathBuf,
}

fn top(p: &Path, write: bool) -> Result<Top, Fail> {
    let held = os::hold_parents(p)?;
    let parent = node::held_node(&held, write)?;
    let name = p.file_name().ok_or(Refusal::WrongKind)?.to_os_string();
    Ok(Top { _held: held, parent, name, path: p.to_path_buf() })
}

/// What a delete or copy ended with.
pub struct Outcome {
    pub ok: bool,
    pub pairs: Vec<(PathBuf, PathBuf)>,
    pub missing: u32,
}

// ---------------------------------------------------------------------------------------------------------------- delete

/// Deletes each item for good. Items already gone count as done (`missing`). Failures inside are counted, the rest goes on; the
/// first one is the error when anything failed.
pub fn delete(paths: &[PathBuf], t: &Tracker) -> Result<Outcome, Fail> {
    let mut tops = Vec::new();
    let mut missing = 0;
    let mut total = 0u64;
    for p in paths {
        let tp = top(p, false)?;
        match node::open_child(&tp.parent, &tp.name, Want::Read) {
            Ok(n) => {
                total += 1;
                if node::kind(&n)? == Kind::Dir {
                    total += survey(n, false)?.0;
                }
                tops.push(tp);
            }
            Err(e) if is_missing(&e) => missing += 1,
            Err(e) => return Err(e.into()),
        }
    }
    let mut pr = Progress { t, total, done: 0 };
    let mut first: Option<std::io::Error> = None;
    for tp in &tops {
        if pr.cancelled() {
            return Ok(Outcome { ok: false, pairs: Vec::new(), missing });
        }
        delete_one(&tp.parent, &tp.name, &mut pr, &mut first);
    }
    if let Some(e) = first {
        return Err(e.into());
    }
    Ok(Outcome { ok: !pr.cancelled(), pairs: Vec::new(), missing })
}

fn delete_one(parent: &Node, name: &OsStr, pr: &mut Progress, first: &mut Option<std::io::Error>) {
    let fail = |e: std::io::Error, first: &mut Option<std::io::Error>| {
        if !is_missing(&e) && first.is_none() {
            *first = Some(e);
        }
    };
    let n = match node::open_child(parent, name, Want::ReadDelete) {
        Ok(n) => n,
        Err(e) => return fail(e, first),
    };
    match node::kind(&n) {
        Ok(Kind::Dir) => {}
        Ok(_) => {
            if let Err(e) = node::remove(parent, name, n, false) {
                fail(e, first);
            }
            pr.add(1, Some(name));
            return;
        }
        Err(e) => return fail(e, first),
    }
    // folders: (the folder, its name, the names still to do in it)
    let kids = match node::children(&n) {
        Ok(k) => k,
        Err(e) => return fail(e, first),
    };
    let mut stack: Vec<(Node, OsString, Vec<OsString>)> = vec![(n, name.to_os_string(), kids)];
    while !stack.is_empty() {
        if pr.cancelled() {
            return;
        }
        let next = stack.last_mut().and_then(|f| f.2.pop());
        match next {
            Some(child) => {
                let Some(top) = stack.last() else { break };
                let c = match node::open_child(&top.0, &child, Want::ReadDelete) {
                    Ok(c) => c,
                    Err(e) => {
                        fail(e, first);
                        continue;
                    }
                };
                match node::kind(&c) {
                    Ok(Kind::Dir) => match node::children(&c) {
                        Ok(k) => stack.push((c, child, k)),
                        Err(e) => fail(e, first),
                    },
                    // a file, or a link / junction removed as itself (never entered)
                    Ok(_) => {
                        if let Err(e) = node::remove(&top.0, &child, c, false) {
                            fail(e, first);
                        }
                        pr.add(1, Some(&child));
                    }
                    Err(e) => fail(e, first),
                }
            }
            None => {
                let Some((d, dname, _)) = stack.pop() else { break };
                let p = stack.last().map(|f| &f.0).unwrap_or(parent);
                if let Err(e) = node::remove(p, &dname, d, true) {
                    fail(e, first);
                }
                pr.add(1, Some(&dname));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------- copy / move

/// Copies or moves `sources` into `dest`. Sources already gone are skipped (`missing`). A link anywhere in a source refuses the
/// whole operation before anything is written.
pub fn transfer(kind: Transfer, sources: &[PathBuf], dest: &Path, t: &Tracker) -> Result<Outcome, Fail> {
    let mv = kind == Transfer::Move;
    let want = if mv { Want::ReadDelete } else { Want::Read };
    // a folder into itself (or below itself) never ends
    if sources.iter().any(|s| dest.starts_with(s)) {
        return Err(Refusal::WrongKind.into());
    }
    let mut tops = Vec::new();
    let (mut missing, mut total) = (0u32, 0u64);
    for p in sources {
        let tp = top(p, mv)?;
        match node::open_child(&tp.parent, &tp.name, Want::Read) {
            Ok(n) => {
                match node::kind(&n)? {
                    Kind::Link => return Err(Refusal::Link.into()),
                    Kind::File(b) => total += b,
                    Kind::Dir => total += survey(n, true)?.1 + 1,
                }
                tops.push(tp);
            }
            Err(e) if is_missing(&e) => missing += 1,
            Err(e) => return Err(e.into()),
        }
    }
    let held_dest = os::hold_folder(dest)?;
    let dst = node::held_node(&held_dest, true)?;
    // names that exist at the destination: asked as leaf asks (one list)
    let mut clashes = Vec::new();
    for (i, tp) in tops.iter().enumerate() {
        if let Ok(there) = node::open_child(&dst, &tp.name, Want::Read) {
            let src = node::open_child(&tp.parent, &tp.name, Want::Read)?;
            let (sm, dm) = (node::meta(&src).ok(), node::meta(&there).ok());
            clashes.push((
                i,
                Conflict {
                    src: tp.path.clone(),
                    dst: dest.join(&tp.name),
                    src_dir: node::kind(&src)? == Kind::Dir,
                    dst_dir: node::kind(&there)? == Kind::Dir,
                    src_size: sm.as_ref().map_or(0, |m| m.len()),
                    dst_size: dm.as_ref().map_or(0, |m| m.len()),
                    src_time: sm.and_then(|m| m.modified().ok()),
                    dst_time: dm.and_then(|m| m.modified().ok()),
                },
            ));
        }
    }
    let mut choice: Vec<Option<Resolve>> = vec![None; tops.len()];
    if !clashes.is_empty() {
        let list: Vec<Conflict> = clashes.iter().map(|(_, c)| c.clone()).collect();
        let answers = match &t.ask {
            Some(ask) => ask(&list),
            None => Some(vec![Resolve::KeepBoth; list.len()]),
        };
        let Some(answers) = answers.filter(|a| a.len() == list.len()) else { return Ok(Outcome { ok: false, pairs: Vec::new(), missing }) };
        for ((i, c), a) in clashes.iter().zip(answers) {
            // a file against a folder cannot replace: both are kept
            choice[*i] = Some(if a == Resolve::Replace && c.mismatched() { Resolve::KeepBoth } else { a });
        }
    }
    let mut pr = Progress { t, total, done: 0 };
    let mut pairs = Vec::new();
    let mut all = true;
    for (tp, c) in tops.iter().zip(choice) {
        if pr.cancelled() {
            return Ok(Outcome { ok: false, pairs, missing });
        }
        let (target, replace) = match c {
            Some(Resolve::Skip) => continue,
            Some(Resolve::KeepBoth) => (free_name(&dst, &tp.name)?, false),
            Some(Resolve::Replace) => (tp.name.clone(), true),
            None => (tp.name.clone(), false),
        };
        all &= copy_one(&tp.parent, &tp.name, &dst, &target, replace, want, &mut pr)?;
        pairs.push((tp.path.clone(), dest.join(&target)));
    }
    Ok(Outcome { ok: all && !pr.cancelled(), pairs, missing })
}

/// "name (2).ext", "name (3).ext", … the first that is free in `dir`.
fn free_name(dir: &Node, name: &OsStr) -> Result<OsString, Fail> {
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2..10_000 {
        let cand = OsString::from(format!("{stem} ({n}){ext}"));
        match node::open_child(dir, &cand, Want::Read) {
            Err(e) if is_missing(&e) => return Ok(cand),
            _ => {}
        }
    }
    Err(Fail::Failed { denied: false, detail: "no free name".into() })
}

/// Makes room for `name` in `dir` ("Replace"): a plain file or a link is removed (a link as itself); a folder stays (merged into).
fn clear_for_file(dir: &Node, name: &OsStr) -> Result<(), Fail> {
    match node::open_child(dir, name, Want::ReadDelete) {
        Ok(n) => match node::kind(&n)? {
            Kind::Dir => Err(Refusal::WrongKind.into()),
            _ => node::remove(dir, name, n, false).map_err(Into::into),
        },
        Err(e) if is_missing(&e) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The folder `name` in `dir` to copy into: new, or (merging) the existing real folder.
fn dir_for(dir: &Node, name: &OsStr, merge: bool) -> Result<Node, Fail> {
    if merge {
        match node::open_child(dir, name, Want::Read) {
            Ok(n) => match node::kind(&n)? {
                Kind::Dir => return node::open_child(dir, name, Want::Merge).map_err(Into::into),
                Kind::Link => return Err(Refusal::Link.into()),
                Kind::File(_) => return Err(Refusal::WrongKind.into()),
            },
            Err(e) if is_missing(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    node::make_dir(dir, name).map_err(Into::into)
}

/// Copies one file from its open handle into a new file `name` in `dst`; the new file goes again when it fails or is cancelled.
fn copy_file(src: &Node, dst: &Node, name: &OsStr, replace: bool, pr: &mut Progress) -> Result<bool, Fail> {
    if replace {
        clear_for_file(dst, name)?;
    }
    let meta = node::meta(src).ok();
    let mut out = node::make_file(dst, name)?;
    let mut input = node::reader(src);
    let mut buf = vec![0u8; 1 << 20];
    let r = (|| -> std::io::Result<bool> {
        loop {
            if pr.cancelled() {
                return Ok(false);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            pr.add(n as u64, None);
        }
        out.sync_all()?;
        if let Some(m) = &meta {
            if let Ok(t) = m.modified() {
                let _ = out.set_times(std::fs::FileTimes::new().set_modified(t));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = out.set_permissions(std::fs::Permissions::from_mode(m.permissions().mode() & 0o777));
            }
        }
        Ok(true)
    })();
    match r {
        Ok(true) => Ok(true),
        Ok(false) => {
            node::discard(dst, name, out);
            Ok(false)
        }
        Err(e) => {
            node::discard(dst, name, out);
            Err(e.into())
        }
    }
}

/// Copies (or moves) the item `name` of `src_parent` to `target` in `dst_parent`. `false`: not all done (cancelled, or a moved
/// folder could not be removed because something new is in it).
fn copy_one(src_parent: &Node, name: &OsStr, dst_parent: &Node, target: &OsStr, replace: bool, want: Want, pr: &mut Progress) -> Result<bool, Fail> {
    let mv = want == Want::ReadDelete;
    let n = node::open_child(src_parent, name, want)?;
    match node::kind(&n)? {
        Kind::Link => return Err(Refusal::Link.into()),
        Kind::File(_) => {
            pr.add(0, Some(name));
            // (the source file stays open for its removal after the copy)
            let done = copy_file(&n, dst_parent, target, replace, pr)?;
            if done && mv {
                node::remove(src_parent, name, n, false)?;
            }
            return Ok(done);
        }
        Kind::Dir => {}
    }
    let d = dir_for(dst_parent, target, replace)?;
    let kids = node::children(&n)?;
    // (source folder, target folder, the source folder's name, names still to do)
    let mut stack: Vec<(Node, Node, OsString, Vec<OsString>)> = vec![(n, d, name.to_os_string(), kids)];
    let mut all = true;
    while !stack.is_empty() {
        if pr.cancelled() {
            return Ok(false);
        }
        let next = stack.last_mut().and_then(|f| f.3.pop());
        match next {
            Some(child) => {
                let Some(f) = stack.last() else { break };
                let c = match node::open_child(&f.0, &child, want) {
                    Ok(c) => c,
                    Err(e) if is_missing(&e) => continue,
                    Err(e) => return Err(e.into()),
                };
                match node::kind(&c)? {
                    // a link where the look found none: swapped in meanwhile; stop here
                    Kind::Link => return Err(Refusal::Link.into()),
                    Kind::File(_) => {
                        pr.add(0, Some(&child));
                        let done = copy_file(&c, &f.1, &child, replace, pr)?;
                        if !done {
                            return Ok(false);
                        }
                        if mv {
                            node::remove(&f.0, &child, c, false)?;
                        }
                    }
                    Kind::Dir => {
                        let sub = dir_for(&f.1, &child, replace)?;
                        let kids = node::children(&c)?;
                        stack.push((c, sub, child, kids));
                    }
                }
            }
            None => {
                let Some((s, _, sname, _)) = stack.pop() else { break };
                if mv {
                    let p = stack.last().map(|f| &f.0).unwrap_or(src_parent);
                    // only when empty: what appeared in it meanwhile stays, and so does the folder
                    if node::remove(p, &sname, s, true).is_err() {
                        all = false;
                    }
                }
                pr.add(1, None);
            }
        }
    }
    Ok(all)
}
