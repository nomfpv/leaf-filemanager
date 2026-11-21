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

/// [`mount_path`] with a user name, domain and password from leaf's own dialog or the keyring: `gio mount` asks on its input
/// and is answered there, each question once, by what it asks (exactly `User:`, `Domain:`, `Password:`; see [`answer_for`]).
/// The password never goes on the command line or into the environment. A question asked a second time (the server refused
/// the answer) ends the input, so gio gives up: one try per call, `PermissionDenied`. Blocks: worker threads only.
pub fn mount_path_login(uri: &str, user: &str, domain: &str, password: &str) -> std::io::Result<PathBuf> {
    let uri = encode_uri(uri);
    let m = gio_answering(&["mount", &uri], Duration::from_secs(90), user, domain, password)?;
    let t = m.text.trim().to_string();
    if !m.ok && !t.contains("already mounted") {
        // (a refusal deletes a saved password: only a question asked again, or gio's own error naming the login; a renamed
        // share, a timeout or a server gone mid-mount stay other errors)
        let kind = if login_refused(&t, m.asked_twice) { std::io::ErrorKind::PermissionDenied } else { std::io::ErrorKind::Other };
        return Err(std::io::Error::new(kind, t));
    }
    gio(&["info", "-a", "standard::name", &uri], Duration::from_secs(15))?.ok_then_path().ok_or_else(|| std::io::Error::other("the place is mounted but gio gives no folder for it"))
}

/// Whether a failed `gio mount` that was answered refused the login: the question came again, or gio's last error line (its
/// `gio: <address>: <message>`, never gvfs's "Password required for share ..." header) names a logon or password failure.
fn login_refused(errors: &str, asked_twice: bool) -> bool {
    if asked_twice {
        return true;
    }
    let Some(last) = errors.lines().rev().map(str::trim).find(|l| !l.is_empty()) else { return false };
    let low = last.to_ascii_lowercase();
    ["permission denied", "access denied", "logon failure", "login failed", "authentication failed", "invalid password", "wrong password"].iter().any(|w| low.contains(w))
}

/// gio's error lines out of all it printed: each line's part from its last `gio: ` on (the questions, printed without a newline,
/// sit before it on the same line); lines without one are left out, gvfs's headers among them.
fn error_lines(all: &str) -> String {
    all.lines().filter_map(|l| l.rfind("gio: ").map(|i| l[i..].trim())).collect::<Vec<_>>().join("\n")
}

struct Answered {
    ok: bool,
    text: String,
    /// A question came again after it was answered (the server refused the answer).
    asked_twice: bool,
}

/// What to say to one question of `gio mount` (the text it printed since the last answer). `None`: not a question yet.
/// Only a last line shaped exactly like gio's own questions (`LC_ALL=C`) is answered: `User:`, `Domain:` or `Password:`,
/// each maybe with a default in brackets (`User [neo]:`). Never a word found inside other text: a header such as
/// "Password required for share s on [fe80::1]", or a user name `password` in `User [password]:`, must not get the password.
fn answer_for<'a>(said: &str, user: &'a str, domain: &'a str, password: &'a str) -> Option<(&'static str, &'a str)> {
    let last = said.rsplit('\n').next().unwrap_or("").trim();
    let q = last.strip_suffix(':')?;
    [("User", "user", user), ("Domain", "domain", domain), ("Password", "password", password)].into_iter().find_map(|(word, what, answer)| {
        let rest = q.strip_prefix(word)?;
        let shaped = rest.is_empty() || rest.strip_prefix(" [").and_then(|r| r.strip_suffix(']')).is_some_and(|d| !d.contains([']', '[']));
        shaped.then_some((what, answer))
    })
}

/// Runs `gio` with its questions answered on its input (see [`mount_path_login`]).
fn gio_answering(args: &[&str], limit: Duration, user: &str, domain: &str, password: &str) -> std::io::Result<Answered> {
    use std::io::{Read, Write};
    use std::sync::mpsc;
    let exe = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("gio")).find(|f| f.is_file()))
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Unsupported))?;
    let mut child = Command::new(exe).args(args).env("LC_ALL", "C").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stdin = child.stdin.take();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    // (gio prints its questions without a newline: the bytes are passed on as they come)
    for pipe in [child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut buf = [0u8; 512];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let (mut all, mut since) = (String::new(), String::new());
    let mut answered: Vec<&'static str> = Vec::new();
    let mut asked_twice = false;
    let start = Instant::now();
    let status = loop {
        while let Ok(bytes) = rx.try_recv() {
            let s = String::from_utf8_lossy(&bytes);
            all.push_str(&s);
            since.push_str(&s);
        }
        if let Some((what, text)) = answer_for(&since, user, domain, password) {
            since.clear();
            if answered.contains(&what) {
                // asked again: the answer was refused; no second try from here
                asked_twice = true;
                stdin = None;
            } else if let Some(i) = stdin.as_mut() {
                answered.push(what);
                let mut line = Vec::with_capacity(text.len() + 1);
                line.extend_from_slice(text.as_bytes());
                line.push(b'\n');
                let wrote = i.write_all(&line).and_then(|_| i.flush());
                for b in line.iter_mut() {
                    // SAFETY: a plain write into our own buffer; volatile so it is not left out (the password's copy)
                    unsafe { std::ptr::write_volatile(b, 0) };
                }
                if wrote.is_err() {
                    stdin = None;
                }
            }
        }
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
    drop(stdin);
    std::thread::sleep(Duration::from_millis(20));
    while let Ok(bytes) = rx.try_recv() {
        all.push_str(&String::from_utf8_lossy(&bytes));
    }
    // (the questions and gvfs's headers are left out of the text an error shows)
    Ok(Answered { ok: status.success(), text: error_lines(&all), asked_twice })
}

/// Windows' share login (`WNetAddConnection2W`): `Unsupported` here (Linux logs in with [`mount_path_login`]).
pub fn connect_share(_remote: &str, _user: &str, _password: &str, _remember: bool) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Windows only: `Unsupported` here.
pub fn cancel_share(_remote: &str) -> std::io::Result<bool> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Whether an error of [`mount_path_login`] or [`mount_path`] says the login was refused.
pub fn is_login_refused(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Whether the refusal says the password is wrong (a saved one is deleted then). gio does not tell a wrong password from
/// other refusals, and [`mount_path_login`] calls it refused only when the question came again or gio named the login: the
/// same as [`is_login_refused`].
pub fn is_bad_password(e: &std::io::Error) -> bool {
    is_login_refused(e)
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

#[cfg(test)]
mod tests {
    use super::answer_for;

    /// Only gio's own question lines get an answer, the password only `Password:`: never a header ending in `]` (an IPv6
    /// server), never a `User [..]:` whose default holds the word "password".
    #[test]
    fn gio_questions_answered_only_on_exact_prompt_lines() {
        let a = |said: &str| answer_for(said, "neo", "WORK", "pw").map(|(w, _)| w);
        assert_eq!(a("Password required for share s on [fe80::1]"), None);
        assert_eq!(a("Password required for share s on [fe80::1]\n"), None);
        assert_eq!(a("Password required for share s on host:"), None);
        assert_eq!(a("User [password]: "), Some("user"));
        assert_eq!(answer_for("User [password]: ", "neo", "WORK", "pw").map(|(_, t)| t), Some("neo"));
        assert_eq!(a("Password required for share s on [fe80::1]\nUser [neo]: "), Some("user"));
        assert_eq!(a("Domain [WORKGROUP]: "), Some("domain"));
        assert_eq!(a("Password: "), Some("password"));
        assert_eq!(a("Domain [my domain]: "), Some("domain"));
        assert_eq!(a("Us"), None);
        assert_eq!(a("Username: "), None);
        assert_eq!(a("User [a]b]: "), None);
        assert_eq!(a("Login anonymously? [y/N]"), None);
    }
}
