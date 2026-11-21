//! The share protocol on a [`Link`](crate::transport::Link). Messages are frames (a 4-byte big-endian length, then JSON); file
//! bytes follow a frame raw, to the end of their stream.
//!
//! - The PC that dials (the friend who joined) opens the first stream, the control stream, and sends [`Open`]. A device the share
//!   does not know, a wrong or used or expired invite, a share it is not stored for: the connection is closed without a reply.
//!   Otherwise [`OpenReply::Pair`] (both users confirm the six-digit code, [`PairConfirm`] each way) and [`OpenReply::Welcome`].
//!   The control stream then carries [`Notice`]s both ways for as long as the connection lasts.
//! - The PC that shared the folder drives every sync: it opens one stream per file operation ([`Request`] then [`Reply`]) on the
//!   friend's folder. The friend's PC answers them, strictly inside its folder; it never asks the sharing PC for anything.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTO: u32 = 1;
/// The largest frame (a listing of a very large folder is the biggest).
const MAX_FRAME: usize = 64 << 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Open {
    /// The first connection: the invite's secret.
    Join { proto: u32, share: String, secret: String, name: String },
    /// Every later one.
    Resume { proto: u32, share: String, name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum OpenReply {
    /// The devices are not paired yet: both show the code.
    Pair,
    /// `folder`: the shared folder's name on the sharing PC. `can_write`: the friend's changes come back.
    Welcome { name: String, folder: String, can_write: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairConfirm {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Notice {
    /// Something changed in the folder on the sender's side.
    Changed,
    /// The sender stopped sharing the folder (Leave).
    Left,
    /// What the sender's setting for the friend's changes is now (the sharing PC tells the friend).
    CanWrite { can_write: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub share: String,
    pub op: Op,
}

/// Paths are relative to the shared folder, `/` between their parts; `""` is the folder itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    List { path: String },
    Stat { path: String },
    /// The bytes from `offset` to the end follow the reply.
    Read { path: String, offset: u64 },
    /// The bytes follow the request, to the end of the stream. Only a temporary name (`.<name>.<n>.leaf-part`), only a new file.
    Write { path: String },
    Mkdir { path: String },
    /// Never over something that is there.
    Rename { from: String, to: String },
    /// Moves it into the folder's own `.leaf-sync-trash`; never deletes it.
    Remove { path: String, recursive: bool },
    SetTime { path: String, mtime_ms: i64 },
    Hash { path: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ErrKind {
    NotFound,
    Denied,
    Exists,
    NotEmpty,
    Unsupported,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireEntry {
    pub name: String,
    #[serde(default)]
    pub dir: bool,
    #[serde(default)]
    pub link: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "r", rename_all = "kebab-case")]
pub enum Reply {
    Ok,
    Err { kind: ErrKind, text: String },
    List { entries: Vec<WireEntry> },
    Stat { entry: WireEntry },
    /// `len` bytes follow.
    Data { len: u64 },
    /// SHA-256, lower-case hex.
    Hash { hex: String },
}

pub async fn write_frame<W: AsyncWrite + Unpin + ?Sized, T: Serialize>(w: &mut W, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    w.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin + ?Sized, T: DeserializeOwned>(r: &mut R) -> anyhow::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "a frame of {len} bytes is too large");
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}
