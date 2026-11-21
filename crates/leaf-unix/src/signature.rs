//! Linux backend of rf_win::signature. Status: STUB

use leaf_common::stub;
use std::path::Path;

// TYPE-COPY(rf-win src/signature.rs:24)
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub signer: String,
    pub issuer: String,
    pub digest: String,
    pub timestamp_ms: Option<i64>,
}

// TYPE-COPY(rf-win src/signature.rs:36)
#[derive(Debug, Clone, PartialEq)]
pub struct Signatures {
    pub list: Vec<Signature>,
    pub verdict: Result<(), String>,
}

pub fn read(_path: &Path) -> Option<Signatures> {
    stub!("signature::read")
}
