//! Free-space check before a copy or move to another drive: the size of the selection against what the target has left.
//! Asks before starting when it will not fit; a target whose free space is unknown (0) is let through.

use gpui_kit::App;
use std::path::{Path, PathBuf};

use crate::t;

/// Only where it can matter: a move inside one drive is a rename, archives and servers have no local free space.
pub fn needed(sources: &[PathBuf], dest: &Path) -> bool {
    if sources.is_empty() || crate::archive_view::split(dest).is_some() || crate::remote::is_address_path(dest) {
        return false;
    }
    !(cfg!(windows) && sources.iter().all(|s| same_drive(s, dest)))
}

fn same_drive(a: &Path, b: &Path) -> bool {
    use std::path::Component;
    let root = |p: &Path| match p.components().next() {
        Some(Component::Prefix(x)) => Some(x.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    };
    root(a).is_some() && root(a) == root(b)
}

/// `Some((needed, free))` when the selection is bigger than the room at `dest`; on a worker (it walks the folders).
pub fn shortfall(sources: &[PathBuf], dest: &Path) -> Option<(u64, u64)> {
    let free = leaf_sys::fileattr::free_space(dest);
    if free == 0 {
        return None;
    }
    let mut total = 0u64;
    for s in sources {
        total = total.saturating_add(tree_size(s));
        if total > free {
            return Some((total, free));
        }
    }
    None
}

fn tree_size(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else { return 0 };
    if m.is_file() {
        return m.len();
    }
    if !m.is_dir() {
        return 0;
    }
    let mut sum = 0u64;
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                sum = sum.saturating_add(e.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    sum
}

/// The question; `go` starts the transfer when the answer is yes.
pub fn ask(cx: &mut App, need: u64, free: u64, go: impl FnOnce(&mut App) + 'static) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        return go(cx);
    };
    let msg = format!(
        "{} {}, {} {}.\n\n{}",
        t!("The selection is"),
        rf_core::organize::summary::size_text(need),
        t!("the target has only"),
        rf_core::organize::summary::size_text(free),
        t!("Start anyway?")
    );
    let mut go = Some(go);
    let mut rx = None;
    let _ = w.update(cx, |_, window, cx| rx = Some(crate::prompt::ask(t!("Not enough free space"), msg, window, cx)));
    match (rx, go.take()) {
        (Some(rx), Some(go)) => cx
            .spawn(async move |cx| {
                if rx.await.unwrap_or(false) {
                    cx.update(go);
                }
            })
            .detach(),
        (None, Some(go)) => go(cx),
        _ => {}
    }
}
