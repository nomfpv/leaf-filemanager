//! Linux backend of rf_win::pe. Status: N/A (Windows PE version resources: the Windows reader uses the version API, not shareable; an ELF file's facts are read by leaf_common::elf for the info card)

use leaf_common::stub;
use std::path::Path;

// TYPE-COPY(rf-win src/pe.rs:11)
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PeInfo {
    pub arch: &'static str,
    pub version: Option<String>,
    pub product: Option<String>,
    pub company: Option<String>,
    pub description: Option<String>,
    pub product_version: Option<String>,
    pub copyright: Option<String>,
    pub original_filename: Option<String>,
    pub language: Option<String>,
    pub trademarks: Option<String>,
    pub internal_name: Option<String>,
}

pub fn info(_path: &Path) -> Option<PeInfo> {
    stub!("pe::info")
}
