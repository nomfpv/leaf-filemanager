//! Linux backend of rf_win::diskusage. Status: STUB
//!
//! No walk yet. A tree is always empty and marked cancelled (the walk did not run to its end), and a scan hands out that empty
//! tree on its first `take`, so whoever polls for it stops waiting.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64};

use leaf_common::stub;

// TYPE-COPY(rf-win diskusage.rs:32)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Logical,
    OnDisk,
}

// TYPE-COPY(rf-win diskusage.rs:42)
#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub bytes: AtomicU64,
}

/// A walked tree. Only the root is kept (`root_path` borrows it); there are no entries.
// TYPE-COPY(rf-win diskusage.rs:48): the entry columns dropped
pub struct DiskTree {
    root: PathBuf,
    cancelled: bool,
}

impl DiskTree {
    pub fn build(root: &Path, _cancel: &AtomicBool, _progress: &Progress) -> DiskTree {
        // STUB(linux): DiskTree has no Default; an empty tree that did not finish
        stub::hit("diskusage::DiskTree::build");
        DiskTree { root: root.to_path_buf(), cancelled: true }
    }

    pub fn root_path(&self) -> &Path {
        stub::hit("diskusage::DiskTree::root_path");
        &self.root
    }

    pub fn len(&self) -> usize {
        stub!("diskusage::DiskTree::len")
    }

    pub fn is_empty(&self) -> bool {
        // (the tree has no entries: true is the honest answer, not the bool default)
        stub::hit("diskusage::DiskTree::is_empty");
        true
    }

    pub fn cancelled(&self) -> bool {
        stub::hit("diskusage::DiskTree::cancelled");
        self.cancelled
    }

    pub fn skipped(&self) -> u32 {
        stub!("diskusage::DiskTree::skipped")
    }

    pub fn memory(&self) -> usize {
        stub!("diskusage::DiskTree::memory")
    }

    pub fn name(&self, _i: u32) -> &str {
        stub::hit("diskusage::DiskTree::name");
        ""
    }

    pub fn is_dir(&self, _i: u32) -> bool {
        stub!("diskusage::DiskTree::is_dir")
    }

    pub fn parent(&self, _i: u32) -> Option<u32> {
        stub!("diskusage::DiskTree::parent")
    }

    pub fn size(&self, _i: u32) -> u64 {
        stub!("diskusage::DiskTree::size")
    }

    pub fn allocated(&self, _i: u32) -> u64 {
        stub!("diskusage::DiskTree::allocated")
    }

    pub fn weight(&self, _i: u32, _m: Metric) -> u64 {
        stub!("diskusage::DiskTree::weight")
    }

    pub fn files(&self, _i: u32) -> u32 {
        stub!("diskusage::DiskTree::files")
    }

    pub fn children(&self, _i: u32) -> std::ops::Range<u32> {
        stub!("diskusage::DiskTree::children")
    }

    pub fn sorted_children(&self, _i: u32, _m: Metric) -> Vec<u32> {
        stub!("diskusage::DiskTree::sorted_children")
    }

    pub fn path(&self, _i: u32) -> PathBuf {
        stub!("diskusage::DiskTree::path")
    }

    pub fn find(&self, _path: &Path) -> Option<u32> {
        stub!("diskusage::DiskTree::find")
    }
}

/// A walk "in the background": none runs; the empty tree is ready at once.
// TYPE-COPY(rf-win diskusage.rs:345): the cancel flag and the shared progress dropped
pub struct DiskScan {
    result: Mutex<Option<DiskTree>>,
}

impl DiskScan {
    pub fn start(root: PathBuf) -> DiskScan {
        // STUB(linux): DiskScan has no Default; request/response: the one answer (the empty tree) waits in `take`
        stub::hit("diskusage::DiskScan::start");
        DiskScan { result: Mutex::new(Some(DiskTree { root, cancelled: true })) }
    }

    pub fn progress(&self) -> (u64, u64) {
        stub!("diskusage::DiskScan::progress")
    }

    pub fn take(&self) -> Option<DiskTree> {
        stub::hit("diskusage::DiskScan::take");
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn cancel(&self) {
        stub!("diskusage::DiskScan::cancel")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_hands_out_one_empty_unfinished_tree() {
        let s = DiskScan::start(PathBuf::from("/data"));
        let t = s.take().expect("the one answer");
        assert!(s.take().is_none());
        assert_eq!(t.root_path(), Path::new("/data"));
        assert!(t.is_empty() && t.cancelled());
        assert_eq!(t.children(0), 0..0);
        assert_eq!(t.name(0), "");
    }
}
