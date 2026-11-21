//! Linux backend of rf_win::clipboard. Status: REAL. leaf's own store (paths plus copy or cut, with a sequence number) is what a
//! paste inside leaf uses; the desktop's clipboard carries the files too, through the clipboard tools when they are installed (GPUI's
//! clipboard offers and reads text only):
//! - Wayland: `wl-copy --type text/uri-list` (one type per offer: Dolphin and Nautilus paste the files, a cut is pasted there as a
//!   copy) and `wl-paste --list-types` / `--type <mime>` to read what another program copied (`text/uri-list`, KDE's
//!   `application/x-kde-cutselection`, GNOME's `x-special/gnome-copied-files` for copy or cut);
//! - X11: the same with `xclip -selection clipboard`.
//!
//! Every tool is started with an argument list (no shell) and a time limit. Without the tools the store alone answers (files copied in
//! leaf reach other programs as text paths, written by the app through GPUI).

use crate::Owner;
use leaf_common::fileops::Transfer;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

static SEQ: AtomicU32 = AtomicU32::new(1);
static STORE: Mutex<Option<(Vec<PathBuf>, Transfer)>> = Mutex::new(None);
/// The desktop's clipboard was given our files (a tool took them): when it no longer holds files, someone copied over them.
static OFFERED: AtomicBool = AtomicBool::new(false);

const URI_LIST: &str = "text/uri-list";
const KDE_CUT: &str = "application/x-kde-cutselection";
const GNOME_FILES: &str = "x-special/gnome-copied-files";

fn bump() {
    SEQ.fetch_add(1, Ordering::Relaxed);
}

fn store() -> std::sync::MutexGuard<'static, Option<(Vec<PathBuf>, Transfer)>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn clear(_owner: Owner) -> anyhow::Result<()> {
    *store() = None;
    OFFERED.store(false, Ordering::Relaxed);
    bump();
    Ok(())
}

pub fn sequence() -> u32 {
    SEQ.load(Ordering::Relaxed)
}

pub fn set_files(_owner: Owner, paths: &[PathBuf], kind: Transfer) -> anyhow::Result<()> {
    *store() = Some((paths.to_vec(), kind));
    bump();
    let offered = !paths.is_empty() && offer(&uri_list(paths)).map_err(|e| log::info!("clipboard: files not offered to other programs: {e:#}")).is_ok();
    OFFERED.store(offered, Ordering::Relaxed);
    Ok(())
}

pub fn has_files() -> bool {
    current().is_some_and(|(p, _)| !p.is_empty())
}

pub fn can_paste() -> bool {
    has_files()
}

pub fn get_files(_owner: Owner) -> anyhow::Result<Option<(Vec<PathBuf>, Transfer)>> {
    Ok(current().filter(|(p, _)| !p.is_empty()))
}

/// What a paste takes: the files another program put on the desktop's clipboard, else our own store (its cut stays a cut when the
/// desktop holds exactly our files). When we had offered our files and the desktop now holds something else, there is nothing.
fn current() -> Option<(Vec<PathBuf>, Transfer)> {
    let own = store().clone();
    match read_external() {
        Some(External::Files(paths, kind)) => match own {
            Some((p, k)) if p == paths => Some((p, k)),
            _ => Some((paths, kind)),
        },
        Some(External::Other) if OFFERED.load(Ordering::Relaxed) => None,
        _ => own,
    }
}

enum External {
    Files(Vec<PathBuf>, Transfer),
    /// The clipboard holds something, but no files.
    Other,
}

/// Which tool set talks to the desktop's clipboard here.
#[derive(Clone, Copy)]
enum Tools {
    Wayland,
    X11,
}

fn tools() -> Option<Tools> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && crate::shell::which("wl-copy").is_some() && crate::shell::which("wl-paste").is_some() {
        Some(Tools::Wayland)
    } else if std::env::var_os("DISPLAY").is_some() && crate::shell::which("xclip").is_some() {
        Some(Tools::X11)
    } else {
        None
    }
}

/// Puts `text` on the clipboard as `text/uri-list`.
fn offer(text: &str) -> anyhow::Result<()> {
    let args: Vec<&str> = match tools().ok_or_else(|| anyhow::anyhow!("no clipboard tool (wl-copy or xclip)"))? {
        Tools::Wayland => vec!["wl-copy", "--type", URI_LIST],
        Tools::X11 => vec!["xclip", "-selection", "clipboard", "-t", URI_LIST, "-i"],
    };
    // (both tools read the text, then leave a child of their own that serves it: the one started here ends at once)
    let out = run(&args, Some(text.as_bytes()), Duration::from_secs(2))?;
    anyhow::ensure!(out.0, "{} failed", args[0]);
    Ok(())
}

/// What another program put on the clipboard (`None`: no tool, or it could not be asked).
fn read_external() -> Option<External> {
    let t = tools()?;
    let types = match t {
        Tools::Wayland => run(&["wl-paste", "--list-types"], None, Duration::from_millis(600)).ok()?,
        Tools::X11 => run(&["xclip", "-selection", "clipboard", "-t", "TARGETS", "-o"], None, Duration::from_millis(600)).ok()?,
    };
    if !types.0 {
        // (wl-paste: "Nothing is copied")
        return Some(External::Other);
    }
    let types: Vec<String> = String::from_utf8_lossy(&types.1).lines().map(|l| l.trim().to_string()).collect();
    let has = |m: &str| types.iter().any(|t| t == m);
    let get = |m: &str| -> Option<String> {
        let args: Vec<&str> = match t {
            Tools::Wayland => vec!["wl-paste", "--no-newline", "--type", m],
            Tools::X11 => vec!["xclip", "-selection", "clipboard", "-t", m, "-o"],
        };
        run(&args, None, Duration::from_secs(2)).ok().filter(|o| o.0).map(|o| String::from_utf8_lossy(&o.1).into_owned())
    };
    if has(GNOME_FILES) {
        if let Some((paths, kind)) = get(GNOME_FILES).and_then(|s| parse_gnome(&s)) {
            return Some(External::Files(paths, kind));
        }
    }
    if has(URI_LIST) {
        let paths = get(URI_LIST).map(|s| parse_uri_list(&s)).unwrap_or_default();
        if !paths.is_empty() {
            let cut = has(KDE_CUT) && get(KDE_CUT).is_some_and(|s| s.trim_start().starts_with('1'));
            return Some(External::Files(paths, if cut { Transfer::Move } else { Transfer::Copy }));
        }
    }
    Some(External::Other)
}

/// Runs a tool (no shell) with `input` on its stdin, for at most `limit`; (exit status success, stdout).
fn run(args: &[&str], input: Option<&[u8]>, limit: Duration) -> anyhow::Result<(bool, Vec<u8>)> {
    let mut c = Command::new(args[0]);
    c.args(&args[1..]).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = c.spawn()?;
    if let (Some(data), Some(mut sin)) = (input, child.stdin.take()) {
        sin.write_all(data)?;
    }
    // (stdout read on a thread of its own: a full pipe must not hold the tool while we wait for it)
    let mut sout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = sout.as_mut() {
            let _ = s.take(64 << 20).read_to_end(&mut buf);
        }
        buf
    });
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{} took too long", args[0]);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // (wl-copy's serving child keeps the pipe open: its output is not waited for)
    let out = if input.is_some() { Vec::new() } else { reader.join().unwrap_or_default() };
    Ok((status.success(), out))
}

/// `file://` URIs, one per line, CRLF as the list's rule says.
fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| format!("file://{}\r\n", encode(p))).collect()
}

fn encode(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// The local paths of a `text/uri-list` (comments, other schemes and other hosts left out).
fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).filter_map(file_uri_path).collect()
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    let rest = if rest.starts_with('/') { rest } else { rest.strip_prefix("localhost")? };
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// GNOME's `copy|cut` line, then one URI per line.
fn parse_gnome(text: &str) -> Option<(Vec<PathBuf>, Transfer)> {
    let mut lines = text.lines();
    let kind = match lines.next()?.trim() {
        "cut" => Transfer::Move,
        "copy" => Transfer::Copy,
        _ => return None,
    };
    let paths: Vec<PathBuf> = lines.map(str::trim).filter_map(file_uri_path).collect();
    (!paths.is_empty()).then_some((paths, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_set_come_back_and_the_sequence_moves() {
        let before = sequence();
        set_files(Owner::default(), &[PathBuf::from("/a")], Transfer::Copy).unwrap();
        assert!(sequence() > before);
        let (p, k) = store().clone().unwrap();
        assert_eq!((p, k), (vec![PathBuf::from("/a")], Transfer::Copy));
        clear(Owner::default()).unwrap();
        assert!(store().is_none());
    }

    #[test]
    fn uri_lists_go_out_and_come_back_with_odd_names() {
        let paths = vec![PathBuf::from("/home/n/a b%c/ü.txt"), PathBuf::from("/tmp/x#y")];
        let text = uri_list(&paths);
        assert_eq!(text, "file:///home/n/a%20b%25c/%C3%BC.txt\r\nfile:///tmp/x%23y\r\n");
        assert_eq!(parse_uri_list(&text), paths);
        assert_eq!(parse_uri_list("# comment\nfile://localhost/etc/hosts\nhttps://x/y\nfile://otherhost/z\n"), vec![PathBuf::from("/etc/hosts")]);
        assert_eq!(parse_gnome("cut\nfile:///a\nfile:///b%20c"), Some((vec![PathBuf::from("/a"), PathBuf::from("/b c")], Transfer::Move)));
        assert_eq!(parse_gnome("copy\nfile:///a").map(|g| g.1), Some(Transfer::Copy));
        assert_eq!(parse_gnome("file:///a"), None);
    }
}
