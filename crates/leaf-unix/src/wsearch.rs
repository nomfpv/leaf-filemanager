//! Linux backend of rf_win::wsearch (Windows Search). Status: N/A (the index of this system is leaf's own: `indexer`, `nameindex`)

use std::path::Path;

use leaf_common::stub_err;

use crate::search::SearchHit;

/// There is no system search index to ask; the app then walks the folder or asks leaf's own index.
pub fn is_indexed(_root: &Path) -> bool {
    false
}

/// Never asked when `is_indexed` is false; answers an error, not an empty success.
pub fn search(_root: &Path, _query: &str, _max_results: usize, _cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<SearchHit>)) -> anyhow::Result<usize> {
    on_batch(Vec::new());
    stub_err!("wsearch::search")
}
