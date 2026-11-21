//! `SftpFs`: `sftp://user@host:port/path` through `leaf-vfs`. Read-only in this lane: list, stat, read from an offset, free
//! space; every write answers `Unsupported`.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, Space, VPath, WriteOpts, WriteSink};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};

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

    /// The folder the server logs the user into (its home), as an address on the same server.
    pub fn home(&self, v: &VPath) -> Result<VPath, FsError> {
        let p = self.with(v, &|| false, async |c| Ok(c.home.clone()))?;
        Ok(v.with_path(&p))
    }
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
        Caps { links: true, free_space: true, ..Caps::default() }
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

    fn open_write(&self, _p: &VPath, _o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        Err(FsError::Unsupported)
    }
    fn mkdir(&self, _p: &VPath) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn rename(&self, _from: &VPath, _to: &VPath, _replace: bool) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn remove(&self, _p: &VPath, _recursive: bool) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn set_meta(&self, _p: &VPath, _m: &MetaChange) -> Result<(), FsError> {
        Err(FsError::Unsupported)
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
