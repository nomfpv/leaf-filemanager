//! A small generic copy between two `Fs` (a file or a whole tree): the seed of `leaf-copy` (PLAN 2.2). Not used by the app
//! yet; local to local copies on Windows keep `fastcopy`.
//!
//! - every file is written under a temporary name next to its target, then renamed into place, so a half-written file
//!   never carries the real name; on cancel or error the partial file is removed;
//! - a name that is there already asks the conflict callback (skip, replace, keep both, cancel);
//! - the modification time is kept when the target can set it;
//! - links are never followed (a link in the tree is skipped and reported), so a copy cannot wander out of the tree.

use std::io::{Read, Write};

use crate::{Fs, FsError, Kind, Meta, MetaChange, VPath, WriteOpts};

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
}

pub struct Copy<'a> {
    pub from_fs: &'a dyn Fs,
    pub to_fs: &'a dyn Fs,
    pub opts: CopyOpts,
    pub cancelled: &'a dyn Fn() -> bool,
    pub on_progress: &'a mut dyn FnMut(&Progress),
    pub on_conflict: &'a mut dyn FnMut(&VPath, &Meta) -> Conflict,
    report: CopyReport,
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
        Copy { from_fs, to_fs, opts: CopyOpts::default(), cancelled, on_progress, on_conflict, report: CopyReport::default() }
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
            let src = from.join(&e.name);
            let meta = Meta::from_entry(&e);
            self.item(&src, &meta, &to.join(&e.name))?;
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
