//! Downloads from the web, as Status Center jobs next to copies: progress and speed graph, pause, resume, cancel, a queue of
//! at most N running at once, the conflict dialog when the name is taken, "Retry" on a failure. The engine is `leaf_download`
//! (segments, resume after a restart, retries); this file is leaf's face for it: wording, settings, the dialog, the command line.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::*;
use leaf_download as dl;
use leaf_download::store::{Past, Saved, Store};
use leaf_sys::fastcopy::{Conflict, Resolve};
use leaf_sys::fileops::Tracker;

use std::cell::Cell;
use std::rc::Rc;

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
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']')))
        .map(|w| w.strip_prefix("URL=").unwrap_or(w))
        .find(|w| dl::is_http_url(w) && !dl::has_password(w))
        .map(str::to_string)
}

/// "address" or "address sha256": the address and the checksum a box holds.
fn url_and_sha(text: &str) -> Option<(String, Option<String>)> {
    let url = url_in(text)?;
    let sha = text.split_whitespace().find_map(dl::parse_sha256);
    Some((url, sha))
}

fn clipboard_url(cx: &App) -> Option<String> {
    url_in(&cx.read_from_clipboard()?.text()?)
}

/// A link dropped on a pane arrives as a one-line file (the drop target's way of passing a web address on): its address.
pub fn dropped_link(paths: &[PathBuf]) -> Option<String> {
    let [p] = paths else { return None };
    // (a drag of a web address that arrives as the address itself: the toolkit's drop of a `text/uri-list` link on Linux)
    if let Some(url) = p.to_str().filter(|s| s.starts_with("http")).and_then(url_in) {
        return Some(url);
    }
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
    let verify = Rc::new(Cell::new(false));
    let verify_on = verify.clone();
    let go = Button::new(t!("Download"), Kind::Primary, move |text, _, cx| {
        let text = text.as_deref().unwrap_or_default();
        let Some((url, sha256)) = url_and_sha(text) else {
            let word = text.split_whitespace().find(|w| dl::is_http_url(w));
            return Err(if word.is_some_and(dl::has_password) {
                t!("An address with a password in it is not accepted: leaf would have to keep the password").into()
            } else {
                t!("Enter a web address that starts with http:// or https://").into()
            });
        };
        let after = view.update(cx, |v, cx| v.select_arrived(dir.clone(), cx)).ok();
        start_with(cx, Request { sha256, verify: verify_on.get(), ..Request::new(url, dir.clone()) }, after);
        Ok(())
    });
    let check = prompt::Check { label: t!("Verify with the checksum file next to it (address.sha256)").into(), note: "".into(), on: verify, enabled: true };
    prompt::show_input_checks(
        t!("Download here"),
        Some(format!("{} {}\n{}", t!("The file is saved in"), shown.display(), t!("To check the file, add its SHA-256 after the address."))),
        (initial, "https://example.com/file.zip".to_string()),
        vec![check],
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

/// What a download is asked for.
#[derive(Clone)]
pub struct Request {
    pub url: String,
    pub dest: PathBuf,
    /// The SHA-256 the file must have.
    pub sha256: Option<String>,
    /// Read that checksum from `url.sha256` before starting.
    pub verify: bool,
    /// Starts paused (a download that was paused when leaf was closed).
    pub paused: bool,
    /// The key of the saved list entry (a restored download keeps it).
    key: Option<u64>,
}

impl Request {
    pub fn new(url: String, dest: PathBuf) -> Self {
        Self { url: url.trim().to_string(), dest, sha256: None, verify: false, paused: false, key: None }
    }
}

/// A running (or waiting) download, for what changes while it runs: the speed limit and the saved list.
struct Live {
    key: u64,
    paused: Arc<AtomicBool>,
    limit: Arc<AtomicU64>,
    /// The user set this download's own limit from its card: the setting no longer changes it.
    own_limit: Arc<AtomicBool>,
}

static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);
/// The saved list: loaded on first use, written after every change.
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(store_path().map(|p| Store::load(&p)).unwrap_or_default()));

fn store_path() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("downloads.json"))
}

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes the saved list (the paused marks read from the downloads as they are now).
pub fn persist_soon() {
    let snapshot = {
        let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = store();
        for s in &mut st.active {
            if let Some(l) = live.iter().find(|l| l.key == s.key) {
                s.paused = l.paused.load(Ordering::Relaxed);
            }
        }
        st.clone()
    };
    if let Some(path) = store_path() {
        std::thread::spawn(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            snapshot.save(&path);
        });
    }
}

/// How many finished or failed downloads are remembered.
pub fn history_len() -> usize {
    store().history.len()
}

pub fn clear_history() {
    store().history.clear();
    persist_soon();
}

/// The speed limit or the per-server cap changed in the settings: running downloads take it at once (except the ones whose own
/// limit was set from their card).
pub fn settings_changed(cx: &App) {
    let s = cx.global::<Settings>();
    dl::set_host_cap(s.download_per_host as usize);
    let bytes = u64::from(s.download_speed_limit) << 20;
    for l in LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|l| !l.own_limit.load(Ordering::Relaxed)) {
        l.limit.store(bytes, Ordering::Relaxed);
    }
}

/// The downloads that were not finished when leaf was last closed, started again: each resumes from its part file, the paused ones
/// come back paused, the others wait their turn in the Status Center.
pub fn restore(cx: &mut App) {
    let saved = std::mem::take(&mut store().active);
    for s in saved {
        if dl::has_password(&s.url) || !dl::is_http_url(&s.url) {
            continue;
        }
        NEXT_KEY.fetch_max(s.key + 1, Ordering::Relaxed);
        let req = Request { url: s.url, dest: s.dest, sha256: s.sha256, verify: s.verify, paused: s.paused, key: Some(s.key) };
        start_with(cx, req, None);
    }
}

/// Starts a download as a Status Center job. `after` gets the saved file (on the UI thread) when it is complete.
pub fn start(cx: &mut App, url: String, dest: PathBuf, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    start_with(cx, Request::new(url, dest), after);
}

pub fn start_with(cx: &mut App, req: Request, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    if dl::has_password(&req.url) {
        log::warn!("download: an address with a password is not taken");
        return;
    }
    let s = cx.global::<Settings>();
    let settings = (s.download_segments.clamp(1, 32) as usize, u64::from(s.download_speed_limit) << 20, s.download_parallel.clamp(1, 16) as usize);
    dl::set_host_cap(s.download_per_host as usize);
    let key = req.key.unwrap_or_else(|| NEXT_KEY.fetch_add(1, Ordering::Relaxed));
    {
        let mut st = store();
        st.forget(key);
        st.active.push(Saved { key, url: req.url.clone(), dest: req.dest.clone(), sha256: req.sha256.clone(), verify: req.verify, paused: req.paused });
    }
    persist_soon();
    let title = format!("{} {} → {}", t!("Downloading"), dl::file_name(None, &req.url), rf_core::history::display_name(&req.dest));
    let arrived: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let id_cell = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let (work_req, got, fail_flag) = (req.clone(), arrived.clone(), failed.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        let req = req.clone();
        Box::new(move |ok, cx| {
            let id = id_cell.get();
            let v = std::mem::take(&mut *arrived.lock().unwrap_or_else(|e| e.into_inner()));
            let was_failed = failed.load(Ordering::Relaxed);
            // the saved list: it is no longer waiting; the last 50 that ended are kept
            LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|l| l.key != key);
            {
                let mut st = store();
                st.forget(key);
                if ok || was_failed {
                    let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    st.remember(Past { url: req.url.clone(), dest: req.dest.clone(), ok, when });
                }
            }
            persist_soon();
            if ok {
                if let Some(p) = v.first() {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    crate::status_center::set_result(cx, id, Some(format!("{} {name}", t!("Downloaded"))), None);
                    crate::status_center::set_extras(cx, id, None, p.parent().map(Path::to_path_buf), None);
                }
                if let Some(after) = after {
                    after(v, cx);
                }
            } else if was_failed {
                // a failed download keeps its partial file: Retry carries on from it
                let retry: crate::status_center::ResultAction = (
                    t!("Retry").to_string(),
                    std::rc::Rc::new(move |cx: &mut App| {
                        crate::status_center::remove(cx, id);
                        start_with(cx, Request { paused: false, key: None, ..req.clone() }, None);
                    }),
                );
                crate::status_center::set_result(cx, id, None, Some(retry));
            }
        })
    };
    let live = Live { key, paused: Arc::default(), limit: Arc::new(AtomicU64::new(settings.1)), own_limit: Arc::default() };
    let (limit, own_limit) = (live.limit.clone(), live.own_limit.clone());
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(live);
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_req, key, settings, (&limit, &got), &fail_flag), Some(on_done));
    id_cell.set(id);
    if id != 0 {
        let limit_now = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|l| l.key == key).map(|l| (l.limit.clone(), own_limit));
        if let Some((limit, own)) = limit_now {
            let ask: crate::status_center::ResultAction = (t!("Limit speed").to_string(), std::rc::Rc::new(move |cx: &mut App| ask_limit(limit.clone(), own.clone(), cx)));
            crate::status_center::set_live_action(cx, id, ask);
        }
    }
}

/// "Limit speed" on a running download's card: asks for the limit in MB/s (0 = none) and applies it at once.
fn ask_limit(limit: Arc<AtomicU64>, own: Arc<AtomicBool>, cx: &mut App) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
    let now = limit.load(Ordering::Relaxed);
    let initial = if now == 0 { String::new() } else { format!("{}", now as f64 / (1u64 << 20) as f64) };
    let _ = w.update(cx, move |_, window, cx| {
        let go = Button::new(t!("Limit"), Kind::Primary, move |text, _, _| {
            let text = text.unwrap_or_default().trim().replace(',', ".");
            let mb = if text.is_empty() { 0.0 } else { text.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(|| t!("Enter a number of MB per second, or 0 for no limit"))? };
            limit.store((mb * (1u64 << 20) as f64) as u64, Ordering::Relaxed);
            own.store(true, Ordering::Relaxed);
            Ok(())
        });
        prompt::show(
            t!("Limit speed"),
            Some(t!("MB per second for this download (0 or empty: no limit). It applies at once.").to_string()),
            Some((initial, "MB/s".to_string())),
            vec![go, Button::cancel()],
            window,
            cx,
        );
    });
}

fn work(tr: &Tracker, req: &Request, key: u64, (segments, _, parallel): (usize, u64, usize), (limit, arrived): (&Arc<AtomicU64>, &Arc<Mutex<Vec<PathBuf>>>), failed: &AtomicBool) -> anyhow::Result<bool> {
    // (the saved list reads this job's paused flag, the one the card's pause button flips)
    if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|l| l.key == key) {
        l.paused = tr.paused.clone();
    }
    if req.paused {
        // came back paused: it holds no place in the line until it is resumed
        tr.paused.store(true, Ordering::Relaxed);
        (tr.on_progress)(0, 0, Some(t!("Paused").to_string()));
        while tr.paused.load(Ordering::Relaxed) && !tr.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        if tr.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
    }
    let Some(_turn) = dl::queue::wait_turn(parallel, &tr.cancel, |place| (tr.on_progress)(0, 0, Some(format!("{} ({place})", t!("Waiting for the downloads before it"))))) else {
        return Ok(false);
    };
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    let link = Link { tr, label: Mutex::new(String::new()), asked_cancel: AtomicBool::new(false) };
    let ctl = dl::Control { cancel: tr.cancel.clone(), paused: tr.paused.clone(), limit: limit.clone() };
    let mut opts = dl::Options { segments, sha256: req.sha256.clone(), ..dl::Options::default() };
    if opts.sha256.is_none() && req.verify {
        match dl::fetch_sidecar(&req.url, &opts) {
            Some(sha) => opts.sha256 = Some(sha),
            None => {
                failed.store(true, Ordering::Relaxed);
                return Err(anyhow::anyhow!("{}\n{}", t!("There is no checksum file next to this download to check it with"), dl::sidecar_url(&req.url)));
            }
        }
    }
    match dl::download(&req.url, &req.dest, &opts, &ctl, &link) {
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
            log::warn!("download {}: {e}", req.url);
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
        dl::Error::Busy(c) => (format!("{} (HTTP {c})", t!("The server is busy and kept refusing, even with fewer connections")), None),
        dl::Error::NoRanges => (t!("The server stopped sending parts of the file").to_string(), None),
        dl::Error::Checksum { expected, got } => (
            t!("The downloaded file does not match its checksum. It is kept as a partial file; it may be damaged.").to_string(),
            Some(format!("SHA-256 {got}
{} {expected}", t!("expected"))),
        ),
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
