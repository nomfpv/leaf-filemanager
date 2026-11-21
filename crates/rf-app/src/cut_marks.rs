//! The items cut with Ctrl+X that wait for a paste: the lists grey them and mark them "cut" (the mockups' cut rows).
//! Set when leaf puts a move on the clipboard, cleared by the next copy or cut, and by a paste (which moves them away). A cut made in
//! another program is not shown.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static MARKS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
/// Read once per frame by every list: no lock while nothing is cut.
static ANY: AtomicBool = AtomicBool::new(false);

/// These items are now the ones cut (`paths` empty: none).
pub fn set(paths: &[PathBuf]) {
    let mut m = MARKS.lock();
    *m = (!paths.is_empty()).then(|| paths.iter().cloned().collect());
    ANY.store(m.is_some(), Ordering::Relaxed);
}

pub fn clear() {
    set(&[]);
}

pub fn any() -> bool {
    ANY.load(Ordering::Relaxed)
}

pub fn contains(p: &Path) -> bool {
    MARKS.lock().as_ref().is_some_and(|m| m.contains(p))
}
