//! Linux backend of rf_win::network. Status: PARTIAL (gio mounts, smbclient shares, avahi/smbtree discovery, lan_link: never run on a desktop; the drive-letter dialogs do not apply)

use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A UNC name (`\\server`) is a Windows way of writing a place: on Linux a server is reached as `smb://server` (an address leaf opens
/// as a remote place, see [`mount_path`]) and is never a folder path, so no path is a server root here.
pub fn server_root(_path: &Path) -> Option<String> {
    None
}

// TYPE-COPY(rf-win src/network.rs:27)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub remark: String,
}

/// Disk shares of `server` through `smbclient -L` (anonymous: no password is ever put on a command line). Without `smbclient`:
/// `Unsupported`; a server that wants a login: `PermissionDenied` (the share is then entered through [`mount_path_login`]).
pub fn shares(server: &str) -> std::io::Result<Vec<Share>> {
    if server.is_empty() || server.starts_with('-') || server.contains(['/', '\\', ' ']) {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let target = format!("//{server}");
    let o = tool("smbclient", &["-g", "-N", "-L", &target], Duration::from_secs(20))?;
    if !o.ok {
        let low = o.text.to_ascii_uppercase();
        let kind = if low.contains("ACCESS_DENIED") || low.contains("LOGON_FAILURE") { std::io::ErrorKind::PermissionDenied } else { std::io::ErrorKind::Other };
        return Err(std::io::Error::new(kind, o.text.trim().to_string()));
    }
    Ok(parse_smb_shares(&o.text))
}

/// The disk shares in the text of `smbclient -g -L`: `Disk|name|remark` lines; printers, IPC and hidden (`$`) shares are left out.
fn parse_smb_shares(text: &str) -> Vec<Share> {
    let mut v: Vec<Share> = text
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '|');
            let (kind, name, remark) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            (kind == "Disk" && !name.is_empty() && !name.ends_with('$')).then(|| Share { name: name.to_string(), remark: remark.trim().to_string() })
        })
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

/// A machine of the local network that offers files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// What to show (the service's name, else the host).
    pub name: String,
    /// The address to open: `smb://host`, `nfs://host`, `sftp://host`, `ftp://host`, `webdav://host`.
    pub uri: String,
}

/// Servers on the local network, the way a KDE desktop's Network place lists them: DNS-SD (`avahi-browse -ptr`, for `_smb._tcp`,
/// `_nfs._tcp`, `_sftp-ssh._tcp`, `_ftp._tcp`, `_webdav._tcp`) and, for Windows-style neighbours that do not announce themselves, `smbtree -N -S`
/// when installed. Each source only when its program exists; none: an empty list. Blocks (a few seconds): worker threads only.
pub fn servers() -> Vec<Server> {
    let mut v: Vec<Server> = Vec::new();
    if let Ok(o) = tool("avahi-browse", &["-p", "-t", "-r", "-k", "-a"], Duration::from_secs(8)).map(|o| o.text) {
        v.extend(parse_avahi(&o));
    }
    if let Ok(o) = tool("smbtree", &["-N", "-S"], Duration::from_secs(12)) {
        for s in parse_smbtree(&o.text) {
            if !v.iter().any(|x| x.uri.eq_ignore_ascii_case(&s.uri)) {
                v.push(s);
            }
        }
    }
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.uri.cmp(&b.uri)));
    v.dedup_by(|a, b| a.uri == b.uri);
    v
}

/// The resolved services in the text of `avahi-browse -ptr`: `=;eth0;IPv4;Name;_smb._tcp;local;host.local;192.168.1.5;445;"txt"`.
/// Only IPv4 lines are used (one machine is listed once; the address is `host.local`).
fn parse_avahi(text: &str) -> Vec<Server> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(';').collect();
            if f.len() < 9 || f[0] != "=" || f[2] != "IPv4" {
                return None;
            }
            let scheme = match f[4] {
                "_smb._tcp" => "smb",
                "_nfs._tcp" => "nfs",
                "_sftp-ssh._tcp" => "sftp",
                "_ftp._tcp" => "ftp",
                "_webdav._tcp" => "webdav",
                _ => return None,
            };
            let host = f[6].trim();
            if host.is_empty() || host.contains(['/', ' ']) {
                return None;
            }
            // (avahi writes a non-ASCII name as decimal escapes, `\032` for a space)
            let name = unescape_avahi(f[3]);
            Some(Server { name: if name.is_empty() { host.to_string() } else { name }, uri: format!("{scheme}://{host}") })
        })
        .collect()
}

/// `\032` (a decimal byte) and `\\` in avahi's parseable output.
fn unescape_avahi(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            let n: u32 = s[i + 1..i + 4].parse().unwrap_or(63);
            out.push(n.min(255) as u8);
            i += 4;
        } else if b[i] == b'\\' && i + 1 < b.len() && b[i + 1] == b'\\' {
            out.push(b'\\');
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The servers in the text of `smbtree -N -S`: workgroup lines are at the margin, `\\SERVER   comment` lines under them are indented one
/// step, share lines (`\\SERVER\share`) deeper; only the server lines count.
fn parse_smbtree(text: &str) -> Vec<Server> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if l.len() == t.len() {
                return None;
            }
            let rest = t.strip_prefix("\\\\")?;
            let name = rest.split_whitespace().next()?;
            (!name.contains('\\')).then(|| Server { name: name.to_string(), uri: format!("smb://{name}") })
        })
        .collect()
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
    tool("gio", args, limit)
}

/// [`gio`] for any program on `PATH` (argument list, `LC_ALL=C`, a time limit).
fn tool(name: &str, args: &[&str], limit: Duration) -> std::io::Result<GioOut> {
    use std::io::Read;
    let exe = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.is_file()))
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

/// The link this machine reaches the network over: its kind and rate in Mbit/s (Wi-Fi's current transmit rate from `iw` when installed,
/// Ethernet's link speed from sysfs). The interface is the default route's (`/proc/net/route`, lowest metric). None without a route
/// or without a rate.
pub fn lan_link() -> Option<(LinkType, u32)> {
    let iface = default_route_interface(&std::fs::read_to_string("/proc/net/route").ok()?)?;
    if iface.contains(['/', '\0']) || iface.starts_with('.') {
        return None;
    }
    let sys = PathBuf::from("/sys/class/net").join(&iface);
    if sys.join("wireless").exists() || sys.join("phy80211").exists() {
        let o = tool("iw", &["dev", &iface, "link"], Duration::from_secs(3)).ok().filter(|o| o.ok)?;
        let mbps = parse_iw_rate(&o.text)?;
        return Some((LinkType::Wifi, mbps));
    }
    // (a virtual interface -- a VPN, a bridge, a container -- has no `device`)
    let kind = if sys.join("device").exists() { LinkType::Ethernet } else { LinkType::Other };
    let speed: i64 = std::fs::read_to_string(sys.join("speed")).ok()?.trim().parse().ok()?;
    // (-1: the driver does not know)
    (speed > 0).then_some((kind, speed as u32))
}

/// The interface of the default route in `/proc/net/route` (`Iface Destination Gateway Flags RefCnt Use Metric ...`, hex, tab separated):
/// destination 0, flag UP (1) and GATEWAY (2); the lowest metric wins.
fn default_route_interface(text: &str) -> Option<String> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 || f[1] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            (flags & 3 == 3).then(|| (f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
        })
        .min()
        .map(|(_, i)| i)
}

/// `tx bitrate: 866.7 MBit/s ...` in the text of `iw dev X link`.
fn parse_iw_rate(text: &str) -> Option<u32> {
    let rest = text.lines().find_map(|l| l.trim().strip_prefix("tx bitrate:"))?;
    let n: f64 = rest.split_whitespace().next()?.parse().ok()?;
    (n >= 1.0).then_some(n as u32)
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

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn avahi_lines_become_servers() {
        let t = "+;eth0;IPv4;NAS;_smb._tcp;local\n=;eth0;IPv4;My\\032NAS;_smb._tcp;local;nas.local;192.168.1.5;445;\"\"\n=;eth0;IPv6;NAS;_smb._tcp;local;nas.local;fe80::1;445;\"\"\n=;eth0;IPv4;Pi;_sftp-ssh._tcp;local;pi.local;192.168.1.9;22;\"\"\n=;eth0;IPv4;Printer;_ipp._tcp;local;p.local;192.168.1.7;631;\"\"\n";
        assert_eq!(
            parse_avahi(t),
            vec![Server { name: "My NAS".into(), uri: "smb://nas.local".into() }, Server { name: "Pi".into(), uri: "sftp://pi.local".into() }]
        );
    }

    #[test]
    fn smbclient_and_smbtree_text() {
        let t = "Disk|Music|Family music\nDisk|C$|Default share\nIPC|IPC$|IPC Service\nPrinter|hp|\nDisk|Docs|\n";
        assert_eq!(parse_smb_shares(t), vec![Share { name: "Docs".into(), remark: String::new() }, Share { name: "Music".into(), remark: "Family music".into() }]);
        let tree = "WORKGROUP\n\t\\\\NAS             \tNAS server\n\t\t\\\\NAS\\Music         \t\n";
        assert_eq!(parse_smbtree(tree), vec![Server { name: "NAS".into(), uri: "smb://NAS".into() }]);
    }

    #[test]
    fn default_route_and_wifi_rate() {
        let t = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\nwlan0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\neth0\t00000000\t0100A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\neth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(default_route_interface(t).as_deref(), Some("eth0"));
        assert_eq!(parse_iw_rate("Connected to aa\n\ttx bitrate: 866.7 MBit/s VHT-MCS 9\n"), Some(866));
    }
}
