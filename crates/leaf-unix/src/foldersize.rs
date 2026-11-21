//! Linux backend of rf_win::foldersize. Status: STUB
//!
//! No folder is measured yet. A batch is finished at once and every folder in it stays at `Partial(0)` (the Size column's "not
//! measured"); the cache holds nothing. The shared cache is an inert static.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leaf_common::{stub, stub_err};

// TYPE-COPY(rf-win foldersize.rs:37)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderSize {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub allocated: Option<u64>,
}

static CACHE: FolderSizeCache = FolderSizeCache { _p: () };

/// Folder sizes shared by the Size column, the preview pane and Properties: an inert static.
pub fn shared_cache() -> &'static FolderSizeCache {
    stub::hit("foldersize::shared_cache");
    &CACHE
}

// TYPE-COPY(rf-win foldersize.rs:321)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeState {
    Partial(u64),
    Provisional(FolderSize),
    Done(FolderSize),
}

/// Sizes of several folders: none is measured; the batch is over as soon as it starts.
// TYPE-COPY(rf-win foldersize.rs:333): the slots and the stop flag dropped
pub struct SizeBatch {
    len: usize,
}

impl SizeBatch {
    pub fn start(dirs: Vec<PathBuf>) -> Arc<Self> {
        stub::hit("foldersize::SizeBatch::start");
        Arc::new(SizeBatch { len: dirs.len() })
    }

    pub fn len(&self) -> usize {
        stub::hit("foldersize::SizeBatch::len");
        self.len
    }

    pub fn is_empty(&self) -> bool {
        stub::hit("foldersize::SizeBatch::is_empty");
        self.len == 0
    }

    pub fn state(&self, _i: usize) -> SizeState {
        // STUB(linux): SizeState has no Default; nothing counted (never `Done`: that would be a size of 0 bytes)
        stub::hit("foldersize::SizeBatch::state");
        SizeState::Partial(0)
    }

    pub fn totals(&self, _i: usize) -> FolderSize {
        stub!("foldersize::SizeBatch::totals")
    }

    /// True: nothing runs, so nothing is left to wait for (a poller stops).
    pub fn finished(&self) -> bool {
        stub::hit("foldersize::SizeBatch::finished");
        true
    }

    pub fn cancel(&self) {
        stub!("foldersize::SizeBatch::cancel")
    }
}

/// The cache of measured folders: holds nothing.
// TYPE-COPY(rf-win foldersize.rs:439): the map, the counters and the save signal dropped
pub struct FolderSizeCache {
    _p: (),
}

impl FolderSizeCache {
    pub fn new(_capacity: usize) -> Self {
        stub::hit("foldersize::FolderSizeCache::new");
        FolderSizeCache { _p: () }
    }

    pub fn get(&self, _dir: &Path) -> Option<FolderSize> {
        stub!("foldersize::FolderSizeCache::get")
    }

    pub fn provisional(&self, _dir: &Path) -> Option<FolderSize> {
        stub!("foldersize::FolderSizeCache::provisional")
    }

    pub fn insert(&self, _dir: &Path, _size: FolderSize) {
        stub!("foldersize::FolderSizeCache::insert")
    }

    pub fn invalidate(&self, _dir: &Path) {
        stub!("foldersize::FolderSizeCache::invalidate")
    }

    pub fn clear(&self) {
        stub!("foldersize::FolderSizeCache::clear")
    }

    /// Request/response with an error: `progress` is never called (no running totals), the answer is `Err`.
    pub fn get_or_compute(&self, _dir: &Path, _cancelled: &dyn Fn() -> bool, _progress: &mut dyn FnMut(&FolderSize)) -> anyhow::Result<FolderSize> {
        stub_err!("foldersize::FolderSizeCache::get_or_compute")
    }

    pub fn save_to(&self, _path: &Path, _limit: usize) -> std::io::Result<()> {
        stub_err!("foldersize::FolderSizeCache::save_to")
    }

    pub fn load_from(&self, _path: &Path) -> usize {
        stub!("foldersize::FolderSizeCache::load_from")
    }

    pub fn persist(&'static self, _path: PathBuf) {
        stub!("foldersize::FolderSizeCache::persist")
    }
}
