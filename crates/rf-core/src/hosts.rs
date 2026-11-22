//! Saved hosts (W1b): `hosts.toml` in the config folder, and the import from OpenSSH's `~/.ssh/config`.
//!
//! The file holds where a server is and how to reach it (name, scheme, host, port, user, start folder, key file, jump host),
//! never a password or any other secret: those go to the system's credential store once connecting is built.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "hosts.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// What the user calls it (unique in the list; the OpenSSH alias on import).
    pub name: String,
    /// `sftp` (default), `ftp`, `ftps`...
    #[serde(default = "default_scheme")]
    pub scheme: String,
    /// The server's name or address.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The folder a connection opens in (the server's default when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_path: Option<String>,
    /// A private key file (the path as written, `~` allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,
    /// OpenSSH `ProxyJump`: `user@jump.example.com:22` or another saved host's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    /// Folders on the server kept as bookmarks (paths as the server writes them); shown under the host in the sidebar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<String>,
    /// Deletes on this host are permanent: leaf's trash on the server (`.leaf-trash` in the login folder) is not used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_trash: bool,
    /// What the machine is (a server, a NAS, a router...): picked in the add / edit dialog or guessed from the name and port, for
    /// the host's icon. A host saved before this field, or with a kind this build does not know, is a server.
    #[serde(default, skip_serializing_if = "HostKind::is_default", deserialize_with = "HostKind::lenient")]
    pub kind: HostKind,
    /// Connections to it go through leaf's VPN (an address on the local network never does). With the VPN's kill switch on and
    /// the VPN down, it is not connected at all.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vpn: bool,
    /// How an SFTP host's files are reached: SFTP, falling back to shell commands when the server has no SFTP (the default),
    /// or always one of the two.
    #[serde(default, skip_serializing_if = "FileAccess::is_default", deserialize_with = "FileAccess::lenient")]
    pub access: FileAccess,
}

/// "File access" of an SFTP host (H1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileAccess {
    /// SFTP, and shell commands over SSH when the server refuses the SFTP subsystem.
    #[default]
    Auto,
    /// SFTP only (a server without it is an error).
    Sftp,
    /// Shell commands over SSH only (`cat`, `ls`, `mv`...), for a host whose SFTP is broken.
    Shell,
}

impl FileAccess {
    pub const ALL: [FileAccess; 3] = [FileAccess::Auto, FileAccess::Sftp, FileAccess::Shell];

    fn is_default(&self) -> bool {
        *self == FileAccess::Auto
    }

    /// An unknown word (a newer version's) reads as Automatic.
    fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<FileAccess, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.to_ascii_lowercase().as_str() {
            "sftp" => FileAccess::Sftp,
            "shell" => FileAccess::Shell,
            _ => FileAccess::Auto,
        })
    }

    /// The name shown to the user (English; the app translates it where it draws it).
    pub fn label(self) -> &'static str {
        match self {
            FileAccess::Auto => "Automatic",
            FileAccess::Sftp => "SFTP",
            FileAccess::Shell => "Shell",
        }
    }
}

/// What kind of machine a saved host is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    #[default]
    Server,
    Nas,
    Router,
    Switch,
    Ap,
    Firewall,
    Pc,
    Laptop,
    Phone,
    Printer,
    Vm,
    Container,
    Cloud,
}

impl HostKind {
    pub const ALL: [HostKind; 13] = [
        HostKind::Server,
        HostKind::Nas,
        HostKind::Router,
        HostKind::Switch,
        HostKind::Ap,
        HostKind::Firewall,
        HostKind::Pc,
        HostKind::Laptop,
        HostKind::Phone,
        HostKind::Printer,
        HostKind::Vm,
        HostKind::Container,
        HostKind::Cloud,
    ];

    fn is_default(&self) -> bool {
        *self == HostKind::Server
    }

    /// An unknown word in the file (a newer version's kind) reads as a server instead of making the whole list unreadable.
    fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<HostKind, D::Error> {
        let s = String::deserialize(d)?;
        Ok(HostKind::ALL.into_iter().find(|k| k.id() == s.to_ascii_lowercase()).unwrap_or_default())
    }

    /// The word in `hosts.toml`.
    pub fn id(self) -> &'static str {
        match self {
            HostKind::Server => "server",
            HostKind::Nas => "nas",
            HostKind::Router => "router",
            HostKind::Switch => "switch",
            HostKind::Ap => "ap",
            HostKind::Firewall => "firewall",
            HostKind::Pc => "pc",
            HostKind::Laptop => "laptop",
            HostKind::Phone => "phone",
            HostKind::Printer => "printer",
            HostKind::Vm => "vm",
            HostKind::Container => "container",
            HostKind::Cloud => "cloud",
        }
    }

    /// The name shown to the user (English; the app translates it where it draws it).
    pub fn label(self) -> &'static str {
        match self {
            HostKind::Server => "Server",
            HostKind::Nas => "NAS",
            HostKind::Router => "Router",
            HostKind::Switch => "Switch",
            HostKind::Ap => "Access point",
            HostKind::Firewall => "Firewall",
            HostKind::Pc => "PC",
            HostKind::Laptop => "Laptop",
            HostKind::Phone => "Phone",
            HostKind::Printer => "Printer",
            HostKind::Vm => "VM",
            HostKind::Container => "Container",
            HostKind::Cloud => "Cloud",
        }
    }

    /// The kind where the host's name, address, port or scheme makes it obvious ("nas" in the name, port 9100 = a printer,
    /// an rclone remote = cloud storage); a server otherwise. Words are matched whole (`nas01` counts, `canvas` does not).
    pub fn guess(h: &Host) -> HostKind {
        if h.scheme.eq_ignore_ascii_case("rclone") {
            return HostKind::Cloud;
        }
        match h.port {
            Some(9100 | 631 | 515) => return HostKind::Printer,
            Some(8291) => return HostKind::Router,
            _ => {}
        }
        let lower = format!("{} {}", h.name, h.host).to_ascii_lowercase();
        let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.trim_end_matches(|c: char| c.is_ascii_digit())).collect();
        let has = |list: &[&str]| words.iter().any(|w| list.contains(w));
        const RULES: [(&[&str], HostKind); 12] = [
            (&["nas", "synology", "diskstation", "qnap", "truenas", "unraid", "freenas"], HostKind::Nas),
            (&["printer", "print", "laserjet", "officejet", "mfp"], HostKind::Printer),
            (&["firewall", "fw", "pfsense", "opnsense", "fortigate"], HostKind::Firewall),
            (&["router", "gateway", "gw", "openwrt", "mikrotik", "edgerouter", "fritzbox"], HostKind::Router),
            (&["switch", "sw"], HostKind::Switch),
            (&["ap", "wap", "unifi", "accesspoint"], HostKind::Ap),
            (&["laptop", "notebook", "macbook", "thinkpad"], HostKind::Laptop),
            (&["phone", "iphone", "android", "pixel"], HostKind::Phone),
            (&["vm", "virtual"], HostKind::Vm),
            (&["docker", "container", "lxc", "k8s", "pod"], HostKind::Container),
            (&["pc", "desktop", "workstation"], HostKind::Pc),
            (&["cloud", "amazonaws", "s3"], HostKind::Cloud),
        ];
        RULES.iter().find(|(list, _)| has(list)).map_or(HostKind::Server, |&(_, k)| k)
    }
}

fn default_scheme() -> String {
    "sftp".into()
}

impl Host {
    pub fn new(name: &str, host: &str) -> Host {
        Host { name: name.into(), scheme: default_scheme(), host: host.into(), port: None, user: None, start_path: None, key_file: None, jump: None, bookmarks: Vec::new(), no_trash: false, kind: HostKind::Server, vpn: false, access: FileAccess::Auto }
    }

    /// Whether a remote place is on this host (same scheme, server, user and port).
    pub fn is_server_of(&self, v: &crate::vpath::VPath) -> bool {
        self.scheme.eq_ignore_ascii_case(v.scheme())
            && self.host.eq_ignore_ascii_case(v.host())
            && self.user.as_deref() == v.user()
            && self.port == v.port()
    }

    /// The address the host's start folder has (`sftp://user@example.com:2222/srv`). No secret is in it.
    pub fn address(&self) -> String {
        let mut s = format!("{}://", self.scheme);
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        if self.host.contains(':') && !self.host.starts_with('[') {
            s.push_str(&format!("[{}]", self.host));
        } else {
            s.push_str(&self.host);
        }
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        let path = self.start_path.as_deref().unwrap_or("/");
        if !path.starts_with('/') {
            s.push('/');
        }
        s.push_str(path);
        s
    }

    /// The host as one line of text, for the edit box: `web = sftp://deploy@www.example.com:2222/srv key="~/.ssh/id" jump=bastion`.
    pub fn to_line(&self) -> String {
        let mut s = format!("{} = {}", self.name, self.address());
        let quote = |v: &str| if v.contains(char::is_whitespace) { format!("\"{v}\"") } else { v.to_string() };
        if let Some(k) = &self.key_file {
            s.push_str(&format!(" key={}", quote(k)));
        }
        if let Some(j) = &self.jump {
            s.push_str(&format!(" jump={}", quote(j)));
        }
        if self.no_trash {
            s.push_str(" trash=off");
        }
        s
    }

    /// Reads [`Host::to_line`]'s form back. A bare `user@host` (no scheme) is taken as SFTP; a password in the address is refused.
    pub fn from_line(line: &str) -> Result<Host, String> {
        let (name, rest) = line.split_once('=').ok_or("write it as: name = sftp://user@host:port/folder")?;
        let name = name.trim();
        if name.is_empty() {
            return Err("the host needs a name".into());
        }
        let mut parts = words(rest).into_iter();
        let addr = parts.next().ok_or("the address is missing")?;
        let addr = if crate::vpath::VPath::looks_like(&addr) { addr } else { format!("sftp://{addr}") };
        let v = crate::vpath::VPath::parse(&addr).map_err(|e| e.to_string())?;
        if v.host().is_empty() {
            return Err("the address has no server name".into());
        }
        let mut h = Host::new(name, v.host());
        h.scheme = v.scheme().to_string();
        h.user = v.user().map(str::to_string);
        h.port = v.port();
        h.start_path = (!v.is_root()).then(|| v.path().to_string());
        for p in parts {
            match p.split_once('=') {
                Some(("key", k)) if !k.is_empty() => h.key_file = Some(k.to_string()),
                Some(("jump", j)) if !j.is_empty() => h.jump = Some(j.to_string()),
                Some(("trash", t)) if t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("on") => h.no_trash = t.eq_ignore_ascii_case("off"),
                _ => return Err(format!("\"{p}\" is not understood (key=..., jump=... or trash=off)")),
            }
        }
        Ok(h)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hosts {
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
}

impl Hosts {
    /// Reads `hosts.toml` from `dir`; a missing file is an empty list, a broken one is moved aside (`backup_corrupt`) and
    /// read as empty so a later save cannot destroy it.
    pub fn load(dir: &Path) -> Hosts {
        let path = dir.join(FILE_NAME);
        match crate::read_config(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!("hosts: {} is not readable ({e}); kept aside", path.display());
                crate::backup_corrupt(&path);
                Hosts::default()
            }),
            Err(_) => Hosts::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)
    }

    pub fn find(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name.eq_ignore_ascii_case(name))
    }

    /// Bookmarks the folder on the host it belongs to, or takes the bookmark away when it is one. `Some(true)`: added,
    /// `Some(false)`: removed, `None`: no saved host is this server.
    pub fn toggle_bookmark(&mut self, v: &crate::vpath::VPath) -> Option<bool> {
        let h = self.hosts.iter_mut().find(|h| h.is_server_of(v))?;
        let path = v.path().to_string();
        if let Some(i) = h.bookmarks.iter().position(|b| *b == path) {
            h.bookmarks.remove(i);
            Some(false)
        } else {
            h.bookmarks.push(path);
            Some(true)
        }
    }

    /// Adds the imported hosts whose name is not in the list yet; returns how many were added.
    pub fn merge(&mut self, imported: Vec<Host>) -> usize {
        let mut n = 0;
        for h in imported {
            if self.find(&h.name).is_none() {
                self.hosts.push(h);
                n += 1;
            }
        }
        n
    }
}

/// What an import read, and what it left out (with the reason, for the user).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshImport {
    pub hosts: Vec<Host>,
    pub skipped: Vec<String>,
}

/// `~/.ssh/config` of the user running leaf (`%USERPROFILE%\.ssh\config` on Windows).
pub fn ssh_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".ssh").join("config"))
}

/// Reads an OpenSSH client config: every `Host` alias without wildcards becomes a host with its `HostName`, `User`,
/// `Port`, `IdentityFile` and `ProxyJump`. As in OpenSSH the first value given for a keyword wins. `Include`, `Match`
/// blocks and wildcard or negated patterns are skipped and reported; other keywords are ignored silently.
pub fn parse_ssh_config(text: &str) -> SshImport {
    parse_lines(lines_of(text, ""), Vec::new())
}

/// Reads the OpenSSH client config at `path` (`~/.ssh/config`) with the files its `Include` lines name, read in place as
/// OpenSSH does: a relative name is under `~/.ssh` (the config's folder), `~` is the home folder, and `*` / `?` in the
/// file's name take every matching file in name order. Includes inside included files are followed up to 16 deep; a
/// file that is not there is left out silently (as OpenSSH does), one that cannot be read is reported.
pub fn parse_ssh_config_file(path: &Path) -> std::io::Result<SshImport> {
    let text = std::fs::read_to_string(path)?;
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut skipped = Vec::new();
    let lines = expand_includes(lines_of(&text, ""), &base, 0, &mut skipped);
    Ok(parse_lines(lines, skipped))
}

/// One line of a config: where it came from (`""` for the main file, else the included file's name), its number, the text.
type Line = (String, usize, String);

fn lines_of(text: &str, from: &str) -> Vec<Line> {
    text.lines().enumerate().map(|(n, l)| (from.to_string(), n + 1, l.to_string())).collect()
}

fn where_(from: &str, n: usize) -> String {
    if from.is_empty() { format!("line {n}") } else { format!("{from} line {n}") }
}

/// The lines with every `Include` replaced by the lines of the files it names.
fn expand_includes(lines: Vec<Line>, base: &Path, depth: usize, skipped: &mut Vec<String>) -> Vec<Line> {
    let mut out = Vec::new();
    for (from, n, line) in lines {
        let (key, value) = split_keyword(line.trim());
        if !key.eq_ignore_ascii_case("include") {
            out.push((from, n, line));
            continue;
        }
        if depth >= 16 {
            skipped.push(format!("{}: Include {value} (included too deep)", where_(&from, n)));
            continue;
        }
        for pattern in words(value) {
            for file in include_files(&pattern, base) {
                let name = file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                match std::fs::read_to_string(&file) {
                    Ok(t) => out.extend(expand_includes(lines_of(&t, &name), base, depth + 1, skipped)),
                    Err(e) => skipped.push(format!("{}: Include {} ({e})", where_(&from, n), file.display())),
                }
            }
        }
    }
    out
}

/// The files an `Include` pattern names (wildcards in the file's name only), sorted.
fn include_files(pattern: &str, base: &Path) -> Vec<PathBuf> {
    let p = if let Some(rest) = pattern.strip_prefix("~/").or_else(|| pattern.strip_prefix("~\\")) {
        match ssh_config_path().and_then(|c| c.parent().and_then(Path::parent).map(Path::to_path_buf)) {
            Some(home) => home.join(rest),
            None => return Vec::new(),
        }
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        base.join(pattern)
    };
    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    if !name.contains(['*', '?']) {
        return if p.is_file() { vec![p] } else { Vec::new() };
    }
    let Some(dir) = p.parent() else { return Vec::new() };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(Result::ok).filter(|e| e.path().is_file() && wildcard(&name, &e.file_name().to_string_lossy())).map(|e| e.path()).collect())
        .unwrap_or_default();
    found.sort();
    found
}

/// `*` and `?` matching of a whole name.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

fn parse_lines(lines: Vec<Line>, skipped: Vec<String>) -> SshImport {
    #[derive(Default)]
    struct Block {
        aliases: Vec<String>,
        hostname: Option<String>,
        user: Option<String>,
        port: Option<u16>,
        identity: Option<String>,
        jump: Option<String>,
    }
    let mut out = SshImport { hosts: Vec::new(), skipped };
    let mut blocks: Vec<Block> = Vec::new();
    // None: before the first Host, or inside a Match block (both not imported)
    let mut cur: Option<usize> = None;
    for (from, n, raw) in &lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = split_keyword(line);
        let lineno = where_(from, *n);
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                let mut b = Block::default();
                for pat in words(value) {
                    if pat.contains(['*', '?']) || pat.starts_with('!') {
                        out.skipped.push(format!("{lineno}: Host pattern \"{pat}\" (wildcards are not imported)"));
                    } else {
                        b.aliases.push(pat);
                    }
                }
                blocks.push(b);
                cur = Some(blocks.len() - 1);
            }
            "match" => {
                out.skipped.push(format!("{lineno}: Match block (not imported)"));
                cur = None;
            }
            "include" => out.skipped.push(format!("{lineno}: Include {value} (included files are not read)")),
            k => {
                let Some(b) = cur.map(|i| &mut blocks[i]) else { continue };
                let v = words(value).into_iter().next().unwrap_or_default();
                if v.is_empty() {
                    continue;
                }
                match k {
                    "hostname" => {
                        b.hostname.get_or_insert(v);
                    }
                    "user" => {
                        b.user.get_or_insert(v);
                    }
                    "port" => match v.parse::<u16>() {
                        Ok(p) if p > 0 => {
                            b.port.get_or_insert(p);
                        }
                        _ => out.skipped.push(format!("{lineno}: Port \"{v}\" is not a port number")),
                    },
                    "identityfile" => {
                        b.identity.get_or_insert(v);
                    }
                    "proxyjump" if !v.eq_ignore_ascii_case("none") => {
                        b.jump.get_or_insert(v);
                    }
                    _ => {}
                }
            }
        }
    }
    for b in blocks {
        for alias in &b.aliases {
            if out.hosts.iter().any(|h: &Host| h.name.eq_ignore_ascii_case(alias)) {
                continue; // a later block for the same alias only adds values OpenSSH would ignore here
            }
            let mut h = Host::new(alias, b.hostname.as_deref().unwrap_or(alias));
            h.user = b.user.clone();
            h.port = b.port.filter(|&p| p != 22);
            h.key_file = b.identity.clone();
            h.jump = b.jump.clone();
            out.hosts.push(h);
        }
    }
    out
}

/// `Keyword value`, `Keyword=value`, `Keyword = value`.
fn split_keyword(line: &str) -> (&str, &str) {
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let key = &line[..end];
    let rest = line[end..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    (key, rest)
}

/// Splits on spaces, keeping "quoted parts" together (quotes removed).
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# global settings are not a host
User nobody
Include ~/.ssh/config.d/*

Host web web-alias
    HostName www.example.com
    User deploy
    Port 2222
    IdentityFile "~/.ssh/id ed25519"

Host *.internal !bad
    User x

Host db
  HostName=db.example.org
  Port = 22
  User admin
  User ignored-second-value
  ProxyJump jump@bastion.example.net:22

Match host foo
  User matched

Host plain
Host Web
  HostName other.example.com
"#;

    #[test]
    fn ssh_config_table() {
        let r = parse_ssh_config(CONFIG);
        // name, host, user, port, key, jump
        let want: &[(&str, &str, Option<&str>, Option<u16>, Option<&str>, Option<&str>)] = &[
            ("web", "www.example.com", Some("deploy"), Some(2222), Some("~/.ssh/id ed25519"), None),
            ("web-alias", "www.example.com", Some("deploy"), Some(2222), Some("~/.ssh/id ed25519"), None),
            ("db", "db.example.org", Some("admin"), None, None, Some("jump@bastion.example.net:22")),
            ("plain", "plain", None, None, None, None),
        ];
        assert_eq!(r.hosts.len(), want.len(), "{:#?}", r.hosts);
        for (h, &(name, host, user, port, key, jump)) in r.hosts.iter().zip(want) {
            assert_eq!(h.name, name);
            assert_eq!(h.host, host, "{name}");
            assert_eq!(h.user.as_deref(), user, "{name}");
            assert_eq!(h.port, port, "{name}");
            assert_eq!(h.key_file.as_deref(), key, "{name}");
            assert_eq!(h.jump.as_deref(), jump, "{name}");
            assert_eq!(h.scheme, "sftp");
        }
        assert_eq!(r.skipped.len(), 4, "{:#?}", r.skipped);
        assert!(r.skipped[0].contains("Include"));
        assert!(r.skipped[1].contains("*.internal"));
        assert!(r.skipped[2].contains("!bad"));
        assert!(r.skipped[3].contains("Match"));
    }

    #[test]
    fn file_round_trip_and_merge() {
        let dir = std::env::temp_dir().join(format!("leaf-hosts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Hosts::load(&dir), Hosts::default(), "no file yet: empty");
        let mut hs = Hosts::default();
        let mut h = Host::new("web", "www.example.com");
        h.user = Some("deploy".into());
        h.port = Some(2222);
        h.start_path = Some("/srv/www".into());
        hs.hosts.push(h.clone());
        hs.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(!text.to_ascii_lowercase().contains("password"));
        assert_eq!(Hosts::load(&dir), hs);
        assert_eq!(h.address(), "sftp://deploy@www.example.com:2222/srv/www");
        assert_eq!(Host::new("v6", "::1").address(), "sftp://[::1]/");
        // the one-line form of the edit box
        let mut k = h.clone();
        k.key_file = Some("~/.ssh/id ed25519".into());
        k.jump = Some("bastion".into());
        assert_eq!(Host::from_line(&k.to_line()), Ok(k));
        assert_eq!(Host::from_line("db = admin@db.example.org").unwrap().address(), "sftp://admin@db.example.org/");
        assert!(Host::from_line("db = sftp://admin:pw@db.example.org/").is_err(), "no passwords");
        assert!(Host::from_line("no address here").is_err());
        assert_eq!(hs.merge(parse_ssh_config(CONFIG).hosts), 3, "web is there already");
        std::fs::write(dir.join(FILE_NAME), "this is [not toml").unwrap();
        assert_eq!(Hosts::load(&dir), Hosts::default());
        assert!(!dir.join(FILE_NAME).exists(), "the broken file was moved aside");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
