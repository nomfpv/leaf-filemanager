//! Downloads the browser starts, taken over by leaf: the leaf extension (`extension/` in the repository) sends each new download to
//! `leaf-nmhost`, which hands it here over the hand-over channel as [`PREFIX`] + the extension's JSON. leaf checks it and starts it
//! as an ordinary download (Status Center job, the download folder, the conflict dialog). The answer `ok` tells the extension to
//! cancel the browser's own copy; anything else leaves the download to the browser.
//!
//! The browser's Cookie and Authorization come along so a download behind a sign-in works. They are credentials: kept in memory
//! for that one job (`leaf_download::Headers`, which shows names only), never logged, never in `downloads.json` or the part state.
//!
//! Also here: registering the host with the browsers ("Connect browsers", `leaf nmhost --register` / `--unregister`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::App;
use leaf_download as dl;
use serde::Deserialize;

use crate::settings::Settings;
use crate::t;

pub const PREFIX: &str = "\u{1}capture:";
/// The host's name in the browsers' manifests.
pub const HOST_NAME: &str = "dev.leaf.nmhost";
/// The Chromium extension id (fixed by the `key` in `extension/manifest.chromium.json`); a store listing's id is added here.
pub const CHROMIUM_IDS: &[&str] = &["opbdlmjnfgldjmlacejhcflbdpimolpi"];
/// The Firefox add-on id (`browser_specific_settings.gecko.id` in `extension/manifest.firefox.json`).
pub const FIREFOX_ID: &str = "browser-capture@leaf-filemanager";
const HOST_STEM: &str = "leaf-nmhost";

/// The request headers leaf takes from the browser; everything else is dropped.
const HEADERS: &[&str] = &["Cookie", "Authorization", "Referer", "User-Agent", "Accept-Language"];
const MAX_URL: usize = 8 << 10;
const MAX_HEADER: usize = 16 << 10;

/// When the extension was last heard from (seconds since 1970; 0 = not since leaf started).
static LAST_HEARD: AtomicU64 = AtomicU64::new(0);
/// What "Connect browsers" did last, for the settings card.
static STATUS: Mutex<Option<String>> = Mutex::new(None);

/// The extension's message. No `Debug`: the headers are credentials.
#[derive(Deserialize)]
struct Msg {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    method: Option<String>,
    /// Bytes; -1 or absent when the browser does not know.
    #[serde(default)]
    size: Option<i64>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    /// "Download video with leaf": the browser whose sign-in yt-dlp may read itself (the extension's opt-in); leaf never gets cookies.
    #[serde(default, rename = "cookiesFrom")]
    cookies_from: Option<String>,
}

/// Why a capture is left to the browser (only for the log; the extension just hears "no").
#[derive(Debug, PartialEq)]
enum Refused {
    Off,
    BadMessage,
    NotHttp,
    Password,
    NotGet,
    Small,
}

/// A checked download: the address and the headers that passed the allowlist.
struct Capture {
    url: String,
    headers: dl::Headers,
}

fn check(msg: Msg, min_bytes: u64) -> Result<Capture, Refused> {
    let url = msg.url.trim().to_string();
    if url.is_empty() || url.len() > MAX_URL {
        return Err(Refused::BadMessage);
    }
    if !dl::is_http_url(&url) {
        return Err(Refused::NotHttp);
    }
    if dl::has_password(&url) {
        return Err(Refused::Password);
    }
    if msg.method.as_deref().is_some_and(|m| !m.eq_ignore_ascii_case("GET")) {
        return Err(Refused::NotGet);
    }
    if msg.size.is_some_and(|s| s >= 0 && (s as u64) < min_bytes) {
        return Err(Refused::Small);
    }
    let mut headers = dl::Headers::default();
    for (name, value) in &msg.headers {
        if let Some(keep) = HEADERS.iter().find(|h| h.eq_ignore_ascii_case(name.trim()))
            && value.len() <= MAX_HEADER
        {
            headers.set(keep, value);
        }
    }
    if headers.get("Referer").is_none()
        && let Some(r) = msg.referrer.as_deref().map(str::trim).filter(|r| dl::is_http_url(r) && r.len() <= MAX_URL)
    {
        headers.set("Referer", r);
    }
    Ok(Capture { url, headers })
}

/// Acts on a request (the text after [`PREFIX`]) and answers it: `ok` when leaf took the download over. A `vpn` request is
/// answered with one line of data first (see [`crate::vpn::browser_state`]).
// (`reply` takes `&mut self` on Linux)
#[allow(unused_mut)]
pub fn handle(req: String, mut ack: leaf_sys::handover::Ack, cx: &mut App) {
    let msg = serde_json::from_str::<Msg>(&req);
    drop(req);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    LAST_HEARD.store(now, Ordering::Relaxed);
    // "Browse through Mullvad": the VPN's state and the proxy's address, as one line of data
    if matches!(&msg, Ok(m) if m.kind == "vpn") {
        ack.reply(&crate::vpn::browser_state().to_string());
        if ack.done() {
            crate::last_window::settle(cx);
        }
        return;
    }
    let s = cx.global::<Settings>();
    let (on, min_bytes) = (s.capture_browser, u64::from(s.capture_min_mb) << 20);
    let taken = match msg {
        Ok(m) if m.kind == "ping" || m.kind == "hello" => Ok(None),
        // "Download video with leaf": a page address for yt-dlp (the media dialog's defaults; no cookies come along)
        Ok(m) if m.kind == "video" => {
            let url = m.url.trim().to_string();
            if url.len() > MAX_URL || !dl::is_http_url(&url) || dl::has_password(&url) {
                Err(Refused::NotHttp)
            } else {
                crate::videodl::from_browser(url, m.cookies_from, cx);
                Ok(None)
            }
        }
        Ok(m) if m.kind == "download" => if on { check(m, min_bytes).map(Some) } else { Err(Refused::Off) },
        _ => Err(Refused::BadMessage),
    };
    let held = match taken {
        Ok(Some(c)) => {
            log::info!("capture: a download from the browser, {}", c.headers);
            start(c, cx);
            ack.done()
        }
        Ok(None) => ack.done(),
        Err(why) => {
            log::info!("capture: left to the browser ({why:?})");
            ack.refuse()
        }
    };
    if held {
        crate::last_window::settle(cx);
    }
}

fn start(c: Capture, cx: &mut App) {
    let dest = crate::downloads::default_folder(cx.global::<Settings>());
    if let Err(e) = std::fs::create_dir_all(&dest) {
        log::warn!("capture: the folder {} could not be made: {e}", dest.display());
    }
    let mut req = crate::downloads::Request::new(c.url, dest);
    req.sort = true;
    req.browser = c.headers.has_credentials();
    req.headers = c.headers;
    let after: Box<dyn FnOnce(Vec<std::path::PathBuf>, &mut App)> = Box::new(|arrived, cx| {
        if let Some(p) = arrived.first() {
            crate::workspace::select_in_open_folder(p, cx);
            // an NZB link the browser handed over: what it lists is downloaded from Usenet
            if crate::usenet::is_nzb(p) {
                crate::usenet::open_nzb(p.clone(), cx);
            }
        }
    });
    crate::downloads::start_with(cx, req, Some(after));
}

// ---------------------------------------------------------------------------------------------------------------------------------
// registration

fn host_exe() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name(leaf_sys::exe_name(HOST_STEM)))
}

fn manifest_dir() -> std::path::PathBuf {
    crate::settings::app_dir().unwrap_or_default().join("native_messaging")
}

fn manifests(host: &std::path::Path) -> (String, String) {
    let base = |key: &str, ids: serde_json::Value| {
        let mut v = serde_json::json!({
            "name": HOST_NAME,
            "description": "leaf: takes over downloads the browser starts",
            "path": host.to_string_lossy(),
            "type": "stdio",
        });
        v[key] = ids;
        serde_json::to_string_pretty(&v).unwrap_or_default()
    };
    let origins: Vec<String> = CHROMIUM_IDS.iter().map(|id| format!("chrome-extension://{id}/")).collect();
    (base("allowed_origins", serde_json::json!(origins)), base("allowed_extensions", serde_json::json!([FIREFOX_ID])))
}

/// Registers `leaf-nmhost` with the browsers for this user (no admin). The text says what was reached.
pub fn register() -> Result<String, String> {
    let host = host_exe().filter(|h| h.is_file()).ok_or_else(|| t!("leaf-nmhost is missing next to leaf: reinstall leaf").to_string())?;
    let (chromium, firefox) = manifests(&host);
    let r = leaf_sys::nmhost::register(HOST_NAME, &chromium, &firefox, &manifest_dir()).map_err(|e| format!("{}\n{e}", t!("The browsers could not be connected")))?;
    let mut text = if r.connected.is_empty() { String::new() } else { format!("{} {}", t!("Connected:"), r.connected.join(", ")) };
    if !r.sandboxed.is_empty() {
        if !text.is_empty() {
            text.push_str(". ");
        }
        text.push_str(&format!("{} {}", t!("Sandboxed, cannot reach leaf:"), r.sandboxed.join(", ")));
    }
    log::info!("capture: host registered ({text})");
    Ok(text)
}

pub fn unregister() {
    leaf_sys::nmhost::unregister(HOST_NAME, &manifest_dir());
}

/// The browsers that find the host.
pub fn registered() -> Vec<&'static str> {
    leaf_sys::nmhost::registered(HOST_NAME).into_iter().map(|(b, _)| b).collect()
}

/// After a move or an update: a registration that names another `leaf-nmhost` is written again for this one.
pub fn refresh() {
    let Some(host) = host_exe().filter(|h| h.is_file()) else { return };
    let found = leaf_sys::nmhost::registered(HOST_NAME);
    let stale = found.iter().any(|(_, file)| {
        let named = std::fs::read(file).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v["path"].as_str().map(std::path::PathBuf::from));
        named.as_deref() != Some(host.as_path())
    });
    if stale && let Err(e) = register() {
        log::warn!("capture: the host registration could not be renewed: {e}");
    }
}

/// "Connect browsers" on the settings page.
pub fn connect(cx: &mut App) {
    let text = match register() {
        Ok(t) => format!("{t}. {}", t!("Now add the leaf extension to the browser.")),
        Err(e) => e,
    };
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    cx.refresh_windows();
}

pub fn disconnect(cx: &mut App) {
    unregister();
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(t!("The browsers no longer hand downloads to leaf").to_string());
    cx.refresh_windows();
}

/// The settings card's line: what "Connect browsers" did, else which browsers are connected and whether the extension was heard.
pub fn status_text() -> String {
    if let Some(s) = STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return s;
    }
    let found = registered();
    if found.is_empty() {
        return t!("Not connected: the browser extension cannot reach leaf yet").to_string();
    }
    let heard = if LAST_HEARD.load(Ordering::Relaxed) > 0 { t!("the extension has been heard from") } else { t!("the extension has not been heard from since leaf started") };
    format!("{} {}; {heard}", t!("Connected:"), found.join(", "))
}

/// `leaf nmhost --register` / `--unregister`: the exit code.
pub fn cli(arg: &str) -> i32 {
    match arg {
        "--register" => match register() {
            Ok(t) => {
                println!("{t}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "--unregister" => {
            unregister();
            0
        }
        _ => {
            eprintln!("leaf nmhost --register | --unregister");
            2
        }
    }
}
