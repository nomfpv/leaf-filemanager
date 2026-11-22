//! Linux backend of rf_win::overlays. Status: N/A (deliberately empty: nothing to ask the system)
//!
//! On Windows the badges on files (cloud state, shared) come from the shell's overlay handlers of other programs. A Linux file manager has
//! no such registry: Nextcloud and Dropbox paint theirs through their own file manager plug-ins, which leaf does not load. What leaf
//! itself knows about sync jobs and shares lives in the app, not in this backend, so the system layer answers honestly that no file has a
//! badge: `indexes` returns nothing and `picture` has no picture. The app already skips links (it draws their arrow itself).

use crate::thumbs::Bgra;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn indexes(_dir: &Path, _names: Vec<String>, _cancel: Arc<AtomicBool>) -> Vec<(String, i32)> {
    Vec::new()
}

pub fn picture(_index: i32, _px: u32) -> Option<Bgra> {
    None
}
