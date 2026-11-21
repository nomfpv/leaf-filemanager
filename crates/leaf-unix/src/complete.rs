//! Linux backend of rf_win::complete. Status: STUB

use leaf_common::stub;
use std::path::PathBuf;

pub fn complete_dirs(_input: &str, _limit: usize, _include_hidden: bool) -> Vec<PathBuf> {
    stub!("complete::complete_dirs")
}

pub fn completable(_p: &str) -> bool {
    stub!("complete::completable")
}
