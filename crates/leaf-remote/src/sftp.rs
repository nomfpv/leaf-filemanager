//! `SftpFs`: `sftp://user@host:port/path` through `leaf-vfs`: list, stat, read from an offset, free space, and the writes.
//!
//! Writes (W1a part 2), for a reviewer:
//! - `open_write` to a real name writes a temporary file beside it (`.name.leaf-up-<n>`, created exclusively) and moves it
//!   over the real name only in `finish`, after every byte was acknowledged: a cut connection leaves the old file as it was
//!   (and at worst the temporary file). The move replaces with `posix-rename@openssh.com` when the server has it, else the
//!   old file is moved aside, the new one put in and the old one removed (moved back when the second step fails). A name
//!   that is already a part file of the generic copy (`.name.<n>.leaf-part`) is written in place, created exclusively: it is
//!   the temporary file (a new name per job).
//! - `WriteMode::At` (resume) writes into the existing file in place from the offset, cutting off what follows.
//! - `rename` never replaces unless asked, never a folder; `remove` unlinks a link (never its target); a recursive remove
//!   looks at every folder with LSTAT before going into it, so a link to a folder is unlinked, not followed.
//! - `set_meta` refuses a link (SETSTAT would change its target).

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, Space, VPath, WriteOpts, WriteSink};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};

use crate::conn::{self, Config, Conn, Shared, Target, block_on};
use crate::prompt::Prompter;

pub struct SftpFs {
    cfg: Config,
    prompter: Arc<dyn Prompter>,
    shared: Arc<Shared>,
}

impl SftpFs {
    pub fn new(cfg: Config, prompter: Arc<dyn Prompter>) -> SftpFs {
        SftpFs { cfg, prompter, shared: Arc::default() }
    }

    /// Runs `op` on the server's connection (connecting first when needed). A connection that broke is dropped and the
    /// operation tried once more on a new one; every request has the per-operation timeout.
    fn with<T>(
        &self,
        v: &VPath,
        cancelled: &dyn Fn() -> bool,
        op: impl AsyncFn(Arc<Conn>) -> Result<T, SftpError>,
    ) -> Result<T, FsError> {
        let t = conn::target(&self.cfg, v)?;
        let mut tries = 0;
        loop {
            tries += 1;
            let r = block_on(cancelled, async {
                let c = self.shared.conn(&self.cfg, &self.prompter, &t).await?;
                match tokio::time::timeout(self.cfg.op_timeout, op(c.clone())).await {
                    Ok(Ok(x)) => Ok(Ok(x)),
                    Ok(Err(e)) => Ok(Err((e, c.alive()))),
                    Err(_) => Ok(Err((SftpError::Timeout, false))),
                }
            })?;
            match r {
                Ok(x) => return Ok(x),
                Err((e, alive)) => {
                    let lost = !alive || matches!(e, SftpError::Timeout | SftpError::IO(_)) || is_closed_text(&e);
                    if lost {
                        self.shared.drop_conn(&t);
                        if tries < 2 && !cancelled() {
                            log::info!("remote: the connection broke ({e}); connecting again");
                            continue;
                        }
                        return Err(FsError::Offline(e.to_string()));
                    }
                    return Err(map_err(e));
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn drop_connections(&self) {
        self.shared.drop_all();
    }

    fn target(&self, v: &VPath) -> Result<Target, FsError> {
        conn::target(&self.cfg, v)
    }

    /// Whether a connection to the server of `v` is open now.
    pub fn target_connected(&self, v: &VPath) -> bool {
        self.target(v).is_ok_and(|t| self.shared.is_connected(&t))
    }

    /// Free space on the server, only while a connection to it is open already (nothing connects for this; `None` otherwise
    /// and when the server does not tell).
    pub fn free_if_connected(&self, v: &VPath) -> Option<Space> {
        let t = self.target(v).ok()?;
        if !self.shared.is_connected(&t) {
            return None;
        }
        self.free_space(v).ok().flatten()
    }

    /// Runs a shell command on the server of `v` over the connection that is open (or a new one) and returns what it printed.
    /// The command text is parsed by the server's shell: quote every value that comes from outside (`crate::checksum::quote`).
    pub fn exec(&self, v: &VPath, command: &str, limit: std::time::Duration, cancelled: &dyn Fn() -> bool) -> Result<conn::ExecOut, FsError> {
        let t = self.target(v)?;
        block_on(cancelled, async {
            let c = self.shared.conn(&self.cfg, &self.prompter, &t).await?;
            c.busy.fetch_add(1, Ordering::Relaxed);
            let r = c.exec(command, limit).await;
            c.busy.fetch_sub(1, Ordering::Relaxed);
            r
        })
    }

    /// The folder the server logs the user into (its home), as an address on the same server.
    pub fn home(&self, v: &VPath) -> Result<VPath, FsError> {
        let p = self.with(v, &|| false, async |c| Ok(c.home.clone()))?;
        Ok(v.with_path(&p))
    }
}

impl SftpFs {
    /// The live connection of `v`'s server (connecting when needed).
    fn conn_of(&self, v: &VPath) -> Result<Arc<Conn>, FsError> {
        self.with(v, &|| false, async |c| Ok(c))
    }

    /// Writes `name.leaf-bak`-style: like `open_write` with `Replace`, but in `finish` the file that is there now becomes
    /// `backup` (a sibling name; an older backup of that name is replaced) before the new one takes the name. The file must
    /// be there and be a regular file.
    pub fn open_write_keeping_backup(&self, p: &VPath, backup: &str) -> Result<Box<dyn WriteSink>, FsError> {
        if leaf_vfs::names::address_problem(backup).is_some() {
            return Err(FsError::Other("The backup's name cannot be used".into()));
        }
        self.writer(p, leaf_vfs::WriteMode::Replace, Some(backup))
    }

    fn writer(&self, p: &VPath, mode: leaf_vfs::WriteMode, backup: Option<&str>) -> Result<Box<dyn WriteSink>, FsError> {
        use leaf_vfs::WriteMode;
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let parent = p.parent().ok_or(FsError::Denied)?;
        let c = self.conn_of(p)?;
        let limit = self.cfg.op_timeout;
        let target = spath(&c, p);
        let existing = lstat_on(&c, &target, limit)?;
        if existing.as_ref().is_some_and(|m| m.kind == Kind::Dir) {
            return Err(FsError::Exists);
        }
        let open = |path: String, flags: OpenFlags, attrs: FileAttributes| run(&c, limit, c.sftp.open(path, flags, attrs)).map(|h| h.handle);
        let (handle, off, place) = match mode {
            WriteMode::At(off) => {
                let h = open(target.clone(), OpenFlags::WRITE, FileAttributes::empty())?;
                let size = existing.as_ref().map_or(0, |m| m.size);
                let r = if off > size {
                    Err(FsError::Other("the offset is past the end of the file".into()))
                } else {
                    let cut = FileAttributes { size: Some(off), ..FileAttributes::empty() };
                    run(&c, limit, c.sftp.fsetstat(h.clone(), cut)).map(|_| ())
                };
                if let Err(e) = r {
                    let _ = run(&c, limit, c.sftp.close(h));
                    return Err(e);
                }
                (h, off, None)
            }
            WriteMode::CreateNew | WriteMode::Replace => {
                if mode == WriteMode::CreateNew && existing.is_some() {
                    return Err(FsError::Exists);
                }
                if backup.is_some() && existing.as_ref().is_none_or(|m| m.kind != Kind::File) {
                    return Err(FsError::Other("There is no file on the server to keep a backup of".into()));
                }
                // a new version keeps the mode bits of the file it replaces (a script stays executable)
                let attrs = FileAttributes { permissions: existing.as_ref().and_then(|m| m.mode), ..FileAttributes::empty() };
                if is_part_name(p.name()) && backup.is_none() {
                    // the generic copy's own temporary file (a new name per job): written in place, created exclusively, so
                    // nothing that is there (another job's part file, a planted link) is ever truncated or written through
                    if existing.is_some() {
                        return Err(FsError::Exists);
                    }
                    (open(target.clone(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE, attrs)?, 0, None)
                } else {
                    let temp = spath(&c, &parent.join(&side_name(p.name(), "leaf-up")));
                    let h = open(temp.clone(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::TRUNCATE, attrs)?;
                    let backup = backup.map(|b| spath(&c, &parent.join(b)));
                    (h, 0, Some(Place { temp, target, replace: mode == WriteMode::Replace, backup }))
                }
            }
        };
        c.busy.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Writer { c, handle, off, buf: Vec::new(), queue: VecDeque::new(), op_timeout: limit, place, handle_closed: false, done: false, failed: false }))
    }

    /// A folder and everything in it. Every folder is looked at with LSTAT before it is entered: a link (to a folder or
    /// anything else) is unlinked, never followed, so nothing outside the folder is touched.
    fn remove_tree(&self, dir: &VPath, depth: usize) -> Result<(), FsError> {
        if depth >= leaf_vfs::copy::MAX_DEPTH {
            return Err(leaf_vfs::copy::too_deep());
        }
        let mut children = Vec::new();
        self.list(dir, 1024, &|| false, &mut |b| children.extend(b))?;
        for e in children {
            // (the listing left out names that are not one part of an address)
            let child = leaf_vfs::names::vchild(dir, &e.name).map_err(|why| FsError::Other(format!("A name in the folder cannot be used ({why})")))?;
            let dir_now = !e.link && e.is_dir() && self.stat(&child)?.kind == Kind::Dir;
            if dir_now {
                self.remove_tree(&child, depth + 1)?;
            } else {
                self.with(&child, &|| false, async |c| c.sftp.remove(spath(&c, &child)).await.map(|_| ()))?;
            }
        }
        self.rmdir(dir)
    }

    fn rmdir(&self, dir: &VPath) -> Result<(), FsError> {
        match self.with(dir, &|| false, async |c| c.sftp.rmdir(spath(&c, dir)).await.map(|_| ())) {
            Ok(()) => Ok(()),
            // a plain "failure" is most often a folder with something in it
            Err(FsError::Other(m)) => {
                let mut any = false;
                let _ = self.list(dir, 1, &|| false, &mut |b| any |= !b.is_empty());
                Err(if any { FsError::NotEmpty } else { FsError::Other(m) })
            }
            Err(e) => Err(e),
        }
    }
}

/// One request on this connection with the time limit, from a worker thread (no retry: a write must not be sent twice).
fn run<T>(c: &Conn, limit: std::time::Duration, fut: impl std::future::Future<Output = Result<T, SftpError>>) -> Result<T, FsError> {
    let r = conn::rt().block_on(async { tokio::time::timeout(limit, fut).await });
    c.touch();
    match r {
        Err(_) => Err(FsError::Offline("the server did not answer in time".into())),
        Ok(Ok(x)) => Ok(x),
        Ok(Err(e)) if !c.alive() || matches!(e, SftpError::IO(_)) || is_closed_text(&e) => Err(FsError::Offline(e.to_string())),
        Ok(Err(e)) => Err(map_err(e)),
    }
}

/// What is at `path` (not following a link), `None` when nothing is.
fn lstat_on(c: &Conn, path: &str, limit: std::time::Duration) -> Result<Option<Meta>, FsError> {
    match run(c, limit, c.sftp.lstat(path.to_string())) {
        Ok(a) => Ok(Some(meta_of(&a.attrs))),
        Err(FsError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `posix-rename@openssh.com`: replaces `to` in one step.
async fn posix_rename(c: &Conn, from: &str, to: &str) -> Result<(), SftpError> {
    let mut data = Vec::with_capacity(8 + from.len() + to.len());
    for s in [from, to] {
        data.extend_from_slice(&(s.len() as u32).to_be_bytes());
        data.extend_from_slice(s.as_bytes());
    }
    match c.sftp.extended(conn::POSIX_RENAME, data).await? {
        Packet::Status(s) if s.status_code == StatusCode::Ok => Ok(()),
        Packet::Status(s) => Err(SftpError::Status(s)),
        _ => Err(SftpError::UnexpectedPacket),
    }
}

fn plain_rename(c: &Conn, from: &str, to: &str, limit: std::time::Duration) -> Result<(), FsError> {
    run(c, limit, c.sftp.rename(from.to_string(), to.to_string())).map(|_| ())
}

/// Moves `from` to `to` (server paths). Nothing at `to`: a plain rename. A file at `to`: `Exists` unless `replace`; then
/// replaced in one step (posix-rename), or by moving the old one aside first (moved back when the new one cannot take the
/// name). With `backup`, the old one becomes that name instead (an older backup there is replaced). A folder at `to` is
/// never replaced.
fn put_in_place(c: &Conn, from: &str, to: &str, replace: bool, backup: Option<&str>, limit: std::time::Duration) -> Result<(), FsError> {
    let Some(old) = lstat_on(c, to, limit)? else {
        // (a plain SFTP rename does not replace: a name that appeared meanwhile fails here, it is not overwritten)
        return plain_rename(c, from, to, limit);
    };
    if !replace || old.kind == Kind::Dir {
        return Err(FsError::Exists);
    }
    let name = to.rsplit('/').next().unwrap_or(to);
    let dir = &to[..to.len() - name.len()];
    let aside = match backup {
        Some(b) => {
            if lstat_on(c, b, limit)?.is_some_and(|m| m.kind == Kind::Dir) {
                return Err(FsError::Other("A folder has the backup's name".into()));
            }
            b.to_string()
        }
        None if c.posix_rename => return run(c, limit, posix_rename(c, from, to)),
        None => format!("{dir}{}", side_name(name, "leaf-old")),
    };
    // the old version out of the way (over an older backup in one step where the server can)
    if c.posix_rename {
        run(c, limit, posix_rename(c, to, &aside))?;
    } else {
        if backup.is_some() && lstat_on(c, &aside, limit)?.is_some() {
            run(c, limit, c.sftp.remove(aside.clone()))?;
        }
        plain_rename(c, to, &aside, limit)?;
    }
    match plain_rename(c, from, to, limit) {
        Ok(()) => {
            if backup.is_none() {
                let _ = run(c, limit, c.sftp.remove(aside));
            }
            Ok(())
        }
        Err(e) => {
            // the old one back under its name
            if let Err(back) = plain_rename(c, &aside, to, limit) {
                log::warn!("remote: the old file could not be moved back after a failed replace ({back}); it is kept beside it");
                // the user must learn where the old version is: under its name there is nothing now
                return Err(FsError::Other(format!(
                    "The new version could not take the name {name} ({e}), and the old version could not be put back ({back}).                      The old version is kept on the server as {aside}"
                )));
            }
            Err(e)
        }
    }
}

/// The generic copy's temporary name (`.name.<n>.leaf-part`): such a file is written in place.
fn is_part_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".leaf-part")
}

/// A hidden name beside `name` that nothing else uses: `.name.<tag>-<n>` (the name shortened to keep it under 255 bytes).
/// It carries no secret: the process id and a counter.
fn side_name(name: &str, tag: &str) -> String {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mut short = name;
    while short.len() > 200 {
        let mut cut = short.len() - 1;
        while !short.is_char_boundary(cut) {
            cut -= 1;
        }
        short = &short[..cut];
    }
    format!(".{short}.{tag}-{:x}{n:x}{nanos:x}", std::process::id())
}

fn is_closed_text(e: &SftpError) -> bool {
    matches!(e, SftpError::UnexpectedBehavior(m) if m.contains("closed") || m.contains("SendError") || m.contains("RecvError"))
}

fn map_err(e: SftpError) -> FsError {
    match e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => FsError::NotFound,
            StatusCode::PermissionDenied => FsError::Denied,
            StatusCode::OpUnsupported => FsError::Unsupported,
            StatusCode::NoConnection | StatusCode::ConnectionLost => FsError::Offline(s.error_message),
            _ if s.error_message.is_empty() => FsError::Other(s.status_code.to_string()),
            _ => FsError::Other(s.error_message),
        },
        SftpError::Timeout => FsError::Offline("the server did not answer in time".into()),
        e => FsError::Other(e.to_string()),
    }
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

fn kind_of(a: &FileAttributes) -> Kind {
    match a.permissions.map(|p| p & S_IFMT) {
        Some(S_IFDIR) => Kind::Dir,
        Some(S_IFLNK) => Kind::Link,
        _ => Kind::File,
    }
}

fn mtime_ms(a: &FileAttributes) -> i64 {
    a.mtime.map_or(0, |t| t as i64 * 1000)
}

fn meta_of(a: &FileAttributes) -> Meta {
    Meta { kind: kind_of(a), size: a.size.unwrap_or(0), modified_ms: mtime_ms(a), created_ms: 0, mode: a.permissions.map(|p| p & 0o7777) }
}

fn entry_of(name: &str, a: &FileAttributes) -> Entry {
    let mut e = entry_of_plain(name, a);
    e.mode = a.permissions.map_or(0, |p| (p & 0o7777) as u16);
    e.uid = a.uid.unwrap_or(u32::MAX);
    e
}

fn entry_of_plain(name: &str, a: &FileAttributes) -> Entry {
    let m = mtime_ms(a);
    // names starting with a dot are hidden on the server's side (the list's "dot files hidden" setting decides)
    match kind_of(a) {
        Kind::Dir => Entry::new(name, EntryKind::Dir, 0, m, m, rf_core::attrs::DIRECTORY),
        Kind::File => Entry::new(name, EntryKind::File, a.size.unwrap_or(0), m, m, 0),
        Kind::Link => {
            let mut e = Entry::new(name, EntryKind::File, 0, m, m, rf_core::attrs::REPARSE_POINT);
            e.link = true;
            e
        }
    }
}

/// The path as the server takes it: `/~` and `/~/...` stand for the user's home folder there (as in WinSCP and curl).
fn spath(c: &Conn, v: &VPath) -> String {
    let p = v.path();
    match p.strip_prefix("/~") {
        Some("") => c.home.clone(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", c.home.trim_end_matches('/')),
        _ => p.to_string(),
    }
}

impl Fs for SftpFs {
    fn caps(&self) -> Caps {
        Caps { write: true, permissions: true, set_times: true, links: true, free_space: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let handle = self.with(dir, cancelled, async |c| Ok(c.sftp.opendir(spath(&c, dir)).await?.handle))?;
        let t = self.target(dir)?;
        let mut sent = 0;
        let mut pending: Vec<Entry> = Vec::new();
        let mut left_out = 0usize;
        let r = (|| {
            loop {
                if cancelled() {
                    return Err(FsError::Cancelled);
                }
                // the server sends its listing in parts; each part is handed on in batches of `batch`
                let part = self.with(dir, cancelled, async |c| match c.sftp.readdir(handle.as_str()).await {
                    Ok(n) => Ok(Some(n.files)),
                    Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => Ok(None),
                    Err(e) => Err(e),
                })?;
                let Some(files) = part else { break };
                // a name that cannot be one part of an address (empty, `.`, `..`, a `/` or `\`, NUL) is left out and counted:
                // joined to the folder's address it would name another place
                for f in &files {
                    match leaf_vfs::names::address_problem(&f.filename) {
                        None => pending.push(entry_of(&f.filename, &f.attrs)),
                        Some(_) if f.filename == "." || f.filename == ".." => {}
                        Some(why) => {
                            left_out += 1;
                            log::debug!("remote: a name in the listing left out: {why}");
                        }
                    }
                }
                while pending.len() >= batch.max(1) {
                    let rest = pending.split_off(batch.max(1));
                    sent += pending.len();
                    on_batch(std::mem::replace(&mut pending, rest));
                }
            }
            if !pending.is_empty() {
                sent += pending.len();
                on_batch(std::mem::take(&mut pending));
            }
            if left_out > 0 {
                log::warn!("remote: {left_out} names in a listing of {} cannot be used (empty, . or .., / or \\, NUL) and are left out", dir.host());
            }
            Ok(sent)
        })();
        // the handle is closed whatever happened (on the same connection, when it is still there)
        let _ = block_on(&|| false, async {
            if let Ok(c) = self.shared.conn(&self.cfg, &self.prompter, &t).await {
                let _ = tokio::time::timeout(self.cfg.op_timeout, c.sftp.close(handle.as_str())).await;
            }
            Ok(())
        });
        r
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        self.with(p, &|| false, async |c| Ok(meta_of(&c.sftp.lstat(spath(&c, p)).await?.attrs)))
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let (c, handle) = self.with(p, &|| false, async |c| {
            let h = c.sftp.open(spath(&c, p), OpenFlags::READ, FileAttributes::empty()).await?;
            Ok((c, h.handle))
        })?;
        c.busy.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Reader { c, handle, next: offset, queue: VecDeque::new(), buf: Vec::new(), pos: 0, eof: false, op_timeout: self.cfg.op_timeout }))
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        self.writer(p, o.mode, None)
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        match self.with(p, &|| false, async |c| c.sftp.mkdir(spath(&c, p), FileAttributes::empty()).await.map(|_| ())) {
            Ok(()) => Ok(()),
            // most servers answer a plain "failure" for a name that is taken
            Err(e) => match self.stat(p) {
                Ok(_) => Err(FsError::Exists),
                Err(_) => Err(e),
            },
        }
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        if from.is_root() || to.is_root() {
            return Err(FsError::Denied);
        }
        let c = self.conn_of(from)?;
        let (a, b) = (spath(&c, from), spath(&c, to));
        let src = lstat_on(&c, &a, self.cfg.op_timeout)?.ok_or(FsError::NotFound)?;
        // a folder never replaces anything
        if src.kind == Kind::Dir && lstat_on(&c, &b, self.cfg.op_timeout)?.is_some() {
            return Err(FsError::Exists);
        }
        put_in_place(&c, &a, &b, replace, None, self.cfg.op_timeout)
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() || p.path() == "/~" {
            return Err(FsError::Denied);
        }
        let c = self.conn_of(p)?;
        // never the root, the home folder (by any address of it) or a folder the home folder is in
        let sp = spath(&c, p);
        let (norm, home) = (sp.trim_end_matches('/'), c.home.trim_end_matches('/'));
        if norm.is_empty() || norm == home || home.starts_with(&format!("{norm}/")) {
            return Err(FsError::Denied);
        }
        let m = lstat_on(&c, &spath(&c, p), self.cfg.op_timeout)?.ok_or(FsError::NotFound)?;
        match m.kind {
            // a link is unlinked (REMOVE never follows it), a file removed
            Kind::Link | Kind::File => self.with(p, &|| false, async |c| c.sftp.remove(spath(&c, p)).await.map(|_| ())),
            Kind::Dir if recursive => self.remove_tree(p, 0),
            Kind::Dir => self.rmdir(p),
        }
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.modified_ms.is_none() && m.mode.is_none() {
            return Ok(());
        }
        // SETSTAT follows a link: a link's target is never changed through it
        if self.stat(p)?.kind == Kind::Link {
            return Err(FsError::Unsupported);
        }
        self.with(p, &|| false, async |c| {
            let path = spath(&c, p);
            let mut a = FileAttributes::empty();
            if let Some(ms) = m.modified_ms {
                // SFTP sets both times at once: the access time is kept as it is
                let now = c.sftp.lstat(path.clone()).await?.attrs;
                let secs = (ms.max(0) / 1000).min(u32::MAX as i64) as u32;
                a.mtime = Some(secs);
                a.atime = now.atime.or(Some(secs));
            }
            if let Some(mode) = m.mode {
                a.permissions = Some(mode & 0o7777);
            }
            c.sftp.setstat(path, a).await.map(|_| ())
        })
    }

    fn free_space(&self, p: &VPath) -> Result<Option<Space>, FsError> {
        self.with(p, &|| false, async |c| {
            if !c.statvfs {
                return Ok(None);
            }
            let s = c.sftp.statvfs(spath(&c, p)).await?;
            let unit = if s.fragment_size > 0 { s.fragment_size } else { s.block_size };
            Ok(Some(Space { free: s.blocks_avail.saturating_mul(unit), total: Some(s.blocks.saturating_mul(unit)) }))
        })
    }
}

/// Bytes asked for in one read request, and how many requests are on their way at once (the line's round trip is paid
/// once per window, not once per chunk).
const CHUNK: u32 = 64 * 1024;
const AHEAD: usize = 8;

type Pending = tokio::task::JoinHandle<Result<Vec<u8>, SftpError>>;

/// A file being read from the server, with reads ahead of the caller.
struct Reader {
    c: Arc<Conn>,
    handle: String,
    /// The offset the next request asks for.
    next: u64,
    queue: VecDeque<(u64, Pending)>,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    op_timeout: std::time::Duration,
}

impl Reader {
    fn fill_queue(&mut self) {
        while self.queue.len() < AHEAD {
            let (sftp, h, off) = (self.c.sftp.clone(), self.handle.clone(), self.next);
            let task = conn::rt().spawn(async move {
                match sftp.read(h, off, CHUNK).await {
                    Ok(d) => Ok(d.data),
                    Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            });
            self.queue.push_back((off, task));
            self.next += CHUNK as u64;
        }
    }

    fn restart_at(&mut self, off: u64) {
        for (_, t) in self.queue.drain(..) {
            t.abort();
        }
        self.next = off;
    }
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.eof || out.is_empty() {
                return Ok(0);
            }
            self.fill_queue();
            let (off, task) = self.queue.pop_front().expect("queue filled");
            let timeout = self.op_timeout;
            let data = conn::rt()
                .block_on(async { tokio::time::timeout(timeout, task).await })
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "the server did not answer in time"))?
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .map_err(|e| match map_err(e) {
                    FsError::NotFound => std::io::Error::from(std::io::ErrorKind::NotFound),
                    FsError::Denied => std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                    e => std::io::Error::other(e.to_string()),
                })?;
            self.c.touch();
            if data.is_empty() {
                self.eof = true;
                self.restart_at(off);
                continue;
            }
            if (data.len() as u64) < CHUNK as u64 {
                // a short read: what is ahead was asked for at the wrong offsets
                self.restart_at(off + data.len() as u64);
            }
            self.buf = data;
            self.pos = 0;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.restart_at(0);
        self.c.busy.fetch_sub(1, Ordering::Relaxed);
        let (sftp, h) = (self.c.sftp.clone(), std::mem::take(&mut self.handle));
        conn::rt().spawn(async move {
            let _ = sftp.close(h).await;
        });
    }
}

/// Where a written temporary file goes in `finish`.
struct Place {
    temp: String,
    target: String,
    replace: bool,
    backup: Option<String>,
}

type PendingWrite = tokio::task::JoinHandle<Result<(), SftpError>>;

/// A file being written to the server: `CHUNK`-sized writes, `AHEAD` of them on their way at once.
struct Writer {
    c: Arc<Conn>,
    handle: String,
    /// The offset of the next write.
    off: u64,
    buf: Vec<u8>,
    queue: VecDeque<PendingWrite>,
    op_timeout: std::time::Duration,
    /// `None`: written in place.
    place: Option<Place>,
    /// CLOSE was sent for the handle: its id may belong to another file on this connection from then on, so it is never
    /// used again (not even by Drop).
    handle_closed: bool,
    /// Finished: everything is in place.
    done: bool,
    failed: bool,
}

fn io_of(e: FsError) -> std::io::Error {
    match e {
        FsError::NotFound => std::io::Error::from(std::io::ErrorKind::NotFound),
        FsError::Denied => std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        e => std::io::Error::other(e.to_string()),
    }
}

impl Writer {
    fn send(&mut self, data: Vec<u8>) -> std::io::Result<()> {
        let (sftp, h, off) = (self.c.sftp.clone(), self.handle.clone(), self.off);
        self.off += data.len() as u64;
        self.queue.push_back(conn::rt().spawn(async move { sftp.write(h, off, data).await.map(|_| ()) }));
        while self.queue.len() > AHEAD {
            self.wait_one()?;
        }
        Ok(())
    }

    fn wait_one(&mut self) -> std::io::Result<()> {
        let Some(task) = self.queue.pop_front() else { return Ok(()) };
        let limit = self.op_timeout;
        let r = conn::rt().block_on(async { tokio::time::timeout(limit, task).await });
        let r = match r {
            Err(_) => Err(FsError::Offline("the server did not answer in time".into())),
            Ok(Err(e)) => Err(FsError::Other(e.to_string())),
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) if !self.c.alive() || matches!(e, SftpError::IO(_)) || is_closed_text(&e) => Err(FsError::Offline(e.to_string())),
            Ok(Ok(Err(e))) => Err(map_err(e)),
        };
        self.c.touch();
        r.map_err(|e| {
            self.failed = true;
            io_of(e)
        })
    }

    fn drain(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.send(rest)?;
        }
        while !self.queue.is_empty() {
            self.wait_one()?;
        }
        Ok(())
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.failed || self.handle_closed {
            return Err(std::io::Error::other("the upload broke off"));
        }
        self.buf.extend_from_slice(data);
        while self.buf.len() >= CHUNK as usize {
            let rest = self.buf.split_off(CHUNK as usize);
            let chunk = std::mem::replace(&mut self.buf, rest);
            self.send(chunk)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("the upload broke off"));
        }
        self.drain()
    }
}

impl WriteSink for Writer {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        let r = (|| {
            self.drain().map_err(FsError::from)?;
            // (marked before the request: whatever its answer, the id is not ours any more)
            self.handle_closed = true;
            run(&self.c, self.op_timeout, self.c.sftp.close(std::mem::take(&mut self.handle)))?;
            match &self.place {
                Some(p) => put_in_place(&self.c, &p.temp, &p.target, p.replace, p.backup.as_deref(), self.op_timeout),
                None => Ok(()),
            }
        })();
        if r.is_err() {
            // the temporary file goes (the real name was not touched); Drop does it
            self.failed = true;
        } else {
            self.done = true;
            self.place = None;
        }
        r
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.c.busy.fetch_sub(1, Ordering::Relaxed);
        if self.done {
            return;
        }
        // not finished: what is on its way is dropped, the handle closed (unless `finish` closed it already), the temporary
        // file removed (best effort: on a cut line it stays beside the real file, which is as it was)
        for t in self.queue.drain(..) {
            t.abort();
        }
        let sftp = self.c.sftp.clone();
        let h = (!self.handle_closed).then(|| std::mem::take(&mut self.handle));
        let temp = self.place.take().map(|p| p.temp);
        if h.is_none() && temp.is_none() {
            return;
        }
        conn::rt().spawn(async move {
            if let Some(h) = h {
                let _ = sftp.close(h).await;
            }
            if let Some(t) = temp {
                let _ = sftp.remove(t).await;
            }
        });
    }
}
