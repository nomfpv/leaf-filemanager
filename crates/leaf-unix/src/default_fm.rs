//! Linux backend of rf_win::default_fm. Status: STUB

use leaf_common::{stub, stub_err};

pub const CLASSES: &str = r"Software\Classes";

pub fn enable(_exe: &str, _classes_root: &str) -> anyhow::Result<()> {
    stub_err!("default_fm::enable")
}

pub const APP_NAME: &str = "leaf";

pub fn add_archives(_exe: &str, _classes_root: &str) -> bool {
    stub!("default_fm::add_archives")
}

pub fn disable(_classes_root: &str) -> anyhow::Result<()> {
    stub_err!("default_fm::disable")
}

pub fn is_enabled(_classes_root: &str) -> bool {
    stub!("default_fm::is_enabled")
}

pub fn heal(_current: &str, _classes_root: &str) -> bool {
    stub!("default_fm::heal")
}

pub fn forwarded_recently(_arg: &str) -> bool {
    stub!("default_fm::forwarded_recently")
}

pub fn note_forward(_arg: &str) {
    stub!("default_fm::note_forward")
}
