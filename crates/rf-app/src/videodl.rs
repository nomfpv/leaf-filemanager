//! Videos and audio from web pages (YouTube, TikTok, Medal and the other sites the separate program `yt-dlp` knows). leaf does not
//! ship or install yt-dlp: it is found on the PATH (or at the path in Settings), like rclone and ffmpeg. This file is leaf's face for
//! the engine `leaf_ytdlp`: an address typed, pasted or dropped is asked about first (only when yt-dlp is there); a media page opens
//! the media dialog, anything else goes on as the plain download it always was. The download is a Status Center job (progress, pause,
//! cancel), the file is named from the title and never replaces another one without the conflict dialog.
//!
//! DRM-protected services are refused with a fixed message. Nothing here is specific to a site.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use leaf_sys::fastcopy::{Conflict, Resolve};
use leaf_sys::fileops::Tracker;
use leaf_ytdlp as yt;

use crate::downloads::Request;
use crate::prompt::{self, Button, Check, Choice, Kind};
use crate::settings::Settings;
use crate::t;

type After = Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>;

/// The words for a refused service.
pub fn drm_text() -> &'static str {
    t!("This service protects its streams; leaf does not download from it.")
}


fn with_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
    let _ = w.update(cx, move |_, window, cx| f(window, cx));
}

fn notice(cx: &mut App, title: &'static str, message: String) {
    with_window(cx, move |window, cx| prompt::notice(title, message, window, cx));
}

// ---------------------------------------------------------------------------------------------------------------------------------
// what is installed (the settings card reads this every frame: the program is asked once, in the background)

struct Found {
    custom: String,
    exe: Option<PathBuf>,
    version: Option<String>,
    ffmpeg: bool,
    ready: bool,
}

static FOUND: Mutex<Option<Found>> = Mutex::new(None);
static UPDATE_TEXT: Mutex<String> = Mutex::new(String::new());

/// yt-dlp as the settings say (the path, else the PATH). Not cached: it only looks at files.
pub fn exe(s: &Settings) -> Option<PathBuf> {
    yt::find_exe(&s.ytdlp_path)
}

/// What the settings card shows: the program, its version once it has answered, and whether ffmpeg is there too.
pub fn status(s: &Settings) -> (Option<PathBuf>, Option<String>, bool) {
    let custom = s.ytdlp_path.trim().to_string();
    let mut g = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|f| f.custom != custom) {
        *g = Some(Found { custom: custom.clone(), exe: None, version: None, ffmpeg: false, ready: false });
        std::thread::Builder::new()
            .name("ytdlp-version".into())
            .spawn(move || {
                let exe = yt::find_exe(&custom);
                let version = exe.as_deref().and_then(yt::version);
                let ffmpeg = yt::find_ffmpeg().is_some();
                let mut g = FOUND.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(f) = g.as_mut().filter(|f| f.custom == custom) {
                    *f = Found { custom, exe, version, ffmpeg, ready: true };
                }
            })
            .ok();
    }
    let f = g.as_ref().expect("set above");
    (f.exe.clone().or_else(|| exe(s)), f.version.clone(), f.ready && f.ffmpeg)
}

/// "Update yt-dlp": only offered for a copy `yt-dlp -U` can update.
pub fn can_update(s: &Settings) -> bool {
    exe(s).is_some_and(|e| yt::can_self_update(&e))
}

pub fn update_text() -> String {
    UPDATE_TEXT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn update(cx: &mut App) {
    let Some(exe) = exe(cx.global::<Settings>()) else { return };
    let on_done: crate::status_center::OnDone = Box::new(|_, cx| {
        *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        cx.refresh_windows();
    });
    crate::status_center::start_with(
        cx,
        t!("Updating yt-dlp").to_string(),
        move |tr| {
            (tr.on_progress)(0, 0, Some(t!("Asking yt-dlp to update itself").to_string()));
            let (ok, text) = yt::self_update(&exe);
            let line = if ok { t!("yt-dlp is up to date.").to_string() } else { format!("{}\n{text}", t!("yt-dlp could not update itself.")) };
            *UPDATE_TEXT.lock().unwrap_or_else(|e| e.into_inner()) = if ok { line } else { text.lines().last().map(|l| format!("{} {l}", t!("yt-dlp could not update itself:"))).unwrap_or(line) };
            Ok(ok)
        },
        Some(on_done),
    );
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the way in: an address typed, pasted, dropped or given on the command line

/// Starts `req`. When yt-dlp is installed and the address is on a site it handles, the media dialog opens (`batch`: it does not,
/// the default choices are taken); otherwise it is the plain download it always was.
pub fn route(cx: &mut App, req: Request, after: After, batch: bool) {
    // (a checksum asked for means a file, not a page; a sign-in carried over from the browser is for the browser's own download)
    if req.sha256.is_some() || req.verify || req.browser || !yt::probe_candidate(&req.url) {
        return crate::downloads::start_with(cx, req, after);
    }
    if yt::is_drm_host(&req.url) {
        return notice(cx, t!("This service is protected"), drm_text().to_string());
    }
    let Some(exe) = exe(cx.global::<Settings>()) else { return crate::downloads::start_with(cx, req, after) };
    let result: Arc<Mutex<Option<Result<yt::Probe, yt::Error>>>> = Arc::default();
    let id_cell = Rc::new(Cell::new(0u64));
    let host = req.url.split_once("://").map_or("", |(_, r)| r).split(['/', '?', '#']).next().unwrap_or("").to_string();
    let (work_result, url) = (result.clone(), req.url.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        let req = req.clone();
        Box::new(move |_, cx| {
            crate::status_center::remove(cx, id_cell.get());
            let outcome = result.lock().unwrap_or_else(|e| e.into_inner()).take();
            match outcome {
                Some(Ok(probe)) if batch => start_media(cx, MediaRequest::defaults(cx, req, &probe), after),
                Some(Ok(probe)) => show_dialog(cx, req, probe, after, None),
                None | Some(Err(yt::Error::Unsupported)) => crate::downloads::start_with(cx, req, after),
                Some(Err(yt::Error::Drm)) => notice(cx, t!("This service is protected"), drm_text().to_string()),
                Some(Err(yt::Error::TimedOut)) => notice(cx, t!("Video page"), t!("yt-dlp did not answer within 20 seconds, so leaf could not tell what is on this page.").to_string()),
                Some(Err(e)) => {
                    log::warn!("videodl: {} -> {e}", req.url);
                    notice(cx, t!("Video page"), format!("{}\n{e}", t!("yt-dlp could not read this page.")));
                }
            }
        })
    };
    let id = crate::status_center::start_with_id(
        cx,
        format!("{} {host}", t!("Checking")),
        move |tr| {
            (tr.on_progress)(0, 0, Some(t!("Asking yt-dlp what is on this page").to_string()));
            *work_result.lock().unwrap_or_else(|e| e.into_inner()) = probe_limited(&tr, &exe, &url, None);
            Ok(true)
        },
        Some(on_done),
    );
    id_cell.set(id);
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the dialog

/// What a media download is asked for.
#[derive(Clone)]
pub struct MediaRequest {
    pub req: Request,
    pub title: String,
    pub kind: yt::Kind,
    pub subtitles: bool,
    pub playlist: bool,
    /// A browser yt-dlp reads the sign-in from (the extension's opt-in).
    pub cookies_from: Option<String>,
}

fn quality_of(s: &Settings) -> yt::Quality {
    match s.ytdlp_quality {
        1080 => yt::Quality::P1080,
        720 => yt::Quality::P720,
        _ => yt::Quality::Best,
    }
}

impl MediaRequest {
    /// The settings' own choices (a batch, or the browser's "Download video with leaf").
    fn defaults(cx: &App, req: Request, probe: &yt::Probe) -> Self {
        let title = probe.video.as_ref().map(|m| m.title.clone()).or_else(|| probe.playlist.as_ref().map(|p| p.title.clone())).unwrap_or_default();
        Self { req, title, kind: yt::Kind::Video(quality_of(cx.global::<Settings>())), subtitles: false, playlist: probe.video.is_none(), cookies_from: None }
    }
}

fn length(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn show_dialog(cx: &mut App, req: Request, probe: yt::Probe, after: After, cookies_from: Option<String>) {
    with_window(cx, move |window, cx| {
        let ffmpeg = yt::find_ffmpeg().is_some();
        let dest = req.dest.clone();
        let title = probe.video.as_ref().map(|m| m.title.clone()).or_else(|| probe.playlist.as_ref().map(|p| p.title.clone())).unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        if let Some(m) = &probe.video {
            lines.push(if m.title.is_empty() { t!("(no title)").to_string() } else { m.title.clone() });
            let mut meta: Vec<String> = Vec::new();
            if !m.uploader.is_empty() {
                meta.push(m.uploader.clone());
            }
            if let Some(s) = m.seconds {
                meta.push(length(s));
            }
            if let Some(h) = m.heights.first() {
                meta.push(format!("{h}p"));
            }
            if !m.site.is_empty() {
                meta.push(m.site.clone());
            }
            if !meta.is_empty() {
                lines.push(meta.join(" · "));
            }
        }
        if let Some(p) = &probe.playlist {
            let mut line = format!("{} {} {}", t!("List:"), if p.title.is_empty() { "…".to_string() } else { p.title.clone() }, format_args!("({} {})", p.count, t!("videos")));
            if !p.titles.is_empty() {
                line.push('\n');
                line.push_str(&p.titles.iter().take(5).map(|t| format!("  {t}")).collect::<Vec<_>>().join("\n"));
                if p.count > 5 {
                    line.push_str("\n  …");
                }
            }
            lines.push(line);
        }
        if !ffmpeg {
            lines.push(t!("ffmpeg was not found: leaf picks one ready-made file (the quality can be lower) and cannot convert to MP3.").to_string());
        }
        lines.push(format!("{} {}", t!("The file is saved in"), dest.display()));
        lines.push(t!("Only download what you are allowed to.").to_string());
        let default_q = match cx.global::<Settings>().ytdlp_quality {
            1080 => 1,
            720 => 2,
            _ => 0,
        };
        let (kind, quality, audio) = (Rc::new(Cell::new(0usize)), Rc::new(Cell::new(default_q)), Rc::new(Cell::new(0usize)));
        let mut choices = vec![
            Choice { label: t!("Download").into(), options: vec![t!("Video").into(), t!("Audio only").into()], selected: kind.clone() },
            Choice { label: t!("Video quality").into(), options: vec![t!("Best").into(), "1080p".into(), "720p".into()], selected: quality.clone() },
        ];
        if ffmpeg {
            choices.push(Choice { label: t!("Audio file").into(), options: vec![t!("As delivered").into(), "MP3".into()], selected: audio.clone() });
        }
        let subs = Rc::new(Cell::new(false));
        let all = Rc::new(Cell::new(probe.video.is_none()));
        let can_all = probe.video.is_some() && probe.playlist.is_some();
        let mut checks = vec![Check { label: t!("Subtitles").into(), note: if probe.video.as_ref().is_some_and(|m| m.has_subs) { "".into() } else { t!("automatic captions, if any").into() }, on: subs.clone(), enabled: true }];
        if let Some(p) = &probe.playlist {
            checks.push(Check { label: format!("{} {} {}", t!("All"), p.count, t!("videos of the list")).into(), note: "".into(), on: all.clone(), enabled: can_all });
        }
        let after = Rc::new(std::cell::RefCell::new(after));
        let go = Button::new(t!("Download"), Kind::Primary, move |_, _, cx| {
            let q = match quality.get() {
                1 => yt::Quality::P1080,
                2 => yt::Quality::P720,
                _ => yt::Quality::Best,
            };
            let k = if kind.get() == 1 { yt::Kind::Audio(if audio.get() == 1 { yt::Audio::Mp3 } else { yt::Audio::AsDelivered }) } else { yt::Kind::Video(q) };
            let m = MediaRequest { req: req.clone(), title: title.clone(), kind: k, subtitles: subs.get(), playlist: all.get(), cookies_from: cookies_from.clone() };
            start_media(cx, m, after.borrow_mut().take());
            Ok(())
        });
        prompt::show_with_checks(t!("Download video or audio"), Some(lines.join("\n")), None, choices, checks, vec![go, Button::cancel()], window, cx);
    });
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the download

/// A request that arrived with a browser's choice: asked about like a typed address, then started with the settings' defaults (the
/// extension's "Download video with leaf"; `cookies_from` is the browser whose sign-in yt-dlp may use).
pub fn from_browser(url: String, cookies_from: Option<String>, cx: &mut App) {
    let dest = crate::downloads::default_folder(cx.global::<Settings>());
    if let Err(e) = std::fs::create_dir_all(&dest) {
        log::warn!("videodl: the folder {} could not be made: {e}", dest.display());
    }
    let mut req = Request::new(url, dest);
    req.sort = true;
    let cookies_from = cookies_from.filter(|b| yt::browser_ok(b));
    let after: After = Some(Box::new(|arrived, cx| {
        if let Some(p) = arrived.first() {
            crate::workspace::select_in_open_folder(p, cx);
        }
    }));
    let Some(exe) = exe(cx.global::<Settings>()) else {
        return notice(cx, t!("Video page"), format!("{} {}", t!("yt-dlp is not installed. To install it:"), yt::install_hint()));
    };
    if yt::is_drm_host(&req.url) {
        return notice(cx, t!("This service is protected"), drm_text().to_string());
    }
    let result: Arc<Mutex<Option<Result<yt::Probe, yt::Error>>>> = Arc::default();
    let id_cell = Rc::new(Cell::new(0u64));
    let (work_result, url, cookies_probe) = (result.clone(), req.url.clone(), cookies_from.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        Box::new(move |_, cx| {
            crate::status_center::remove(cx, id_cell.get());
            match result.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(Ok(probe)) => show_dialog(cx, req, probe, after, cookies_from),
                Some(Err(yt::Error::Drm)) => notice(cx, t!("This service is protected"), drm_text().to_string()),
                Some(Err(yt::Error::Unsupported)) | None => notice(cx, t!("Video page"), t!("yt-dlp does not know this page, so there is no video for leaf to download.").to_string()),
                Some(Err(yt::Error::TimedOut)) => notice(cx, t!("Video page"), t!("yt-dlp did not answer within 20 seconds, so leaf could not tell what is on this page.").to_string()),
                Some(Err(e)) => notice(cx, t!("Video page"), format!("{}\n{e}", t!("yt-dlp could not read this page."))),
            }
        })
    };
    let id = crate::status_center::start_with_id(
        cx,
        t!("Checking the video page").to_string(),
        move |tr| {
            (tr.on_progress)(0, 0, Some(t!("Asking yt-dlp what is on this page").to_string()));
            *work_result.lock().unwrap_or_else(|e| e.into_inner()) = probe_limited(&tr, &exe, &url, cookies_probe.as_deref());
            Ok(true)
        },
        Some(on_done),
    );
    id_cell.set(id);
}

/// leaf's words for what yt-dlp said went wrong; its own line goes second.
fn fail_text(errors: &[String]) -> String {
    let all = errors.join("\n");
    let low = all.to_ascii_lowercase();
    let what = if all.contains("DRM") {
        drm_text()
    } else if low.contains("sign in") || low.contains("log in") || low.contains("login") || low.contains("private video") || low.contains("members-only") || low.contains("age") && low.contains("confirm") {
        t!("This video needs you to be signed in. In the browser extension, turn on \"Use this browser's sign-in\" and use \"Download video with leaf\".")
    } else if low.contains("ffmpeg") && (low.contains("not found") || low.contains("not installed")) {
        t!("This needs ffmpeg, which was not found on this PC.")
    } else if low.contains("429") || low.contains("too many requests") {
        t!("The site is limiting requests. Try again later.")
    } else if low.contains("unavailable") || low.contains("not available") || low.contains("removed") || low.contains("404") {
        t!("This video is not available.")
    } else if low.contains("urlopen error") || low.contains("timed out") || low.contains("connection") {
        t!("The connection to the site broke and did not come back.")
    } else {
        t!("yt-dlp could not download this video.")
    };
    match errors.last().map(|e| e.chars().take(300).collect::<String>()) {
        Some(d) if !d.trim().is_empty() && what != drm_text() => format!("{what}\n{d}"),
        _ => what.to_string(),
    }
}

fn phase_text(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.contains("merg") {
        t!("Merging picture and sound")
    } else if n.contains("extractaudio") || n.contains("convert") {
        t!("Converting")
    } else if n.contains("subtitle") {
        t!("Subtitles")
    } else {
        t!("Finishing")
    }
}


/// Everything the job thread needs from the settings, read on the UI thread.
struct Plan {
    exe: PathBuf,
    parallel: usize,
    limit: u64,
    categories: Vec<(String, Vec<String>)>,
    through_vpn: bool,
    ffmpeg: bool,
}

pub fn start_media(cx: &mut App, m: MediaRequest, after: After) {
    let s = cx.global::<Settings>();
    let Some(exe) = exe(s) else {
        return notice(cx, t!("Video page"), format!("{} {}", t!("yt-dlp is not installed. To install it:"), yt::install_hint()));
    };
    let plan = Plan {
        exe,
        parallel: s.download_parallel.clamp(1, 16) as usize,
        limit: u64::from(s.download_speed_limit) << 20,
        categories: if m.req.sort && s.download_sort { s.download_categories.iter().map(|c| (c.folder.clone(), c.exts.clone())).collect() } else { Vec::new() },
        through_vpn: m.req.vpn.unwrap_or(s.vpn_downloads),
        ffmpeg: yt::find_ffmpeg().is_some(),
    };
    let name = if m.title.trim().is_empty() { m.req.url.clone() } else { m.title.clone() };
    let title = format!("{} {} → {}", t!("Downloading"), name, rf_core::history::display_name(&m.req.dest));
    let arrived: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let id_cell = Rc::new(Cell::new(0u64));
    let (work_m, got, fail_flag) = (m.clone(), arrived.clone(), failed.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        let m = m.clone();
        Box::new(move |ok, cx| {
            let id = id_cell.get();
            let v = std::mem::take(&mut *arrived.lock().unwrap_or_else(|e| e.into_inner()));
            if ok {
                if let Some(p) = v.first() {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let more = if v.len() > 1 { format!(" (+{})", v.len() - 1) } else { String::new() };
                    crate::status_center::set_result(cx, id, Some(format!("{} {name}{more}", t!("Downloaded"))), None);
                    crate::status_center::set_extras(cx, id, None, p.parent().map(Path::to_path_buf), None);
                }
                if let Some(after) = after {
                    after(v, cx);
                }
            } else if failed.load(Ordering::Relaxed) {
                // a failed download keeps the parts it has: Retry carries on from them
                let retry: crate::status_center::ResultAction = (
                    t!("Retry").to_string(),
                    Rc::new(move |cx: &mut App| {
                        crate::status_center::remove(cx, id);
                        start_media(cx, m.clone(), None);
                    }),
                );
                crate::status_center::set_result(cx, id, None, Some(retry));
            }
        })
    };
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_m, plan, &got, &fail_flag), Some(on_done));
    id_cell.set(id);
}

/// leaf's VPN proxy as yt-dlp takes it, when the VPN is up and lets other programs use it.
fn vpn_proxy() -> Option<String> {
    let v = crate::vpn::browser_state();
    if v["state"] != "on" {
        return None;
    }
    let p = v.get("proxy").filter(|p| p.is_object())?;
    let (host, port) = (p["host"].as_str()?, p["port"].as_u64()?);
    Some(match (p["user"].as_str(), p["password"].as_str()) {
        (Some(u), Some(pw)) => format!("socks5h://{u}:{pw}@{host}:{port}"),
        _ => format!("socks5h://{host}:{port}"),
    })
}

fn work(tr: &Tracker, m: &MediaRequest, plan: Plan, arrived: &Mutex<Vec<PathBuf>>, failed: &AtomicBool) -> anyhow::Result<bool> {
    let fail = |text: String| -> anyhow::Result<bool> {
        failed.store(true, Ordering::Relaxed);
        Err(anyhow::anyhow!(text))
    };
    let Some(_turn) = leaf_download::queue::wait_turn(plan.parallel, &tr.cancel, |place| {
        let text = if leaf_download::queue::is_open() { format!("{} ({place})", t!("Waiting for the downloads before it")) } else { t!("Waiting for the scheduled time").to_string() };
        (tr.on_progress)(0, 0, Some(text))
    }) else {
        return Ok(false);
    };
    // (through the VPN or not at all: yt-dlp is another program, so the VPN's proxy is the only way to send it through)
    let proxy = if plan.through_vpn {
        match vpn_proxy() {
            Some(p) => Some(p),
            None => return fail(t!("This download is set to go through leaf's VPN, but the VPN is not connected or \"Let other programs use the VPN\" is off, so yt-dlp could not be sent through it. Nothing was downloaded.").to_string()),
        }
    } else {
        None
    };
    let choices = format!("{:?}|{}|{}", m.kind, m.subtitles, m.playlist);
    let work_dir = yt::work_dir(&m.req.dest, &yt::key_for(&m.req.url, &choices));
    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return fail(format!("{}\n{e}", t!("The file could not be written in this folder")));
    }
    let job = yt::Job { url: m.req.url.clone(), work_dir: work_dir.clone(), kind: m.kind, subtitles: m.subtitles, playlist: m.playlist, ffmpeg: plan.ffmpeg, cookies_from: m.cookies_from.clone(), limit: plan.limit, proxy };
    // (a proxy file left by a job that was killed is removed; this job's own is written readable by the user only and goes when the job
    // ends, however it ends. Without the file the job does not start: it would go out directly)
    yt::remove_stale_proxy_files(&m.req.dest);
    let _proxy_file = match yt::write_proxy_config(&job) {
        Ok(f) => f,
        Err(e) => return fail(format!("{}
{e}", t!("The VPN's proxy could not be handed to yt-dlp, so nothing was downloaded."))),
    };
    let args = yt::args(&job);
    // (the VPN proxy's password goes in the process's environment, not on its command line)
    let ctl = yt::RunCtl { cancel: tr.cancel.clone(), paused: tr.paused.clone(), env: job.proxy.as_deref().map(yt::proxy_env).unwrap_or_default() };
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    let name = if m.title.is_empty() { String::new() } else { m.title.clone() };
    let end = yt::run(&plan.exe, &args, &ctl, &mut |ev| match ev {
        yt::Event::Progress(p, stream) => {
            let total = p.total.unwrap_or(0);
            tr.bytes.set(p.done.unwrap_or(0), total);
            let permille = if total > 0 { (p.done.unwrap_or(0) as f64 / total as f64 * 1000.0).min(1000.0) as u32 } else { 0 };
            let label = if stream > 1 { format!("{name} · {} {stream}", t!("part")) } else { name.clone() };
            (tr.on_progress)(if total > 0 { 1000 } else { 0 }, permille, Some(label));
        }
        yt::Event::Processing(p) => (tr.on_progress)(0, 0, Some(phase_text(&p).to_string())),
        yt::Event::Paused => (tr.on_progress)(0, 0, Some(t!("Paused").to_string())),
        yt::Event::Resumed => (tr.on_progress)(0, 0, Some(t!("Connecting").to_string())),
        yt::Event::Item { .. } => {}
    });
    let end = match end {
        Ok(e) => e,
        Err(e) => return fail(format!("{}\n{e}", t!("yt-dlp could not be started."))),
    };
    if end.cancelled || tr.cancel.load(Ordering::Relaxed) {
        yt::remove_work_dir(&work_dir);
        return Ok(false);
    }
    if end.items.is_empty() {
        log::warn!("videodl: {} -> code {:?}, {} error lines", m.req.url, end.code, end.errors.len());
        // (yt-dlp may repeat the proxy address with its login in an error line)
        let errors: Vec<String> = match job.proxy.as_deref() {
            Some(p) => end.errors.iter().map(|e| yt::redact(e, p)).collect(),
            None => end.errors.clone(),
        };
        return fail(fail_text(&errors));
    }
    // every finished video goes from the work folder to its place, named from its title
    let mut saved: Vec<PathBuf> = Vec::new();
    for (title, file) in &end.items {
        let Some(file_name) = file.file_name() else { continue };
        let file = work_dir.join(file_name);
        if !file.is_file() {
            continue;
        }
        match place(tr, &m.req.dest, &plan.categories, &work_dir, &file, title) {
            Placed::Saved(p) => saved.push(p),
            Placed::Skipped => {}
            Placed::Cancelled => {
                yt::remove_work_dir(&work_dir);
                return Ok(false);
            }
            Placed::Failed(e) => return fail(format!("{}\n{e}", t!("The file could not be written in this folder"))),
        }
    }
    // (what is left in the folder is leaf's own: stale parts of a skipped video)
    yt::remove_work_dir(&work_dir);
    if saved.is_empty() {
        return Ok(false);
    }
    arrived.lock().unwrap_or_else(|e| e.into_inner()).extend(saved);
    if end.code.is_some_and(|c| c != 0) {
        log::info!("videodl: some videos of the list failed ({} error lines)", end.errors.len());
    }
    Ok(true)
}

enum Placed {
    Saved(PathBuf),
    Skipped,
    Cancelled,
    Failed(String),
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// `stem.ext`, else `stem (2).ext`, ... : the first name that is free.
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    (2..).map(|i| dir.join(format!("{stem} ({i}){dot}"))).find(|p| !exists(p)).expect("an endless range")
}

/// Moves a finished file (and its subtitle files) out of the work folder: named from the title, into the category's folder when the
/// settings sort, and, when that name is taken, after the conflict dialog's answer (never replacing without it).
fn place(tr: &Tracker, dest: &Path, categories: &[(String, Vec<String>)], work_dir: &Path, file: &Path, title: &str) -> Placed {
    let ext = file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = categories
        .iter()
        .find(|(f, exts)| !f.is_empty() && leaf_download::sanitize(f) == *f && exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)))
        .map(|(f, _)| dest.join(f))
        .unwrap_or_else(|| dest.to_path_buf());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Placed::Failed(e.to_string());
    }
    let stem = yt::stem_for_title(if title.trim().is_empty() { file.file_stem().and_then(|s| s.to_str()).unwrap_or("") } else { title });
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let mut target = dir.join(format!("{stem}{dot}"));
    let mut final_stem = stem.clone();
    if exists(&target) {
        let meta = std::fs::metadata(&target).ok();
        let incoming = std::fs::metadata(file).ok();
        let conflict = Conflict {
            src: PathBuf::from(target.file_name().unwrap_or_default()),
            dst: target.clone(),
            src_dir: false,
            dst_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
            src_size: incoming.as_ref().map_or(0, |m| m.len()),
            dst_size: meta.as_ref().map_or(0, |m| m.len()),
            src_time: None,
            dst_time: meta.as_ref().and_then(|m| m.modified().ok()),
        };
        let mismatched = conflict.mismatched();
        let answer = match tr.ask.as_ref() {
            Some(ask) => match ask(std::slice::from_ref(&conflict)).and_then(|v| v.first().copied()) {
                Some(Resolve::Replace) if !mismatched && !conflict.dst_dir && meta.as_ref().is_some_and(|m| m.is_file()) => Some(true),
                Some(Resolve::Skip) => return Placed::Skipped,
                Some(_) => Some(false),
                None => return Placed::Cancelled,
            },
            None => Some(false),
        };
        if answer == Some(true) {
            if let Err(e) = std::fs::remove_file(&target) {
                return Placed::Failed(e.to_string());
            }
        } else {
            target = free_name(&dir, &stem, &ext);
            final_stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(stem.clone());
        }
    }
    if let Err(e) = move_file(file, &target) {
        return Placed::Failed(e.to_string());
    }
    // subtitle files: `leaf-video.en.srt` becomes `Title.en.srt`
    if let Some(old_stem) = file.file_stem().map(|s| s.to_string_lossy().into_owned())
        && let Ok(rd) = std::fs::read_dir(work_dir)
    {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = n.strip_prefix(&format!("{old_stem}.")) else { continue };
            if n == file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default() || rest.ends_with(".part") || rest.ends_with(".ytdl") || rest.contains(".temp.") {
                continue;
            }
            let side = dir.join(format!("{final_stem}.{rest}"));
            if !exists(&side) {
                let _ = move_file(&e.path(), &side);
            }
        }
    }
    Placed::Saved(target)
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to).or_else(|_| {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)
    })
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the settings card

/// "Videos from websites" on the Downloads page.
pub fn settings_card(p: &crate::theme::Palette, s: &Settings) -> AnyElement {
    use crate::settings;
    use crate::settings_view::{button, card_body, dropdown, shell};
    use gpui_kit::assets::IconName as L;
    let (found, version, ffmpeg) = status(s);
    let line = match (&found, &version) {
        (Some(path), Some(v)) => format!("yt-dlp {v}\n{}", path.display()),
        (Some(path), None) => format!("{}\n{}", t!("yt-dlp was found but did not answer yet"), path.display()),
        (None, _) if !s.ytdlp_path.trim().is_empty() => format!("{} {}", t!("There is no program at this path:"), s.ytdlp_path.trim()),
        (None, _) => format!("{} {}", t!("yt-dlp is not installed. To install it:"), yt::install_hint()),
    };
    let ffmpeg_line = if ffmpeg { t!("ffmpeg was found: pictures and sound are merged, and audio can be saved as MP3.") } else { t!("ffmpeg was not found: leaf picks one ready-made file (the quality can be lower).") };
    let desc = format!("{}\n{line}\n{ffmpeg_line}", t!("Download videos and audio from YouTube, TikTok, Medal and other sites by pasting their address in Download here. leaf uses the separate program yt-dlp for this; it does not install it. Services that protect their streams are not supported."));
    let mut shell = shell(p).child(card_body(p, L::Globe, t!("Videos from websites").into(), Some(desc.into()), None));
    shell = shell.child(card_body(
        p,
        L::Gauge,
        t!("Video quality").into(),
        Some(t!("What the dialog starts with.").into()),
        Some(dropdown("ytdlp-quality", p, s.ytdlp_quality, vec![(0u32, t!("Best").to_string()), (1080, "1080p".to_string()), (720, "720p".to_string())], |s, v| s.ytdlp_quality = v)),
    ));
    let current = s.ytdlp_path.trim().to_string();
    shell = shell.child(card_body(
        p,
        L::Folder,
        t!("Path to yt-dlp").into(),
        Some(if current.is_empty() { t!("Not set: found on the PATH").to_string() } else { current.clone() }.into()),
        Some(
            gpui_kit::component::h_flex()
                .gap_2()
                .children((!current.is_empty()).then(|| button("ytdlp-path-clear", p, t!("Clear"), |_, cx| settings::update(cx, |s| s.ytdlp_path.clear()))))
                .child(button("ytdlp-path", p, t!("Change…"), move |window, cx| {
                    let initial = current.clone();
                    crate::prompt::show(
                        t!("Path to yt-dlp"),
                        Some(t!("The full path of the yt-dlp program. Leave it empty to find it on the PATH.").to_string()),
                        Some((initial, if cfg!(windows) { "C:\\Tools\\yt-dlp.exe".to_string() } else { "/usr/local/bin/yt-dlp".to_string() })),
                        vec![
                            crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                                let v = value.unwrap_or_default().trim().trim_matches('"').to_string();
                                if !v.is_empty() && !Path::new(&v).is_file() {
                                    return Err(t!("There is no program at this path").to_string());
                                }
                                settings::update(cx, move |s| s.ytdlp_path = v);
                                Ok(())
                            }),
                            crate::prompt::Button::cancel(),
                        ],
                        window,
                        cx,
                    );
                }))
                .into_any_element(),
        ),
    ));
    if can_update(s) {
        let note = update_text();
        shell = shell.child(card_body(
            p,
            L::RefreshCw,
            t!("Update yt-dlp").into(),
            Some(if note.is_empty() { t!("Sites change often; yt-dlp is updated about weekly. This asks yt-dlp to update itself.").to_string() } else { note }.into()),
            Some(button("ytdlp-update", p, t!("Update"), |_, cx| update(cx))),
        ));
    }
    shell.into_any_element()
}

/// At most two probes run at once (a batch of many addresses must not start as many yt-dlp processes).
fn probe_limited(tr: &Tracker, exe: &Path, url: &str, cookies_from: Option<&str>) -> Option<Result<yt::Probe, yt::Error>> {
    static RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    loop {
        let n = RUNNING.load(Ordering::SeqCst);
        if n < 2 && RUNNING.compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            break;
        }
        if tr.cancel.load(Ordering::Relaxed) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let r = yt::probe(exe, url, cookies_from);
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    Some(r)
}

/// Check scripts (`demo video`): the "Download video or audio" question for a made-up page, as if yt-dlp had answered. Nothing is
/// asked of the network and nothing is started unless the Download button is pressed.
pub fn demo_dialog(cx: &mut App) {
    let dest = crate::downloads::default_folder(cx.global::<Settings>());
    let req = Request::new("https://example.com/watch?v=x".into(), dest);
    let media = yt::Media {
        title: "A walk in the woods".into(),
        uploader: "Example Channel".into(),
        seconds: Some(754),
        site: "Example".into(),
        heights: vec![1080, 720, 480],
        has_subs: true,
        ..yt::Media::default()
    };
    show_dialog(cx, req, yt::Probe { video: Some(media), playlist: None }, None, None);
}
