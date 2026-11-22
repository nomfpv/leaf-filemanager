//! Videos and audio from web pages (YouTube, TikTok, Medal and the other sites yt-dlp knows) through the separate program
//! `yt-dlp`, the same pattern as rclone and ffmpeg in leaf: found on the PATH (or at a path the user set), never shipped or installed
//! by leaf. leaf starts it with argument lists (never a shell string; the address always follows `--`), asks it what a page holds
//! (`--dump-single-json`), and runs the download while reading its progress lines. No site-specific code lives here: which sites
//! work is yt-dlp's business (it is updated weekly because sites break weekly). Blocking calls only: run them on a worker thread.
//!
//! DRM-protected services are refused (`Error::Drm`), never worked around.

mod args;
mod probe;
mod run;

pub use args::{Audio, Job, Kind, Quality, args, browser_ok, stem_for_title};
pub use probe::{Media, Playlist, Probe, is_drm_host, probe, probe_candidate};
pub use run::{Event, Progress, RunCtl, RunEnd, WORK_PREFIX, key_for, remove_work_dir, run, work_dir};

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Error {
    /// A service that protects its streams (DRM): refused with a fixed message.
    Drm,
    /// yt-dlp does not handle this address (not a media page).
    Unsupported,
    /// yt-dlp did not answer in time.
    TimedOut,
    /// yt-dlp could not be started.
    Start(String),
    /// yt-dlp's own error line (its words: shown only as the details line).
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Drm => write!(f, "DRM"),
            Error::Unsupported => write!(f, "unsupported address"),
            Error::TimedOut => write!(f, "no answer in time"),
            Error::Start(s) | Error::Failed(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

/// The words for a refused service.
pub const DRM_TEXT: &str = "This service protects its streams; leaf does not download from it.";

fn exe_names(stem: &str) -> Vec<String> {
    if cfg!(windows) { vec![format!("{stem}.exe")] } else { vec![stem.to_string()] }
}

fn on_path(stem: &str) -> Option<PathBuf> {
    let names = exe_names(stem);
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
}

/// The `yt-dlp` program: the path the user set when it is a file, else the first one on the PATH.
pub fn find_exe(custom: &str) -> Option<PathBuf> {
    let custom = custom.trim();
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        return p.is_file().then_some(p);
    }
    on_path("yt-dlp")
}

/// `ffmpeg` on the PATH: with it formats are merged and audio converted; without it a single-file format is chosen.
pub fn find_ffmpeg() -> Option<PathBuf> {
    on_path("ffmpeg")
}

/// What to tell a user who has not installed yt-dlp (the official way, one line).
pub fn install_hint() -> &'static str {
    if cfg!(windows) { "winget install yt-dlp" } else if cfg!(target_os = "macos") { "brew install yt-dlp" } else { "install the yt-dlp package of your distribution (for example: sudo apt install yt-dlp)" }
}

pub(crate) fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.stdin(Stdio::null()).env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c
}

/// Runs `c` for at most `limit`; stdout and stderr (each capped) when it ends, `None` for the status when it was killed.
pub(crate) fn run_limited(mut c: Command, limit: Duration, cap: usize) -> Result<(Option<i32>, String, String), Error> {
    use std::io::Read;
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| Error::Start(e.to_string()))?;
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut r).take(cap as u64).read_to_end(&mut buf);
            // (anything past the cap is drained so the program is not blocked on a full pipe)
            let _ = std::io::copy(&mut r, &mut std::io::sink());
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = child.stdout.take().map(|s| read(Box::new(s)));
    let err = child.stderr.take().map(|s| read(Box::new(s)));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s.code().unwrap_or(-1)),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                run::kill_tree(&mut child);
                break None;
            }
        }
    };
    let out = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = err.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok((status, out, err))
}

/// yt-dlp's version (`2025.01.01`), when it runs.
pub fn version(exe: &Path) -> Option<String> {
    let mut c = command(exe);
    c.arg("--version");
    let (code, out, _) = run_limited(c, Duration::from_secs(15), 4096).ok()?;
    (code == Some(0)).then(|| out.trim().to_string()).filter(|v| !v.is_empty())
}

/// Whether `yt-dlp -U` can update this copy: the program that yt-dlp publishes itself (a single file of many megabytes). A copy a
/// package manager or pip installed (a launcher script, a few kilobytes) is updated by that tool, not by itself.
pub fn can_self_update(exe: &Path) -> bool {
    std::fs::metadata(exe).is_ok_and(|m| m.len() > 5 << 20)
}

/// `yt-dlp -U`: its own words about what happened (the last lines), and whether it ended well.
pub fn self_update(exe: &Path) -> (bool, String) {
    let mut c = command(exe);
    c.arg("-U");
    match run_limited(c, Duration::from_secs(180), 64 << 10) {
        Ok((code, out, err)) => {
            let text: Vec<&str> = out.lines().chain(err.lines()).map(str::trim).filter(|l| !l.is_empty()).collect();
            let tail = text[text.len().saturating_sub(3)..].join("\n");
            (code == Some(0), tail)
        }
        Err(e) => (false, e.to_string()),
    }
}
