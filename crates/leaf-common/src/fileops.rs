//! The data of a copy, move or delete: its kind and its progress tracker. The engines are the backends' (`fileops`, `fastcopy`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

/// Bytes done and bytes in all, for an operation whose engine knows them (our copy engine,
/// the archive code). `on_progress` only carries abstract units, which is enough for a
/// percentage but not for a speed in MB/s; the interface reads this when a progress report
/// arrives. Both stay 0 for the shell's engine and for operations counted in items.
#[derive(Default)]
pub struct ByteProgress {
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
}

impl ByteProgress {
    #[inline]
    pub fn set(&self, done: u64, total: u64) {
        // total first: a reader that sees the new `done` never pairs it with an older, smaller total
        self.total.store(total, std::sync::atomic::Ordering::Relaxed);
        self.done.store(done.min(total), std::sync::atomic::Ordering::Relaxed);
    }

    /// `(done, total)`; `total == 0` means the engine does not count bytes.
    #[inline]
    pub fn get(&self) -> (u64, u64) {
        let done = self.done.load(std::sync::atomic::Ordering::Relaxed);
        let total = self.total.load(std::sync::atomic::Ordering::Relaxed);
        (done.min(total), total)
    }
}

/// Progress reporting for one operation. `on_progress(total, done, current_item)`;
/// `total`/`done` are the shell's abstract work units (use done/total as a fraction).
pub struct Tracker {
    /// Bytes, when the engine counts them (see [`ByteProgress`]). Shared so the interface can
    /// keep a handle while the worker owns the tracker.
    pub bytes: Arc<ByteProgress>,
    pub on_progress: Box<dyn Fn(u32, u32, Option<String>) + Send + Sync>,
    pub cancel: Arc<AtomicBool>,
    /// (source, result) for every item the shell finished: the new copy/moved location, or the
    /// item's path inside the Recycle Bin for deletes. Used to build undo records.
    pub results: std::sync::Mutex<Vec<(PathBuf, PathBuf)>>,
    /// Our engine waits while this is set (the shell engine can't pause).
    pub paused: Arc<AtomicBool>,
    /// Copy speed limit in bytes per second; 0 = none (our engine only).
    pub limit: std::sync::atomic::AtomicU64,
    /// Check copies after writing them (our engine only); see [`Tracker::set_verify`].
    pub verify: std::sync::atomic::AtomicU8,
    /// Asked (on the worker thread, blocking) what to do with names that already exist at the
    /// destination; `None` cancels the operation. Without it, both are kept.
    #[allow(clippy::type_complexity)]
    pub ask: Option<Box<dyn Fn(&[crate::fastcopy::Conflict]) -> Option<Vec<crate::fastcopy::Resolve>> + Send + Sync>>,
    /// Our engine only, never the system's (an agent's job, plan F9): what our engine would hand to the shell (a tree with a link
    /// inside, a file it could not copy, a path that is not on a local file system, a copy into the items' own folder) is left and
    /// counted as not done instead. The shell would follow links and could ask about replacing.
    pub own_engine_only: bool,
}

impl Tracker {
    #[inline]
    pub fn set_verify(&self, v: crate::fastcopy::Verify) {
        self.verify.store(v as u8, Ordering::Relaxed);
    }

    #[inline]
    pub fn verify_mode(&self) -> crate::fastcopy::Verify {
        match self.verify.load(Ordering::Relaxed) {
            1 => crate::fastcopy::Verify::Quick,
            2 => crate::fastcopy::Verify::Full,
            _ => crate::fastcopy::Verify::Off,
        }
    }

    pub fn new(on_progress: Box<dyn Fn(u32, u32, Option<String>) + Send + Sync>, cancel: Arc<AtomicBool>) -> Self {
        Self {
            bytes: Default::default(),
            on_progress,
            cancel,
            results: Default::default(),
            paused: Default::default(),
            limit: Default::default(),
            verify: Default::default(),
            ask: None,
            own_engine_only: false,
        }
    }
}
