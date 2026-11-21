//! Linux backend of rf_win::props. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

pub fn media_props(_path: &Path) -> Vec<(&'static str, String)> {
    stub!("props::media_props")
}

// TYPE-COPY(rf-win src/props.rs:79)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Editable {
    pub title: String,
    pub subject: String,
    pub tags: String,
    pub comment: String,
    pub authors: String,
}

pub fn editable(_path: &Path) -> Option<Editable> {
    stub!("props::editable")
}

pub fn set_editable(_path: &Path, _old: &Editable, _new: &Editable) -> anyhow::Result<()> {
    stub_err!("props::set_editable")
}
