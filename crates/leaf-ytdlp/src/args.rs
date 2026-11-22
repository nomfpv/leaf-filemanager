//! The argument list for one download. Always a list (never a shell string), and the address is the last element, after `--`, so an
//! address that starts with `-` can never become an option.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Best,
    P1080,
    P720,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Audio {
    /// m4a or opus, as the site delivers it.
    AsDelivered,
    /// Converted (needs ffmpeg).
    Mp3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Video(Quality),
    Audio(Audio),
}

pub struct Job {
    pub url: String,
    /// leaf's own folder for this download's files while it runs (see [`crate::run::work_dir`]).
    pub work_dir: PathBuf,
    pub kind: Kind,
    pub subtitles: bool,
    /// All the videos of a list (else only the one the address names).
    pub playlist: bool,
    /// ffmpeg is on this PC: formats are merged and audio converted.
    pub ffmpeg: bool,
    /// A browser yt-dlp reads the sign-in from itself (`chrome`, `firefox`, ...): leaf never sees the cookies.
    pub cookies_from: Option<String>,
    /// Bytes per second; 0 = none.
    pub limit: u64,
    /// A proxy address for yt-dlp (leaf's VPN proxy): the traffic goes through it or the download does not start. It may hold a user
    /// name and password, so it is never put on the command line: [`proxy_env`] makes the environment of the yt-dlp process from it.
    pub proxy: Option<String>,
}

/// The stem of the files yt-dlp writes into the work folder (the real name comes from the title, after the download).
pub const SINGLE_STEM: &str = "leaf-video";
pub const LIST_STEM: &str = "leaf-%(playlist_index)03d";

/// Browsers yt-dlp can read the sign-in from; anything else is not passed on.
pub fn browser_ok(b: &str) -> bool {
    ["chrome", "edge", "brave", "firefox", "chromium", "opera", "vivaldi", "whale", "safari"].contains(&b)
}

/// A file name (without extension) from a video's title, through leaf's sanitiser; never empty.
pub fn stem_for_title(title: &str) -> String {
    let t: String = title.chars().map(|c| if matches!(c, '/' | '\\') { '_' } else { c }).collect();
    if t.trim().is_empty() {
        return "video".to_string();
    }
    let s = leaf_download::sanitize(&t);
    let s: String = s.chars().take(120).collect();
    s.trim_end_matches(|c: char| c == ' ' || c == '.').to_string()
}

fn height(q: Quality) -> Option<u32> {
    match q {
        Quality::Best => None,
        Quality::P1080 => Some(1080),
        Quality::P720 => Some(720),
    }
}

/// The format selector (`-f`) for a kind of download.
fn selector(kind: Kind, ffmpeg: bool) -> String {
    match kind {
        Kind::Video(q) => {
            let cap = height(q).map(|h| format!("[height<={h}]")).unwrap_or_default();
            if ffmpeg { format!("bv*{cap}+ba/b{cap}/b") } else { format!("b{cap}/b") }
        }
        Kind::Audio(Audio::Mp3) => "ba/b".to_string(),
        Kind::Audio(Audio::AsDelivered) => "ba[ext=m4a]/ba/b".to_string(),
    }
}

pub fn args(job: &Job) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut put = |s: &str| a.push(s.into());
    for s in ["--ignore-config", "--quiet", "--progress", "--newline", "--no-warnings", "--no-mtime", "--continue", "--socket-timeout", "30"] {
        put(s);
    }
    put("--progress-template");
    put("download:LEAFP|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s");
    put("--progress-template");
    put("postprocess:LEAFPP|%(progress.postprocessor)s|%(progress.status)s");
    put("--print");
    put("after_move:LEAFTITLE|%(title)s");
    put("--print");
    put("after_move:LEAFFILE|%(filepath)s");
    put("-f");
    put(&selector(job.kind, job.ffmpeg));
    match job.kind {
        Kind::Video(_) if job.ffmpeg => {
            put("--merge-output-format");
            put("mp4/mkv");
        }
        Kind::Audio(Audio::Mp3) if job.ffmpeg => {
            for s in ["-x", "--audio-format", "mp3", "--audio-quality", "0"] {
                put(s);
            }
        }
        _ => {}
    }
    put(if job.playlist { "--yes-playlist" } else { "--no-playlist" });
    if job.subtitles {
        for s in ["--write-subs", "--write-auto-subs", "--sub-langs", "en.*,-live_chat"] {
            put(s);
        }
        if job.ffmpeg {
            put("--convert-subs");
            put("srt");
        }
    }
    if job.limit > 0 {
        put("--limit-rate");
        put(&job.limit.to_string());
    }
    if let Some(b) = job.cookies_from.as_deref().filter(|b| browser_ok(b)) {
        put("--cookies-from-browser");
        put(b);
    }
    // the template: a `%` in the folder's own name is doubled, or yt-dlp would read it as a field
    let stem = if job.playlist { LIST_STEM } else { SINGLE_STEM };
    let mut out = OsString::new();
    out.push(job.work_dir.join("").as_os_str().to_string_lossy().replace('%', "%%"));
    out.push(format!("{stem}.%(ext)s"));
    a.push("-o".into());
    a.push(out);
    a.push("--".into());
    a.push(job.url.clone().into());
    a
}

/// The environment variables that send yt-dlp through `proxy` (it reads them when it has no `--proxy`), set on that one process only:
/// where other programs of the user can list command lines, they do not see them. Both spellings, and no exceptions, so that nothing
/// goes direct because the system's own settings say so.
pub fn proxy_env(proxy: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for name in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        v.push((name.to_string(), proxy.to_string()));
    }
    for name in ["NO_PROXY", "no_proxy"] {
        v.push((name.to_string(), String::new()));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(url: &str) -> Job {
        Job { url: url.into(), work_dir: PathBuf::from("C:/dl/.leaf-video-1"), kind: Kind::Video(Quality::P1080), subtitles: true, playlist: false, ffmpeg: true, cookies_from: Some("firefox".into()), limit: 1 << 20, proxy: None }
    }

    #[test]
    fn the_address_is_the_last_argument_after_double_dash() {
        for url in ["https://example.com/watch?v=1", "--exec=calc", "-o/etc/passwd"] {
            let a = args(&job(url));
            let n = a.len();
            assert_eq!(a[n - 1], OsString::from(url));
            assert_eq!(a[n - 2], OsString::from("--"));
            // nowhere else: no option can carry it, and no argument is a joined command line
            assert_eq!(a.iter().filter(|x| x.to_string_lossy().contains(url)).count(), 1);
        }
    }

    #[test]
    fn an_unknown_browser_is_not_passed_on() {
        let mut j = job("https://example.com/");
        j.cookies_from = Some("x --exec calc".into());
        assert!(!args(&j).iter().any(|a| a == "--cookies-from-browser"));
    }

    #[test]
    fn title_becomes_a_safe_file_name() {
        assert_eq!(stem_for_title("AC/DC: Back In Black?"), "AC_DC_ Back In Black_");
        assert_eq!(stem_for_title("  "), "video");
        assert_eq!(stem_for_title("CON"), "_CON");
        assert_eq!(stem_for_title("..."), "download");
        assert!(stem_for_title(&"x".repeat(500)).chars().count() <= 120);
    }
}
