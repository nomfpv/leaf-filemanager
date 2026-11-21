//! The friend's folder as a `leaf_vfs::Fs`, for leaf-sync's planner and run on the PC that shared it: every call is one request
//! on its own stream of the connection ([`crate::wire`]). Addresses are `leafshare://<share id>/<path>`; the path is relative to
//! the friend's shared folder (the friend's PC checks it again, see [`crate::folder`]). Calls block: made from the sync's own
//! thread, never from the runtime.

use std::io::{Read, Write};
use std::sync::Arc;

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, VPath, WriteMode, WriteOpts, WriteSink};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::runtime::Handle;

use crate::transport::{Link, RecvHalf, SendHalf};
use crate::wire::{self, ErrKind, Op, Reply, Request, WireEntry};

pub const SCHEME: &str = "leafshare";

pub struct RemoteFs {
    pub(crate) link: Arc<dyn Link>,
    pub(crate) share: String,
    pub(crate) rt: Handle,
}

fn rel_of(p: &VPath) -> String {
    p.path().trim_start_matches('/').to_string()
}

fn offline(e: impl std::fmt::Display) -> FsError {
    FsError::Offline(format!("the other PC: {e}"))
}

fn from_wire(kind: ErrKind, text: String) -> FsError {
    match kind {
        ErrKind::NotFound => FsError::NotFound,
        ErrKind::Denied => FsError::Denied,
        ErrKind::Exists => FsError::Exists,
        ErrKind::NotEmpty => FsError::NotEmpty,
        ErrKind::Unsupported => FsError::Unsupported,
        ErrKind::Other => FsError::Other(text),
    }
}

fn meta_of(e: &WireEntry) -> Meta {
    let kind = if e.link {
        Kind::Link
    } else if e.dir {
        Kind::Dir
    } else {
        Kind::File
    };
    Meta { kind, size: e.size, modified_ms: e.mtime_ms, created_ms: 0, mode: None }
}

fn entry_of(e: WireEntry) -> Entry {
    let kind = if e.dir { EntryKind::Dir } else { EntryKind::File };
    let mut out = Entry::new(e.name.as_str(), kind, e.size, e.mtime_ms, 0, 0);
    out.link = e.link;
    out
}

impl RemoteFs {
    fn block<T>(&self, fut: impl std::future::Future<Output = Result<T, FsError>>) -> Result<T, FsError> {
        if self.link.is_closed() {
            return Err(offline("the connection is gone"));
        }
        self.rt.block_on(fut)
    }

    async fn open(&self, op: Op) -> Result<(SendHalf, RecvHalf), FsError> {
        let (mut s, r) = self.link.open().await.map_err(offline)?;
        wire::write_frame(&mut s, &Request { share: self.share.clone(), op }).await.map_err(offline)?;
        Ok((s, r))
    }

    async fn call(&self, op: Op) -> Result<Reply, FsError> {
        let (mut s, mut r) = self.open(op).await?;
        let _ = s.shutdown().await;
        let reply: Reply = wire::read_frame(&mut r).await.map_err(offline)?;
        match reply {
            Reply::Err { kind, text } => Err(from_wire(kind, text)),
            other => Ok(other),
        }
    }

    fn simple(&self, op: Op) -> Result<(), FsError> {
        match self.block(self.call(op))? {
            Reply::Ok => Ok(()),
            other => Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
        }
    }

    /// The SHA-256 of a file there (hex), read on the friend's PC.
    pub fn hash(&self, p: &VPath) -> Result<String, FsError> {
        match self.block(self.call(Op::Hash { path: rel_of(p) }))? {
            Reply::Hash { hex } => Ok(hex),
            other => Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
        }
    }
}

impl Fs for RemoteFs {
    fn caps(&self) -> Caps {
        Caps { write: true, set_times: true, links: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let entries = match self.block(self.call(Op::List { path: rel_of(dir) }))? {
            Reply::List { entries } => entries,
            other => return Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
        };
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        let n = entries.len();
        let mut it = entries.into_iter().map(entry_of).peekable();
        while it.peek().is_some() {
            on_batch(it.by_ref().take(batch.max(1)).collect());
        }
        Ok(n)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        match self.block(self.call(Op::Stat { path: rel_of(p) }))? {
            Reply::Stat { entry } => Ok(meta_of(&entry)),
            other => Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
        }
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let (len, r) = self.block(async {
            let (mut s, mut r) = self.open(Op::Read { path: rel_of(p), offset }).await?;
            let _ = s.shutdown().await;
            match wire::read_frame::<_, Reply>(&mut r).await.map_err(offline)? {
                Reply::Data { len } => Ok((len, r)),
                Reply::Err { kind, text } => Err(from_wire(kind, text)),
                other => Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
            }
        })?;
        Ok(Box::new(RemoteRead { r, left: len, rt: self.rt.clone() }))
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        if o.mode != WriteMode::CreateNew {
            return Err(FsError::Unsupported);
        }
        let (s, r) = self.block(self.open(Op::Write { path: rel_of(p) }))?;
        Ok(Box::new(RemoteWrite { s: Some(s), r, rt: self.rt.clone() }))
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        self.simple(Op::Mkdir { path: rel_of(p) })
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if replace {
            // the friend's PC never replaces a file by a rename: the old one goes to its trash first
            return Err(FsError::Unsupported);
        }
        self.simple(Op::Rename { from: rel_of(from), to: rel_of(to) })
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        self.simple(Op::Remove { path: rel_of(p), recursive })
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.mode.is_some() {
            return Err(FsError::Unsupported);
        }
        match m.modified_ms {
            Some(ms) => self.simple(Op::SetTime { path: rel_of(p), mtime_ms: ms }),
            None => Ok(()),
        }
    }
}

struct RemoteRead {
    r: RecvHalf,
    left: u64,
    rt: Handle,
}

impl Read for RemoteRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.rt.block_on(self.r.read(&mut buf[..want]))?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the other PC stopped sending"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

struct RemoteWrite {
    s: Option<SendHalf>,
    r: RecvHalf,
    rt: Handle,
}

impl Write for RemoteWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = self.s.as_mut().ok_or_else(|| std::io::Error::other("finished"))?;
        self.rt.block_on(s.write_all(buf))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl WriteSink for RemoteWrite {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        let Some(mut s) = self.s.take() else { return Ok(()) };
        let rt = self.rt.clone();
        rt.block_on(async {
            s.shutdown().await.map_err(offline)?;
            match wire::read_frame::<_, Reply>(&mut self.r).await.map_err(offline)? {
                Reply::Ok => Ok(()),
                Reply::Err { kind, text } => Err(from_wire(kind, text)),
                other => Err(FsError::Other(format!("an unexpected answer: {other:?}"))),
            }
        })
    }
}
