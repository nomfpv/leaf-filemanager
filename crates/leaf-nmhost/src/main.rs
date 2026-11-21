//! leaf-nmhost: the browser's native-messaging host for leaf's extension (`extension/`). The browser starts it for one message
//! (`runtime.sendNativeMessage`): a 4-byte length in native byte order, then that many bytes of JSON. It hands the message to the
//! running leaf through the hand-over channel (`leaf_sys::handover::ask`), as one line `\u{1}capture:` + the JSON, writes one answer
//! frame and exits. When leaf is not running it starts it and asks again.
//!
//! Browser to host: `{"v":1,"type":"download","url":..,"referrer":..,"size":..,"headers":{..}}` or `{"v":1,"type":"ping"}`.
//! Host to browser: `{"v":1,"ok":true}`, or `{"v":1,"ok":false,"why":"not-running|leaving|refused|too-large|bad-message|failed"}`.
//! The extension cancels the browser's own download only on `ok:true`.
//!
//! The message may carry the browser's cookies and sign-in header: nothing here writes it anywhere but to leaf's pipe (no log, no
//! file, no error text), and stdout carries frames only.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// leaf's side: `rf-app/src/capture.rs`.
const PREFIX: &str = "\u{1}capture:";
/// The browser allows 1 MB to the host and leaf cuts a request line at 64 KiB.
const MAX_IN: usize = 1 << 20;
const MAX_LINE: usize = 60 << 10;
/// Taking over a download is quick (leaf only queues it).
const WAIT: Duration = Duration::from_secs(20);
/// How long a leaf that was just started is given to come up.
const START_WAIT: Duration = Duration::from_secs(25);

fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let len = u32::from_ne_bytes(len) as usize;
    if len == 0 || len > MAX_IN {
        return None;
    }
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_frame(out: &mut impl Write, v: &Value) {
    let bytes = v.to_string().into_bytes();
    let _ = out.write_all(&(bytes.len() as u32).to_ne_bytes());
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

fn answer(ok: bool, why: &str) -> Value {
    if ok { json!({ "v": 1, "ok": true }) } else { json!({ "v": 1, "ok": false, "why": why }) }
}

/// The handover's error text, as a short word for the extension.
fn why_of(err: &str) -> &'static str {
    match err {
        "leaf is not running" => "not-running",
        "leaf is closing" => "leaving",
        "leaf could not do it" => "refused",
        _ => "failed",
    }
}

/// Starts leaf (the program next to this one), out of the browser's job so it outlives this process.
fn start_leaf() -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    let leaf = me.with_file_name(leaf_sys::exe_name(leaf_sys::APP_EXE_STEM));
    if !leaf.is_file() {
        return false;
    }
    let mut cmd = std::process::Command::new(&leaf);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(dir) = leaf.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // (a browser whose job does not allow breaking away refuses the flag: then without it)
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return true;
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().is_ok()
}

/// Hands `line` to leaf; starts leaf first when `start` and it is not running.
fn ask(line: &str, start: bool) -> Result<(), String> {
    match leaf_sys::handover::ask(line, WAIT) {
        Err(e) if e == "leaf is not running" && start => {
            if !start_leaf() {
                return Err(e);
            }
            let begun = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(300));
                match leaf_sys::handover::ask(line, WAIT) {
                    Err(e) if e == "leaf is not running" && begun.elapsed() < START_WAIT => continue,
                    r => return r.map(drop),
                }
            }
        }
        r => r.map(drop),
    }
}

fn handle(frame: &[u8]) -> Value {
    let Ok(msg) = serde_json::from_slice::<Value>(frame) else { return answer(false, "bad-message") };
    let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        // "is leaf there": never starts it
        "ping" | "hello" => match ask(&format!("{PREFIX}{}", json!({ "v": 1, "type": "ping" })), false) {
            Ok(()) => answer(true, ""),
            Err(e) => answer(false, why_of(&e)),
        },
        "download" => {
            let url = msg.get("url").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return answer(false, "bad-message");
            }
            // (one line: serde_json writes no line break, and escapes any in the strings)
            let line = format!("{PREFIX}{msg}");
            if line.len() > MAX_LINE || line.contains(['\r', '\n']) {
                return answer(false, "too-large");
            }
            match ask(&line, true) {
                Ok(()) => answer(true, ""),
                Err(e) => answer(false, why_of(&e)),
            }
        }
        _ => answer(false, "bad-message"),
    }
}

fn main() {
    // (the browser passes the extension's origin, or the manifest path and the extension id: the browser itself only starts this
    // host for the ids its manifest allows, so the arguments are not needed)
    let mut input = std::io::stdin().lock();
    let mut out = std::io::stdout().lock();
    if let Some(frame) = read_frame(&mut input) {
        write_frame(&mut out, &handle(&frame));
    }
}
