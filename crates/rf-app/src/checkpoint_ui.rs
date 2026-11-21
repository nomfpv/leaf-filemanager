//! The commands "Checkpoint" and "Checkpoints": the question before a checkpoint is taken (with its size first, outside a
//! repository), and opening the folder's Checkpoints list (the Checkpoints side of the change feed page, `changes_view`). The work is
//! `leaf_dev::checkpoint`, always off the UI thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use leaf_dev::checkpoint::{self, Store};
use rf_core::{Location, format};

use crate::folder_view::{FolderEvent, FolderView};
use crate::t;

/// The folder whose change feed page should open on its Checkpoints side (set just before the page is opened).
static WANT_LIST: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Whether the page being made for `dir` opens on the Checkpoints side (asked once).
pub fn take_want(dir: &Path) -> bool {
    let Ok(mut g) = WANT_LIST.lock() else { return false };
    if g.as_deref() == Some(dir) {
        *g = None;
        return true;
    }
    false
}

/// Where the checkpoints of `dir` are kept (blocks: runs git).
pub fn store_of(dir: &Path) -> Store {
    let data = crate::settings::local_data_dir().unwrap_or_else(std::env::temp_dir);
    checkpoint::store_for(dir, &data)
}

pub fn cap_bytes(cx: &App) -> u64 {
    cx.global::<crate::settings::Settings>().checkpoint_cap_mb.max(1) as u64 * 1024 * 1024
}

pub fn keep(cx: &App) -> usize {
    cx.global::<crate::settings::Settings>().checkpoint_keep.max(1) as usize
}

/// The words of a failure: leaf's sentence, then what git or the file system said on a line of its own.
pub fn error_text(e: &checkpoint::Error) -> String {
    if e.details.is_empty() { format!("{}.", e.message) } else { format!("{}.\n{}", e.message, e.details) }
}

/// Moves files to the Trash (or the Recycle Bin), never deleting them; blocks.
pub fn to_trash(paths: &[PathBuf]) -> Result<(), String> {
    let tracker = Arc::new(leaf_sys::fileops::Tracker::new(Box::new(|_, _, _| {}), Arc::new(AtomicBool::new(false))));
    match leaf_sys::fileops::delete_tracked(paths, false, tracker) {
        Ok(true) => Ok(()),
        Ok(false) => Err(t!("Moving to the Trash was stopped").to_string()),
        Err(e) => Err(format!("{} ({e:#})", t!("They could not be moved to the Trash"))),
    }
}

/// Opens the Checkpoints list of `dir`: on the tab itself when it shows that folder's change feed, else in a new tab.
pub fn show_list(dir: PathBuf, tab: WeakEntity<FolderView>, cx: &mut App) {
    let _ = tab.update(cx, |v, cx| {
        if matches!(v.location(), Location::Changes(p) if *p == dir)
            && let Some(cv) = v.changes_view()
        {
            cv.update(cx, |cv, cx| cv.show_checkpoints(cx));
            return;
        }
        if let Ok(mut g) = WANT_LIST.lock() {
            *g = Some(dir.clone());
        }
        cx.emit(FolderEvent::OpenInNewTab(Location::Changes(dir)));
    });
}

/// "Checkpoint": what it will be (and, outside a repository, how big) first, a note, then the checkpoint and the list.
pub fn new_checkpoint(dir: PathBuf, tab: WeakEntity<FolderView>, window: &mut Window, cx: &mut App) {
    let d2 = dir.clone();
    let work = cx.background_spawn(async move {
        let store = store_of(&d2);
        let est = if store.is_git() { None } else { Some(checkpoint::estimate(&store)) };
        (store, est)
    });
    let cap = cap_bytes(cx);
    window
        .spawn(cx, async move |cx| {
            let (store, est) = work.await;
            let _ = cx.update(|window, cx| ask(dir, store, est, cap, tab, window, cx));
        })
        .detach();
}

fn ask(dir: PathBuf, store: Store, est: Option<Result<(u64, u64), checkpoint::Error>>, cap: u64, tab: WeakEntity<FolderView>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let message = match &est {
        None => format!(
            "{}\n{}",
            t!("A hidden snapshot inside the git repository of every file that is not ignored. Your files, the index, the branch and the stash stay as they are."),
            store.folder().display()
        ),
        Some(Err(e)) => {
            crate::prompt::notice(t!("The checkpoint could not be taken"), error_text(e), window, cx);
            return;
        }
        Some(Ok((files, bytes))) if *bytes > cap => {
            crate::prompt::notice(
                t!("The folder is too big for a checkpoint"),
                format!(
                    "{} {} ({} {}). {} {}. {}",
                    t!("This folder holds"),
                    format::size(*bytes),
                    files,
                    t!("files"),
                    t!("The checkpoint size limit is"),
                    format::size(cap),
                    t!("The limit is on the Settings page, under Developer tools.")
                ),
                window,
                cx,
            );
            return;
        }
        Some(Ok((files, bytes))) => format!(
            "{} {} {} ({}) {}",
            t!("About"),
            files,
            t!("files"),
            format::size(*bytes),
            t!("will be copied to leaf's data folder. Copies that the drive can share with the originals take no extra room.")
        ),
    };
    let store = Arc::new(store);
    let keep_n = keep(cx);
    let go = Button::new(t!("Take checkpoint"), Kind::Primary, move |text, window, cx| {
        let note = text.unwrap_or_default().trim().to_string();
        let (store, dir, tab) = (store.clone(), dir.clone(), tab.clone());
        let work = cx.background_spawn(async move {
            let made = checkpoint::create(&store, &note, false, Some(cap));
            if let Ok(cp) = &made {
                checkpoint::prune(&store, keep_n, &[cp.id.as_str()]);
            }
            made
        });
        window
            .spawn(cx, async move |cx| {
                let made = work.await;
                let _ = cx.update(|window, cx| match made {
                    Ok(_) => show_list(dir, tab, cx),
                    Err(e) => crate::prompt::notice(t!("The checkpoint could not be taken"), error_text(&e), window, cx),
                });
            })
            .detach();
        Ok(())
    });
    crate::prompt::show(t!("Checkpoint"), Some(message), Some((String::new(), t!("Note (optional)").to_string())), vec![go, Button::cancel()], window, cx);
}
