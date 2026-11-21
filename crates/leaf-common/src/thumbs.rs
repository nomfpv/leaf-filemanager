//! The data of icons and thumbnails: the pixels, and a request to the thumbnail pool and its answer. The pools are the backends'.

use std::path::PathBuf;
use std::sync::Arc;

/// Straight-alpha BGRA, top-down rows.
#[derive(Clone)]
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Icon,
    /// Real thumbnail if the shell has one; otherwise falls back to the icon.
    Thumbnail,
    /// Real thumbnail or nothing (a video without a frame: the caller draws its own icon, not the player's).
    ThumbnailOnly,
}

impl Kind {
    /// A picture of the file's content is wanted (with or without the icon to fall back to).
    pub fn wants_picture(self) -> bool {
        matches!(self, Kind::Thumbnail | Kind::ThumbnailOnly)
    }
}

pub struct Request {
    pub key: u64,
    pub path: PathBuf,
    pub size: u32,
    pub kind: Kind,
    /// The picture the user is looking at right now (the preview pane's): see `ThumbPool` lanes.
    pub urgent: bool,
    /// When it was handed to the pool (for `Response::waited`).
    queued: std::time::Instant,
}

impl Request {
    pub fn new(key: u64, path: PathBuf, size: u32, kind: Kind) -> Self {
        Self { key, path, size, kind, urgent: false, queued: std::time::Instant::now() }
    }

    /// When it was handed to the pool (made).
    #[inline]
    pub fn queued(&self) -> std::time::Instant {
        self.queued
    }
}

pub struct Response {
    pub key: u64,
    pub image: Option<Arc<Bgra>>,
    /// Request was discarded (backlog) — not a failure; may be retried.
    pub dropped: bool,
    /// How long the request stood in the queue, and how long making the picture took (zero when dropped).
    pub waited: std::time::Duration,
    pub took: std::time::Duration,
    /// Made by the lane of urgent requests.
    pub urgent: bool,
}
