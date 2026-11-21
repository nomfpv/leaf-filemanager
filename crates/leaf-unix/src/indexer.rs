//! Linux backend of rf_win::indexer. Status: STUB

use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;

/// Request/response: the answer is the return value. `None` (no index here: the caller searches another way); `rank` is never
/// called.
pub fn search_ranked(
    _pattern: &str,
    _under: &Path,
    _include_hidden: bool,
    _max: usize,
    _rank: &(dyn Fn(&str) -> Option<i32> + Sync),
) -> Option<Vec<(i32, PathBuf)>> {
    stub!("indexer::search_ranked")
}

// TYPE-COPY(rf-win src/indexer.rs:23)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Building,
    Ready,
    Failed,
}

pub fn start(_roots: Vec<PathBuf>, _dir: PathBuf) {
    stub!("indexer::start")
}

pub fn stop() {
    stub!("indexer::stop")
}

pub fn search(_pattern: &str, _under: &Path, _include_hidden: bool, _max: usize) -> Option<Vec<PathBuf>> {
    stub!("indexer::search")
}

pub fn status() -> Vec<(PathBuf, State, usize, usize)> {
    stub!("indexer::status")
}

pub fn local_roots() -> Vec<PathBuf> {
    stub!("indexer::local_roots")
}
