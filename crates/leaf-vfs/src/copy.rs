//! A small generic copy between two `Fs` (a file or a whole tree): the seed of `leaf-copy` (PLAN 2.2). Not used by the app
//! yet; local to local copies on Windows keep `fastcopy`.
//!
//! - every file is written under a temporary name next to its target, then renamed into place, so a half-written file
//!   never carries the real name; on cancel or error the partial file is removed;
//! - a name that is there already asks the conflict callback (skip, replace, keep both, cancel);
//! - the modification time is kept when the target can set it;
//! - links are never followed (a link in the tree is skipped and reported);
//! - every name read from the source is checked before it becomes part of a target path (`names::vchild`: no `..`, no
//!   separators, no drive letters or streams, no device names...), and the target's parent must be the folder it goes into; a
//!   bad name is left out and reported in `CopyReport::refused`. With both, a copy cannot wander out of the target folder.

use std::io::{Read, Write};

use crate::{Fs, FsError, Kind, Meta, MetaChange, VPath, WriteOpts, names};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    Skip,
    Replace,
    /// Copy under the next free name (`name (2).ext`).
    KeepBoth,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct CopyOpts {
    pub keep_mtime: bool,
    /// Bytes read and written at a time (the cancel flag is looked at between two).
    pub chunk: usize,
}

impl Default for CopyOpts {
    fn default() -> Self {
        CopyOpts { keep_mtime: true, chunk: 256 * 1024 }
    }
}

/// Where a copy stands, after every chunk.
#[derive(Debug, Clone)]
pub struct Progress<'a> {
    /// The source file being copied.
    pub file: &'a VPath,
    pub file_done: u64,
    pub file_size: u64,
    pub files_done: u64,
    pub bytes_done: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyReport {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// What was left out, with the reason ("link", "skipped": the conflict answer).
    pub skipped: Vec<(VPath, &'static str)>,
    /// Names in the source that cannot be a local name or lead out of the folder: (the source folder, the name, why). Never
    /// written anywhere.
    pub refused: Vec<(VPath, String, &'static str)>,
}

pub struct Copy<'a> {
    pub from_fs: &'a dyn Fs,
    pub to_fs: &'a dyn Fs,
    pub opts: CopyOpts,
    pub cancelled: &'a dyn Fn() -> bool,
    pub on_progress: &'a mut dyn FnMut(&Progress),
    pub on_conflict: &'a mut dyn FnMut(&VPath, &Meta) -> Conflict,
    /// Called with the part file once its bytes are written, before it gets its name (the app marks downloads there).
    pub on_written: Option<&'a mut dyn FnMut(&VPath)>,
    report: CopyReport,
    depth: usize,
}

/// Folders below the copied one are followed this deep at most (then the copy stops with an error): a hostile listing ends
/// in an error, never in a stack overflow.
pub const MAX_DEPTH: usize = 64;

/// The error of a tree deeper than `MAX_DEPTH`.
pub fn too_deep() -> FsError {
    FsError::Other(format!("The folders go deeper than {MAX_DEPTH} levels (a folder that contains itself?); leaf stopped there"))
}

/// The temporary name a file is written under before it gets its own.
pub fn part_name(name: &str) -> String {
    format!(".{name}.leaf-part")
}

impl<'a> Copy<'a> {
    pub fn new(
        from_fs: &'a dyn Fs,
        to_fs: &'a dyn Fs,
        cancelled: &'a dyn Fn() -> bool,
        on_progress: &'a mut dyn FnMut(&Progress),
        on_conflict: &'a mut dyn FnMut(&VPath, &Meta) -> Conflict,
    ) -> Copy<'a> {
        Copy { from_fs, to_fs, opts: CopyOpts::default(), cancelled, on_progress, on_conflict, on_written: None, report: CopyReport::default(), depth: 0 }
    }

    /// Copies `from` (a file or a folder) to `to` (the new item's own path, not the folder it goes into). A folder that is
    /// there already is merged into.
    pub fn run(mut self, from: &VPath, to: &VPath) -> Result<CopyReport, FsError> {
        if std::ptr::addr_eq(self.from_fs, self.to_fs) && to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be copied into itself".into()));
        }
        let m = self.from_fs.stat(from)?;
        self.item(from, &m, to)?;
        Ok(self.report)
    }

    fn item(&mut self, from: &VPath, m: &Meta, to: &VPath) -> Result<(), FsError> {
        if (self.cancelled)() {
            return Err(FsError::Cancelled);
        }
        match m.kind {
            Kind::Link => {
                self.report.skipped.push((from.clone(), "link"));
                Ok(())
            }
            Kind::Dir => self.dir(from, m, to),
            Kind::File => self.file(from, m, to),
        }
    }

    fn dir(&mut self, from: &VPath, m: &Meta, to: &VPath) -> Result<(), FsError> {
        // a server can list a folder inside itself forever (or a loop of mounts): stop at a fixed depth with an error, long
        // before the stack runs out
        if self.depth >= MAX_DEPTH {
            return Err(too_deep());
        }
        self.depth += 1;
        let r = self.dir_in(from, m, to);
        self.depth -= 1;
        r
    }

    fn dir_in(&mut self, from: &VPath, m: &Meta, to: &VPath) -> Result<(), FsError> {
        let to = match self.to_fs.stat(to) {
            Ok(t) if t.is_dir() => to.clone(),
            Ok(t) => match (self.on_conflict)(to, &t) {
                Conflict::Cancel => return Err(FsError::Cancelled),
                Conflict::KeepBoth => {
                    let free = self.free_name(to)?;
                    self.to_fs.mkdir(&free)?;
                    free
                }
                // a file is not replaced by a folder here
                Conflict::Skip | Conflict::Replace => {
                    self.report.skipped.push((from.clone(), "skipped"));
                    return Ok(());
                }
            },
            Err(FsError::NotFound) => {
                self.to_fs.mkdir(to)?;
                to.clone()
            }
            Err(e) => return Err(e),
        };
        self.report.dirs += 1;
        let mut children = Vec::new();
        self.from_fs.list(from, 1024, self.cancelled, &mut |b| children.extend(b))?;
        for e in children {
            // the source's names are not trusted: checked before they become part of a target path
            let (src, dst) = match (names::vchild(from, &e.name), names::vchild(&to, &e.name)) {
                (Ok(s), Ok(d)) => (s, d),
                (Err(why), _) | (_, Err(why)) => {
                    self.report.refused.push((from.clone(), e.name.to_string(), why));
                    continue;
                }
            };
            let meta = Meta::from_entry(&e);
            self.item(&src, &meta, &dst)?;
        }
        if self.opts.keep_mtime && self.to_fs.caps().set_times && m.modified_ms > 0 {
            let _ = self.to_fs.set_meta(&to, &MetaChange { modified_ms: Some(m.modified_ms), mode: None });
        }
        Ok(())
    }

    fn file(&mut self, from: &VPath, m: &Meta, to: &VPath) -> Result<(), FsError> {
        let (target, replace) = match self.to_fs.stat(to) {
            Ok(t) => match (self.on_conflict)(to, &t) {
                Conflict::Cancel => return Err(FsError::Cancelled),
                Conflict::Skip => {
                    self.report.skipped.push((from.clone(), "skipped"));
                    return Ok(());
                }
                Conflict::Replace if t.kind == Kind::File => (to.clone(), true),
                Conflict::Replace => {
                    self.report.skipped.push((from.clone(), "skipped"));
                    return Ok(());
                }
                Conflict::KeepBoth => (self.free_name(to)?, false),
            },
            Err(FsError::NotFound) => (to.clone(), false),
            Err(e) => return Err(e),
        };
        let parent = target.parent().ok_or(FsError::Denied)?;
        let part = parent.join(&part_name(target.name()));
        let r = self.write_part(from, m, &part);
        let r = r.and_then(|_| {
            // (before the time is set: writing a stream may touch it)
            if let Some(f) = self.on_written.as_mut() {
                f(&part);
            }
            if self.opts.keep_mtime && self.to_fs.caps().set_times && m.modified_ms > 0 {
                self.to_fs.set_meta(&part, &MetaChange { modified_ms: Some(m.modified_ms), mode: None })?;
            }
            self.to_fs.rename(&part, &target, replace)
        });
        if let Err(e) = r {
            let _ = self.to_fs.remove(&part, false);
            return Err(e);
        }
        self.report.files += 1;
        Ok(())
    }

    fn write_part(&mut self, from: &VPath, m: &Meta, part: &VPath) -> Result<(), FsError> {
        let mut src = self.from_fs.open_read(from, 0)?;
        let mut sink = self.to_fs.open_write(part, WriteOpts::REPLACE)?;
        let mut buf = vec![0u8; self.opts.chunk.max(1)];
        let mut done = 0u64;
        loop {
            if (self.cancelled)() {
                return Err(FsError::Cancelled);
            }
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            sink.write_all(&buf[..n])?;
            done += n as u64;
            self.report.bytes += n as u64;
            (self.on_progress)(&Progress { file: from, file_done: done, file_size: m.size, files_done: self.report.files, bytes_done: self.report.bytes });
        }
        sink.finish()
    }

    /// `name (2).ext`, `name (3).ext`... the first one not taken.
    fn free_name(&self, to: &VPath) -> Result<VPath, FsError> {
        let parent = to.parent().ok_or(FsError::Denied)?;
        let name = to.name();
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        for n in 2..10_000 {
            let cand = parent.join(&format!("{stem} ({n}){ext}"));
            match self.to_fs.stat(&cand) {
                Err(FsError::NotFound) => return Ok(cand),
                Err(e) => return Err(e),
                Ok(_) => {}
            }
        }
        Err(FsError::Exists)
    }
}

/// Disk space a fetch or a download leaves free at least.
pub const KEEP_FREE: u64 = 64 << 20;

/// Fetches ONE file of `fs` into the local file `dest` (a cache copy that is opened next): refused unless the source is a
/// regular file when it is looked at here, never a folder or a link, so no tree is ever copied and no name of the source is
/// used for a local path. At most `max` bytes are taken, counted as they arrive (not the size the source claims), and no
/// more than the disk can hold with `KEEP_FREE` left. Written under a part name first, marked as downloaded (`mark_from`: the
/// address shown in the mark) and then moved over an older copy at `dest` (which may be read-only: it is the cache's own).
pub fn fetch_file(
    fs: &dyn Fs,
    from: &VPath,
    dest: &std::path::Path,
    max: u64,
    mark_from: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Meta, FsError> {
    let m = fs.stat(from)?;
    if m.kind != Kind::File {
        return Err(FsError::Other("This is not a regular file on the server (it is a folder or a link now)".into()));
    }
    if m.size > max {
        return Err(FsError::Other(format!("The file is bigger than {} MB", max >> 20)));
    }
    let dir = dest.parent().ok_or(FsError::Denied)?;
    let name = dest.file_name().and_then(|n| n.to_str()).ok_or(FsError::Denied)?;
    let free = leaf_sys::fileattr::free_space(dir);
    let room = if free == 0 { max } else { free.saturating_sub(KEEP_FREE) };
    if m.size > room {
        return Err(FsError::Other("There is not enough free space on this PC for the file".into()));
    }
    let budget = max.min(room);
    let part = dir.join(part_name(name));
    let r = (|| -> Result<(), FsError> {
        let mut src = fs.open_read(from, 0)?;
        let mut out = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&part)?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut done = 0u64;
        loop {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            done = done.saturating_add(n as u64);
            if done > budget {
                return Err(FsError::Other(if done > max {
                    format!("The server sent more than {} MB for this file; leaf stopped", max >> 20)
                } else {
                    "There is not enough free space on this PC for the file".into()
                }));
            }
            out.write_all(&buf[..n])?;
            progress(done, m.size.max(done));
        }
        out.flush()?;
        drop(out);
        // the mark first: writing a stream may touch the time
        if let Some(from) = mark_from
            && let Err(e) = leaf_sys::fileinfo::mark_downloaded(&part, from)
        {
            log::debug!("fetch: no download mark on {}: {e}", part.display());
        }
        if m.modified_ms > 0
            && let Ok(f) = std::fs::OpenOptions::new().write(true).open(&part)
        {
            let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_millis(m.modified_ms as u64));
        }
        if let Ok(old) = std::fs::symlink_metadata(dest) {
            if !old.is_file() {
                return Err(FsError::Exists);
            }
            let mut perm = old.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(dest, perm);
        }
        std::fs::rename(&part, dest)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    r.map(|_| m)
}
