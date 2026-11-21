//! Linux backend of rf_win::fileactions. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;

pub fn is_image(_p: &Path) -> bool {
    stub!("fileactions::is_image")
}

pub fn is_font(_p: &Path) -> bool {
    stub!("fileactions::is_font")
}

pub fn is_runnable(_p: &Path) -> bool {
    stub!("fileactions::is_runnable")
}

pub fn set_wallpaper(_path: &Path) -> anyhow::Result<()> {
    stub_err!("fileactions::set_wallpaper")
}

pub fn install_font_for_user(_path: &Path) -> anyhow::Result<PathBuf> {
    stub_err!("fileactions::install_font_for_user")
}
