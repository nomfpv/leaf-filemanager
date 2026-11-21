//! Linux backend of rf_win::wsearch. Status: STUB (no system index: nothing is indexed)

use std::path::Path;

use leaf_common::{stub, stub_err};

use crate::search::SearchHit;

pub fn is_indexed(_root: &Path) -> bool {
    stub!("wsearch::is_indexed")
}

/// Request/response: one empty batch, then `Err`.
pub fn search(_root: &Path, _query: &str, _max_results: usize, _cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<SearchHit>)) -> anyhow::Result<usize> {
    on_batch(Vec::new());
    stub_err!("wsearch::search")
}
