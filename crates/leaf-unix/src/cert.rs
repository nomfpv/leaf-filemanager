//! Linux backend of rf_win::cert. Status: N/A (Authenticode certificates: none on Linux)

use leaf_common::stub;
use std::path::Path;

// TYPE-COPY(rf-win src/cert.rs:12)
#[derive(Debug, Clone, PartialEq)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: i64,
    pub not_after: i64,
}

pub fn info(_path: &Path) -> Option<CertInfo> {
    stub!("cert::info")
}
