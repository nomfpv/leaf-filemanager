//! Linux backend of rf_win::dnd. Status: PARTIAL (the drop rules are real; dragging out is B1's)

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use leaf_common::stub_err;

pub use leaf_common::fileops::Transfer;

use crate::Owner;

/// Paths of drags that must never be moved, each with the time its mark ends.
static COPY_ONLY: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());

/// Marks `items` as the subject of a copy-only drag for `for_how_long` (replacing an earlier mark of the same path). Paths are told
/// apart by case here.
pub fn mark_copy_only(items: &[PathBuf], for_how_long: Duration) {
    let now = Instant::now();
    let Ok(mut marks) = COPY_ONLY.lock() else { return };
    marks.retain(|(_, until)| *until > now);
    for p in items {
        marks.retain(|(m, _)| m != p);
        marks.push((p.clone(), now + for_how_long));
    }
}

/// Whether every one of `sources` belongs to a copy-only drag that is going on or has just ended at `now`.
fn copy_only_at(sources: &[PathBuf], now: Instant) -> bool {
    let Ok(marks) = COPY_ONLY.lock() else { return false };
    !sources.is_empty() && sources.iter().all(|s| marks.iter().any(|(m, until)| *until > now && m == s))
}

/// The device a path lives on: the item itself for a source (a link is moved as a link), the folder it names for a destination.
fn device(p: &Path, follow: bool) -> Option<u64> {
    let m = if follow { std::fs::metadata(p) } else { std::fs::symlink_metadata(p) };
    m.ok().map(|m| m.dev())
}

/// The default for a plain drag: on the same device (file system) a move, otherwise a copy; when either side cannot be read, a copy.
/// Ctrl forces copy, Shift forces move. (Not the first path component, as on Windows: here that is always `/`.)
pub fn default_transfer(sources: &[PathBuf], dest_dir: &Path, ctrl: bool, shift: bool) -> Transfer {
    // (a drag that was started copy-only is copied by our own windows as well: not even Shift moves it)
    if copy_only_at(sources, Instant::now()) {
        return Transfer::Copy;
    }
    if ctrl {
        return Transfer::Copy;
    }
    if shift {
        return Transfer::Move;
    }
    match sources.first() {
        Some(s) => match (device(s, false), device(dest_dir, true)) {
            (Some(a), Some(b)) if a == b => Transfer::Move,
            _ => Transfer::Copy,
        },
        None => Transfer::Copy,
    }
}

/// True when dropping `sources` into `dest_dir` makes no sense (onto itself or its own child).
pub fn is_invalid_drop(sources: &[PathBuf], dest_dir: &Path) -> bool {
    sources.iter().any(|s| dest_dir == s || dest_dir.starts_with(s)) || sources.iter().all(|s| s.parent() == Some(dest_dir))
}

/// Starts a drag of `items` out of leaf (it must run on the thread that owns the window): no drag is started on Linux yet. The answer
/// is the drop effect the target chose, as `DROPEFFECT` bits.
pub fn drag_out(_owner: Owner, _ui_thread: u32, _items: &[PathBuf]) -> anyhow::Result<u32> {
    stub_err!("dnd::drag_out")
}

/// [`drag_out`] for files that must not be MOVED by the target (a copy only): no drag is started on Linux yet.
pub fn drag_out_copy_only(_owner: Owner, _ui_thread: u32, _items: &[PathBuf]) -> anyhow::Result<u32> {
    stub_err!("dnd::drag_out_copy_only")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn same_device_moves_another_device_copies_and_the_keys_decide_first() {
        let s = Scratch(std::env::temp_dir().join(format!("leaf-unix-dnd-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&s.0);
        std::fs::create_dir_all(s.0.join("a")).unwrap();
        std::fs::create_dir_all(s.0.join("b")).unwrap();
        let file = s.0.join("a/x.txt");
        std::fs::write(&file, b"x").unwrap();
        let src = vec![file.clone()];
        let b = s.0.join("b");
        assert_eq!(default_transfer(&src, &b, false, false), Transfer::Move, "same file system");
        assert_eq!(default_transfer(&src, &b, true, false), Transfer::Copy, "Ctrl copies");
        // /proc is always another file system (only read, never written)
        let other = Path::new("/proc");
        assert_ne!(device(other, true), device(&b, true));
        assert_eq!(default_transfer(&src, other, false, false), Transfer::Copy, "another device");
        assert_eq!(default_transfer(&src, other, false, true), Transfer::Move, "Shift moves");
        // what cannot be read is copied, never moved
        assert_eq!(default_transfer(&[s.0.join("gone.txt")], &b, false, false), Transfer::Copy);
        assert_eq!(default_transfer(&src, &s.0.join("no-such-dir"), false, false), Transfer::Copy);
        assert_eq!(default_transfer(&[], &b, false, false), Transfer::Copy);
        // a link is judged where it lies, not where it points
        let link = s.0.join("a/to-proc");
        std::os::unix::fs::symlink("/proc/self", &link).unwrap();
        assert_eq!(default_transfer(&[link], &b, false, false), Transfer::Move);
    }

    #[test]
    fn a_copy_only_drag_is_copied_even_on_the_same_device() {
        let s = Scratch(std::env::temp_dir().join(format!("leaf-unix-dnd-co-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&s.0);
        std::fs::create_dir_all(s.0.join("dest")).unwrap();
        let mine = s.0.join("mine.txt");
        let other = s.0.join("other.txt");
        std::fs::write(&mine, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        let dest = s.0.join("dest");
        let one = |p: &PathBuf, shift: bool| default_transfer(std::slice::from_ref(p), &dest, false, shift);
        assert_eq!(one(&mine, false), Transfer::Move);
        mark_copy_only(std::slice::from_ref(&mine), Duration::from_secs(60));
        assert_eq!(one(&mine, false), Transfer::Copy);
        assert_eq!(one(&mine, true), Transfer::Copy, "Shift does not move it");
        assert_eq!(one(&other, false), Transfer::Move);
        assert_eq!(default_transfer(&[mine.clone(), other], &dest, false, false), Transfer::Move);
        assert!(!copy_only_at(std::slice::from_ref(&mine), Instant::now() + Duration::from_secs(61)));
        mark_copy_only(std::slice::from_ref(&mine), Duration::ZERO);
        assert_eq!(one(&mine, false), Transfer::Move);
    }

    #[test]
    fn invalid_drops() {
        let src = vec![PathBuf::from("/data/a/x.txt")];
        assert!(is_invalid_drop(&src, Path::new("/data/a")), "into its own folder");
        assert!(is_invalid_drop(&[PathBuf::from("/data/a")], Path::new("/data/a/sub")), "into itself");
        assert!(!is_invalid_drop(&src, Path::new("/data/b")));
        assert!(!is_invalid_drop(&[PathBuf::from("/data/a")], Path::new("/data/ab")), "a sibling that begins alike");
    }
}
