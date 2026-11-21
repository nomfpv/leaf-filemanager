//! Linux backend of rf_win::search. Status: STUB
//!
//! The worked example of a request/response callback: [`search`] answers once with an empty batch and then with `Err`, so a caller
//! that waits for batches or for the end is never left waiting.

use std::path::PathBuf;

use leaf_common::stub_err;
use rf_core::Entry;

// TYPE-COPY(rf-win search.rs:28)
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub root: PathBuf,
    pub pattern: String,
    pub include_hidden: bool,
    pub max_results: usize,
}

// TYPE-COPY(rf-win search.rs:37)
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub entry: Entry,
}

// TYPE-COPY(rf-win search.rs:43)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub dirs_scanned: usize,
    pub entries_scanned: usize,
    pub hits: usize,
    pub skipped_dirs: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Request/response: one empty batch, then `Err` (no search on Linux yet).
pub fn search(_q: &SearchQuery, _cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<SearchHit>)) -> anyhow::Result<SearchStats> {
    on_batch(Vec::new());
    stub_err!("search::search")
}
