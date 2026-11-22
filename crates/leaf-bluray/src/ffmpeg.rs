//! The optional helper: `ffmpeg`, when it is on PATH, re-encodes to a smaller file (leaf's own remux is fed into its standard input
//! as Matroska) and copies the tracks leaf cannot remux itself. Arguments are always a list, never a shell string; on Windows no
//! console window opens.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// `ffmpeg` on PATH.
pub fn find() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// "Smaller file (re-encode)".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// H.265, quality 22, every track kept (MKV).
    Hevc,
    /// H.264, quality 20, every track kept (MKV): plays everywhere.
    H264,
    /// H.264 up to 1080p, stereo AAC, no subtitles (MP4): for phones and tablets.
    Phone,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Hevc, Preset::H264, Preset::Phone];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Hevc => "Smaller: H.265",
            Preset::H264 => "Smaller: H.264",
            Preset::Phone => "MP4 for phones",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Preset::Phone => "mp4",
            _ => "mkv",
        }
    }
}

fn os(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

/// Re-encode Matroska read from standard input into `out`.
pub fn encode_args(preset: Preset, out: &Path) -> Vec<OsString> {
    let mut a = os(&["-hide_banner", "-loglevel", "error", "-f", "matroska", "-i", "pipe:0"]);
    match preset {
        Preset::Hevc => a.extend(os(&["-map", "0", "-c:v", "libx265", "-crf", "22", "-preset", "medium", "-c:a", "copy", "-c:s", "copy", "-f", "matroska"])),
        Preset::H264 => a.extend(os(&["-map", "0", "-c:v", "libx264", "-crf", "20", "-preset", "medium", "-c:a", "copy", "-c:s", "copy", "-f", "matroska"])),
        Preset::Phone => a.extend(os(&[
            "-map", "0:v:0", "-map", "0:a:0?", "-c:v", "libx264", "-crf", "22", "-preset", "medium", "-profile:v", "high", "-pix_fmt", "yuv420p",
            "-vf", "scale=-2:'min(1080,ih)'", "-c:a", "aac", "-b:a", "160k", "-ac", "2", "-sn", "-movflags", "+faststart", "-f", "mp4",
        ])),
    }
    a.push(out.as_os_str().to_owned());
    a
}

/// Copy the tracks `pids` of the clips (joined) into Matroska at `out`, without re-encoding: for the stream types leaf does not
/// remux itself. Progress comes on standard output (`out_time_us=`).
pub fn copy_args(clips: &[PathBuf], pids: &[u16], out: &Path) -> Vec<OsString> {
    let mut a = os(&["-hide_banner", "-loglevel", "error", "-nostdin", "-progress", "pipe:1", "-fflags", "+genpts"]);
    let mut input = OsString::from("concat:");
    for (k, c) in clips.iter().enumerate() {
        if k > 0 {
            input.push("|");
        }
        input.push(c.as_os_str());
    }
    a.push("-i".into());
    a.push(input);
    for p in pids {
        a.push("-map".into());
        a.push(format!("0:i:{p}").into());
    }
    a.extend(os(&["-c", "copy", "-f", "matroska"]));
    a.push(out.as_os_str().to_owned());
    a
}

/// A running ffmpeg and the last lines it wrote to standard error (its own words, for the details line of a failure).
pub struct Running {
    pub child: Child,
    pub errors: Arc<Mutex<Vec<String>>>,
}

pub fn spawn(ffmpeg: &Path, args: &[OsString], stdin: bool, stdout: bool) -> std::io::Result<Running> {
    let mut c = Command::new(ffmpeg);
    c.args(args)
        .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(if stdout { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let mut child = c.spawn()?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    if let Some(mut e) = child.stderr.take() {
        let keep = errors.clone();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            let mut k = keep.lock().unwrap_or_else(|p| p.into_inner());
            *k = s.lines().rev().take(6).map(str::to_string).collect::<Vec<_>>().into_iter().rev().collect();
        });
    }
    Ok(Running { child, errors })
}

impl Running {
    pub fn last_words(&self) -> String {
        self.errors.lock().map(|e| e.join(" / ")).unwrap_or_default()
    }
}
