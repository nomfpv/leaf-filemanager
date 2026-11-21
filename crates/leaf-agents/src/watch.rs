//! File watching with a debounce: `on_change` runs on a thread of its own, at most once every two seconds, after the first change of a burst.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

const QUIET: Duration = Duration::from_secs(2);

/// Dropping it stops the watching and the thread.
pub struct Watch {
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Watch {
    /// Watches the folders that exist (recursively). Folders that do not exist now are not watched: nothing says an agent will ever
    /// create them, and the caller's slow tick finds them. `None` when no folder could be watched.
    pub fn start(roots: &[PathBuf], on_change: impl Fn() + Send + 'static) -> Option<Watch> {
        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // access events (a read) are not changes
            if res.is_ok_and(|e| !matches!(e.kind, notify::EventKind::Access(_))) {
                let _ = tx.send(());
            }
        })
        .ok()?;
        let mut any = false;
        for r in roots.iter().filter(|r| r.is_dir()) {
            any |= watcher.watch(r, RecursiveMode::Recursive).is_ok();
        }
        if !any {
            return None;
        }
        std::thread::Builder::new()
            .name("leaf-agents-watch".into())
            .spawn(move || {
                // ends when the watcher (and with it the sender) is dropped
                while rx.recv().is_ok() {
                    // a session that writes all the time never goes quiet: wait once, then take what came in as one change
                    std::thread::sleep(QUIET);
                    while rx.try_recv().is_ok() {}
                    on_change();
                }
            })
            .ok()?;
        Some(Watch { _watcher: Some(watcher) })
    }
}
