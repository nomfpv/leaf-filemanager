//! Linux backend of rf_win::ftp. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

use rf_core::Entry;

pub fn is_ftp(_p: &Path) -> bool {
    stub!("ftp::is_ftp")
}

pub fn list(_url: &str) -> std::io::Result<Vec<Entry>> {
    stub_err!("ftp::list")
}

pub fn download(_url: &str, _dest: &Path) -> std::io::Result<()> {
    stub_err!("ftp::download")
}

pub fn upload(_local: &Path, _dir_url: &str) -> std::io::Result<()> {
    stub_err!("ftp::upload")
}
