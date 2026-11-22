//! "Convert to video file…" on an unprotected Blu-ray (a disc, a `BDMV` folder, an `.iso` holding one), an AVCHD card or `.m2ts`
//! files, and "Mount" / "Eject" on a disc image. The work is leaf-bluray's (reading the disc, the lossless remux, the optional
//! ffmpeg re-encode); images are mounted through `leaf_sys::discimage`. A copy-protected disc gets one message and nothing else.
//!
//! The dialog: the title when several are long (length, size, chapters), then the audio and subtitle tracks to keep, the target
//! file (folder and name), and the mode (Keep quality = remux to MKV; with ffmpeg also the smaller re-encodes). The job runs in the
//! Status Center (progress, speed, time left, pause, cancel), writes `name.mkv.leaf-part` and renames it at the end, never over a
//! file that is there (the usual conflict dialog asks first), and checks the free space before it starts.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui_kit::*;
use leaf_bluray::disc::{self, OpenError, Title, Track};
use leaf_bluray::ffmpeg::{self, Preset};
use leaf_bluray::mpls::StreamKind;
use leaf_bluray::{output, remux};
use leaf_sys::fileops::Tracker;

use crate::t;

/// The selection is something to convert (cheap: for the right-click menu).
pub fn offers(items: &[PathBuf]) -> bool {
    match items {
        [] => false,
        [one] if one.is_dir() => disc::looks_like_disc(one),
        // an image is offered when it is big enough to hold a film (a Windows installer is not one)
        [one] if disc::is_image(one) => one.metadata().is_ok_and(|m| m.is_file() && m.len() >= 1 << 30),
        many => many.iter().all(|p| disc::is_stream_file(p)) && many.iter().all(|p| p.is_file()),
    }
}

/// One disc image is selected (for Mount / Eject).
pub fn is_image(items: &[PathBuf]) -> bool {
    matches!(items, [one] if leaf_sys::discimage::can_mount(one) && one.is_file())
}

/// The image is mounted (as far as a menu can know cheaply).
pub fn image_mounted(items: &[PathBuf]) -> bool {
    matches!(items, [one] if leaf_sys::discimage::is_mounted_quick(one))
}

fn size_text(b: u64) -> String {
    rf_core::format::size(b)
}

fn length_text(ns: u64) -> String {
    let s = ns / 1_000_000_000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn title_line(tl: &Title) -> String {
    let mut parts = Vec::new();
    if tl.duration_ns > 0 {
        parts.push(length_text(tl.duration_ns));
    }
    parts.push(size_text(tl.size));
    if tl.chapters_ns.len() > 1 {
        parts.push(format!("{} {}", tl.chapters_ns.len(), t!("chapters")));
    }
    parts.join(" · ")
}

/// Where the converted file goes by default: beside loose files, beside a copied disc folder, else the Videos folder (a disc or a
/// mounted image cannot be written to).
fn default_folder(src: &disc::Source, read_only: bool) -> PathBuf {
    let videos = || leaf_sys::KnownFolder::Videos.path().or_else(|| leaf_sys::KnownFolder::Profile.path()).unwrap_or_else(|| PathBuf::from("."));
    if read_only {
        return videos();
    }
    match src {
        disc::Source::Files(f) => f[0].parent().map(Path::to_path_buf).unwrap_or_else(videos),
        disc::Source::Disc(bdmv) => {
            // the folder that holds the disc's folder (the disc's own root when it is a drive: not that)
            let mut root = bdmv.parent();
            while let Some(r) = root {
                let n = r.file_name().map(|n| n.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
                if n == "AVCHD" || n == "PRIVATE" {
                    root = r.parent();
                } else {
                    break;
                }
            }
            match root.and_then(Path::parent) {
                Some(p) => p.to_path_buf(),
                None => videos(),
            }
        }
    }
}

fn safe_name(s: &str) -> String {
    let n: String = s.chars().map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let n = n.trim().trim_end_matches('.').to_string();
    if n.is_empty() { "Video".into() } else { n }
}

struct Opened {
    disc: disc::Disc,
    folder: PathBuf,
    /// What the conflict dialog names as the source.
    source: PathBuf,
}

/// Mounts an image when it is one, finds the disc and reads its titles (on a worker thread). Err: (title, message) to show.
fn open_source(items: Vec<PathBuf>) -> Result<Opened, (String, String)> {
    let mut read_only = false;
    let mut name_hint = None;
    let mut source = items[0].clone();
    let paths = if let [one] = items.as_slice()
        && disc::is_image(one)
    {
        let root = leaf_sys::discimage::mount(one)
            .map_err(|e| (t!("The image could not be mounted").to_string(), format!("{}\n{e}", t!("Mount the image first, then convert the disc it shows."))))?;
        read_only = true;
        name_hint = one.file_stem().map(|s| s.to_string_lossy().into_owned());
        source = one.clone();
        vec![root]
    } else {
        items
    };
    let src = disc::locate(&paths).ok_or_else(|| (t!("Nothing to convert").to_string(), t!("There is no Blu-ray or AVCHD video here.").to_string()))?;
    if let disc::Source::Disc(bdmv) = &src {
        // a disc in a drive (its folder is the root of a volume)
        read_only |= bdmv.parent().is_some_and(|r| r.parent().is_none());
    }
    let mut d = disc::open(&src).map_err(|e| match e {
        OpenError::Protected => (t!("Can't convert this disc").to_string(), leaf_bluray::PROTECTED.to_string()),
        OpenError::NotADisc => (t!("Nothing to convert").to_string(), t!("There is no Blu-ray or AVCHD video here.").to_string()),
        OpenError::Io(e) => (t!("The disc could not be read").to_string(), e),
    })?;
    if let Some(n) = name_hint {
        d.name = n;
    } else if let disc::Source::Disc(bdmv) = &src
        && d.name == "Blu-ray"
        && let Some(root) = bdmv.parent()
        && let Ok(v) = leaf_sys::volume::volume_info(root)
        && !v.label.trim().is_empty()
    {
        d.name = v.label.trim().to_string();
    }
    let folder = default_folder(&src, read_only);
    Ok(Opened { disc: d, folder, source })
}

/// "Convert to video file…".
pub fn convert(items: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    if items.is_empty() {
        return;
    }
    let work = cx.background_spawn(async move { open_source(items) });
    window
        .spawn(cx, async move |cx| {
            let got = work.await;
            let _ = cx.update(|window, cx| match got {
                Err((title, msg)) => crate::prompt::notice(title, msg, window, cx),
                Ok(o) => choose_title(Rc::new(o), window, cx),
            });
        })
        .detach();
}

fn choose_title(o: Rc<Opened>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Choice, Kind};
    let long = o.disc.long_titles().len();
    if long <= 1 {
        tracks_dialog(o, 0, window, cx);
        return;
    }
    let pick = Rc::new(Cell::new(0usize));
    let options: Vec<SharedString> = o.disc.titles[..long].iter().map(|tl| SharedString::from(title_line(tl))).collect();
    let choices = vec![Choice { label: t!("Title").into(), options, selected: pick.clone() }];
    let next = Button::new(t!("Next"), Kind::Primary, move |_, window, cx| {
        let (o, k) = (o.clone(), pick.get());
        window.defer(cx, move |window, cx| tracks_dialog(o, k, window, cx));
        Ok(())
    });
    crate::prompt::show_with(
        t!("Convert to video file"),
        Some(t!("This disc has several long titles (a second cut, another language). Choose one.").to_string()),
        None,
        choices,
        vec![next, Button::cancel()],
        window,
        cx,
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Remux,
    Encode(Preset),
}

fn tracks_dialog(o: Rc<Opened>, k: usize, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Check, Choice, Kind};
    let Some(tl) = o.disc.titles.get(k).cloned() else { return };
    let ff = ffmpeg::find();
    let video: Vec<Track> = tl.tracks.iter().filter(|t| t.kind == StreamKind::Video).take(1).cloned().collect();
    let others: Vec<Track> = tl.tracks.iter().filter(|t| t.kind != StreamKind::Video).cloned().collect();
    let first_audio = others.iter().position(|t| t.kind == StreamKind::Audio && t.remuxable());
    let mut checks = Vec::new();
    for v in &video {
        checks.push(Check { label: format!("{}: {}", t!("Video"), v.label()).into(), note: "".into(), on: Rc::new(Cell::new(true)), enabled: false });
    }
    let ticks: Vec<(Track, Rc<Cell<bool>>)> = others
        .iter()
        .enumerate()
        .map(|(i, tr)| {
            let on = match tr.kind {
                StreamKind::Audio => Some(i) == first_audio,
                _ => tr.remuxable(),
            };
            (tr.clone(), Rc::new(Cell::new(on && (tr.remuxable() || ff.is_some()))))
        })
        .collect();
    for (tr, on) in &ticks {
        let what = if tr.kind == StreamKind::Audio { t!("Audio") } else { t!("Subtitles") };
        let note = if tr.remuxable() { String::new() } else { t!("needs ffmpeg").to_string() };
        checks.push(Check { label: format!("{what}: {}", tr.label()).into(), note: note.into(), on: on.clone(), enabled: tr.remuxable() || ff.is_some() });
    }
    let mut modes = vec![Mode::Remux];
    if ff.is_some() {
        modes.extend(Preset::ALL.iter().map(|p| Mode::Encode(*p)));
    }
    let mode = Rc::new(Cell::new(0usize));
    let mode_names: Vec<SharedString> = modes
        .iter()
        .map(|m| match m {
            Mode::Remux => SharedString::from(t!("Keep quality (MKV)")),
            Mode::Encode(p) => SharedString::from(crate::i18n::td(p.label())),
        })
        .collect();
    let choices = vec![Choice { label: t!("Mode").into(), options: mode_names, selected: mode.clone() }];
    let name = if tl.name.to_ascii_lowercase().ends_with(".mpls") && o.disc.long_titles().len() > 1 {
        format!("{} ({})", o.disc.name, tl.name.split('.').next().unwrap_or(""))
    } else {
        o.disc.name.clone()
    };
    let initial = o.folder.join(format!("{}.mkv", safe_name(&name))).to_string_lossy().into_owned();
    let mut message = format!("{} — {}", o.disc.name, title_line(&tl));
    if ff.is_none() {
        message.push('\n');
        message.push_str(t!("A smaller re-encoded file needs ffmpeg, which is not installed: Keep quality only."));
    }
    let folder = o.folder.clone();
    let source = o.source.clone();
    let go = Button::new(t!("Convert"), Kind::Primary, move |typed, _, cx| {
        let typed = typed.unwrap_or_default();
        let typed = typed.trim();
        if typed.is_empty() {
            return Err(t!("Enter a file name").into());
        }
        let mut target = PathBuf::from(typed);
        if target.is_relative() {
            target = folder.join(target);
        }
        if !target.parent().is_some_and(Path::is_dir) {
            return Err(t!("That folder does not exist").into());
        }
        let m = modes[mode.get().min(modes.len() - 1)];
        target.set_extension(match m {
            Mode::Remux => "mkv",
            Mode::Encode(p) => p.ext(),
        });
        let chosen: Vec<&Track> = video.iter().chain(ticks.iter().filter(|(_, on)| on.get()).map(|(t, _)| t)).collect();
        if chosen.is_empty() {
            return Err(t!("Tick at least one track").into());
        }
        let foreign = chosen.iter().any(|t| !t.remuxable());
        if foreign && matches!(m, Mode::Encode(_)) {
            return Err(t!("A track marked \"needs ffmpeg\" can only be kept with Keep quality: untick it or choose Keep quality").into());
        }
        let job = Job {
            title: tl.clone(),
            pids: chosen.iter().map(|t| t.pid).collect(),
            target,
            mode: m,
            ffmpeg: ff.clone(),
            via_ffmpeg: foreign,
            source: source.clone(),
        };
        start(job, cx);
        Ok(())
    });
    crate::prompt::show_with_checks(
        t!("Convert to video file"),
        Some(message),
        Some((initial, t!("Folder and file name").to_string())),
        choices,
        checks,
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

struct Job {
    title: Title,
    pids: Vec<u16>,
    target: PathBuf,
    mode: Mode,
    ffmpeg: Option<PathBuf>,
    /// A chosen track is one leaf does not remux: ffmpeg copies them all.
    via_ffmpeg: bool,
    source: PathBuf,
}

fn start(job: Job, cx: &mut App) {
    let name = job.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = format!("{} {name}", t!("Converting to video file:"));
    crate::status_center::start_with_id(cx, title, move |tracker| run(job, tracker), None);
}

/// The free space of the folder's volume.
fn free_space(dir: &Path) -> Option<u64> {
    let at = if cfg!(windows) { dir.ancestors().last().unwrap_or(dir) } else { dir };
    leaf_sys::volume::volume_info(at).ok().map(|v| v.free).filter(|&f| f > 0)
}

fn failed(words: &str, details: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{words}\n{details}")
}

/// The job, on the Status Center's worker thread. Ok(false): cancelled (or skipped at the name question).
fn run(job: Job, tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    let mut target = job.target.clone();
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let need = match job.mode {
        Mode::Remux => job.title.size,
        Mode::Encode(_) => job.title.size / 4,
    };
    if let Some(free) = free_space(&dir)
        && free < need
    {
        return Err(failed(t!("Not enough free space for the video file"), format!("{} {}, {} {}", t!("needs about"), size_text(need), size_text(free), t!("free"))));
    }
    // the name is taken: the usual question (the converted file never replaces it: keep both, or skip)
    if target.exists() {
        let meta = std::fs::metadata(&target).ok();
        let c = leaf_sys::fastcopy::Conflict {
            src: job.source.clone(),
            dst: target.clone(),
            src_dir: true,
            dst_dir: false,
            src_size: job.title.size,
            dst_size: meta.as_ref().map_or(0, |m| m.len()),
            src_time: None,
            dst_time: meta.and_then(|m| m.modified().ok()),
        };
        let answer = match &tracker.ask {
            Some(ask) => ask(&[c]),
            None => Some(vec![leaf_sys::fastcopy::Resolve::KeepBoth]),
        };
        match answer.as_deref() {
            None | Some([leaf_sys::fastcopy::Resolve::Skip, ..]) => return Ok(false),
            _ => target = output::free_name(&target),
        }
    }
    let progress = |done: u64, all: u64| {
        tracker.bytes.set(done, all);
        (tracker.on_progress)(1000, (done.saturating_mul(1000) / all.max(1)) as u32, None);
    };
    (tracker.on_progress)(1000, 0, Some(target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    let r = if job.via_ffmpeg {
        copy_with_ffmpeg(&job, &target, &tracker)
    } else {
        match job.mode {
            Mode::Remux => {
                let file = output::create_part(&target).map_err(|e| failed(t!("The video file could not be created"), e))?;
                let ctl = remux::Ctl { cancel: &tracker.cancel, paused: &tracker.paused, progress: &progress };
                let name = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                finish_remux(remux::remux(&job.title, &job.pids, &name, file, &ctl).map(drop).map_err(remux_error), &tracker)
            }
            Mode::Encode(p) => encode(&job, p, &target, &tracker, &progress),
        }
    };
    let part = output::part_path(&target);
    match r {
        Ok(true) => {
            let at = output::finish(&target, leaf_sys::fileops::rename_no_replace).map_err(|e| {
                failed(t!("The finished video file could not be given its name"), format!("{}: {e}", part.display()))
            })?;
            log::info!("bluray: converted to {}", at.display());
            Ok(true)
        }
        Ok(false) => {
            let _ = std::fs::remove_file(&part);
            Ok(false)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn remux_error(e: remux::Error) -> anyhow::Error {
    match e {
        remux::Error::Cancelled => anyhow::anyhow!("cancelled"),
        remux::Error::Protected => anyhow::anyhow!("{}", leaf_bluray::PROTECTED),
        remux::Error::Io(e) => failed(t!("The disc could not be read or the video file written"), e),
        remux::Error::Bad(s) => failed(t!("This title could not be converted"), s),
    }
}

/// Ok(true) done, Ok(false) cancelled.
fn finish_remux(r: Result<(), anyhow::Error>, tracker: &Tracker) -> anyhow::Result<bool> {
    match r {
        Ok(()) => Ok(true),
        Err(_) if tracker.cancel.load(Ordering::Relaxed) => Ok(false),
        Err(e) => Err(e),
    }
}

/// leaf's remux into ffmpeg's standard input, ffmpeg re-encoding into the part file.
fn encode(job: &Job, preset: Preset, target: &Path, tracker: &Tracker, progress: &dyn Fn(u64, u64)) -> anyhow::Result<bool> {
    let ff = job.ffmpeg.as_deref().ok_or_else(|| anyhow::anyhow!("{}", t!("ffmpeg is not installed")))?;
    let part = output::part_path(target);
    if part.is_file() {
        let _ = std::fs::remove_file(&part);
    }
    let mut run = ffmpeg::spawn(ff, &ffmpeg::encode_args(preset, &part), true, false).map_err(|e| failed(t!("ffmpeg could not be started"), e))?;
    let stdin = run.child.stdin.take().ok_or_else(|| anyhow::anyhow!("{}", t!("ffmpeg could not be started")))?;
    let ctl = remux::Ctl { cancel: &tracker.cancel, paused: &tracker.paused, progress };
    let name = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let fed = remux::remux(&job.title, &job.pids, &name, leaf_bluray::mkv::Pipe(stdin), &ctl).map(drop);
    // (the pipe is closed by now: ffmpeg finishes the file)
    if tracker.cancel.load(Ordering::Relaxed) {
        let _ = run.child.kill();
        let _ = run.child.wait();
        return Ok(false);
    }
    let status = run.child.wait().map_err(|e| failed(t!("ffmpeg could not be started"), e))?;
    if !status.success() {
        return Err(failed(t!("ffmpeg could not make the smaller file"), run.last_words()));
    }
    finish_remux(fed.map_err(remux_error), tracker)
}

/// ffmpeg copies the chosen tracks (some of them a kind leaf does not remux) into the part file, the clips joined.
fn copy_with_ffmpeg(job: &Job, target: &Path, tracker: &Tracker) -> anyhow::Result<bool> {
    use std::io::BufRead;
    let ff = job.ffmpeg.as_deref().ok_or_else(|| anyhow::anyhow!("{}", t!("ffmpeg is not installed")))?;
    let part = output::part_path(target);
    if part.is_file() {
        let _ = std::fs::remove_file(&part);
    }
    let clips: Vec<PathBuf> = job.title.clips.iter().map(|c| c.path.clone()).collect();
    let mut run = ffmpeg::spawn(ff, &ffmpeg::copy_args(&clips, &job.pids, &part), false, true).map_err(|e| failed(t!("ffmpeg could not be started"), e))?;
    let duration_us = job.title.duration_ns / 1000;
    if let Some(out) = run.child.stdout.take() {
        for line in std::io::BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tracker.cancel.load(Ordering::Relaxed) {
                let _ = run.child.kill();
                let _ = run.child.wait();
                return Ok(false);
            }
            if let Some(v) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<u64>().ok())
                && duration_us > 0
            {
                (tracker.on_progress)(1000, (v.min(duration_us) * 1000 / duration_us) as u32, None);
            }
        }
    }
    let status = run.child.wait().map_err(|e| failed(t!("ffmpeg could not be started"), e))?;
    if tracker.cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    if !status.success() {
        return Err(failed(t!("ffmpeg could not copy the tracks"), run.last_words()));
    }
    Ok(true)
}

/// "Mount": the image read-only; `then` gets the folder it shows in (to open it).
pub fn mount(image: PathBuf, then: impl FnOnce(PathBuf, &mut Window, &mut App) + 'static, window: &mut Window, cx: &mut App) {
    let work = cx.background_spawn(async move { leaf_sys::discimage::mount(&image) });
    window
        .spawn(cx, async move |cx| {
            let got = work.await;
            let _ = cx.update(|window, cx| match got {
                Ok(root) => then(root, window, cx),
                Err(e) => crate::prompt::notice(t!("The image could not be mounted"), e, window, cx),
            });
        })
        .detach();
}

/// "Eject": unmounts the image.
pub fn eject(image: PathBuf, window: &mut Window, cx: &mut App) {
    let work = cx.background_spawn(async move { leaf_sys::discimage::unmount(&image) });
    window
        .spawn(cx, async move |cx| {
            if let Err(e) = work.await {
                let _ = cx.update(|window, cx| crate::prompt::notice(t!("The image could not be ejected"), e, window, cx));
            }
        })
        .detach();
}

/// "Mount discs in drives" (Linux: a disc that was put in but not mounted).
pub fn mount_discs(window: &mut Window, cx: &mut App) {
    let work = cx.background_spawn(async move { leaf_sys::discimage::mount_discs() });
    window
        .spawn(cx, async move |cx| {
            let got = work.await;
            let _ = cx.update(|window, cx| match got {
                Ok(v) if v.is_empty() => crate::prompt::notice(t!("Mount discs in drives"), t!("There is no disc waiting to be mounted.").to_string(), window, cx),
                Ok(_) => {}
                Err(e) => crate::prompt::notice(t!("The disc could not be mounted"), e, window, cx),
            });
        })
        .detach();
}

/// Check scripts (`demo bluray`): an empty disc folder (`BDMV` with its three folders and no streams) in the config folder, opened
/// the way "Convert to video file" opens a selected disc.
pub fn demo(window: &mut Window, cx: &mut App) {
    let Some(dir) = crate::settings::app_dir().map(|d| d.join("demo").join("EXAMPLE_DISC")) else { return };
    for sub in ["BDMV/PLAYLIST", "BDMV/STREAM", "BDMV/CLIPINF"] {
        let _ = std::fs::create_dir_all(dir.join(sub));
    }
    let _ = std::fs::write(dir.join("BDMV/index.bdmv"), b"INDX0200");
    convert(vec![dir], window, cx);
}
