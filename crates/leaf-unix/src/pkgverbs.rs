//! Linux backend of rf_win::pkgverbs. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::PathBuf;

// TYPE-COPY(rf-win src/pkgverbs.rs:197)
#[derive(Clone, Debug)]
pub struct Shown {
    pub id: usize,
    pub title: String,
    pub icon: Option<PathBuf>,
    pub enabled: bool,
    pub sub: Vec<Shown>,
}

pub fn for_items(_items: &[PathBuf], _background: bool) -> Vec<Shown> {
    stub!("pkgverbs::for_items")
}

pub fn invoke(_id: usize) -> anyhow::Result<()> {
    stub_err!("pkgverbs::invoke")
}
