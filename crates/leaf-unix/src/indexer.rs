//! Linux backend of rf_win::indexer. Status: STUB

/// Request/response: the answer is the return value. `None` (no index here: the caller searches another way); `rank` is never
/// called.
pub fn search_ranked(_pattern: &str, _under: &Path, _include_hidden: bool, _max: usize, _rank: &(dyn Fn(&str) -> Option<i32> + Sync)) -> Option<Vec<(i32, PathBuf)>> {
    stub!("indexer::search_ranked")
}
