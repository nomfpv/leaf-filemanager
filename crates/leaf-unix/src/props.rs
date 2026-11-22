//! Linux backend of rf_win::props. Status: REAL for the read side; editing is not supported
//!
//! `media_props`: for a picture the portable decoders' size (`Dimensions`); for audio and video what `ffprobe` (ffmpeg) says, when it is
//! installed: duration, resolution, frame rate, bit rates, sample rate, channels, codec names and the common tags. Without `ffprobe`
//! an audio or video file simply has no facts. Editing the descriptive fields is not supported here: `editable` answers `None` (the
//! page shows no edit boxes) and `set_editable` says so.

use serde_json::Value;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn media_props(path: &Path) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Ok((w, h)) = image::image_dimensions(path) {
        out.push(("Dimensions", format!("{w} × {h}")));
        return out;
    }
    if let Some(json) = ffprobe_json(path) {
        out = parse_ffprobe(&json);
    }
    out
}

/// The JSON `ffprobe` prints for `path`; `None` when it is not installed, fails, or takes more than 8 seconds (it is then killed).
fn ffprobe_json(path: &Path) -> Option<String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "--"]).arg(path).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let (mut child, tree) = leaf_proc::spawn_helper_tree(&mut cmd).ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.by_ref().take(4 << 20).read_to_string(&mut s);
        let _ = tx.send(s);
    });
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(s) => {
            let ok = child.wait().map(|st| st.success()).unwrap_or(false);
            (ok && !s.is_empty()).then_some(s)
        }
        Err(_) => {
            leaf_proc::kill_tree(&mut child, &tree);
            let _ = child.wait();
            None
        }
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn clock(secs: f64) -> String {
    let t = secs.round().max(0.0) as u64;
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t % 3600 / 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) }
}

fn rate(bits: f64) -> String {
    if bits >= 1_000_000.0 { format!("{:.1} Mbps", bits / 1e6) } else { format!("{} kbps", (bits / 1000.0).round() as u64) }
}

/// "30000/1001" as 29.97.
fn frame_rate(s: &str) -> Option<String> {
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    if b == 0.0 || a == 0.0 {
        return None;
    }
    let f = a / b;
    Some(if (f - f.round()).abs() < 0.005 { format!("{} fps", f.round() as u64) } else { format!("{f:.2} fps") })
}

fn tag(v: &Value, name: &str) -> Option<String> {
    let t = v.get("tags")?.as_object()?;
    t.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The facts of `ffprobe -print_format json -show_format -show_streams`, in the order the Windows list has them.
fn parse_ffprobe(json: &str) -> Vec<(&'static str, String)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let streams = v.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let kind = |k: &str| streams.iter().find(|s| s.get("codec_type").and_then(|c| c.as_str()) == Some(k));
    // a cover picture is a "video" stream too: it has no frame rate worth showing, and is not the movie
    let video = streams.iter().find(|s| {
        s.get("codec_type").and_then(|c| c.as_str()) == Some("video") && s.pointer("/disposition/attached_pic").and_then(|d| d.as_i64()) != Some(1)
    });
    let audio = kind("audio");
    let format = v.get("format");
    let mut out = Vec::new();
    let duration = format.and_then(|f| f.get("duration")).and_then(num).or_else(|| video.or(audio).and_then(|s| s.get("duration")).and_then(num));
    if let Some(d) = duration.filter(|d| *d > 0.0) {
        out.push(("Duration", clock(d)));
    }
    if let Some(s) = video {
        if let (Some(w), Some(h)) = (s.get("width").and_then(num), s.get("height").and_then(num)) {
            out.push(("Resolution", format!("{} × {}", w as u64, h as u64)));
        }
        if let Some(f) = s.get("avg_frame_rate").and_then(|f| f.as_str()).and_then(frame_rate) {
            out.push(("Frame rate", f));
        }
        if let Some(b) = s.get("bit_rate").and_then(num) {
            out.push(("Video bit rate", rate(b)));
        }
        if let Some(c) = s.get("codec_long_name").or_else(|| s.get("codec_name")).and_then(|c| c.as_str()) {
            out.push(("Video codec", c.to_string()));
        }
    }
    if let Some(b) = format.and_then(|f| f.get("bit_rate")).and_then(num) {
        out.push(("Total bit rate", rate(b)));
    }
    if let Some(s) = audio {
        if let Some(b) = s.get("bit_rate").and_then(num) {
            out.push(("Audio bit rate", rate(b)));
        }
        if let Some(r) = s.get("sample_rate").and_then(num) {
            out.push(("Sample rate", format!("{:.1} kHz", r / 1000.0)));
        }
        if let Some(c) = s.get("channels").and_then(num) {
            out.push(("Channels", (c as u64).to_string()));
        }
        if let Some(c) = s.get("codec_long_name").or_else(|| s.get("codec_name")).and_then(|c| c.as_str()) {
            out.push(("Audio codec", c.to_string()));
        }
    }
    if let Some(f) = format {
        for (label, name) in [("Artist", "artist"), ("Album", "album"), ("Title", "title")] {
            if let Some(t) = tag(f, name) {
                out.push((label, t));
            }
        }
    }
    out
}

// TYPE-COPY(rf-win src/props.rs:79)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Editable {
    pub title: String,
    pub subject: String,
    pub tags: String,
    pub comment: String,
    pub authors: String,
}

/// No editable descriptive fields here: the page shows none.
pub fn editable(_path: &Path) -> Option<Editable> {
    None
}

pub fn set_editable(_path: &Path, _old: &Editable, _new: &Editable) -> anyhow::Result<()> {
    Err(anyhow::anyhow!("Editing a file's description is not supported on this system yet."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffprobe_json_becomes_the_facts_list() {
        let sample = r#"{"streams":[
          {"codec_type":"video","codec_name":"h264","codec_long_name":"H.264 / AVC","width":1920,"height":1080,"avg_frame_rate":"30000/1001","bit_rate":"4500000","disposition":{"attached_pic":0}},
          {"codec_type":"audio","codec_name":"aac","codec_long_name":"AAC (Advanced Audio Coding)","sample_rate":"48000","channels":2,"bit_rate":"128000"}],
          "format":{"duration":"3725.4","bit_rate":"4700000","tags":{"TITLE":"Demo","artist":"Me"}}}"#;
        let m: std::collections::HashMap<_, _> = parse_ffprobe(sample).into_iter().collect();
        assert_eq!(m["Duration"], "1:02:05");
        assert_eq!(m["Resolution"], "1920 × 1080");
        assert_eq!(m["Frame rate"], "29.97 fps");
        assert_eq!(m["Video bit rate"], "4.5 Mbps");
        assert_eq!(m["Audio bit rate"], "128 kbps");
        assert_eq!(m["Sample rate"], "48.0 kHz");
        assert_eq!(m["Channels"], "2");
        assert_eq!(m["Video codec"], "H.264 / AVC");
        assert_eq!(m["Artist"], "Me");
        assert_eq!(m["Title"], "Demo");
        assert!(parse_ffprobe("not json").is_empty());
    }
}
