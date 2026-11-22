//! Linux backend of rf_win::scrollbars. Status: N/A (no classic always-on scroll bars setting on Linux; the answer false is the truth)

use leaf_common::stub;

pub fn always_shown() -> bool {
    stub!("scrollbars::always_shown")
}
