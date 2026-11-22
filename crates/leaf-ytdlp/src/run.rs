//! Running one download: yt-dlp's output is read line by line (progress, finished files, errors); pause stops the process tree and
//! starts it again with `--continue` on resume; cancel stops it and the caller removes the work folder.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use crate::{Error, command};

/// What the caller can change while it runs.
#[derive(Clone, Default)]
pub struct RunCtl {
    pub cancel: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    /// Environment variables for the yt-dlp process only (the proxy with its password: not on the command line).
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Progress {
    pub done: Option<u64>,
    /// The size, or yt-dlp's estimate of it.
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum Line {
    Progress(Progress),
    /// A post-processing step (merging, converting) is running.
    Processing(String),
    Title(String),
    File(String),
    Error(String),
    Other,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
}

/// One output line of yt-dlp, as the argument list of [`crate::args`] makes it print.
pub fn parse_line(line: &str) -> Line {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix("LEAFP|") {
        let f: Vec<&str> = rest.split('|').collect();
        let get = |i: usize| f.get(i).copied().and_then(num);
        let total = get(1).or_else(|| get(2)).map(|n| n as u64);
        return Line::Progress(Progress { done: get(0).map(|n| n as u64), total, speed: get(3), eta: get(4).map(|n| n as u64) });
    }
    if let Some(rest) = line.strip_prefix("LEAFPP|") {
        let mut f = rest.split('|');
        let name = f.next().unwrap_or("").to_string();
        return if f.next() == Some("finished") { Line::Other } else { Line::Processing(name) };
    }
    if let Some(t) = line.strip_prefix("LEAFTITLE|") {
        return Line::Title(t.to_string());
    }
    if let Some(p) = line.strip_prefix("LEAFFILE|") {
        return Line::File(p.to_string());
    }
    if let Some(e) = line.strip_prefix("ERROR:") {
        return Line::Error(e.trim().to_string());
    }
    Line::Other
}

#[derive(Debug)]
pub enum Event {
    /// Progress; the second is which stream of the video it is (the picture and the sound come as two).
    Progress(Progress, u32),
    Processing(String),
    Paused,
    Resumed,
    /// A video is finished: its title and the file in the work folder.
    Item { title: String, file: PathBuf },
}

#[derive(Debug, Default)]
pub struct RunEnd {
    pub code: Option<i32>,
    pub cancelled: bool,
    /// yt-dlp's error lines (the last few).
    pub errors: Vec<String>,
    pub items: Vec<(String, PathBuf)>,
}

/// Stops the process and everything it started (ffmpeg).
pub(crate) fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill").args(["/PID", &child.id().to_string(), "/T", "/F"]).creation_flags(0x0800_0000).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(unix)]
    {
        // (the process leads its own group: see `command`)
        let _ = std::process::Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn pump(r: impl std::io::Read + Send + 'static, err: bool, tx: std::sync::mpsc::Sender<(bool, String)>) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
            if tx.send((err, String::from_utf8_lossy(&buf).into_owned())).is_err() {
                return;
            }
            buf.clear();
        }
    });
}

fn wait_while_paused(ctl: &RunCtl) -> bool {
    while ctl.paused.load(Ordering::Relaxed) && !ctl.cancel.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(150));
    }
    !ctl.cancel.load(Ordering::Relaxed)
}

/// Runs `exe` with `args` to its end. Blocks; `on` is called on this thread.
pub fn run(exe: &Path, args: &[OsString], ctl: &RunCtl, on: &mut dyn FnMut(Event)) -> Result<RunEnd, Error> {
    let mut end = RunEnd::default();
    let mut title: Option<String> = None;
    let (mut stream, mut last_done) = (1u32, 0u64);
    'spawn: loop {
        if ctl.cancel.load(Ordering::Relaxed) {
            end.cancelled = true;
            return Ok(end);
        }
        let mut c = command(exe);
        c.args(args).envs(ctl.env.iter().map(|(k, v)| (k, v))).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = leaf_proc::spawn_helper(&mut c).map_err(|e| Error::Start(e.to_string()))?;
        let (tx, rx) = channel();
        if let Some(o) = child.stdout.take() {
            pump(o, false, tx.clone());
        }
        if let Some(e) = child.stderr.take() {
            pump(e, true, tx.clone());
        }
        drop(tx);
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok((_, text)) => match parse_line(&text) {
                    Line::Progress(p) => {
                        // (a smaller count than before: the next stream began)
                        if let Some(d) = p.done {
                            if d + 1024 < last_done {
                                stream += 1;
                            }
                            last_done = d;
                        }
                        on(Event::Progress(p, stream));
                    }
                    Line::Processing(n) => on(Event::Processing(n)),
                    Line::Title(t) => title = Some(t),
                    Line::File(f) => {
                        let file = PathBuf::from(f.trim());
                        if !file.as_os_str().is_empty() && !end.items.iter().any(|(_, p)| *p == file) {
                            let t = title.take().unwrap_or_default();
                            end.items.push((t.clone(), file.clone()));
                            on(Event::Item { title: t, file });
                        }
                        (stream, last_done) = (1, 0);
                    }
                    Line::Error(e) => {
                        if !end.errors.contains(&e) {
                            end.errors.push(e);
                            if end.errors.len() > 10 {
                                end.errors.remove(0);
                            }
                        }
                    }
                    Line::Other => {}
                },
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    end.code = child.wait().ok().and_then(|s| s.code());
                    return Ok(end);
                }
            }
            if ctl.cancel.load(Ordering::Relaxed) {
                kill_tree(&mut child);
                end.cancelled = true;
                return Ok(end);
            }
            if ctl.paused.load(Ordering::Relaxed) {
                kill_tree(&mut child);
                on(Event::Paused);
                if !wait_while_paused(ctl) {
                    end.cancelled = true;
                    return Ok(end);
                }
                on(Event::Resumed);
                (stream, last_done) = (1, 0);
                continue 'spawn;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the work folder

/// The folders leaf makes for a download in progress start with this; only such a folder is ever removed.
pub const WORK_PREFIX: &str = ".leaf-video-";

/// A short stable key for a download: the same address and choices find the same work folder (Retry carries on from the parts).
pub fn key_for(url: &str, choices: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.bytes().chain([0]).chain(choices.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn work_dir(dest: &Path, key: &str) -> PathBuf {
    dest.join(format!("{WORK_PREFIX}{key}"))
}

/// Removes a work folder and what is in it. Refuses anything that is not a folder leaf made (a name with [`WORK_PREFIX`] and a hex
/// key, not a link).
pub fn remove_work_dir(dir: &Path) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ok = name.strip_prefix(WORK_PREFIX).is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_hexdigit()));
    if !ok || !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    std::fs::remove_dir_all(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_of_made_up_output() {
        assert_eq!(parse_line("LEAFP|1048576|10485760|NA|524288.5|18\n"), Line::Progress(Progress { done: Some(1048576), total: Some(10485760), speed: Some(524288.5), eta: Some(18) }));
        // (no exact size: the estimate; no speed yet)
        assert_eq!(parse_line("LEAFP|0|NA|2048000.7|None|None"), Line::Progress(Progress { done: Some(0), total: Some(2048000), speed: None, eta: None }));
        assert_eq!(parse_line("LEAFPP|Merger|started"), Line::Processing("Merger".into()));
        assert_eq!(parse_line("LEAFPP|Merger|finished"), Line::Other);
        assert_eq!(parse_line("LEAFTITLE|A | B"), Line::Title("A | B".into()));
        assert_eq!(parse_line("LEAFFILE|C:/x/leaf-video.mp4\r\n"), Line::File("C:/x/leaf-video.mp4".into()));
        assert_eq!(parse_line("ERROR: [youtube] abc: This video is DRM protected"), Line::Error("[youtube] abc: This video is DRM protected".into()));
        assert_eq!(parse_line("[info] something"), Line::Other);
    }

    #[test]
    fn only_a_leaf_work_folder_is_removed() {
        let base = std::env::temp_dir().join(format!("leaf-ytdlp-test-{}", std::process::id()));
        let work = work_dir(&base, &key_for("https://example.com/v", "video"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("leaf-video.mp4.part"), b"x").unwrap();
        let other = base.join("Holiday");
        std::fs::create_dir_all(&other).unwrap();
        assert!(!remove_work_dir(&other) && other.is_dir());
        assert!(!remove_work_dir(&base.join(".leaf-video-notahexkey")));
        assert!(remove_work_dir(&work) && !work.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
