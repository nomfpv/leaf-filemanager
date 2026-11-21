//! The address of a place that is not a local folder: `scheme://authority/path` (`sftp://user@example.com:22/srv/www`,
//! `ftp://example.com/pub`, `mem://demo/`, `archive://...`). Plain local paths never become a `VPath` in the app: they stay
//! `Location::Path`, so the hot path does not change (PLAN 2.2).
//!
//! Credentials never live in the address: `user:password@host` is refused. The path is kept decoded, absolute (starts with
//! `/`), without `.`/`..` segments and without a trailing slash (except the root `/`). The text form writes `%` as `%25` so a
//! parse of the display gives the same value back; `%XX` in a typed address is decoded.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VPath {
    scheme: String,
    authority: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VPathError {
    /// No `scheme://` in front.
    NoScheme,
    /// The scheme has characters other than letters, digits, `+`, `-`, `.`.
    BadScheme,
    /// `user:password@host`: secrets are not kept in addresses.
    Password,
    /// A port that is not a number from 1 to 65535.
    BadPort,
    /// `%` not followed by two hex digits, or a decoded path that is not UTF-8.
    BadEscape,
}

impl fmt::Display for VPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VPathError::NoScheme => "the address has no scheme (like sftp://)",
            VPathError::BadScheme => "the scheme of the address is not valid",
            VPathError::Password => "passwords are not written in addresses; save them with the host instead",
            VPathError::BadPort => "the port is not a number from 1 to 65535",
            VPathError::BadEscape => "the address has a broken % escape",
        })
    }
}

impl std::error::Error for VPathError {}

impl VPath {
    /// Builds one from parts; `path` is normalized the same way a parse does it.
    pub fn new(scheme: &str, authority: &str, path: &str) -> Result<VPath, VPathError> {
        if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return Err(VPathError::BadScheme);
        }
        check_authority(authority)?;
        Ok(VPath { scheme: scheme.to_ascii_lowercase(), authority: authority.to_string(), path: normalize(path) })
    }

    pub fn parse(s: &str) -> Result<VPath, VPathError> {
        let s = s.trim();
        let (scheme, rest) = s.split_once("://").ok_or(VPathError::NoScheme)?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        VPath::new(scheme, authority, &decode(path)?)
    }

    /// True when `s` looks like one of our addresses (a scheme, then `://`), without checking the rest.
    pub fn looks_like(s: &str) -> bool {
        let s = s.trim();
        s.split_once("://").is_some_and(|(sc, _)| {
            sc.len() > 1 && sc.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && sc.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }
    /// `user@host:port` as written (may be empty, for `mem:///`).
    pub fn authority(&self) -> &str {
        &self.authority
    }
    /// The decoded absolute path (`/`, `/srv/www`).
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn user(&self) -> Option<&str> {
        self.authority.rsplit_once('@').map(|(u, _)| u).filter(|u| !u.is_empty())
    }
    /// The host without user and port (`[::1]` keeps its brackets off: `::1`).
    pub fn host(&self) -> &str {
        let hp = self.authority.rsplit_once('@').map_or(self.authority.as_str(), |(_, h)| h);
        if let Some(rest) = hp.strip_prefix('[') {
            return rest.split_once(']').map_or(rest, |(h, _)| h);
        }
        match hp.rsplit_once(':') {
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => hp,
        }
    }
    pub fn port(&self) -> Option<u16> {
        split_port(&self.authority).1
    }

    pub fn is_root(&self) -> bool {
        self.path == "/"
    }
    /// The last part of the path ("" for the root).
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }
    /// The path's parts, without the empty root part.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.path.split('/').filter(|s| !s.is_empty())
    }
    pub fn parent(&self) -> Option<VPath> {
        if self.is_root() {
            return None;
        }
        let cut = self.path.rfind('/').unwrap_or(0);
        Some(VPath { path: if cut == 0 { "/".into() } else { self.path[..cut].to_string() }, ..self.clone() })
    }
    /// The child `name` (one part; a `/` in it adds more parts, `..` goes up).
    pub fn join(&self, name: &str) -> VPath {
        let mut p = self.path.clone();
        if !p.ends_with('/') {
            p.push('/');
        }
        p.push_str(name);
        VPath { path: normalize(&p), ..self.clone() }
    }
    /// The same place on the same server, another path.
    pub fn with_path(&self, path: &str) -> VPath {
        VPath { path: normalize(path), ..self.clone() }
    }
    /// True when `self` is `other` or lies under it (same scheme and authority).
    pub fn starts_with(&self, other: &VPath) -> bool {
        self.scheme == other.scheme
            && self.authority == other.authority
            && (other.is_root() || self.path == other.path || self.path.strip_prefix(&other.path).is_some_and(|r| r.starts_with('/')))
    }
    /// `scheme://authority/` (the top of this server).
    pub fn root(&self) -> VPath {
        VPath { path: "/".into(), ..self.clone() }
    }
}

impl fmt::Display for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.authority)?;
        for c in self.path.chars() {
            match c {
                '%' => f.write_str("%25")?,
                c if c.is_control() => {
                    let mut b = [0u8; 4];
                    for byte in c.encode_utf8(&mut b).bytes() {
                        write!(f, "%{byte:02X}")?;
                    }
                }
                c => write!(f, "{c}")?,
            }
        }
        Ok(())
    }
}

fn split_port(auth: &str) -> (&str, Option<u16>) {
    let hp = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let tail = match hp.rfind(']') {
        Some(i) => &hp[i + 1..],
        None => hp,
    };
    match tail.rsplit_once(':') {
        Some((_, p)) => (hp, p.parse().ok()),
        None => (hp, None),
    }
}

fn check_authority(auth: &str) -> Result<(), VPathError> {
    if let Some((user, _)) = auth.rsplit_once('@')
        && user.contains(':')
    {
        return Err(VPathError::Password);
    }
    let hp = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    let tail = match hp.rfind(']') {
        Some(i) => &hp[i + 1..],
        None if hp.matches(':').count() > 1 => "", // a bare IPv6 address: no port part
        None => hp,
    };
    // (only digits are a port: `archive://C:%5C...` keeps an opaque authority)
    if let Some((_, p)) = tail.rsplit_once(':')
        && (p.is_empty() || p.bytes().all(|b| b.is_ascii_digit()))
    {
        match p.parse::<u16>() {
            Ok(n) if n > 0 => {}
            _ => return Err(VPathError::BadPort),
        }
    }
    Ok(())
}

fn decode(s: &str) -> Result<String, VPathError> {
    if !s.contains('%') {
        return Ok(s.to_string());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or(VPathError::BadEscape)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| VPathError::BadEscape)?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| VPathError::BadEscape)
}

fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::with_capacity(p.len() + 1);
    for s in &parts {
        out.push('/');
        out.push_str(s);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table() {
        // text, scheme, authority, user, host, port, path, shown again as
        let rows: &[(&str, &str, &str, Option<&str>, &str, Option<u16>, &str, &str)] = &[
            ("sftp://user@example.com:22/srv/www", "sftp", "user@example.com:22", Some("user"), "example.com", Some(22), "/srv/www", "sftp://user@example.com:22/srv/www"),
            ("SFTP://example.com", "sftp", "example.com", None, "example.com", None, "/", "sftp://example.com/"),
            ("ftp://example.org/pub/", "ftp", "example.org", None, "example.org", None, "/pub", "ftp://example.org/pub"),
            ("mem://demo/", "mem", "demo", None, "demo", None, "/", "mem://demo/"),
            ("mem:///a/./b/../c", "mem", "", None, "", None, "/a/c", "mem:///a/c"),
            ("sftp://[::1]:2222/x", "sftp", "[::1]:2222", None, "::1", Some(2222), "/x", "sftp://[::1]:2222/x"),
            ("sftp://host.example.net/a%20b/100%25", "sftp", "host.example.net", None, "host.example.net", None, "/a b/100%", "sftp://host.example.net/a b/100%25"),
            ("archive://C:%5Cdata%5Cx.zip/inner/file.txt", "archive", "C:%5Cdata%5Cx.zip", None, "C:%5Cdata%5Cx.zip", None, "/inner/file.txt", "archive://C:%5Cdata%5Cx.zip/inner/file.txt"),
        ];
        for &(text, scheme, auth, user, host, port, path, shown) in rows {
            let v = VPath::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(v.scheme(), scheme, "{text}");
            assert_eq!(v.authority(), auth, "{text}");
            assert_eq!(v.user(), user, "{text}");
            assert_eq!(v.host(), host, "{text}");
            assert_eq!(v.port(), port, "{text}");
            assert_eq!(v.path(), path, "{text}");
            assert_eq!(v.to_string(), shown, "{text}");
            assert_eq!(VPath::parse(&v.to_string()).unwrap(), v, "{text} round trip");
        }
    }

    #[test]
    fn refused() {
        assert_eq!(VPath::parse("/srv/www"), Err(VPathError::NoScheme));
        assert_eq!(VPath::parse("sftp://user:secret@example.com/"), Err(VPathError::Password));
        assert_eq!(VPath::parse("sftp://example.com:0/"), Err(VPathError::BadPort));
        assert_eq!(VPath::parse("sftp://example.com:99999/"), Err(VPathError::BadPort));
        assert_eq!(VPath::parse("sftp://example.com/%zz"), Err(VPathError::BadEscape));
        assert_eq!(VPath::parse("s p://example.com/"), Err(VPathError::BadScheme));
        assert!(VPath::looks_like("sftp://x"));
        assert!(!VPath::looks_like(r"C:\data"));
        assert!(!VPath::looks_like("c://x"), "a drive letter is not a scheme");
    }

    #[test]
    fn walk() {
        let v = VPath::parse("sftp://example.com/a/b").unwrap();
        assert_eq!(v.name(), "b");
        assert_eq!(v.parent().unwrap().path(), "/a");
        assert_eq!(v.parent().unwrap().parent().unwrap().path(), "/");
        assert!(v.root().parent().is_none());
        assert_eq!(v.join("c d").path(), "/a/b/c d");
        assert_eq!(v.join("../x").path(), "/a/x");
        assert_eq!(v.segments().collect::<Vec<_>>(), ["a", "b"]);
        assert!(v.join("c").starts_with(&v));
        assert!(!VPath::parse("sftp://example.com/a/bc").unwrap().starts_with(&v));
        assert!(v.starts_with(&v.root()));
    }
}
