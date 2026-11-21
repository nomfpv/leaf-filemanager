//! Linux backend of rf_win::locks: who holds a path open, found by scanning `/proc/*/fd`. A hint only (other users' processes cannot be
//! looked into, and a file held through a mapping alone is not seen). Never panics.

use std::path::PathBuf;

pub use leaf_common::locks::{Locker, LockerKind};

/// The programs that have any of `paths` open (a folder counts for what is open inside it too).
pub fn who_locks(paths: &[PathBuf]) -> Vec<Locker> {
    let wanted: Vec<PathBuf> = paths.iter().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())).collect();
    let mut out = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else { return out };
    let me = std::process::id();
    for e in procs.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
        let held = fds.flatten().any(|fd| std::fs::read_link(fd.path()).map(|t| wanted.iter().any(|w| t.starts_with(w))).unwrap_or(false));
        if held {
            let name = std::fs::read_to_string(e.path().join("comm")).map(|s| s.trim().to_string()).unwrap_or_default();
            out.push(Locker { name: if name.is_empty() { format!("pid {pid}") } else { name }, pid, kind: LockerKind::Other });
        }
    }
    out
}
