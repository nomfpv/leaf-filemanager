//! Linux backend of rf_win::recycle. Status: STUB

use leaf_common::stub_err;

pub use leaf_common::recycle::RecycleItem;

/// Lists everything in the bin: nothing is listed on Linux yet.
pub fn list() -> anyhow::Result<Vec<RecycleItem>> {
    stub_err!("recycle::list")
}

/// Restores `items` to their original locations: nothing is restored on Linux yet.
pub fn restore(_items: &[RecycleItem]) -> anyhow::Result<()> {
    stub_err!("recycle::restore")
}

/// The real bin behind [`rf_core::recycle_bin::BinBackend`]: no bin here yet.
// TYPE-COPY(rf-win recycle.rs:284)
pub struct ShellBin;

/// Empties the bin on all drives: nothing is emptied on Linux yet.
pub fn empty(_confirm: bool) -> anyhow::Result<()> {
    stub_err!("recycle::empty")
}

impl rf_core::recycle_bin::BinBackend for ShellBin {
    fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        stub_err!("recycle::ShellBin::list_keys")
    }

    fn delete_keys(&self, _keys: &[String]) -> anyhow::Result<()> {
        stub_err!("recycle::ShellBin::delete_keys")
    }
}
