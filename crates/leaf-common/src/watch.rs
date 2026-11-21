//! Directory change notifications (ReadDirectoryChangesW via `notify`), debounced.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Keeps the watch alive; drop to stop.
pub struct DirWatch {
    _w: RecommendedWatcher,
}

/// What changed in a watched folder during one burst.
#[derive(Debug, Default, Clone)]
pub struct Changes {
    /// Names (directly in the folder) that were added, removed, renamed from or to, or changed.
    pub names: Vec<String>,
    /// The names can't be told (the system dropped events, too many changed, the folder itself
    /// changed): list the folder again.
    pub rescan: bool,
}

/// More changed names than this in one burst: listing the folder again is cheaper.
const MAX_NAMES: usize = 256;

/// The path given to `notify`. On Windows notify opens the directory without the `\\?\` prefix: past MAX_PATH that fails and the
/// view would silently stop updating. Give it the long form there.
#[cfg(windows)]
fn watch_target(dir: &Path) -> std::path::PathBuf {
    if dir.as_os_str().len() >= 240 {
        use std::os::windows::ffi::OsStringExt;
        let mut long = crate::wide::long_path(dir);
        long.pop(); // NUL
        std::path::PathBuf::from(std::ffi::OsString::from_wide(&long))
    } else {
        dir.to_path_buf()
    }
}

/// The path given to `notify`: the folder as it is.
#[cfg(not(windows))]
fn watch_target(dir: &Path) -> std::path::PathBuf {
    dir.to_path_buf()
}

/// Calls `on_change` (from a background thread) once changes in `dir` pause for `debounce`, and
/// during a burst that never pauses (files renamed in the background without end) at least every
/// `debounce` × 5, so the view never falls behind for long.
pub fn watch_dir(dir: &Path, debounce: Duration, on_change: impl Fn() + Send + Sync + 'static) -> anyhow::Result<DirWatch> {
    watch_dir_changes(dir, debounce, move |_| on_change())
}

/// [`watch_dir`] telling what changed: the names, or that the folder must be listed again.
pub fn watch_dir_changes(dir: &Path, debounce: Duration, on_change: impl Fn(Changes) + Send + Sync + 'static) -> anyhow::Result<DirWatch> {
    enum Ev {
        Paths(Vec<std::path::PathBuf>),
        Rescan,
    }
    let (tx, rx) = crossbeam_channel::unbounded::<Ev>();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
        Ok(ev) => {
            if !matches!(ev.kind, notify::EventKind::Access(_)) {
                let _ = tx.send(if ev.need_rescan() || ev.paths.is_empty() { Ev::Rescan } else { Ev::Paths(ev.paths) });
            }
        }
        Err(_) => {
            let _ = tx.send(Ev::Rescan);
        }
    })?;
    let watched = dir.to_string_lossy().trim_start_matches(r"\\?\").trim_end_matches('\\').to_lowercase();
    let name_of = move |p: &Path| -> Option<String> {
        // a direct child of the folder (anything else, the folder itself included: rescan)
        let parent = p.parent()?.to_string_lossy().trim_start_matches(r"\\?\").trim_end_matches('\\').to_lowercase();
        if parent != watched {
            return None;
        }
        Some(p.file_name()?.to_str()?.to_string())
    };
    let target = watch_target(dir);
    w.watch(&target, RecursiveMode::NonRecursive)?;
    let on_change = Arc::new(on_change);
    let max_wait = debounce * 5;
    std::thread::Builder::new().name("rf-watch".into()).spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut ch = Changes::default();
            let mut seen = std::collections::HashSet::new();
            let mut take = |ev: Ev, ch: &mut Changes| match ev {
                Ev::Rescan => ch.rescan = true,
                Ev::Paths(ps) => {
                    for p in ps {
                        match name_of(&p) {
                            Some(n) if !ch.rescan => {
                                if seen.insert(n.clone()) {
                                    ch.names.push(n);
                                }
                                if ch.names.len() > MAX_NAMES {
                                    ch.rescan = true;
                                }
                            }
                            Some(_) => {}
                            None => ch.rescan = true,
                        }
                    }
                }
            };
            take(first, &mut ch);
            // swallow the burst, but not for longer than max_wait
            let burst = std::time::Instant::now();
            loop {
                let left = max_wait.saturating_sub(burst.elapsed());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(debounce.min(left)) {
                    Ok(ev) => take(ev, &mut ch),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                }
            }
            if ch.rescan {
                ch.names.clear();
            }
            on_change(ch);
        }
    })?;
    Ok(DirWatch { _w: w })
}
