//! Linux backend of rf_win::osver. Status: N/A (Windows 11 system backdrops (Mica) have no Linux counterpart: false)

use leaf_common::stub;

pub fn system_backdrops() -> bool {
    stub!("osver::system_backdrops")
}
