//! Asking yt-dlp what an address holds: `--simulate --dump-single-json`, 20 seconds at most.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::{Error, command, run_limited};

const LIMIT: Duration = Duration::from_secs(20);
/// What is read of the JSON at most (a long video lists thousands of formats and chapters).
const CAP: usize = 48 << 20;

#[derive(Debug, Clone, Default)]
pub struct Media {
    pub title: String,
    pub uploader: String,
    pub seconds: Option<u64>,
    pub thumbnail: Option<String>,
    /// yt-dlp's name for the site (`Youtube`, `TikTok`).
    pub site: String,
    /// Video heights it offers, highest first.
    pub heights: Vec<u32>,
    /// The page has subtitles of its own (automatic captions are not counted).
    pub has_subs: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Playlist {
    pub title: String,
    pub count: usize,
    /// The first few titles.
    pub titles: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// The one video the address is for (none: the address is a list or a channel page).
    pub video: Option<Media>,
    pub playlist: Option<Playlist>,
}

const FILE_EXTS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "xz", "bz2", "zst", "exe", "msi", "iso", "dmg", "pkg", "deb", "rpm", "appimage", "pdf", "apk", "jar", "whl", "torrent", "img", "bin", "nzb", "docx", "xlsx", "pptx",
];

/// Services that protect their streams. Refused before anything is asked; yt-dlp's own DRM errors are mapped the same way.
const DRM_HOSTS: &[&str] = &[
    "spotify.com", "netflix.com", "disneyplus.com", "primevideo.com", "hulu.com", "max.com", "hbomax.com", "tv.apple.com", "music.apple.com", "deezer.com", "tidal.com", "paramountplus.com", "peacocktv.com",
];

fn host_of(url: &str) -> String {
    let rest = url.trim().split_once("://").map_or("", |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    hostport.rsplit_once(':').map_or(hostport, |(h, _)| h).to_ascii_lowercase()
}

/// Whether the address is on a service that protects its streams.
pub fn is_drm_host(url: &str) -> bool {
    let h = host_of(url);
    DRM_HOSTS.iter().any(|d| h == *d || h.ends_with(&format!(".{d}")))
}

/// Whether it is worth asking yt-dlp about an address: a web address that does not name a plain file (an archive, an installer).
pub fn probe_candidate(url: &str) -> bool {
    let url = url.trim();
    let l = url.to_ascii_lowercase();
    if !(l.starts_with("http://") || l.starts_with("https://")) || l.contains(char::is_whitespace) {
        return false;
    }
    let path = l.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    !last.rsplit_once('.').is_some_and(|(_, ext)| FILE_EXTS.contains(&ext))
}

fn error_line(stderr: &str) -> String {
    let line = stderr.lines().rev().map(str::trim).find(|l| l.starts_with("ERROR")).or_else(|| stderr.lines().rev().map(str::trim).find(|l| !l.is_empty())).unwrap_or("yt-dlp failed");
    line.chars().take(300).collect()
}

/// Maps yt-dlp's error output to an [`Error`].
pub(crate) fn classify(stderr: &str) -> Error {
    if stderr.contains("DRM") {
        Error::Drm
    } else if stderr.contains("Unsupported URL") {
        Error::Unsupported
    } else {
        Error::Failed(error_line(stderr))
    }
}

fn ask(exe: &Path, url: &str, extra: &[&str], no_generic: bool, cookies_from: Option<&str>) -> Result<Value, Error> {
    let mut c = command(exe);
    c.arg("--ignore-config").args(["--simulate", "--dump-single-json", "--no-warnings", "--flat-playlist"]).args(extra);
    if let Some(b) = cookies_from.filter(|b| crate::browser_ok(b)) {
        c.args(["--cookies-from-browser", b]);
    }
    if no_generic {
        // (only the sites yt-dlp has an extractor for: a page that merely holds a video somewhere is not taken as a media page)
        c.args(["--use-extractors", "default,-generic"]);
    }
    c.arg("--").arg(url);
    let (code, out, err) = run_limited(c, LIMIT, CAP)?;
    match code {
        None => Err(Error::TimedOut),
        Some(0) => serde_json::from_str(&out).map_err(|e| Error::Failed(e.to_string())),
        Some(_) if no_generic && err.contains("no such option") => ask(exe, url, extra, false, cookies_from),
        Some(_) => Err(classify(&err)),
    }
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

fn media_of(v: &Value) -> Media {
    let mut heights: Vec<u32> = v.get("formats").and_then(Value::as_array).map(|f| f.iter().filter_map(|f| f.get("height").and_then(Value::as_u64)).filter(|h| *h > 0).map(|h| h as u32).collect()).unwrap_or_default();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    let uploader = [text(v, "uploader"), text(v, "channel")].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
    Media {
        title: text(v, "title"),
        uploader,
        seconds: v.get("duration").and_then(Value::as_f64).filter(|d| *d >= 0.0).map(|d| d as u64),
        thumbnail: Some(text(v, "thumbnail")).filter(|t| t.starts_with("http")),
        site: [text(v, "extractor_key"), text(v, "extractor")].into_iter().find(|s| !s.is_empty()).unwrap_or_default(),
        heights,
        has_subs: v.get("subtitles").and_then(Value::as_object).is_some_and(|o| o.keys().any(|k| k != "live_chat")),
    }
}

fn playlist_of(v: &Value) -> Option<Playlist> {
    if v.get("_type").and_then(Value::as_str) != Some("playlist") {
        return None;
    }
    let entries = v.get("entries").and_then(Value::as_array)?;
    let titles: Vec<String> = entries.iter().filter_map(|e| e.get("title").and_then(Value::as_str)).map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).take(8).collect();
    let count = v.get("playlist_count").and_then(Value::as_u64).map_or(entries.len(), |n| n as usize).max(entries.len());
    Some(Playlist { title: text(v, "title"), count, titles })
}

/// What the address holds. `Err(Unsupported)`: yt-dlp does not handle it (go on as a plain download). `Err(Drm)`: refused.
pub fn probe(exe: &Path, url: &str, cookies_from: Option<&str>) -> Result<Probe, Error> {
    if is_drm_host(url) {
        return Err(Error::Drm);
    }
    let v = ask(exe, url, &["--no-playlist"], true, cookies_from)?;
    let mut p = Probe::default();
    match playlist_of(&v) {
        Some(list) => p.playlist = Some(list),
        None => p.video = Some(media_of(&v)),
    }
    // a video that is also in a list (`watch?v=..&list=..`): the list is looked at too, so "all N" can be offered
    let l = url.to_ascii_lowercase();
    if p.video.is_some() && (l.contains("list=") || l.contains("/playlist")) {
        if let Ok(v) = ask(exe, url, &["--yes-playlist"], true, cookies_from)
            && let Some(list) = playlist_of(&v).filter(|l| l.count > 1)
        {
            p.playlist = Some(list);
        }
    }
    Ok(p)
}
