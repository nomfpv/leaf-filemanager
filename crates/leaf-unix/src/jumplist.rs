//! Linux backend of rf_win::jumplist. Status: N/A (the desktop actions are static in leaf's own `.desktop` file; the recent-places list of `recently-used.xbel` belongs to every program, leaf does not write it)

use leaf_common::stub_err;
use std::path::Path;
use std::path::PathBuf;

pub const NEW_WINDOW_ARG: &str = "--new-window";

pub type Category = (String, Vec<(String, PathBuf)>);

pub fn set(_exe: &Path, _tasks: &[(&str, &str)], _categories: &[Category]) -> anyhow::Result<()> {
    stub_err!("jumplist::set")
}
