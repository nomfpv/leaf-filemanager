//! Linux backend of rf_win::apppkg. Status: STUB

use leaf_common::stub;
use std::path::Path;

// TYPE-COPY(rf-win src/apppkg.rs:14)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Msix,
    MsixBundle,
    Apk,
    Ipa,
    Nuget,
}

// TYPE-COPY(rf-win src/apppkg.rs:23)
#[derive(Debug, Clone, Default)]
pub struct AppInfo {
    pub name: Option<String>,
    pub id: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub arch: Vec<String>,
    pub min_os: Option<String>,
    pub permissions: usize,
    pub description: Option<String>,
    pub icon_png: Option<Vec<u8>>,
}

pub fn icon(_bytes: &[u8], _max: u32) -> Option<crate::thumbs::Bgra> {
    stub!("apppkg::icon")
}

pub fn kind_of(_path: &Path) -> Option<Kind> {
    stub!("apppkg::kind_of")
}

pub fn info(_path: &Path) -> Option<(Kind, AppInfo)> {
    stub!("apppkg::info")
}
