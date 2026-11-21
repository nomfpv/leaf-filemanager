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
}

fn default_scheme() -> String {
    "sftp".into()
}

impl Host {
    pub fn new(name: &str, host: &str) -> Host {
        Host { name: name.into(), scheme: default_scheme(), host: host.into(), port: None, user: None, start_path: None, key_file: None, jump: None }
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
    #[derive(Default)]
    struct Block {
        aliases: Vec<String>,
        hostname: Option<String>,
        user: Option<String>,
        port: Option<u16>,
        identity: Option<String>,
        jump: Option<String>,
    }
    let mut out = SshImport::default();
    let mut blocks: Vec<Block> = Vec::new();
    // None: before the first Host, or inside a Match block (both not imported)
    let mut cur: Option<usize> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = split_keyword(line);
        let lineno = n + 1;
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                let mut b = Block::default();
                for pat in words(value) {
                    if pat.contains(['*', '?']) || pat.starts_with('!') {
                        out.skipped.push(format!("line {lineno}: Host pattern \"{pat}\" (wildcards are not imported)"));
                    } else {
                        b.aliases.push(pat);
                    }
                }
                blocks.push(b);
                cur = Some(blocks.len() - 1);
            }
            "match" => {
                out.skipped.push(format!("line {lineno}: Match block (not imported)"));
                cur = None;
            }
            "include" => out.skipped.push(format!("line {lineno}: Include {value} (included files are not read)")),
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
                        _ => out.skipped.push(format!("line {lineno}: Port \"{v}\" is not a port number")),
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
        assert_eq!(hs.merge(parse_ssh_config(CONFIG).hosts), 3, "web is there already");
        std::fs::write(dir.join(FILE_NAME), "this is [not toml").unwrap();
        assert_eq!(Hosts::load(&dir), Hosts::default());
        assert!(!dir.join(FILE_NAME).exists(), "the broken file was moved aside");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
