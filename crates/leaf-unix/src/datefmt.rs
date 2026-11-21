//! Linux backend of rf_win::datefmt. Status: STUB

use chrono::DateTime;
use chrono::Local;
use leaf_common::stub;

pub fn short_date_time(_dt: &DateTime<Local>) -> Option<String> {
    stub!("datefmt::short_date_time")
}
