//! OpenSSH `known_hosts`: reading, matching a server's key, and adding a line ("Trust and remember").
//!
//! Lines are `[marker] patterns keytype base64 [comment]`. Patterns are a comma list of names (`example.com`,
//! `[example.com]:2222`, wildcards `*`/`?`, negation `!name`) or one hashed name `|1|salt|hmac` (HMAC-SHA1 of the name with
//! the salt as key). The name looked up is `host` for port 22 and `[host]:port` for any other port, as OpenSSH writes it.
//! `@revoked` lines refuse a key; `@cert-authority` lines are skipped (host certificates are checked by their plain key).
//!
//! The verdict never accepts by itself what the file does not say: an unknown key goes to the user, a changed or revoked
//! key is refused.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use russh::keys::{HashAlg, PublicKey};

/// What the files say about a server's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// A line has this very key for this host.
    Known,
    /// No line has any key for the host: asked.
    Unknown,
    /// A line has a key of the same type for this host, and it is a different key. Refused.
    Changed { file: PathBuf, line: usize },
    /// The files have keys for this host, but only of other types (`known`) than the one the server shows. Refused like
    /// `Changed`: an attacker who cannot show the known key would otherwise offer another type and get the "unknown" dialog.
    /// (leaf asks the server for the known types first, so an honest server that has one shows it.)
    OtherType { known: Vec<String>, file: PathBuf, line: usize },
    /// The key is marked `@revoked`. Refused.
    Revoked { file: PathBuf, line: usize },
}

/// The name a host is written under: `host` on port 22, `[host]:port` on others.
pub fn host_name(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("[{host}]:{port}") }
}

/// `SHA256:...` as OpenSSH shows it.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// `ssh-ed25519`, `ssh-rsa`, `ecdsa-sha2-nistp256`, ...
pub fn key_type(key: &PublicKey) -> String {
    key.algorithm().as_str().to_string()
}

/// One parsed line.
struct Line<'a> {
    revoked: bool,
    patterns: &'a str,
    key_type: &'a str,
    key_b64: &'a str,
}

fn parse_line(raw: &str) -> Option<Line<'_>> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut it = line.split_whitespace();
    let mut first = it.next()?;
    let mut revoked = false;
    if let Some(marker) = first.strip_prefix('@') {
        match marker {
            "revoked" => revoked = true,
            // CA keys and anything else we do not know: not used for plain key checks
            _ => return None,
        }
        first = it.next()?;
    }
    let key_type = it.next()?;
    let key_b64 = it.next()?;
    Some(Line { revoked, patterns: first, key_type, key_b64 })
}

/// True when the pattern list of a line names `name` (`host` or `[host]:port`).
fn names_host(patterns: &str, name: &str) -> bool {
    if let Some(rest) = patterns.strip_prefix("|1|") {
        return hashed_match(rest, name);
    }
    let lower = name.to_ascii_lowercase();
    let mut hit = false;
    for p in patterns.split(',') {
        let p = p.to_ascii_lowercase();
        if let Some(neg) = p.strip_prefix('!') {
            if glob(neg, &lower) {
                return false; // a negated match wins over everything
            }
        } else if glob(&p, &lower) {
            hit = true;
        }
    }
    hit
}

fn hashed_match(rest: &str, name: &str) -> bool {
    let Some((salt, hash)) = rest.split_once('|') else { return false };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else { return false };
    let Ok(mut mac) = Hmac::<sha1::Sha1>::new_from_slice(&salt) else { return false };
    // OpenSSH hashes the name in lower case
    mac.update(name.to_ascii_lowercase().as_bytes());
    mac.verify_slice(&hash).is_ok()
}

/// OpenSSH's `*` and `?` patterns.
fn glob(pat: &str, s: &str) -> bool {
    let (p, s) = (pat.as_bytes(), s.as_bytes());
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Checks `key` for `host:port` against the text of known_hosts files (`(file, text)`, the user's own first).
pub fn check_texts(files: &[(PathBuf, String)], host: &str, port: u16, key: &PublicKey) -> Verdict {
    let name = host_name(host, port);
    let want_type = key_type(key);
    let mut known = false;
    let mut changed = None;
    let mut other_types = Vec::new();
    let mut other_at = None;
    for (file, text) in files {
        for (n, raw) in text.lines().enumerate() {
            let Some(l) = parse_line(raw) else { continue };
            if !names_host(l.patterns, &name) {
                continue;
            }
            let Ok(recorded) = PublicKey::from_openssh(&format!("{} {}", l.key_type, l.key_b64)) else {
                log::warn!("known_hosts: {} line {}: the key cannot be read", file.display(), n + 1);
                continue;
            };
            let same_key = recorded.key_data() == key.key_data();
            if l.revoked {
                if same_key {
                    return Verdict::Revoked { file: file.clone(), line: n + 1 };
                }
                continue;
            }
            if same_key {
                known = true;
            } else if recorded.algorithm() == key.algorithm() {
                changed.get_or_insert((file.clone(), n + 1));
            } else {
                let t = key_type(&recorded);
                other_at.get_or_insert((file.clone(), n + 1));
                if t != want_type && !other_types.contains(&t) {
                    other_types.push(t);
                }
            }
        }
    }
    // a revoked mark anywhere wins (handled above); then a matching line; a different key of the same type is refused, and so
    // is a key of a type the files do not have when they have another one for this host
    if known {
        Verdict::Known
    } else if let Some((file, line)) = changed {
        Verdict::Changed { file, line }
    } else if let Some((file, line)) = other_at {
        Verdict::OtherType { known: other_types, file, line }
    } else {
        Verdict::Unknown
    }
}

/// The key types the files have for `host:port` (not counting revoked lines), for the negotiation: leaf asks the server for
/// these first, as OpenSSH does.
pub fn known_types(paths: &[PathBuf], host: &str, port: u16) -> Vec<String> {
    let name = host_name(host, port);
    let mut out = Vec::new();
    for p in paths {
        let Ok(b) = std::fs::read(p) else { continue };
        for raw in String::from_utf8_lossy(&b).lines() {
            if let Some(l) = parse_line(raw)
                && !l.revoked
                && names_host(l.patterns, &name)
                && !out.iter().any(|t: &String| t == l.key_type)
            {
                out.push(l.key_type.to_string());
            }
        }
    }
    out
}

/// The family name of a host key algorithm (`ssh-rsa` for every RSA signature hash), as known_hosts writes the key type.
pub fn family(a: &russh::keys::Algorithm) -> String {
    match a {
        russh::keys::Algorithm::Rsa { .. } => "ssh-rsa".into(),
        a => a.as_str().to_string(),
    }
}

/// Reads the files (missing ones are skipped) and checks the key.
pub fn check(paths: &[PathBuf], host: &str, port: u16, key: &PublicKey) -> Verdict {
    let files: Vec<(PathBuf, String)> = paths
        .iter()
        .filter_map(|p| std::fs::read(p).ok().map(|b| (p.clone(), String::from_utf8_lossy(&b).into_owned())))
        .collect();
    check_texts(&files, host, port, key)
}

/// A host name or address that can be written into known_hosts as itself and never as a pattern: letters, digits and
/// `. _ - : %` only (`:` and `%` for IPv6 addresses and their zone), no more than 253 characters.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '%'))
}

/// The line "Trust and remember" adds: `name keytype base64`, the name in plain text (OpenSSH's default on Windows).
pub fn line_for(host: &str, port: u16, key: &PublicKey) -> Result<String, String> {
    if !valid_host(host) {
        return Err("the server's name cannot be written into known_hosts".into());
    }
    let k = key.to_openssh().map_err(|e| e.to_string())?;
    // `to_openssh` adds the comment when there is one; a server key has none, but keep only type and data
    let mut parts = k.split_whitespace();
    let (t, b) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    Ok(format!("{} {t} {b}", host_name(host, port)))
}

/// Adds the line for `host:port` to `path` (made, with its folder, when missing), on a line of its own. Atomic: the whole
/// new text is written to a file beside it, given the old file's permissions, and moved over it, so a crash leaves the old
/// file or the new one, never half a line.
pub fn remember(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<(), String> {
    use std::io::Write;
    let line = line_for(host, port, key)?;
    let dir = path.parent().ok_or("known_hosts has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut text = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    text.extend_from_slice(line.as_bytes());
    text.push(b'\n');
    let tmp = dir.join(format!(".known_hosts.leaf-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&text)?;
        f.sync_all()?;
        drop(f);
        if let Ok(m) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| e.to_string())
}

/// `~/.ssh/known_hosts` of the user running leaf (`%USERPROFILE%\.ssh\known_hosts` on Windows).
pub fn user_file() -> Option<PathBuf> {
    rf_core::hosts::ssh_config_path().map(|c| c.with_file_name("known_hosts"))
}

/// The system-wide file, read but never written.
pub fn global_file() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("PROGRAMDATA").map(|d| PathBuf::from(d).join("ssh").join("ssh_known_hosts"))
    } else {
        Some(PathBuf::from("/etc/ssh/ssh_known_hosts"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{Algorithm, PrivateKey};

    fn key(alg: Algorithm) -> PublicKey {
        PrivateKey::random(&mut russh::keys::key::safe_rng(), alg).unwrap().public_key().clone()
    }

    fn hashed(name: &str, salt: &[u8]) -> String {
        let mut mac = Hmac::<sha1::Sha1>::new_from_slice(salt).unwrap();
        mac.update(name.as_bytes());
        format!("|1|{}|{}", B64.encode(salt), B64.encode(mac.finalize().into_bytes()))
    }

    fn openssh(k: &PublicKey) -> String {
        let s = k.to_openssh().unwrap();
        s.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn matching_table() {
        let ed = key(Algorithm::Ed25519);
        let ed_other = key(Algorithm::Ed25519);
        let ec = key(Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP256 });
        let bad = key(Algorithm::Ed25519);
        let text = [
            "# a comment".to_string(),
            String::new(),
            format!("example.com,192.0.2.1 {}", openssh(&ed)),
            format!("[example.com]:2222 {}", openssh(&ed_other)),
            format!("{} {}", hashed("hashed.example.com", b"0123456789abcdefghij"), openssh(&ed)),
            format!("{} {}", hashed("[hashed.example.com]:2200", b"another salt 2345678"), openssh(&ec)),
            format!("*.wild.example.com,!no.wild.example.com {}", openssh(&ed)),
            format!("@revoked * {}", openssh(&bad)),
            format!("@cert-authority *.example.com {}", openssh(&ed_other)),
            "broken line".to_string(),
            format!("127.0.0.1 {} a comment", openssh(&ec)),
        ]
        .join("\n");
        let files = [(PathBuf::from("known_hosts"), text)];
        let v = |h: &str, p: u16, k: &PublicKey| check_texts(&files, h, p, k);
        let unknown = Verdict::Unknown;
        // plain names, a second name on the line, the case of the name
        assert_eq!(v("example.com", 22, &ed), Verdict::Known);
        assert_eq!(v("EXAMPLE.com", 22, &ed), Verdict::Known);
        assert_eq!(v("192.0.2.1", 22, &ed), Verdict::Known);
        // a port other than 22 is its own name
        assert_eq!(v("example.com", 2222, &ed_other), Verdict::Known);
        assert_eq!(v("example.com", 2222, &ed), Verdict::Changed { file: "known_hosts".into(), line: 4 }, "same type, other key: refused");
        assert_eq!(v("example.com", 2223, &ed), unknown);
        // changed key on the default port
        assert_eq!(v("example.com", 22, &ed_other), Verdict::Changed { file: "known_hosts".into(), line: 3 });
        // a key of another type than the one known: refused, with the known types named
        assert_eq!(v("example.com", 22, &ec), Verdict::OtherType { known: vec!["ssh-ed25519".into()], file: "known_hosts".into(), line: 3 });
        // hashed names, with and without a port; the name is hashed in lower case, as OpenSSH does
        assert_eq!(v("hashed.example.com", 22, &ed), Verdict::Known);
        assert_eq!(v("Hashed.Example.COM", 22, &ed), Verdict::Known);
        assert_eq!(v("Hashed.Example.COM", 22, &ed_other), Verdict::Changed { file: "known_hosts".into(), line: 5 });
        assert_eq!(v("hashed.example.com", 2200, &ec), Verdict::Known);
        assert_eq!(v("hashed.example.com", 2201, &ec), unknown);
        assert_eq!(v("other.example.com", 22, &ed), unknown);
        // wildcards and negation
        assert_eq!(v("a.wild.example.com", 22, &ed), Verdict::Known);
        assert_eq!(v("no.wild.example.com", 22, &ed), unknown);
        // revoked: refused for every host, even one the key is not listed for
        assert_eq!(v("example.com", 22, &bad), Verdict::Revoked { file: "known_hosts".into(), line: 8 });
        assert_eq!(v("new.example.org", 22, &bad), Verdict::Revoked { file: "known_hosts".into(), line: 8 });
        // a cert-authority line is not a host key
        assert_eq!(v("x.example.com", 22, &ed_other), unknown);
        // a comment after the key
        assert_eq!(v("127.0.0.1", 22, &ec), Verdict::Known);
        // nothing at all
        assert_eq!(check_texts(&[], "example.com", 22, &ed), unknown);
        assert!(fingerprint(&ed).starts_with("SHA256:"));
        assert_eq!(key_type(&ec), "ecdsa-sha2-nistp256");
    }

    #[test]
    fn remember_appends_a_line() {
        let dir = std::env::temp_dir().join(format!("leaf-kh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sub").join("known_hosts");
        let k = key(Algorithm::Ed25519);
        remember(&path, "127.0.0.1", 2222, &k).unwrap();
        // a file without a newline at its end gets one before the new line
        std::fs::write(&path, format!("{}no-newline.example.com {}", std::fs::read_to_string(&path).unwrap(), openssh(&k))).unwrap();
        remember(&path, "example.com", 22, &k).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("[127.0.0.1]:2222 ssh-ed25519 "));
        assert!(lines[2].starts_with("example.com ssh-ed25519 "));
        assert_eq!(check(&[path.clone()], "127.0.0.1", 2222, &k), Verdict::Known);
        assert_eq!(check(&[path], "example.com", 22, &k), Verdict::Known);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
