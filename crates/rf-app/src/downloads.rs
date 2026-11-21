//! Downloads from the web, as Status Center jobs next to copies: progress and speed graph, pause, resume, cancel, a queue of
//! at most N running at once, the conflict dialog when the name is taken, "Retry" on a failure. The engine is `leaf_download`
//! (segments, resume after a restart, retries); this file is leaf's face for it: wording, settings, the dialog, the command line.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use leaf_download as dl;
use leaf_sys::fastcopy::{Conflict, Resolve};
use leaf_sys::fileops::Tracker;

use crate::folder_view::FolderView;
use crate::prompt::{self, Button, Kind};
use crate::settings::Settings;
use crate::t;

/// What a second launch of `leaf download <url> [folder]` carries to the running window: the prefix, the address, `\u{1}`, the folder.
pub const MESSAGE_PREFIX: &str = "\u{1}download:";

const SEP: char = char::from_u32(1).unwrap();

/// Where a download goes when nothing says: the setting, else the Downloads folder.
pub fn default_folder(s: &Settings) -> PathBuf {
    let custom = s.download_folder.trim();
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_default()
}

/// The web address in a piece of text: the first word that is one (a pasted line, an `.url` file's text), without the brackets or
/// quotes a mail or a chat puts round it.
pub fn url_in(text: &str) -> Option<String> {
    text.split_whitespace().map(|w| w.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']'))).map(|w| w.strip_prefix("URL=").unwrap_or(w)).find(|w| dl::is_http_url(w)).map(str::to_string)
}

fn clipboard_url(cx: &App) -> Option<String> {
    url_in(&cx.read_from_clipboard()?.text()?)
}

/// A link dropped on a pane arrives as a one-line file (the drop target's way of passing a web address on): its address.
pub fn dropped_link(paths: &[PathBuf]) -> Option<String> {
    let [p] = paths else { return None };
    if p.extension().is_none_or(|e| !e.eq_ignore_ascii_case(LINK_EXT)) {
        return None;
    }
    let text = std::fs::read_to_string(p).ok();
    let _ = std::fs::remove_file(p);
    text.and_then(|t| url_in(&t))
}

/// The extension of that one-line file.
pub const LINK_EXT: &str = "leaf-url";

/// A folder on this PC a download can be saved in (not an archive, a server or an FTP address).
pub fn can_save_in(dir: &Path) -> bool {
    !crate::remote::is_address_path(dir) && crate::archive_view::split(dir).is_none()
}

/// "Download here…": asks for the address (a web address on the clipboard is already in the box) and starts the download into `dir`;
/// the finished file is selected in the view.
pub fn ask(view: WeakEntity<FolderView>, dir: PathBuf, window: &mut Window, cx: &mut App) {
    let initial = clipboard_url(cx).unwrap_or_default();
    let shown = dir.clone();
    let go = Button::new(t!("Download"), Kind::Primary, move |text, _, cx| {
        let Some(url) = url_in(text.as_deref().unwrap_or_default()) else {
            return Err(t!("Enter a web address that starts with http:// or https://").into());
        };
        let after = view.update(cx, |v, cx| v.select_arrived(dir.clone(), cx)).ok();
        start(cx, url, dir.clone(), after);
        Ok(())
    });
    prompt::show(
        t!("Download here"),
        Some(format!("{} {}", t!("The file is saved in"), shown.display())),
        Some((initial, "https://example.com/file.zip".to_string())),
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

/// `leaf download <url> [folder]`, as the running window (or this process, once its window is up) takes it over.
pub fn message(url: &str, folder: Option<&str>) -> String {
    format!("{MESSAGE_PREFIX}{url}{SEP}{}", folder.unwrap_or_default())
}

/// What a second launch's request holds, as the running window takes it over.
pub fn from_message(rest: &str, cx: &mut App) -> bool {
    let (url, folder) = rest.split_once(SEP).unwrap_or((rest, ""));
    start_cli(url.to_string(), (!folder.is_empty()).then(|| folder.to_string()), cx);
    true
}

pub fn start_cli(url: String, folder: Option<String>, cx: &mut App) {
    let dest = folder.map(PathBuf::from).unwrap_or_else(|| default_folder(cx.global::<Settings>()));
    if let Err(e) = std::fs::create_dir_all(&dest) {
        log::warn!("download: the folder {} could not be made: {e}", dest.display());
    }
    // (the finished file is selected where a window shows its folder)
    let after: Box<dyn FnOnce(Vec<PathBuf>, &mut App)> = Box::new(|arrived, cx| {
        if let Some(p) = arrived.first() {
            crate::workspace::select_in_open_folder(p, cx);
        }
    });
    start(cx, url, dest, Some(after));
}

/// Starts a download as a Status Center job. `after` gets the saved file (on the UI thread) when it is complete.
pub fn start(cx: &mut App, url: String, dest: PathBuf, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    let url = url.trim().to_string();
    let s = cx.global::<Settings>();
    let settings = (s.download_segments.clamp(1, 32) as usize, u64::from(s.download_speed_limit) << 20, s.download_parallel.clamp(1, 16) as usize);
    let title = format!("{} {} → {}", t!("Downloading"), dl::file_name(None, &url), rf_core::history::display_name(&dest));
    let arrived: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let id_cell = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let (work_url, work_dest, got, fail_flag) = (url.clone(), dest.clone(), arrived.clone(), failed.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        Box::new(move |ok, cx| {
            let id = id_cell.get();
            let v = std::mem::take(&mut *arrived.lock().unwrap_or_else(|e| e.into_inner()));
            if ok {
                if let Some(p) = v.first() {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    crate::status_center::set_result(cx, id, Some(format!("{} {name}", t!("Downloaded"))), None);
                    crate::status_center::set_extras(cx, id, None, p.parent().map(Path::to_path_buf), None);
                }
                if let Some(after) = after {
                    after(v, cx);
                }
            } else if failed.load(Ordering::Relaxed) {
                // a failed download keeps its partial file: Retry carries on from it
                let retry: crate::status_center::ResultAction = (
                    t!("Retry").to_string(),
                    std::rc::Rc::new(move |cx: &mut App| {
                        crate::status_center::remove(cx, id);
                        start(cx, url.clone(), dest.clone(), None);
                    }),
                );
                crate::status_center::set_result(cx, id, None, Some(retry));
            }
        })
    };
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_url, &work_dest, settings, &got, &fail_flag), Some(on_done));
    id_cell.set(id);
}

fn work(tr: &Tracker, url: &str, dest: &Path, (segments, limit, parallel): (usize, u64, usize), arrived: &Mutex<Vec<PathBuf>>, failed: &AtomicBool) -> anyhow::Result<bool> {
    let Some(_turn) = dl::queue::wait_turn(parallel, &tr.cancel, |place| (tr.on_progress)(0, 0, Some(format!("{} ({place})", t!("Waiting for the downloads before it"))))) else {
        return Ok(false);
    };
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    let link = Link { tr, label: Mutex::new(String::new()), asked_cancel: AtomicBool::new(false) };
    let ctl = dl::Control { cancel: tr.cancel.clone(), paused: tr.paused.clone(), limit: Arc::new(AtomicU64::new(limit)) };
    let opts = dl::Options { segments, ..dl::Options::default() };
    match dl::download(url, dest, &opts, &ctl, &link) {
        Ok(dl::Outcome::Saved(p)) => {
            arrived.lock().unwrap_or_else(|e| e.into_inner()).push(p);
            Ok(true)
        }
        Ok(dl::Outcome::Skipped) => Ok(false),
        Err(dl::Error::Cancelled) => Ok(false),
        Err(e) => {
            if link.asked_cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            failed.store(true, Ordering::Relaxed);
            log::warn!("download {url}: {e}");
            Err(anyhow::anyhow!(fail_text(&e)))
        }
    }
}

/// leaf's words for a failure; the library's own text only on the second line.
fn fail_text(e: &dl::Error) -> String {
    let (what, detail) = match e {
        dl::Error::Http(c @ (404 | 410)) => (format!("{} (HTTP {c})", t!("The file is not on the server")), None),
        dl::Error::Http(c @ (401 | 403)) => (format!("{} (HTTP {c})", t!("The server does not allow this download")), None),
        dl::Error::Http(c) => (format!("{} (HTTP {c})", t!("The server refused the download")), None),
        dl::Error::Network(d) => (t!("The connection to the server broke and did not come back").to_string(), Some(d.clone())),
        dl::Error::Secure(d) => (t!("The server's secure connection could not be trusted").to_string(), Some(d.clone())),
        dl::Error::Changed => (t!("The file on the server changed while it was downloading").to_string(), None),
        dl::Error::NoRoom(_) => (t!("There is not enough free space on this PC for this download").to_string(), None),
        dl::Error::BadUrl(d) => (t!("This is not a web address leaf can download from").to_string(), Some(d.clone())),
        dl::Error::Io(d) => (t!("The file could not be written in this folder").to_string(), Some(d.to_string())),
        dl::Error::Cancelled => (t!("Cancelled").to_string(), None),
    };
    match detail {
        Some(d) if !d.trim().is_empty() => format!("{what}\n{d}"),
        _ => what,
    }
}

/// The job's side of the engine's questions and reports.
struct Link<'a> {
    tr: &'a Tracker,
    /// What the card's second line said last (it is sent again only when it changes).
    label: Mutex<String>,
    /// The conflict dialog went away without an answer: the job is cancelled.
    asked_cancel: AtomicBool,
}

impl dl::Host for Link<'_> {
    fn progress(&self, p: &dl::Progress) {
        self.tr.bytes.set(p.done, p.total.unwrap_or(0));
        let (total, done) = match p.total.filter(|t| *t > 0) {
            Some(t) => (1000, (p.done as f64 / t as f64 * 1000.0) as u32),
            None => (0, 0),
        };
        let label = if p.connections > 1 { format!("{} · {} {}", p.name, p.connections, t!("connections")) } else { p.name.to_string() };
        let mut last = self.label.lock().unwrap_or_else(|e| e.into_inner());
        let name = (*last != label).then(|| label.clone());
        *last = label;
        (self.tr.on_progress)(total, done, name);
    }

    fn exists(&self, path: &Path, incoming: Option<u64>) -> dl::Resolve {
        let meta = std::fs::metadata(path).ok();
        let conflict = Conflict {
            src: PathBuf::from(path.file_name().unwrap_or_default()),
            dst: path.to_path_buf(),
            src_dir: false,
            dst_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
            src_size: incoming.unwrap_or(0),
            dst_size: meta.as_ref().map_or(0, |m| m.len()),
            src_time: None,
            dst_time: meta.as_ref().and_then(|m| m.modified().ok()),
        };
        let mismatched = conflict.mismatched();
        match self.tr.ask.as_ref() {
            Some(ask) => match ask(std::slice::from_ref(&conflict)).and_then(|v| v.first().copied()) {
                // (a folder of that name can be kept next to, never replaced)
                Some(Resolve::Replace) if !mismatched && !conflict.dst_dir => dl::Resolve::Replace,
                Some(Resolve::Skip) => dl::Resolve::Skip,
                Some(_) => dl::Resolve::KeepBoth,
                None => {
                    self.asked_cancel.store(true, Ordering::Relaxed);
                    dl::Resolve::Skip
                }
            },
            None => dl::Resolve::KeepBoth,
        }
    }

    fn room(&self, need: u64, dir: &Path) -> bool {
        let free = leaf_sys::fileattr::free_space(dir);
        free == 0 || need <= free.saturating_sub(leaf_vfs::copy::KEEP_FREE)
    }
}
