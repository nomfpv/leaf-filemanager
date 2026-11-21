//! Linux backend of rf_win::encoding. Status: STUB

use leaf_common::stub;

pub fn decode_text(_raw: &[u8]) -> String {
    stub!("encoding::decode_text")
}

pub fn user_locale_is_turkish() -> bool {
    stub!("encoding::user_locale_is_turkish")
}
