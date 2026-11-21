//! Linux backend of rf_win::drives. Status: STUB


use leaf_common::stub;

pub use leaf_common::drives::{Drive, DriveKind};

/// The mounted file systems (`/proc/self/mountinfo`, later: C2). Re-exported at the crate root, like rf-win's.
pub fn list_drives() -> Vec<Drive> {
    stub!("drives::list_drives")
}

pub fn letter_bit(_letter: char) -> Option<u32> {
    stub!("drives::letter_bit")
}

pub fn mask_of_letters<S: AsRef<str>>(_letters: &[S]) -> u32 {
    stub!("drives::mask_of_letters")
}

pub fn letters_of_mask(_mask: u32) -> Vec<char> {
    stub!("drives::letters_of_mask")
}

pub fn set_hidden(_mask: u32) {
    stub!("drives::set_hidden")
}

pub fn hidden() -> u32 {
    stub!("drives::hidden")
}

pub fn policy_hidden() -> u32 {
    stub!("drives::policy_hidden")
}

pub fn visible_drives() -> Vec<Drive> {
    stub!("drives::visible_drives")
}

pub fn slow_to_read(_path: &std::path::Path) -> bool {
    stub!("drives::slow_to_read")
}

pub fn drive_mask() -> u32 {
    stub!("drives::drive_mask")
}
