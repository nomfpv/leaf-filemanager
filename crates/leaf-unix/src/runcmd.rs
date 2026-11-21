//! Linux backend of rf_win::runcmd. Status: STUB

use leaf_common::stub;
use std::path::Path;

use rf_core::addrcmd::CommandLine;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/runcmd.rs:24)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub exe: PathBuf,
    pub args: String,
}

pub fn find(_readings: &[CommandLine], _work_dir: &Path) -> Option<Found> {
    stub!("runcmd::find")
}

// TYPE-COPY(rf-win src/runcmd.rs:117)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    Yes,
    Declined,
    Pending,
    Failed(String),
}

pub fn start(_found: &Found, _work_dir: &Path) -> Started {
    // STUB(linux): nothing is started
    stub::hit("runcmd::start");
    Started::Failed("not on Linux yet".to_string())
}

pub fn is_program_file(_p: &Path) -> bool {
    stub!("runcmd::is_program_file")
}

pub fn search_dirs() -> Vec<PathBuf> {
    stub!("runcmd::search_dirs")
}
