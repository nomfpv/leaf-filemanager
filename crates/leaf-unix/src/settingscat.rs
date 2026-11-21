//! Linux backend of rf_win::settingscat. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::sync::Arc;

// TYPE-COPY(rf-win src/settingscat.rs:33)
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Launch {
    Page(String),
    Control(String),
    Command(String),
    Shell(String),
}

impl Launch {
    pub fn encode(&self) -> String {
        stub!("settingscat::Launch::encode")
    }
    pub fn decode(_s: &str) -> Option<Launch> {
        stub!("settingscat::Launch::decode")
    }
    pub fn is_classic(&self) -> bool {
        stub!("settingscat::Launch::is_classic")
    }
}

// TYPE-COPY(rf-win src/settingscat.rs:77)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub place: String,
    pub keywords: String,
    pub launch: Launch,
}

pub fn get_or_start(_cache_dir: &Path) -> Option<Arc<Vec<Entry>>> {
    stub!("settingscat::get_or_start")
}

pub fn open(_launch: &Launch) -> anyhow::Result<()> {
    stub_err!("settingscat::open")
}
