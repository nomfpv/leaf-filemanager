//! Linux backend of rf_win::drives. Status: STUB

use leaf_common::stub;

pub use leaf_common::drives::{Drive, DriveKind};

/// The mounted file systems (`/proc/self/mountinfo`, later: C2). Re-exported at the crate root, like rf-win's.
pub fn list_drives() -> Vec<Drive> {
    stub!("drives::list_drives")
}
