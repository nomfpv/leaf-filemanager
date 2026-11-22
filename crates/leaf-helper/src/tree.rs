//! The helper's own delete, copy and move: an iterative walk where every item is opened RELATIVE to its parent's open handle
//! ([`crate::node`]), never by a path again, its kind read from that handle. leaf's own engines (rf-win's `fastcopy`, `fastdelete`
//! and the shell's engine behind them) work by path and are not used by the helper.
//!
//! - Every item is first opened only to look at it; a folder is opened again for listing, a file for reading, only when that is
//!   needed (one unreadable file does not stop a walk that only has to know it is there).
//! - Delete: a link, a junction (on Linux also a mount point, a device, a socket, a pipe) is removed AS ITSELF and never entered;
//!   a folder is removed through its own handle once it is empty; an item that is already gone counts as done.
//! - Copy and move: the destination and every folder above it are compared with every source BY IDENTITY (volume and file id,
//!   device and inode): a destination that is a source or inside one is refused; a name at the destination that is the source
//!   itself is "already there" and left alone (no "Replace" can then destroy it). The whole tree is looked at first; a link
//!   anywhere in it refuses the operation before anything is written ([`Refusal::Link`]), and so does a cloud placeholder.
//!   While copying, a link met (swapped in after the look) stops the operation.
//! - Every copied file is written under a temp name of the helper's own in the destination folder and given its final name only
//!   when complete (over an existing item only when the answer was "Replace"), so a cancel, a failure or an ended helper never
//!   leaves a half-written file under the real name, and "Replace" never loses the old file before the new one is whole. Temp files
//!   of an earlier helper left in a destination folder are removed when the helper writes there again.
//! - A move opens each source file so that no other program may write to it meanwhile (Windows: shared for reading only; a file
//!   another program has open for writing is not moved, it stays and is counted). Before the source is removed, the bytes copied
//!   must equal the source's size after the copy (and on Linux the size and time must not have changed): otherwise it stays and is
//!   counted. A source folder goes only when empty.
//! - Carried over: the bytes, times, and the attributes read-only, hidden, system, archive (Windows) or owner, group and mode
//!   without set-id bits (Linux), folders too. Not carried over: alternate data streams and encryption (Windows): such a file is
//!   not MOVED (it stays, counted) and a COPY of it is counted as incomplete; the file's own permissions (the copy gets the
//!   destination's inherited ones, Windows).
//!
//! No recursion: the walks keep their own stack of open folders, as deep as the tree (one handle per level, not per folder).

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

fn cloud() -> Fail {
    Fail::Failed { denied: false, detail: "a cloud file that is not on this PC (OneDrive and the like) is not touched as administrator".into() }
}

/// Progress in bytes (copy) or items (delete), as the Status Center reads it.
struct Progress<'a> {
    t: &'a Tracker,
    total: u64,
    done: u64,
}

impl Progress<'_> {
    fn add(&mut self, n: u64, name: Option<&OsStr>) {
        let total = self.total.max(1);
        self.done = self.done.saturating_add(n).min(total);
        self.t.bytes.set(self.done, total);
        (self.t.on_progress)(1000, (u128::from(self.done) * 1000 / u128::from(total)) as u32, name.map(|n| n.to_string_lossy().into_owned()));
    }
    fn cancelled(&self) -> bool {
        self.t.cancel.load(Ordering::Relaxed)
    }
}

/// Counts what is under an open folder: (items, bytes). `deny`: a link or cloud file anywhere refuses (copy, move).
fn survey(dir: Node, deny: bool) -> Result<(u64, u64), Fail> {
    let (mut items, mut bytes) = (0u64, 0u64);
    let names = node::children(&dir)?;
    let mut stack: Vec<(Node, Vec<OsString>)> = vec![(dir, names)];
    while !stack.is_empty() {
        let next = stack.last_mut().and_then(|f| f.1.pop());
        let Some(name) = next else {
            stack.pop();
            continue;
        };
        let Some(top) = stack.last() else { break };
        let c = match node::open_child(&top.0, &name, Want::Look) {
            Ok(c) => c,
            Err(e) if is_missing(&e) => continue,
            Err(e) => return Err(e.into()),
        };
        items = items.saturating_add(1);
        match node::kind(&c)? {
            Kind::Link if deny => return Err(Refusal::Link.into()),
            Kind::Cloud if deny => return Err(cloud()),
            Kind::Link | Kind::Cloud => {}
            Kind::File { size, .. } => bytes = bytes.saturating_add(size),
            Kind::Dir => {
                let d = node::reopen(&top.0, &name, &c, Want::List)?;
                let kids = node::children(&d)?;
                stack.push((d, kids));
            }
        }
    }
    Ok((items, bytes))
}

/// The parent folder of a path, held and opened, with the item's name.
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

/// What a delete, copy or move ended with.
#[derive(Default)]
pub struct Outcome {
    pub ok: bool,
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// Named items already gone.
    pub missing: u32,
    /// Move sources that stayed (another program writing them, changed during the copy, data streams or encryption).
    pub kept: u32,
    /// Copies without something of their source (alternate data streams, encryption).
    pub partial: u32,
}

// ---------------------------------------------------------------------------------------------------------------- the temp names

/// The name a copy is written under until it is complete.
fn temp_name() -> OsString {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    OsString::from(format!(".leaf-helper-{}-{}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// `pid` of a helper temp name `prefix<pid>-<n><suffix>` (`None`: not one of ours).
fn temp_pid(name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let rest = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let (pid, n) = rest.split_once('-')?;
    if pid.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// Removes, through `dir`'s handle, the plain files whose name is a helper temp name of ANOTHER helper process (`prefix`, `suffix`:
/// the copy's `.leaf-helper-` … `.part`, or the editor save's `.<name>.leaf-helper-` … `.tmp`). What can remain after a helper was
/// ended half way: such temp files, until a helper writes into that folder again.
pub fn clean_stale(dir: &Node, prefix: &str, suffix: &str) {
    let Ok(names) = node::children(dir) else { return };
    for n in names {
        let Some(pid) = temp_pid(&n.to_string_lossy(), prefix, suffix) else { continue };
        if pid == std::process::id() {
            continue;
        }
        if let Ok(f) = node::open_child(dir, &n, Want::LookDelete) {
            if matches!(node::kind(&f), Ok(Kind::File { .. })) {
                let _ = node::remove(dir, &n, f, false);
            }
        }
    }
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
        match node::open_child(&tp.parent, &tp.name, Want::Look) {
            Ok(n) => {
                total = total.saturating_add(1);
                if node::kind(&n)? == Kind::Dir {
                    let d = node::reopen(&tp.parent, &tp.name, &n, Want::List)?;
                    total = total.saturating_add(survey(d, false)?.0);
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
            return Ok(Outcome { ok: false, missing, ..Default::default() });
        }
        delete_one(&tp.parent, &tp.name, &mut pr, &mut first);
    }
    if let Some(e) = first {
        return Err(e.into());
    }
    Ok(Outcome { ok: !pr.cancelled(), missing, ..Default::default() })
}

fn note(e: std::io::Error, first: &mut Option<std::io::Error>) {
    if !is_missing(&e) && first.is_none() {
        *first = Some(e);
    }
}

/// Opens `name` in `dir` for its removal: (the node, whether it is a folder to empty first).
fn for_removal(dir: &Node, name: &OsStr) -> std::io::Result<(Node, bool)> {
    let look = node::open_child(dir, name, Want::LookDelete)?;
    if node::kind(&look)? == Kind::Dir {
        return Ok((node::reopen(dir, name, &look, Want::ListDelete)?, true));
    }
    Ok((look, false))
}

fn delete_one(parent: &Node, name: &OsStr, pr: &mut Progress, first: &mut Option<std::io::Error>) {
    let (n, is_dir) = match for_removal(parent, name) {
        Ok(x) => x,
        Err(e) => return note(e, first),
    };
    if !is_dir {
        // a file, or a link / junction / mount point removed as itself (never entered)
        if let Err(e) = node::remove(parent, name, n, false) {
            note(e, first);
        }
        pr.add(1, Some(name));
        return;
    }
    let kids = match node::children(&n) {
        Ok(k) => k,
        Err(e) => return note(e, first),
    };
    // (the folder, its name, the names still to do in it)
    let mut stack: Vec<(Node, OsString, Vec<OsString>)> = vec![(n, name.to_os_string(), kids)];
    while !stack.is_empty() {
        if pr.cancelled() {
            return;
        }
        let next = stack.last_mut().and_then(|f| f.2.pop());
        match next {
            Some(child) => {
                let Some(top) = stack.last() else { break };
                match for_removal(&top.0, &child) {
                    Ok((c, true)) => match node::children(&c) {
                        Ok(k) => stack.push((c, child, k)),
                        Err(e) => note(e, first),
                    },
                    Ok((c, false)) => {
                        if let Err(e) = node::remove(&top.0, &child, c, false) {
                            note(e, first);
                        }
                        pr.add(1, Some(&child));
                    }
                    Err(e) => note(e, first),
                }
            }
            None => {
                let Some((d, dname, _)) = stack.pop() else { break };
                let p = stack.last().map(|f| &f.0).unwrap_or(parent);
                if let Err(e) = node::remove(p, &dname, d, true) {
                    note(e, first);
                }
                pr.add(1, Some(&dname));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------- copy / move

/// Copies or moves `sources` into `dest`. Sources already gone are skipped (`missing`). See the module notes for what is refused,
/// kept and carried over.
pub fn transfer(kind: Transfer, sources: &[PathBuf], dest: &Path, t: &Tracker) -> Result<Outcome, Fail> {
    let mv = kind == Transfer::Move;
    let held_dest = os::hold_folder(dest)?;
    let dst = node::held_node(&held_dest, true)?;
    // the destination and every folder above it, by identity
    let chain = node::held_ids(&held_dest);
    let mut tops = Vec::new();
    let (mut missing, mut total) = (0u32, 0u64);
    for p in sources {
        let tp = top(p, mv)?;
        match node::open_child(&tp.parent, &tp.name, Want::Look) {
            Ok(n) => {
                if chain.contains(&node::id(&n)?) {
                    // the destination is this source or inside it: it would copy into itself, or merge into itself and lose its files
                    return Err(Refusal::WrongKind.into());
                }
                match node::kind(&n)? {
                    Kind::Link => return Err(Refusal::Link.into()),
                    Kind::Cloud => return Err(cloud()),
                    Kind::File { size, .. } => total = total.saturating_add(size),
                    Kind::Dir => {
                        let d = node::reopen(&tp.parent, &tp.name, &n, Want::List)?;
                        total = total.saturating_add(survey(d, true)?.1).saturating_add(1);
                    }
                }
                tops.push((tp, node::id(&n)?));
            }
            Err(e) if is_missing(&e) => missing += 1,
            Err(e) => return Err(e.into()),
        }
    }
    clean_stale(&dst, ".leaf-helper-", ".part");
    // names that exist at the destination: the source itself is "already there"; the others are asked as leaf asks (one list)
    let mut clashes = Vec::new();
    let mut choice: Vec<Option<Resolve>> = vec![None; tops.len()];
    for (i, (tp, src_id)) in tops.iter().enumerate() {
        let Ok(there) = node::open_child(&dst, &tp.name, Want::Look) else { continue };
        if node::id(&there)? == *src_id {
            choice[i] = Some(Resolve::Skip);
            continue;
        }
        let src = node::open_child(&tp.parent, &tp.name, Want::Look)?;
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
    if !clashes.is_empty() {
        let list: Vec<Conflict> = clashes.iter().map(|(_, c)| c.clone()).collect();
        let answers = match &t.ask {
            Some(ask) => ask(&list),
            None => Some(vec![Resolve::KeepBoth; list.len()]),
        };
        let Some(answers) = answers.filter(|a| a.len() == list.len()) else { return Ok(Outcome { missing, ..Default::default() }) };
        for ((i, c), a) in clashes.iter().zip(answers) {
            // a file against a folder cannot replace: both are kept
            choice[*i] = Some(if a == Resolve::Replace && c.mismatched() { Resolve::KeepBoth } else { a });
        }
    }
    let mut pr = Progress { t, total, done: 0 };
    let mut out = Outcome { missing, ..Default::default() };
    let mut all = true;
    for ((tp, _), c) in tops.iter().zip(choice) {
        if pr.cancelled() {
            return Ok(out);
        }
        let (target, replace) = match c {
            Some(Resolve::Skip) => continue,
            Some(Resolve::KeepBoth) => (free_name(&dst, &tp.name)?, false),
            Some(Resolve::Replace) => (tp.name.clone(), true),
            None => (tp.name.clone(), false),
        };
        all &= copy_one(&tp.parent, &tp.name, &dst, &target, replace, mv, &mut pr, &mut out)?;
        out.pairs.push((tp.path.clone(), dest.join(&target)));
    }
    out.ok = all && !pr.cancelled() && out.kept == 0;
    Ok(out)
}

/// "name (2).ext", "name (3).ext", … the first that is free in `dir`.
fn free_name(dir: &Node, name: &OsStr) -> Result<OsString, Fail> {
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2..10_000 {
        let cand = OsString::from(format!("{stem} ({n}){ext}"));
        match node::open_child(dir, &cand, Want::Look) {
            Err(e) if is_missing(&e) => return Ok(cand),
            _ => {}
        }
    }
    Err(Fail::Failed { denied: false, detail: "no free name".into() })
}

/// The folder `name` in `dir` to copy into: new, or (merging) the existing real folder, its kind read from the merge handle itself.
fn dir_for(dir: &Node, name: &OsStr, merge: bool) -> Result<Node, Fail> {
    if merge {
        match node::open_child(dir, name, Want::Merge) {
            Ok(n) => {
                return match node::kind(&n)? {
                    Kind::Dir => {
                        clean_stale(&n, ".leaf-helper-", ".part");
                        Ok(n)
                    }
                    Kind::Link => Err(Refusal::Link.into()),
                    _ => Err(Refusal::WrongKind.into()),
                };
            }
            Err(e) if is_missing(&e) => {}
            // a link there (Linux answers ELOOP), or a file where a folder was looked for
            Err(e) if matches!(e.raw_os_error(), Some(40) | Some(20) | Some(267)) => return Err(Refusal::WrongKind.into()),
            Err(e) => return Err(e.into()),
        }
    }
    node::make_dir(dir, name).map_err(Into::into)
}

#[cfg(test)]
thread_local! {
    /// (tests) Bytes added to the size a move source has after its copy: a source that grew meanwhile.
    pub static GROW: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What happened to one file.
#[derive(PartialEq, Eq)]
enum Filed {
    Done,
    /// A move's source that stayed where it was.
    Kept,
    Cancelled,
}

/// Copies (or moves) the file `name` of `src_dir` (seen as `look`) to `target` in `dst_dir`, through a temp file.
#[allow(clippy::too_many_arguments)]
fn copy_file(src_dir: &Node, name: &OsStr, look: &Node, dst_dir: &Node, target: &OsStr, replace: bool, mv: bool, pr: &mut Progress, out: &mut Outcome) -> Result<Filed, Fail> {
    pr.add(0, Some(name));
    let src = match node::reopen(src_dir, name, look, Want::Read { move_source: mv }) {
        Ok(s) => s,
        // another program has the file open for writing (32: sharing violation): it is not moved, it stays
        Err(e) if mv && e.raw_os_error() == Some(32) => {
            out.kept += 1;
            return Ok(Filed::Kept);
        }
        Err(e) => return Err(e.into()),
    };
    if node::extras(&src)? {
        if mv {
            out.kept += 1;
            return Ok(Filed::Kept);
        }
        out.partial += 1;
    }
    let stamp = node::stamp(&src)?;
    let before = node::meta(&src).ok().map(|m| (m.len(), m.modified().ok()));
    let tmp = temp_name();
    let mut f = node::make_file(dst_dir, &tmp)?;
    let mut input = node::reader(&src);
    let mut buf = vec![0u8; 1 << 20];
    let mut copied = 0u64;
    let r = (|| -> std::io::Result<bool> {
        loop {
            if pr.cancelled() {
                return Ok(false);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n])?;
            copied += n as u64;
            pr.add(n as u64, None);
        }
        f.sync_all()?;
        Ok(true)
    })();
    match r {
        Ok(true) => {}
        Ok(false) => {
            node::discard(dst_dir, &tmp, f);
            return Ok(Filed::Cancelled);
        }
        Err(e) => {
            node::discard(dst_dir, &tmp, f);
            return Err(e.into());
        }
    }
    if mv {
        // every byte the source has now, and (Linux, no share modes) unchanged since the start
        #[allow(unused_mut)]
        let mut size_after = node::size_now(&src)?;
        #[cfg(test)]
        {
            size_after += GROW.with(|g| g.get());
        }
        let after = node::meta(&src).ok().map(|m| (size_after, m.modified().ok()));
        let unchanged = copied == size_after && (cfg!(windows) || before == after);
        if !unchanged {
            node::discard(dst_dir, &tmp, f);
            out.kept += 1;
            return Ok(Filed::Kept);
        }
    }
    if let Err(e) = node::finish_file(dst_dir, &f, &tmp, target, replace) {
        node::discard(dst_dir, &tmp, f);
        return Err(e.into());
    }
    let _ = node::apply_file(&f, &stamp);
    drop(f);
    if mv {
        node::sync_dir(dst_dir);
        node::remove(src_dir, name, src, false)?;
    }
    Ok(Filed::Done)
}

/// Copies (or moves) the item `name` of `src_parent` to `target` in `dst_parent`. `false`: not all done (cancelled, a source
/// kept, or a moved folder not removed because something is still in it).
#[allow(clippy::too_many_arguments)]
fn copy_one(src_parent: &Node, name: &OsStr, dst_parent: &Node, target: &OsStr, replace: bool, mv: bool, pr: &mut Progress, out: &mut Outcome) -> Result<bool, Fail> {
    // (only looked at: a move's source file is opened again shared for reading only, which a handle with delete access would block)
    let look = node::open_child(src_parent, name, Want::Look)?;
    match node::kind(&look)? {
        Kind::Link => return Err(Refusal::Link.into()),
        Kind::Cloud => return Err(cloud()),
        Kind::File { .. } => return Ok(copy_file(src_parent, name, &look, dst_parent, target, replace, mv, pr, out)? == Filed::Done),
        Kind::Dir => {}
    }
    let src = node::reopen(src_parent, name, &look, if mv { Want::ListDelete } else { Want::List })?;
    let stamp = node::stamp(&src)?;
    let d = dir_for(dst_parent, target, replace)?;
    let kids = node::children(&src)?;
    // (source folder, target folder, the source folder's name, its times and attributes, names still to do)
    let mut stack: Vec<(Node, Node, OsString, node::Stamp, Vec<OsString>)> = vec![(src, d, name.to_os_string(), stamp, kids)];
    let mut all = true;
    while !stack.is_empty() {
        if pr.cancelled() {
            return Ok(false);
        }
        let next = stack.last_mut().and_then(|f| f.4.pop());
        match next {
            Some(child) => {
                let Some(f) = stack.last() else { break };
                let c = match node::open_child(&f.0, &child, Want::Look) {
                    Ok(c) => c,
                    Err(e) if is_missing(&e) => continue,
                    Err(e) => return Err(e.into()),
                };
                match node::kind(&c)? {
                    // a link where the look found none: swapped in meanwhile; stop here
                    Kind::Link => return Err(Refusal::Link.into()),
                    Kind::Cloud => return Err(cloud()),
                    Kind::File { .. } => match copy_file(&f.0, &child, &c, &f.1, &child, replace, mv, pr, out)? {
                        Filed::Done => {}
                        Filed::Kept => all = false,
                        Filed::Cancelled => return Ok(false),
                    },
                    Kind::Dir => {
                        let s = node::reopen(&f.0, &child, &c, if mv { Want::ListDelete } else { Want::List })?;
                        let stamp = node::stamp(&s)?;
                        let sub = dir_for(&f.1, &child, replace)?;
                        let kids = node::children(&s)?;
                        stack.push((s, sub, child, stamp, kids));
                    }
                }
            }
            None => {
                let Some((s, d, sname, stamp, _)) = stack.pop() else { break };
                // the folder's times last (what was written into it changed them)
                let _ = node::apply_dir(&d, &stamp);
                if mv {
                    let p = stack.last().map(|f| &f.0).unwrap_or(src_parent);
                    // only when empty: what stayed or appeared in it meanwhile stays, and so does the folder
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
