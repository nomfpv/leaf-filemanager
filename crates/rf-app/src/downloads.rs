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
    // (the VPN choice for this one download, offered once a Mullvad account is signed in; it starts as the setting says)
    let s = cx.global::<Settings>();
    let offer_vpn = s.vpn_signed_in;
    let vpn = Rc::new(Cell::new(s.vpn_downloads));
    let vpn_on = vpn.clone();
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
        let through = offer_vpn.then(|| vpn_on.get());
        crate::videodl::route(cx, Request { sha256, verify: verify_on.get(), vpn: through, ..Request::new(url, dir.clone()) }, after, false);
        Ok(())
    });
    let mut checks = vec![prompt::Check { label: t!("Verify with the checksum file next to it (address.sha256)").into(), note: "".into(), on: verify, enabled: true }];
    if offer_vpn {
        checks.push(prompt::Check { label: t!("Through leaf's VPN").into(), note: "".into(), on: vpn, enabled: true });
    }
    prompt::show_input_checks(
        t!("Download here"),
        Some(format!("{} {}\n{}", t!("The file is saved in"), shown.display(), t!("To check the file, add its SHA-256 after the address."))),
        (initial, "https://example.com/file.zip".to_string()),
        checks,
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

/// The addresses a "Download several" box stands for: its words (links pasted one per line arrive separated by spaces) with the
/// patterns expanded. The second number is how many words were left out (not a web address, a password in it, a pattern that
/// makes too many).
pub fn expand_many(text: &str) -> (Vec<String>, usize) {
    let (mut urls, mut seen, mut skipped) = (Vec::new(), std::collections::HashSet::new(), 0);
    for word in text.split_whitespace() {
        // (the whole paste is capped, not each word: a long list of patterns stays a list a person can look at)
        if urls.len() >= dl::pattern::MAX {
            skipped += 1;
            continue;
        }
        let word = word.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | ','));
        let Some(list) = dl::pattern::expand(word) else {
            skipped += 1;
            continue;
        };
        for u in list {
            if !dl::is_http_url(&u) || dl::has_password(&u) || urls.len() >= dl::pattern::MAX {
                skipped += 1;
            } else if seen.insert(u.clone()) {
                urls.push(u);
            }
        }
    }
    (urls, skipped)
}

/// "Download several…": the addresses (links of the clipboard are already in the box) or a pattern, then a list with the count
/// before anything is added; all go into the queue, into `dir`.
pub fn ask_many(view: WeakEntity<FolderView>, dir: PathBuf, window: &mut Window, cx: &mut App) {
    let initial = cx.read_from_clipboard().and_then(|c| c.text()).map(|t| t.split_whitespace().filter(|w| dl::is_http_url(w)).collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let shown = dir.clone();
    let preview = Button::new(t!("Preview"), Kind::Primary, move |text, window, cx| {
        let (urls, skipped) = expand_many(text.as_deref().unwrap_or_default());
        if urls.is_empty() {
            return Err(t!("Enter web addresses that start with http:// or https://, or a pattern such as https://example.com/file[001-100].zip").into());
        }
        confirm_many(view.clone(), dir.clone(), urls, skipped, window, cx);
        Ok(())
    });
    prompt::show(
        t!("Download several"),
        Some(format!("{} {}\n{}", t!("The files are saved in"), shown.display(), t!("Paste links (one per line) or a pattern: file[001-100].zip, photo[a-f].jpg."))),
        Some((initial, "https://example.com/file[001-100].zip".to_string())),
        vec![preview, Button::cancel()],
        window,
        cx,
    );
}

/// The list before they are added: how many, the first few and the last.
fn confirm_many(view: WeakEntity<FolderView>, dir: PathBuf, urls: Vec<String>, skipped: usize, window: &mut Window, cx: &mut App) {
    let n = urls.len();
    let mut lines: Vec<String> = if n <= 9 { urls.clone() } else { urls[..6].iter().cloned().chain(std::iter::once("…".to_string())).chain(urls[n - 2..].iter().cloned()).collect() };
    let mut head = format!("{n} {}", if n == 1 { t!("download") } else { t!("downloads") });
    if skipped > 0 {
        head.push_str(&format!(" ({skipped} {})", t!("left out: not a web address")));
    }
    lines.insert(0, head);
    let add = Button::new(t!("Add to the queue"), Kind::Primary, move |_, _, cx| {
        for u in &urls {
            crate::videodl::route(cx, Request::new(u.clone(), dir.clone()), None, true);
        }
        let _ = view.update(cx, |_, cx| cx.notify());
        Ok(())
    });
    prompt::show(t!("Download several"), Some(lines.join("\n")), None, vec![add, Button::cancel()], window, cx);
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
    let sort = folder.is_none();
    // `leaf download x.nzb [folder]`: a Usenet job
    if crate::usenet::is_nzb(Path::new(&url)) {
        let dest = folder.map(PathBuf::from).unwrap_or_else(|| default_folder(cx.global::<Settings>()));
        crate::usenet::start(PathBuf::from(url), dest, sort, cx);
        return;
    }
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
    crate::videodl::route(cx, Request { sort, ..Request::new(url, dest) }, Some(after), false);
}

/// The per-server caps of the settings, given to the engine.
fn apply_host_caps(s: &Settings) {
    dl::set_host_cap(s.download_per_host as usize);
    dl::set_host_caps(s.download_servers.iter().map(|r| (r.host.clone(), r.connections as usize)).collect());
}

/// Bytes per second for a download from `url`: the lower of the general limit and its server's (0 = none).
fn limit_for(s: &Settings, url: &str) -> u64 {
    [u64::from(s.download_speed_limit), server_rule(s, url).map_or(0, |r| u64::from(r.limit_mb))].into_iter().filter(|m| *m > 0).min().unwrap_or(0) << 20
}

/// The settings row of the server `url` is on (the most specific name wins).
fn server_rule<'a>(s: &'a Settings, url: &str) -> Option<&'a crate::settings::ServerRule> {
    let name = dl::host_name(url);
    s.download_servers.iter().filter(|r| dl::host_matches(&r.host, &name)).max_by_key(|r| r.host.len())
}

/// The proxy of the settings for the engine, without the password, and the keyring entry that has the password (a manual proxy with
/// a user name).
fn proxy_of(s: &Settings) -> (dl::ProxyMode, Option<leaf_remote::SecretId>) {
    use crate::settings::{DownloadProxy, ProxyType};
    match s.download_proxy {
        DownloadProxy::None => (dl::ProxyMode::Direct, None),
        DownloadProxy::System => (dl::ProxyMode::System, None),
        DownloadProxy::Manual => {
            let user = s.download_proxy_user.trim().to_string();
            let host = s.download_proxy_host.trim().to_string();
            let id = (!user.is_empty()).then(|| proxy_secret_id(&user, &host, s.download_proxy_port));
            let kind = if s.download_proxy_type == ProxyType::Socks5 { dl::ProxyKind::Socks5 } else { dl::ProxyKind::Http };
            (dl::ProxyMode::Manual(dl::ProxyServer { kind, host, port: s.download_proxy_port, user: (!user.is_empty()).then_some(user), password: None }), id)
        }
    }
}

/// Where a manual proxy's password is kept.
pub fn proxy_secret_id(user: &str, host: &str, port: u16) -> leaf_remote::SecretId {
    leaf_remote::SecretId::password("proxy", user, host, port)
}

/// The categories a request is sorted by: the settings' list when sorting is on and the request is for the download folder.
fn categories_for(req: &Request, s: &Settings) -> Vec<(String, Vec<String>)> {
    if req.sort && s.download_sort { s.download_categories.iter().map(|c| (c.folder.clone(), c.exts.clone())).collect() } else { Vec::new() }
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
    /// The browser's request headers for this download (see `capture`): memory only, never in the saved list.
    pub headers: dl::Headers,
    /// It came from the browser with the browser's sign-in (Cookie / Authorization): after a restart those are gone and it has to
    /// be started from the browser again.
    pub browser: bool,
    /// The folder is the download folder: the settings' categories sort the file into a subfolder ("Download here…" and a folder
    /// given on the command line never do).
    pub sort: bool,
    /// The subfolder an earlier run chose (kept in the saved list): reused when the download is taken up again.
    subfolder: Option<String>,
    /// Through leaf's VPN: this download's own choice; none: the "Downloads through the VPN" setting.
    pub vpn: Option<bool>,
}

impl Request {
    pub fn new(url: String, dest: PathBuf) -> Self {
        Self { url: url.trim().to_string(), dest, sha256: None, verify: false, paused: false, key: None, headers: dl::Headers::default(), browser: false, sort: false, subfolder: None, vpn: None }
    }
}

/// The server's name in an address (for the VPN's LAN rule and the kill switch).
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    if let Some(v6) = hostport.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("").to_string();
    }
    hostport.rsplit_once(':').map_or(hostport, |(h, _)| h).to_string()
}

/// A running (or waiting) download, for what changes while it runs: the speed limit and the saved list.
struct Live {
    key: u64,
    paused: Arc<AtomicBool>,
    limit: Arc<AtomicU64>,
    /// The user set this download's own limit from its card: the setting no longer changes it.
    own_limit: Arc<AtomicBool>,
    /// The schedule paused it (it is not a pause of the user's: it is not saved as one, and the window opening resumes it).
    by_schedule: bool,
    url: String,
    /// It runs on one connection the server cannot resume: the schedule lets it finish instead of pausing it.
    no_resume: Arc<AtomicBool>,
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
                s.paused = l.paused.load(Ordering::Relaxed) && !l.by_schedule;
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

/// The downloads that run, wait or are paused.
pub fn live_count() -> usize {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// The queue's schedule opens (`true`) or closes: closed, waiting downloads keep waiting and the running ones are paused; open again,
/// the ones the schedule paused go on.
pub fn schedule_gate(open: bool) {
    dl::queue::set_open(open);
    for l in LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        if open {
            if l.by_schedule {
                l.paused.store(false, Ordering::Relaxed);
                l.by_schedule = false;
            }
        } else if !l.paused.load(Ordering::Relaxed) && !l.no_resume.load(Ordering::Relaxed) {
            l.paused.store(true, Ordering::Relaxed);
            l.by_schedule = true;
        }
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
    apply_host_caps(s);
    for l in LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|l| !l.own_limit.load(Ordering::Relaxed)) {
        l.limit.store(limit_for(s, &l.url), Ordering::Relaxed);
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
        let req = Request { key: Some(s.key), sha256: s.sha256, verify: s.verify, paused: s.paused, browser: s.browser, sort: s.sort, subfolder: s.subfolder, vpn: s.vpn, ..Request::new(s.url, s.dest) };
        start_with(cx, req, None);
    }
}

/// Starts a download as a Status Center job. `after` gets the saved file (on the UI thread) when it is complete.
pub fn start(cx: &mut App, url: String, dest: PathBuf, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    crate::videodl::route(cx, Request::new(url, dest), after, false);
}

pub fn start_with(cx: &mut App, req: Request, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    if dl::has_password(&req.url) {
        log::warn!("download: an address with a password is not taken");
        return;
    }
    let s = cx.global::<Settings>();
    let rule = server_rule(s, &req.url);
    let limit = limit_for(s, &req.url);
    let settings = (s.download_segments.clamp(1, 32) as usize, limit, s.download_parallel.clamp(1, 16) as usize, categories_for(&req, s), rule.map(|r| r.user_agent.trim().to_string()).filter(|u| !u.is_empty()), proxy_of(s));
    apply_host_caps(s);
    let through_vpn = req.vpn.unwrap_or(s.vpn_downloads);
    let key = req.key.unwrap_or_else(|| NEXT_KEY.fetch_add(1, Ordering::Relaxed));
    crate::download_schedule::note_started();
    {
        let mut st = store();
        st.forget(key);
        // (the headers are not saved: `browser` says the download needs the browser again after a restart)
        st.active.push(Saved { key, url: req.url.clone(), dest: req.dest.clone(), sha256: req.sha256.clone(), verify: req.verify, paused: req.paused, browser: req.browser, sort: req.sort, subfolder: req.subfolder.clone(), vpn: req.vpn });
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
                crate::download_schedule::note_finished();
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
    let live = Live { key, paused: Arc::default(), limit: Arc::new(AtomicU64::new(settings.1)), own_limit: Arc::default(), by_schedule: false, url: req.url.clone(), no_resume: Arc::default() };
    let (limit, own_limit) = (live.limit.clone(), live.own_limit.clone());
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(live);
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_req, key, settings, through_vpn, (&limit, &got), &fail_flag), Some(on_done));
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

/// Connections of one download through leaf's VPN at most.
const VPN_SEGMENTS: usize = 8;

/// The kill switch holds this download's connections: it waits (as paused, with the reason on its card) until the VPN is
/// connected or the kill switch is turned off. `false`: it was cancelled meanwhile.
fn wait_for_vpn(tr: &Tracker, host: &str) -> bool {
    if !crate::vpn::held(host) {
        return true;
    }
    log::info!("download: held by the VPN's kill switch until the VPN is connected");
    (tr.on_progress)(0, 0, Some(crate::vpn::held_text()));
    while crate::vpn::held(host) || tr.paused.load(Ordering::Relaxed) {
        if tr.cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    true
}

#[allow(clippy::too_many_arguments)]
fn work(tr: &Tracker, req: &Request, key: u64, (segments, _, parallel, categories, agent, (proxy, proxy_secret)): (usize, u64, usize, Vec<(String, Vec<String>)>, Option<String>, (dl::ProxyMode, Option<leaf_remote::SecretId>)), through_vpn: bool, (limit, arrived): (&Arc<AtomicU64>, &Arc<Mutex<Vec<PathBuf>>>), failed: &AtomicBool) -> anyhow::Result<bool> {
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
    let Some(_turn) = dl::queue::wait_turn(parallel, &tr.cancel, |place| {
        let text = if dl::queue::is_open() { format!("{} ({place})", t!("Waiting for the downloads before it")) } else { t!("Waiting for the scheduled time").to_string() };
        (tr.on_progress)(0, 0, Some(text))
    }) else {
        return Ok(false);
    };
    // a browser download whose sign-in was not kept over a restart: the server would refuse it, or send a login page
    if req.browser && !req.headers.has_credentials() {
        failed.store(true, Ordering::Relaxed);
        return Err(anyhow::anyhow!("{}", t!("This download came from the browser with your sign-in, which leaf does not keep after a restart. Start it again from the browser: leaf carries on from the part already downloaded.")));
    }
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    let link = Link { tr, label: Mutex::new(String::new()), asked_cancel: AtomicBool::new(false), key, no_resume: LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|l| l.key == key).map(|l| l.no_resume.clone()).unwrap_or_default(), keep: Mutex::new(None) };
    let ctl = dl::Control { cancel: tr.cancel.clone(), paused: tr.paused.clone(), limit: limit.clone() };
    let mut opts = dl::Options { segments, max_segments: 32, sha256: req.sha256.clone(), headers: req.headers.clone(), categories: categories.clone(), subfolder: req.subfolder.clone(), ..dl::Options::default() };
    if let Some(ua) = agent {
        opts.user_agent = ua;
    }
    opts.proxy = proxy;
    // the proxy's password comes from the keyring now, here on the download's own thread (a keyring can wait for the user)
    if let (dl::ProxyMode::Manual(m), Some(id)) = (&mut opts.proxy, &proxy_secret) {
        m.password = leaf_remote::vault().get(id).map(|p| p.as_str().to_string());
    }
    let host = host_of(&req.url);
    if through_vpn {
        opts.dialer = Some(Arc::new(crate::vpn::DownloadDialer));
        // (all of them share one UDP socket and one thread inside the tunnel: more than this only adds losses)
        opts.segments = opts.segments.min(VPN_SEGMENTS);
        opts.max_segments = opts.max_segments.min(VPN_SEGMENTS);
        if !wait_for_vpn(tr, &host) {
            return Ok(false);
        }
    }
    if opts.sha256.is_none() && req.verify {
        match dl::fetch_sidecar(&req.url, &opts) {
            Some(sha) => opts.sha256 = Some(sha),
            None => {
                failed.store(true, Ordering::Relaxed);
                return Err(anyhow::anyhow!("{}\n{}", t!("There is no checksum file next to this download to check it with"), dl::sidecar_url(&req.url)));
            }
        }
    }
    // (the VPN dropped while it ran, with the kill switch on: it waits, then carries on from the part already downloaded)
    let mut result = loop_sign_in(tr, req, &mut opts, &ctl, &link);
    while through_vpn && matches!(result, Err(dl::Error::Refused(_))) {
        if !wait_for_vpn(tr, &host) {
            return Ok(false);
        }
        result = loop_sign_in(tr, req, &mut opts, &ctl, &link);
    }
    match result {
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
        dl::Error::Auth(_) => (t!("The server did not accept the sign-in").to_string(), None),
        dl::Error::Checksum { expected, got } => (
            t!("The downloaded file does not match its checksum. It is kept as a partial file; it may be damaged.").to_string(),
            Some(format!("SHA-256 {got}
{} {expected}", t!("expected"))),
        ),
        dl::Error::Cancelled => (t!("Cancelled").to_string(), None),
        dl::Error::Refused(_) => (crate::vpn::held_text(), None),
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
    /// Set when the download runs on one connection that cannot be resumed (the schedule does not pause it).
    no_resume: Arc<AtomicBool>,
    key: u64,
    /// A sign-in the user asked to remember: kept in the keyring once the server has taken it.
    keep: Mutex<Option<(leaf_remote::SecretId, leaf_remote::Secret)>>,
}

impl dl::Host for Link<'_> {
    fn progress(&self, p: &dl::Progress) {
        self.tr.bytes.set(p.done, p.total.unwrap_or(0));
        let (total, done) = match p.total.filter(|t| *t > 0) {
            Some(t) => (1000, (p.done as f64 / t as f64 * 1000.0) as u32),
            None => (0, 0),
        };
        let label = if p.connections > 1 { format!("{} · {} {}", p.name, p.connections, t!("connections")) } else { p.name.to_string() };
        let label = if self.no_resume.load(Ordering::Relaxed) && !dl::queue::is_open() { format!("{label} · {}", t!("finishes before the pause: the server cannot resume")) } else { label };
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

    fn subfolder(&self, name: &str) {
        let changed = {
            let mut st = store();
            st.active.iter_mut().find(|a| a.key == self.key).is_some_and(|a| {
                let new = Some(name.to_string());
                let differs = a.subfolder != new;
                a.subfolder = new;
                differs
            })
        };
        if changed {
            persist_soon();
        }
    }

    fn no_resume(&self) {
        self.no_resume.store(true, Ordering::Relaxed);
    }

    fn signed_in(&self) {
        if let Some((id, secret)) = self.keep.lock().unwrap_or_else(|e| e.into_inner()).take() {
            leaf_remote::vault().keep_if_asked(&id, &secret);
        }
    }

    fn room(&self, need: u64, dir: &Path) -> bool {
        let free = leaf_sys::fileattr::free_space(dir);
        free == 0 || need <= free.saturating_sub(leaf_vfs::copy::KEEP_FREE)
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// HTTP sign-in (a 401 with a Basic or Digest challenge)

/// Runs the download; when the server asks for a sign-in, asks the user (or takes the one saved in the keyring for this server and
/// realm) and tries again, up to four times. A sign-in is never taken from the address (`user:password@` is refused), Basic over plain
/// http is sent only after the user confirmed it in the dialog, and a saved one is not used for it.
fn loop_sign_in(tr: &Tracker, req: &Request, opts: &mut dl::Options, ctl: &dl::Control, link: &Link) -> Result<dl::Outcome, dl::Error> {
    let mut used_saved: Option<(leaf_remote::SecretId, leaf_remote::Secret)> = None;
    for round in 0..4 {
        let ch = match dl::download(&req.url, &req.dest, opts, ctl, link) {
            Err(dl::Error::Auth(ch)) => ch,
            other => return other,
        };
        let (scheme, host, port) = ch.origin.clone();
        let id = leaf_remote::SecretId::web(&scheme, &host, port, &ch.realm);
        let plain_basic = scheme == "http" && ch.scheme == dl::AuthScheme::Basic;
        // the saved one was just refused: forgotten, so the dialog is next (a 401 for another realm or server says nothing about it)
        if let Some((saved_id, secret)) = used_saved.take().filter(|(sid, _)| *sid == id) {
            leaf_remote::vault().refused(&saved_id, &secret);
        }
        let saved = (round == 0 && !plain_basic).then(|| leaf_remote::vault().get(&id)).flatten().filter(|s| s.as_str().contains(':'));
        let (user, password, remember) = match saved {
            Some(s) => {
                let (u, p) = s.as_str().split_once(':').unwrap_or_default();
                used_saved = Some((id.clone(), s.clone()));
                (u.to_string(), p.to_string(), false)
            }
            None => {
                (tr.on_progress)(0, 0, Some(t!("Waiting for your sign-in").to_string()));
                let Some(answer) = ask_sign_in(tr, &host, &ch.realm, plain_basic, round > 0) else { return Err(dl::Error::Cancelled) };
                answer
            }
        };
        *link.keep.lock().unwrap_or_else(|e| e.into_inner()) = (remember && !plain_basic).then(|| (id, leaf_remote::Secret::new(format!("{user}:{password}")).remembered(true)));
        opts.headers.set_web_auth(dl::WebAuth::new(&ch, &user, &password));
        (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    }
    Err(dl::Error::Http(401))
}

/// A question for the user, shown in the main window (one dialog at a time).
struct SignIn {
    host: String,
    realm: String,
    plain_basic: bool,
    retry: bool,
    keyring: leaf_remote::Keyring,
    reply: std::sync::mpsc::Sender<Option<(String, String, bool)>>,
}

static SIGN_INS: LazyLock<(futures::channel::mpsc::UnboundedSender<SignIn>, Mutex<Option<futures::channel::mpsc::UnboundedReceiver<SignIn>>>)> = LazyLock::new(|| {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (tx, Mutex::new(Some(rx)))
});

/// Asks on the download's own thread and waits for the dialog: `(user, password, remember)`; `None` when it was cancelled.
fn ask_sign_in(tr: &Tracker, host: &str, realm: &str, plain_basic: bool, retry: bool) -> Option<(String, String, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let keyring = leaf_remote::vault().state();
    SIGN_INS.0.unbounded_send(SignIn { host: host.to_string(), realm: realm.to_string(), plain_basic, retry, keyring, reply: tx }).ok()?;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(answer) => return answer,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !tr.cancel.load(Ordering::Relaxed) => {}
            Err(_) => return None,
        }
    }
}

/// Shows the sign-in questions as dialogs in the main window, one at a time (started once with the window).
pub fn init_ui(cx: &mut App) {
    use futures::StreamExt as _;
    let Some(mut rx) = SIGN_INS.1.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    cx.spawn(async move |cx| {
        while let Some(q) = rx.next().await {
            let mut pending = Some(q);
            loop {
                let _ = cx.update(|cx| try_show_sign_in(&mut pending, cx));
                if pending.is_none() {
                    break;
                }
                // another dialog is open there: this one waits its turn
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            }
        }
    })
    .detach();
}

fn try_show_sign_in(pending: &mut Option<SignIn>, cx: &mut App) {
    let mains = crate::workspace::main_windows(cx);
    // no window to ask in: no (the question is dropped, which the download reads as "cancelled")
    let Some(w) = cx.active_window().filter(|a| mains.contains(a)).or_else(|| mains.first().copied()) else {
        pending.take();
        return;
    };
    if w.update(cx, |_, window, cx| prompt::current(window, cx).is_some()).unwrap_or(false) {
        return;
    }
    let mut q = pending.take();
    let _ = w.update(cx, |_, window, cx| {
        if let Some(q) = q.take() {
            window.activate_window();
            show_sign_in(q, window, cx);
        }
    });
}

/// Two dialogs, since a dialog holds one box: the user name, then the password (with "Remember" where the keyring is there).
fn show_sign_in(q: SignIn, window: &mut Window, cx: &mut App) {
    let SignIn { host, realm, plain_basic, retry, keyring, reply } = q;
    let reply = Rc::new(Cell::new(Some(reply)));
    let first = if retry { t!("The sign-in was not accepted. Try again.") } else { t!("The server asks you to sign in to download this file.") };
    let realm_line = if realm.is_empty() { String::new() } else { format!("\n{} {realm}", t!("Area:")) };
    let warn = if plain_basic { format!("\n\n{}", t!("Not encrypted: this address starts with http://, so the password is sent as it is. Continue only if you trust the network.")) } else { String::new() };
    let message = format!("{first}\n{host}{realm_line}{warn}");
    let title = format!("{} {host}", t!("Sign in to"));
    let next = {
        let (reply, title, host) = (reply.clone(), title.clone(), host.clone());
        Button::new(t!("Next"), Kind::Primary, move |user, _, cx| {
            let user = user.unwrap_or_default().trim().to_string();
            if user.is_empty() || user.contains(':') {
                return Err(t!("Enter the user name (without a colon)").to_string());
            }
            let (reply, title, host, keyring) = (reply.clone(), title.clone(), host.clone(), keyring.clone());
            // (a dialog started from a button is closed with it: this one comes after)
            cx.defer({
                let w = cx.active_window();
                move |cx| {
                    let Some(w) = w else { return };
                    let _ = w.update(cx, move |_, window, cx| show_password(title, host, user, plain_basic, keyring, reply, window, cx));
                }
            });
            Ok(())
        })
    };
    let cancel = {
        let reply = reply.clone();
        Button::new(t!("Cancel"), Kind::Normal, move |_, _, _| {
            if let Some(r) = reply.take() {
                let _ = r.send(None);
            }
            Ok(())
        })
    };
    prompt::show(title, Some(message), Some((String::new(), t!("User name").to_string())), vec![next, cancel], window, cx);
}

#[allow(clippy::too_many_arguments)]
fn show_password(title: String, host: String, user: String, plain_basic: bool, keyring: leaf_remote::Keyring, reply: Rc<Cell<Option<std::sync::mpsc::Sender<Option<(String, String, bool)>>>>>, window: &mut Window, cx: &mut App) {
    let remember = Rc::new(Cell::new(false));
    let (enabled, line) = match (&keyring, plain_basic) {
        (_, true) => (false, t!("A password sent without encryption is never kept.").to_string()),
        (leaf_remote::Keyring::Ready, _) => (true, t!("Tick Remember to keep it in the system's keyring for this server; leaf deletes it there when the server refuses it.").to_string()),
        (leaf_remote::Keyring::Missing(why), _) => (false, format!("{}\n{}", t!("Used for this download only."), crate::i18n::td(why))),
    };
    let checks = vec![prompt::Check { label: t!("Remember").into(), note: "".into(), on: remember.clone(), enabled }];
    let go = {
        let (reply, user) = (reply.clone(), user.clone());
        Button::new(if plain_basic { t!("Sign in anyway") } else { t!("Sign in") }, Kind::Primary, move |password, _, _| {
            if let Some(r) = reply.take() {
                let _ = r.send(Some((user.clone(), password.unwrap_or_default(), remember.get())));
            }
            Ok(())
        })
    };
    let cancel = Button::new(t!("Cancel"), Kind::Normal, move |_, _, _| {
        if let Some(r) = reply.take() {
            let _ = r.send(None);
        }
        Ok(())
    });
    prompt::show_secret_with(title, Some(format!("{host}\n{user}\n\n{line}")), t!("Password").to_string(), checks, vec![go, cancel], window, cx);
}
