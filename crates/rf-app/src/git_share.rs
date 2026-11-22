//! One `git status` per repository at a time, shared by every view of it. Ten tabs (or two panes) in one repository used to start ten
//! git processes for the same answer: callers of [`status`] for a repository whose status is being read wait for that run, and a
//! finished answer is reused for a moment.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a finished answer is handed to callers that do not insist on a new one.
const REUSE: Duration = Duration::from_millis(1500);

struct Slot<T> {
    running: bool,
    /// The last finished run: when it started, when it ended, what it gave.
    done: Option<(Instant, Instant, T)>,
}

/// Single-flight with a short memory, per key.
struct Flights<K, T> {
    slots: Mutex<HashMap<K, Slot<T>>>,
    changed: Condvar,
}

impl<K: Hash + Eq + Clone, T: Clone> Flights<K, T> {
    fn new() -> Self {
        Self { slots: Mutex::new(HashMap::new()), changed: Condvar::new() }
    }

    /// The answer for `key`, running `f` only when nobody is. With `fresh` the answer must come from a run that started after this call
    /// (the files changed just now: a run already going or a finished one may not have seen it); otherwise one finished within `reuse` will do.
    fn get(&self, key: &K, fresh: bool, reuse: Duration, f: impl FnOnce() -> T) -> T {
        let arrived = Instant::now();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let slot = slots.entry(key.clone()).or_insert(Slot { running: false, done: None });
            if let Some((started, ended, v)) = &slot.done
                && ((!fresh && ended.elapsed() < reuse) || *started >= arrived)
            {
                return v.clone();
            }
            if !slot.running {
                slot.running = true;
                break;
            }
            slots = self.changed.wait(slots).unwrap_or_else(|e| e.into_inner());
        }
        drop(slots);
        // (if `f` panics the slot must not stay "running" for ever)
        struct Reset<'a, K: Hash + Eq + Clone, T>(&'a Flights<K, T>, &'a K, bool);
        impl<K: Hash + Eq + Clone, T> Drop for Reset<'_, K, T> {
            fn drop(&mut self) {
                if self.2 {
                    let mut s = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(slot) = s.get_mut(self.1) {
                        slot.running = false;
                    }
                    self.0.changed.notify_all();
                }
            }
        }
        let mut guard = Reset(self, key, true);
        let started = Instant::now();
        let v = f();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get_mut(key) {
            slot.done = Some((started, Instant::now(), v.clone()));
        }
        guard.2 = false;
        if let Some(slot) = slots.get_mut(key) {
            slot.running = false;
        }
        self.changed.notify_all();
        v
    }
}

/// `leaf_sys::git::status` for the repository at `root`, shared (see the module doc). `fresh`: the caller knows something changed on the disk
/// just now. Blocks: call off the UI thread.
pub fn status(root: &Path, fresh: bool) -> Option<Arc<leaf_sys::git::GitStatus>> {
    static FLIGHTS: OnceLock<Flights<PathBuf, Option<Arc<leaf_sys::git::GitStatus>>>> = OnceLock::new();
    FLIGHTS.get_or_init(Flights::new).get(&root.to_path_buf(), fresh, REUSE, || leaf_sys::git::status(root).ok().map(Arc::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn one_run_is_shared_and_fresh_forces_another() {
        let f: Flights<u8, usize> = Flights::new();
        let runs = AtomicUsize::new(0);
        let run = || runs.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(f.get(&1, false, Duration::from_secs(5), run), 1);
        assert_eq!(f.get(&1, false, Duration::from_secs(5), run), 1);
        assert_eq!(f.get(&1, true, Duration::from_secs(5), run), 2);
        assert_eq!(f.get(&2, false, Duration::from_secs(5), run), 3);
    }
}
