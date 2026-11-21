//! Linux backend of rf_win::network. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn server_root(_path: &Path) -> Option<String> {
    stub!("network::server_root")
}

// TYPE-COPY(rf-win src/network.rs:27)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub remark: String,
}

pub fn shares(_server: &str) -> std::io::Result<Vec<Share>> {
    stub_err!("network::shares")
}

/// The mounted `smb://` and `nfs://` places (gvfs, through `gio mount -l`): (name, address). Empty without `gio`.
pub fn network_shortcuts() -> Vec<(String, PathBuf)> {
    let Some(out) = gio(&["mount", "-l"], Duration::from_secs(5)).ok().filter(|o| o.ok) else { return Vec::new() };
    parse_mounts(&out.text)
}

/// The mounts in the text of `gio mount -l`: `Mount(0): share on host -> smb://host/share/`.
fn parse_mounts(text: &str) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = text
        .lines()
        .filter_map(|l| {
            let l = l.trim_start().strip_prefix("Mount(")?;
            let (_, rest) = l.split_once("): ")?;
            let (name, uri) = rest.rsplit_once(" -> ")?;
            let uri = uri.trim();
            (uri.starts_with("smb://") || uri.starts_with("nfs://")).then(|| (name.trim().to_string(), PathBuf::from(uri.trim_end_matches('/'))))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v.dedup();
    v
}

struct GioOut {
    ok: bool,
    text: String,
}

/// Runs `gio` with an argument list (no shell, no input: the desktop's own prompt asks for a password, leaf never sees it) and
/// waits at most `limit`. `Unsupported` when `gio` is not installed.
fn gio(args: &[&str], limit: Duration) -> std::io::Result<GioOut> {
    use std::io::Read;
    let exe = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("gio")).find(|f| f.is_file()))
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Unsupported))?;
    let mut child = Command::new(exe).args(args).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (mut so, mut se) = (child.stdout.take(), child.stderr.take());
    let reader = std::thread::spawn(move || {
        let (mut a, mut b) = (String::new(), String::new());
        if let Some(o) = so.as_mut() {
            let _ = o.read_to_string(&mut a);
        }
        if let Some(e) = se.as_mut() {
            let _ = e.read_to_string(&mut b);
        }
        (a, b)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "gio took too long"));
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    let (out, err) = reader.join().unwrap_or_default();
    Ok(GioOut { ok: status.success(), text: if status.success() { out } else if err.trim().is_empty() { out } else { err } })
}

/// The percent-encoded form of an address for `gio` (a path with spaces, `%` and the like, as written decoded).
fn encode_uri(uri: &str) -> String {
    let mut o = String::with_capacity(uri.len());
    for b in uri.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:@[]".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// The disk path under `$XDG_RUNTIME_DIR/gvfs` that `gio info` names for a mounted place.
fn parse_local_path(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|l| l.strip_prefix("local path: ")).map(|p| PathBuf::from(p.trim()))
}

/// The gvfs folder of `smb://host/share/dir` or `nfs://host/export/dir`, mounting the place first through `gio mount` when it is
/// not mounted (a password is asked by the desktop's own prompt, never by leaf). Blocks: worker threads only. Errors by kind:
/// `Unsupported` no `gio`, `PermissionDenied` the login was refused or cancelled, anything else the mount failed (the text is
/// gio's own).
pub fn mount_path(uri: &str) -> std::io::Result<PathBuf> {
    let uri = encode_uri(uri);
    let info = |u: &str| -> std::io::Result<Option<PathBuf>> { Ok(gio(&["info", "-a", "standard::name", u], Duration::from_secs(15))?.ok_then_path()) };
    if let Some(p) = info(&uri)? {
        return Ok(p);
    }
    let m = gio(&["mount", &uri], Duration::from_secs(90))?;
    let t = m.text.trim().to_string();
    if !m.ok && !t.contains("already mounted") {
        let low = t.to_lowercase();
        let kind = if low.contains("password") || low.contains("authenticat") || low.contains("permission denied") || low.contains("access denied") || low.contains("aborted") {
            std::io::ErrorKind::PermissionDenied
        } else {
            std::io::ErrorKind::Other
        };
        return Err(std::io::Error::new(kind, t));
    }
    info(&uri)?.ok_or_else(|| std::io::Error::other("the place is mounted but gio gives no folder for it"))
}

impl GioOut {
    fn ok_then_path(self) -> Option<PathBuf> {
        self.ok.then(|| parse_local_path(&self.text)).flatten()
    }
}

/// Lets go of a mounted place (`gio mount -u`). Never forced.
pub fn disconnect(uri: &str) -> std::io::Result<()> {
    let o = gio(&["mount", "-u", &encode_uri(uri)], Duration::from_secs(60))?;
    if o.ok { Ok(()) } else { Err(std::io::Error::other(o.text.trim().to_string())) }
}

pub fn map_drive_dialog(_owner: isize) {
    stub!("network::map_drive_dialog")
}

pub fn disconnect_drive_dialog(_owner: isize) {
    stub!("network::disconnect_drive_dialog")
}

// TYPE-COPY(rf-win src/network.rs:142)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkType {
    Wifi,
    Ethernet,
    Other,
}

pub fn lan_link() -> Option<(LinkType, u32)> {
    stub!("network::lan_link")
}
