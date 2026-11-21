//! Linux backend of the leaf-sys module `lock` (its Windows side, the named mutex of rf-app settings_merge.rs, moves to rf-win in
//! step A3 with this same API). Status: REAL
//!
//! A lock shared by every process of the user that saves one settings file, so that two saves do not read-modify-write at the same
//! moment: an exclusive `flock` on `<file>.lock` beside it. Waits at most three seconds; if it cannot be had the caller goes on without
//! it (a lost change needs a read and a rename of two processes in the same few milliseconds; a settings file that is never saved is
//! worse).

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long [`FileLock::acquire`] waits.
pub const WAIT: Duration = Duration::from_secs(3);

/// Held while the file is saved; dropping it lets the next process in.
pub struct FileLock {
    // (closing the file releases the lock)
    _file: File,
}

impl FileLock {
    /// The lock of the file at `path`, waiting at most [`WAIT`]; `None` when it could not be had in that time.
    pub fn acquire(path: &Path) -> Option<FileLock> {
        Self::acquire_within(path, WAIT)
    }

    /// [`acquire`](Self::acquire) with another limit.
    pub fn acquire_within(path: &Path, limit: Duration) -> Option<FileLock> {
        let lock_path = lock_path(path);
        if let Some(dir) = lock_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&lock_path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("settings: the save lock {} cannot be opened ({e}); saving without it", lock_path.display());
                return None;
            }
        };
        let began = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Some(FileLock { _file: file }),
                Err(TryLockError::WouldBlock) if began.elapsed() < limit => std::thread::sleep(Duration::from_millis(10)),
                Err(TryLockError::WouldBlock) => {
                    log::warn!("settings: the save lock could not be had in {} s; saving without it", limit.as_secs());
                    return None;
                }
                Err(TryLockError::Error(e)) => {
                    log::warn!("settings: the save lock failed ({e}); saving without it");
                    return None;
                }
            }
        }
    }
}

/// `settings.toml` -> `settings.toml.lock`, in the same folder.
fn lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn one_holder_at_a_time_and_the_next_gets_it_when_the_first_lets_go() {
        let dir = std::env::temp_dir().join(format!("leaf-unix-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("settings.toml");
        let first = FileLock::acquire(&file).expect("a free lock is had at once");
        assert!(dir.join("settings.toml.lock").is_file());
        // a second holder (another open file, as another process would have) waits
        let t = Instant::now();
        assert!(FileLock::acquire_within(&file, Duration::from_millis(100)).is_none());
        assert!(t.elapsed() >= Duration::from_millis(100));
        // ... and gets it once the first is dropped
        let (tx, rx) = mpsc::channel();
        let f2 = file.clone();
        let waiter = std::thread::spawn(move || {
            let got = FileLock::acquire_within(&f2, Duration::from_secs(5)).is_some();
            tx.send(got).unwrap();
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        waiter.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
