//! HTTP sign-in for one download: the server answered 401 with a Basic or Digest challenge, the caller asked the user, and the
//! answer rides on every request of that download (and only to the address's own origin: see `Headers::without_credentials`).
//! Memory only; `Debug` shows no user name and no password.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::Digest as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthScheme {
    Basic,
    Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DigestParams {
    nonce: String,
    opaque: Option<String>,
    sha256: bool,
    qop_auth: bool,
    stale: bool,
}

/// What a 401 asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub scheme: AuthScheme,
    pub realm: String,
    digest: Option<DigestParams>,
    /// Scheme, host and port of the server that asked (the address the 401 came from, after any redirects).
    pub origin: (String, String, u16),
}

/// The value of `name="value"` or `name=value` in a challenge's parameters.
fn param(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        let before_ok = at == 0 || matches!(lower.as_bytes()[at - 1], b' ' | b',' | b'\t');
        let rest = &text[at + name.len()..];
        if before_ok && let Some(v) = rest.trim_start().strip_prefix('=') {
            let v = v.trim_start();
            return Some(match v.strip_prefix('"') {
                Some(q) => q.split('"').next().unwrap_or("").to_string(),
                None => v.split([',', ' ']).next().unwrap_or("").to_string(),
            });
        }
        from = at + name.len();
    }
    None
}

/// A `WWW-Authenticate` header line split into its challenges: `Basic realm="a", Digest realm="a", nonce="x"` is two. A piece after a
/// comma that starts with a word without `=` begins a new challenge; the others are parameters of the one before.
fn split_challenges(line: &str) -> Vec<(String, String)> {
    let (mut pieces, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => pieces.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    pieces.push(cur);
    let mut out: Vec<(String, String)> = Vec::new();
    for piece in pieces.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let first = piece.split(' ').next().unwrap_or("");
        if first.contains('=') {
            if let Some((_, params)) = out.last_mut() {
                params.push_str(", ");
                params.push_str(piece);
            }
        } else {
            out.push((first.to_string(), piece[first.len()..].trim().to_string()));
        }
    }
    out
}

impl Challenge {
    /// The challenge to answer from the `WWW-Authenticate` header lines: Digest when the server offers it (MD5 or SHA-256, without
    /// `-sess`), whatever the order, else Basic. `None` when it offers neither (Negotiate, NTLM, Bearer: leaf cannot answer those).
    pub fn parse(lines: &[String]) -> Option<Challenge> {
        let mut basic = None;
        for (scheme, rest) in lines.iter().flat_map(|l| split_challenges(l)) {
            let rest = rest.as_str();
            if scheme.eq_ignore_ascii_case("digest") {
                let algo = param(rest, "algorithm").unwrap_or_else(|| "MD5".into()).to_ascii_uppercase();
                let sha256 = algo == "SHA-256";
                let qop = param(rest, "qop");
                let qop_auth = qop.as_deref().is_some_and(|q| q.split(',').any(|x| x.trim().eq_ignore_ascii_case("auth")));
                if (algo == "MD5" || sha256) && (qop.is_none() || qop_auth) && let Some(nonce) = param(rest, "nonce") {
                    let stale = param(rest, "stale").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                    return Some(Challenge { scheme: AuthScheme::Digest, realm: param(rest, "realm").unwrap_or_default(), origin: Default::default(), digest: Some(DigestParams { nonce, opaque: param(rest, "opaque"), sha256, qop_auth, stale }) });
                }
            } else if scheme.eq_ignore_ascii_case("basic") && basic.is_none() {
                basic = Some(Challenge { scheme: AuthScheme::Basic, realm: param(rest, "realm").unwrap_or_default(), origin: Default::default(), digest: None });
            }
        }
        basic
    }

    /// A Digest challenge that says the nonce has only expired (`stale=true`): the same sign-in is good with the new nonce.
    pub fn is_stale(&self) -> bool {
        self.digest.as_ref().is_some_and(|d| d.stale)
    }
}

/// The user's answer to a [`Challenge`].
pub struct WebAuth {
    user: String,
    password: String,
    challenge: std::sync::Mutex<Challenge>,
    origin: (String, String, u16),
    nc: AtomicU32,
}

impl WebAuth {
    pub fn new(challenge: &Challenge, user: &str, password: &str) -> WebAuth {        WebAuth { user: user.to_string(), password: password.to_string(), origin: challenge.origin.clone(), challenge: std::sync::Mutex::new(challenge.clone()), nc: AtomicU32::new(0) }
    }

    /// The server this sign-in is for: it is sent there and nowhere else.
    pub(crate) fn origin(&self) -> &(String, String, u16) {
        &self.origin
    }

    /// The server said the Digest nonce is stale: the new one (from its new challenge) is used from now on, by every connection.
    /// `false` when that challenge is not a stale Digest one of the same realm.
    pub fn renew(&self, fresh: &Challenge) -> bool {
        let mut ch = self.challenge.lock().unwrap_or_else(|e| e.into_inner());
        if !fresh.is_stale() || fresh.scheme != AuthScheme::Digest || fresh.realm != ch.realm || ch.digest.is_none() {
            return false;
        }
        ch.digest = fresh.digest.clone();
        self.nc.store(0, Ordering::Relaxed);
        true
    }

    /// The `Authorization` header's value for a GET of `uri` (an absolute address).
    pub(crate) fn header(&self, uri: &str) -> String {
        self.header_for("GET", uri)
    }

    /// The `Authorization` header's value for a request with `method` (`GET`, `PROPFIND`, `PUT`...) to `uri` (an absolute address):
    /// the Digest response covers the method, so every method of a WebDAV session asks for its own.
    pub fn header_for(&self, method: &str, uri: &str) -> String {
        let ch = self.challenge.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match &ch.digest {
            None => format!("Basic {}", base64(format!("{}:{}", self.user, self.password).as_bytes())),
            Some(d) => {
                let hash = |s: String| -> String {
                    if d.sha256 { hex(&sha2::Sha256::digest(s.as_bytes())) } else { hex(&md5::Md5::digest(s.as_bytes())) }
                };
                let target = {
                    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
                    let t = rest.find(['/', '?']).map_or("/", |i| &rest[i..]);
                    let t = t.split('#').next().unwrap_or("/");
                    if t.starts_with('?') { format!("/{t}") } else { t.to_string() }
                };
                let realm = &ch.realm;
                let ha1 = hash(format!("{}:{realm}:{}", self.user, self.password));
                let ha2 = hash(format!("{method}:{target}"));
                let nc = self.nc.fetch_add(1, Ordering::Relaxed) + 1;
                let nonce = &d.nonce;
                let mut out = format!("Digest username=\"{}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{target}\"", self.user.replace('\\', "\\\\").replace('"', "\\\""));
                if d.qop_auth {
                    let cnonce = hash(format!("{nc}{:?}{target}", std::time::SystemTime::now()))[..16].to_string();
                    let response = hash(format!("{ha1}:{nonce}:{nc:08x}:{cnonce}:auth:{ha2}"));
                    out.push_str(&format!(", qop=auth, nc={nc:08x}, cnonce=\"{cnonce}\", response=\"{response}\""));
                } else {
                    out.push_str(&format!(", response=\"{}\"", hash(format!("{ha1}:{nonce}:{ha2}"))));
                }
                if let Some(o) = &d.opaque {
                    out.push_str(&format!(", opaque=\"{o}\""));
                }
                if d.sha256 {
                    out.push_str(", algorithm=SHA-256");
                }
                out
            }
        }
    }
}

impl fmt::Debug for WebAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WebAuth(user and password hidden)")
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
