//! `WebDavFs`: `dav://user@host:port/path` (plain http) and `davs://...` (https) (X9) through `leaf-vfs`, on leaf's own small
//! client (`http.rs`: any HTTP method, which the HTTP libraries in the tree refuse for WebDAV's), `quick-xml` for the server's `207 Multi-Status` answers. Every call runs on the
//! caller's worker thread (no runtime of its own).
//!
//! - Listings are `PROPFIND` with `Depth: 1`, a stat is `Depth: 0`; reads are `GET` (with `Range` from an offset); a new folder is
//!   `MKCOL`; a move is `MOVE`; a delete is `DELETE`. Free space comes from the quota properties where the server has them.
//! - Writes never put bytes under a name that matters: `PUT` goes to a hidden temporary name beside the file
//!   (`.name.leaf-up-<n>`, created only if nothing is there: `If-None-Match: *`) and a `MOVE` puts it in place afterwards.
//!   The `MOVE` carries `Overwrite: F` (the server refuses when the name is taken) unless the caller said replace (the
//!   conflict dialog's answer), and then only a file is replaced. A failed upload removes its temporary file. The generic copy's
//!   own part names (`.name.<n>.leaf-part`) are written in place the same create-only way, then renamed by `MOVE`.
//!   Small files (up to `SMALL`) are sent with a length and can be sent again after a sign-in; bigger ones stream (chunked).
//! - Sign-in: the server's `401` names Basic or Digest (`leaf-download`'s code); the user and password are asked in leaf's
//!   dialog (the same as SFTP and FTP, the keyring seam included) and kept in memory for this run, keyed by scheme, server and
//!   port. Basic over plain `dav://` is sent only after the user answered the dialog that says the password crosses the network
//!   as it is; it is never sent on a stale answer. The password is never in an address, a log line or a file: the HTTP client's
//!   log targets are dropped above `warn` (see `QUIET_LOG_TARGETS`).
//! - `davs://`: the system's certificates; one it does not trust is asked about (the FTPS dialog and trusted-certificate file:
//!   "Trust this certificate" pins the SHA-256 of that server's, "Trust once" for this run).
//! - Redirects are not followed, except `/folder` to `/folder/`. Proxies are not used. No locks (`LOCK` is never sent).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use leaf_download::{AuthScheme, Challenge, WebAuth};
use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, Space, VPath, WriteMode, WriteOpts, WriteSink};

use crate::conn::Config;
use crate::ftp::{CertCheck, is_part_name, side_name};
use crate::http::{Endpoint, Fail, Payload, Pool, Reply};
use crate::prompt::{Prompter, Secret, SecretQuestion};
use crate::secrets::SecretId;

/// A file up to this size is held in memory and sent with its length (and can be sent again after a sign-in); a bigger one is
/// streamed.
const SMALL: usize = 8 << 20;
/// What one answer to a `PROPFIND` may hold.
const ANSWER_MAX: u64 = 64 << 20;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------------------------ the server

#[derive(Debug, Clone)]
struct Target {
    /// `scheme://user@host:port` (or without the user): the state's key.
    key: String,
    host: String,
    port: u16,
    user: Option<String>,
    tls: bool,
}

impl Target {
    fn scheme(&self) -> &'static str {
        if self.tls { "davs" } else { "dav" }
    }
    /// `http://host:port` / `https://host:port` (an IPv6 name in brackets).
    fn base(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        format!("{}://{host}:{}", if self.tls { "https" } else { "http" }, self.port)
    }
    /// `host:port` as the certificate file names it.
    fn cert_name(&self) -> String {
        format!("{}:{}", self.host.to_ascii_lowercase(), self.port)
    }
    /// `user@host` as the password dialog shows it.
    fn account(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        let host = if self.port == default { self.host.clone() } else { format!("{}:{}", self.host, self.port) };
        match &self.user {
            Some(u) => format!("{u}@{host}"),
            None => host,
        }
    }
}

fn target(cfg: &Config, v: &VPath) -> Result<Target, FsError> {
    let host = v.host().to_string();
    if host.is_empty() {
        return Err(FsError::Other("The address has no server name".into()));
    }
    if !crate::known_hosts::valid_host(&host) {
        return Err(FsError::Other("The server's name has characters a server name cannot have".into()));
    }
    let scheme = v.scheme().to_ascii_lowercase();
    let tls = match scheme.as_str() {
        "dav" => false,
        "davs" => true,
        _ => return Err(FsError::Unsupported),
    };
    let port = v.port().unwrap_or(if tls { 443 } else { 80 });
    let saved = cfg.config_dir.as_deref().map(rf_core::hosts::Hosts::load).unwrap_or_default();
    let h = saved.hosts.into_iter().find(|h| {
        h.scheme.eq_ignore_ascii_case(&scheme) && h.host.eq_ignore_ascii_case(&host) && h.port.unwrap_or(if tls { 443 } else { 80 }) == port && (v.user().is_none() || h.user.as_deref() == v.user())
    });
    let user = v.user().map(str::to_string).or_else(|| h.and_then(|h| h.user)).filter(|u| !u.is_empty());
    let key = match &user {
        Some(u) => format!("{scheme}://{u}@{}:{port}", host.to_ascii_lowercase()),
        None => format!("{scheme}://{}:{port}", host.to_ascii_lowercase()),
    };
    Ok(Target { key, host, port, user, tls })
}

/// What the server asked and the user answered, for this run.
#[derive(Default)]
struct Auth {
    /// The sign-in that rides on every request.
    web: Option<Arc<WebAuth>>,
    /// The user and the password as typed (or read from the keyring): used again for a new challenge of the same server.
    cred: Option<(String, Secret)>,
    /// Goes up whenever `web` is replaced (a request that waited for another one's dialog sees it).
    generation: u64,
    /// The user answered the dialog that says Basic over plain http sends the password as it is.
    basic_plain_ok: bool,
    /// Refusals since the last request that worked.
    refused: u32,
    /// Saved in the keyring once the server takes it (not before).
    keep: Option<(SecretId, Secret)>,
}

/// Where requests go: the endpoint (with its TLS config for https) and the certificate check behind it.
struct Client {
    endpoint: Endpoint,
    check: Option<Arc<CertCheck>>,
}

#[derive(Default)]
struct Server {
    auth: Mutex<Auth>,
    /// One dialog at a time per server: a second pane waits for the first one's answer.
    gate: Mutex<()>,
    client: Mutex<Option<Arc<Client>>>,
    /// Open connections that are free.
    pool: Arc<Pool>,
    /// A request was answered (not refused for the login).
    connected: AtomicBool,
}

#[derive(Default)]
struct Shared {
    servers: Mutex<HashMap<String, Arc<Server>>>,
    /// `(host:port, fingerprint)` of certificates trusted for this run.
    trusted_once: Mutex<HashSet<(String, String)>>,
}

struct Inner {
    cfg: Config,
    prompter: Arc<dyn Prompter>,
    shared: Shared,
}

pub struct WebDavFs {
    inner: Arc<Inner>,
}

impl WebDavFs {
    pub fn new(cfg: Config, prompter: Arc<dyn Prompter>) -> WebDavFs {
        WebDavFs { inner: Arc::new(Inner { cfg, prompter, shared: Shared::default() }) }
    }

    /// Forgets the sign-ins this run keeps in memory for the server `host:port` (both schemes, any user).
    pub fn forget_passwords(&self, host: &str, port: u16) {
        let suffix = format!("{}:{port}", host.to_ascii_lowercase());
        for (k, s) in lock(&self.inner.shared.servers).iter() {
            if k.ends_with(&suffix) && k[..k.len() - suffix.len()].ends_with(['/', '@']) {
                let mut a = lock(&s.auth);
                a.web = None;
                a.cred = None;
                a.keep = None;
                a.generation += 1;
                s.connected.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Whether the server of `v` has answered a request in this run (never connects).
    pub fn target_connected(&self, v: &VPath) -> bool {
        let Ok(t) = target(&self.inner.cfg, v) else { return false };
        lock(&self.inner.shared.servers).get(&t.key).is_some_and(|s| s.connected.load(Ordering::Relaxed))
    }

    /// Copies `from` (a file or a folder) to `to` on the same server, done by the server (`COPY`): nothing crosses the line.
    /// `to` must be free, or (with `replace`, the conflict dialog's answer) a file. `Unsupported` when the server cannot or
    /// the two are not on one server: nothing was done, copy as before.
    pub fn copy_on_server(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() || from.is_root() || to.is_root() {
            return Err(FsError::Unsupported);
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be copied into itself".into()));
        }
        let t = target(&self.inner.cfg, from)?;
        let src = self.inner.stat(&t, from.path())?.ok_or(FsError::NotFound)?;
        let overwrite = match self.inner.stat(&t, to.path())? {
            None => false,
            Some(d) if replace && !src.is_dir && !d.is_dir => true,
            // (a folder is merged by the generic copy, never by one `COPY`)
            Some(_) if src.is_dir || replace => return Err(FsError::Unsupported),
            Some(_) => return Err(FsError::Exists),
        };
        let r = self.inner.call(&t, "COPY", from.path(), &[("Destination", self.inner.destination(&t, to.path())), ("Overwrite", flag(overwrite))], Payload::None)?;
        match r.status {
            200 | 201 | 204 => Ok(()),
            412 => Err(FsError::Exists),
            // (a server without COPY, or one that copies no folders: the generic copy does it)
            405 | 501 => Err(FsError::Unsupported),
            s => Err(status_error(s)),
        }
    }
}

fn flag(overwrite: bool) -> String {
    if overwrite { "T" } else { "F" }.to_string()
}

// ------------------------------------------------------------------------------------------------ requests

/// The path of a request as the server takes it: `/~` and `/~/x` stand for the root (a WebDAV server has no login folder).
fn spath(p: &str) -> String {
    match p.strip_prefix("/~") {
        Some("") => "/".to_string(),
        Some(rest) if rest.starts_with('/') => rest.to_string(),
        _ => p.to_string(),
    }
}

/// A path as it goes in the request line: every byte that is not unreserved (or `/`) as `%XX`.
fn enc(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unenc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn trim_slash(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    if t.is_empty() { "/" } else { t }
}

fn io_words(io: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match io.kind() {
        K::TimedOut | K::WouldBlock => "the server did not answer in time".into(),
        K::ConnectionRefused => "the server does not take connections on this port".into(),
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::UnexpectedEof | K::NotConnected => "the connection was closed".into(),
        _ => format!("the connection failed ({io})"),
    }
}

/// An HTTP status leaf did not expect, as leaf says it.
fn status_error(s: u16) -> FsError {
    match s {
        401 => FsError::Auth("the user name or password is wrong".into()),
        403 => FsError::Denied,
        404 | 410 => FsError::NotFound,
        405 | 501 => FsError::Unsupported,
        409 => FsError::Other("The folder it belongs in is not there on the server".into()),
        412 => FsError::Exists,
        413 => FsError::Other("The server refuses a file this big".into()),
        423 => FsError::Other("The server has this locked".into()),
        507 => FsError::Other("The server has no room for this".into()),
        408 | 429 | 502 | 503 | 504 => FsError::Offline(format!("the server is busy or did not answer ({s})")),
        _ => FsError::Other(format!("The server refused this ({s})")),
    }
}

impl Inner {
    fn server(&self, t: &Target) -> Arc<Server> {
        lock(&self.shared.servers).entry(t.key.clone()).or_default().clone()
    }

    /// `Destination` of a `MOVE` or `COPY`: the absolute address of `to`.
    fn destination(&self, t: &Target, to: &str) -> String {
        format!("{}{}", t.base(), enc(&spath(to)))
    }

    /// Where the server of `t` is reached (made on first use; again after the user trusted a certificate).
    fn client(&self, t: &Target, server: &Server) -> Result<Arc<Client>, FsError> {
        let mut g = lock(&server.client);
        if let Some(c) = &*g {
            return Ok(c.clone());
        }
        let check = if t.tls { Some(crate::ftp::new_cert_check(crate::ftp::trusted_certs(&self.cfg, &self.shared.trusted_once, &t.cert_name()))?) } else { None };
        let tls = match &check {
            Some(c) => Some(Arc::new(crate::ftp::tls_config(c.clone())?)),
            None => None,
        };
        let endpoint = Endpoint { host: t.host.clone(), port: t.port, tls, connect_timeout: self.cfg.connect_timeout, io_timeout: self.cfg.op_timeout };
        let c = Arc::new(Client { endpoint, check });
        *g = Some(c.clone());
        Ok(c)
    }

    /// Sends one request, answering the server's questions on the way: a sign-in (asked once; again when refused) and a
    /// certificate the system does not trust. `path` is the server path (not yet percent-encoded).
    fn call(&self, t: &Target, method: &str, path: &str, headers: &[(&str, String)], mut payload: Payload) -> Result<Reply, FsError> {
        let server = self.server(t);
        let mut path = spath(path);
        let (mut cert_asks, mut redirected) = (0, false);
        loop {
            let client = self.client(t, &server)?;
            let target = enc(&path);
            let (authorization, generation) = {
                let a = lock(&server.auth);
                (a.web.as_ref().map(|w| w.header_for(method, &format!("{}{target}", t.base()))), a.generation)
            };
            let mut all: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            if let Some(h) = authorization {
                all.push(("Authorization".into(), h));
            }
            let resp = match crate::http::send(&server.pool, &client.endpoint, method, &target, &all, &mut payload) {
                Ok(r) => r,
                Err(Fail::Fs(e)) => return Err(e),
                Err(Fail::Io(e)) => {
                    let refusal = client.check.as_ref().and_then(|c| lock(&c.refusal).take());
                    if let (Some((fp, why)), Some(check)) = (refusal, &client.check) {
                        if cert_asks >= 2 {
                            return Err(FsError::Other(format!("The server's certificate is not trusted ({why}); leaf does not connect. It shows {fp}.")));
                        }
                        cert_asks += 1;
                        crate::ftp::ask_cert(&self.cfg, &*self.prompter, &self.shared.trusted_once, &t.cert_name(), check, fp, why, "webdav")?;
                        // (a new check that knows the trusted fingerprint; connections made with the old one go)
                        *lock(&server.client) = None;
                        server.pool.clear();
                        continue;
                    }
                    return Err(FsError::Offline(io_words(&e)));
                }
            };
            match resp.status {
                401 => {
                    let lines = resp.headers_of("www-authenticate");
                    drop(resp);
                    let Some(mut ch) = Challenge::parse(&lines) else {
                        return Err(FsError::Auth("the server wants a sign-in leaf cannot do (only Basic and Digest are supported)".into()));
                    };
                    ch.origin = (if t.tls { "https" } else { "http" }.into(), t.host.to_ascii_lowercase(), t.port);
                    if matches!(payload, Payload::Stream(_)) {
                        return Err(FsError::Auth("the server asked for the sign-in again during the upload".into()));
                    }
                    self.sign_in(t, &server, &ch, generation)?;
                    continue;
                }
                301 | 302 | 307 | 308 if !redirected && !matches!(payload, Payload::Stream(_)) => {
                    // only a folder's own address with the slash the server wants
                    let loc = resp.header("location").unwrap_or_default().to_string();
                    let at = loc.split_once("://").map_or(loc.as_str(), |(_, r)| r.find('/').map_or("/", |i| &r[i..]));
                    let at = unenc(at.split(['?', '#']).next().unwrap_or(""));
                    if trim_slash(&at) == trim_slash(&path) && at != path {
                        redirected = true;
                        path = at;
                        continue;
                    }
                    return Err(FsError::Other("The server moved this address; use the address it shows now".into()));
                }
                _ => {}
            }
            server.connected.store(true, Ordering::Relaxed);
            {
                let mut a = lock(&server.auth);
                a.refused = 0;
                // (the server took the sign-in: now it may be kept, if the user asked)
                if let Some((id, pw)) = a.keep.take() {
                    self.cfg.vault.keep_if_asked(&id, &pw);
                }
            }
            return Ok(resp);
        }
    }
    /// The server said `401`: renew a stale Digest nonce, or use the user's sign-in, or ask for it. `generation`: the state the
    /// refused request was made with (another request may have signed in while this one waited).
    fn sign_in(&self, t: &Target, server: &Server, ch: &Challenge, generation: u64) -> Result<(), FsError> {
        let _gate = lock(&server.gate);
        let (retry, known) = {
            let mut a = lock(&server.auth);
            if a.generation != generation {
                return Ok(());
            }
            if let Some(w) = &a.web {
                // the nonce only expired: the same sign-in is good with the new one
                if w.renew(ch) {
                    return Ok(());
                }
                // refused: what was sent is wrong
                a.refused += 1;
                if a.refused >= 3 {
                    a.refused = 0;
                    return Err(FsError::Auth("the user name or password is wrong".into()));
                }
            }
            let refused = a.web.take().is_some();
            if refused {
                if let Some((user, pw)) = a.cred.take() {
                    self.cfg.vault.refused(&SecretId::password(t.scheme(), &user, &t.host, t.port), &pw);
                }
            }
            // (Basic over plain http is never sent on a sign-in the user did not confirm in the dialog that says so)
            let plain_basic = !t.tls && ch.scheme == AuthScheme::Basic && !a.basic_plain_ok;
            let known = if refused || plain_basic {
                None
            } else {
                a.cred.clone().or_else(|| t.user.as_ref().and_then(|u| self.cfg.vault.get(&SecretId::password(t.scheme(), u, &t.host, t.port)).map(|p| (u.clone(), p))))
            };
            (refused, known)
        };
        let plain_basic = !t.tls && ch.scheme == AuthScheme::Basic;
        let (user, pw, asked) = match known {
            Some((u, p)) => (u, p, false),
            None => {
                let vault = &self.cfg.vault;
                let q = match &t.user {
                    Some(_) => SecretQuestion::Password { account: t.account(), retry, unencrypted: plain_basic, keyring: vault.state() },
                    None => SecretQuestion::Login { place: t.account(), user: String::new(), retry, keyring: vault.state() },
                };
                let answer = crate::prompt::ask_secret_blocking(&*self.prompter, q).ok_or(FsError::Cancelled)?;
                let user = t.user.clone().or_else(|| answer.user().map(str::to_string)).filter(|u| !u.is_empty()).ok_or_else(|| FsError::Auth("no user name was given".into()))?;
                (user, answer, true)
            }
        };
        let web = Arc::new(WebAuth::new(ch, &user, pw.as_str()));
        let mut a = lock(&server.auth);
        a.web = Some(web);
        a.generation += 1;
        if asked {
            if plain_basic {
                a.basic_plain_ok = true;
            }
            a.keep = Some((SecretId::password(t.scheme(), &user, &t.host, t.port), pw.clone()));
        }
        a.cred = Some((user, pw.remembered(false)));
        Ok(())
    }

    // ---------------------------------------------------------------------------------------- PROPFIND

    fn propfind(&self, t: &Target, path: &str, depth: u8, body: &str) -> Result<Option<Vec<Res>>, FsError> {
        let r = self.call(t, "PROPFIND", path, &[("Depth", depth.to_string()), ("Content-Type", "application/xml; charset=utf-8".into())], Payload::Bytes(body.as_bytes()))?;
        match r.status {
            207 | 200 => {
                let text = r.text(ANSWER_MAX).map_err(|_| FsError::Other("The server's listing is too big or not readable".into()))?;
                parse_multistatus(&text).map(Some)
            }
            404 | 410 => Ok(None),
            s => Err(status_error(s)),
        }
    }

    /// What is at `path`, `None` when nothing is.
    fn stat(&self, t: &Target, path: &str) -> Result<Option<Res>, FsError> {
        let p = spath(path);
        if trim_slash(&p) == "/" {
            return Ok(Some(Res { path: "/".into(), is_dir: true, ..Res::default() }));
        }
        let Some(items) = self.propfind(t, &p, 0, PROPS)? else { return Ok(None) };
        Ok(items.into_iter().next())
    }

    fn move_to(&self, t: &Target, from: &str, to: &str, overwrite: bool) -> Result<(), FsError> {
        let r = self.call(t, "MOVE", from, &[("Destination", self.destination(t, to)), ("Overwrite", flag(overwrite))], Payload::None)?;
        match r.status {
            200 | 201 | 204 => Ok(()),
            s => Err(status_error(s)),
        }
    }

    fn delete(&self, t: &Target, path: &str) -> Result<(), FsError> {
        let r = self.call(t, "DELETE", path, &[], Payload::None)?;
        match r.status {
            200 | 202 | 204 => Ok(()),
            207 => Err(FsError::Other("The server could not remove everything in it".into())),
            s => Err(status_error(s)),
        }
    }
}

const PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/><d:creationdate/></d:prop></d:propfind>"#;
const QUOTA: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:quota-available-bytes/><d:quota-used-bytes/></d:prop></d:propfind>"#;

// ------------------------------------------------------------------------------------------------ 207 answers

/// One `response` of a `207 Multi-Status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Res {
    /// The path the server named (percent-decoded, without the server's name).
    path: String,
    is_dir: bool,
    size: u64,
    modified_ms: i64,
    created_ms: i64,
    quota_available: Option<u64>,
    quota_used: Option<u64>,
}

impl Res {
    fn meta(&self) -> Meta {
        Meta { kind: if self.is_dir { Kind::Dir } else { Kind::File }, size: if self.is_dir { 0 } else { self.size }, modified_ms: self.modified_ms, created_ms: self.created_ms, mode: None }
    }
    fn name(&self) -> String {
        trim_slash(&self.path).rsplit('/').next().unwrap_or("").to_string()
    }
    fn entry(&self) -> Entry {
        let (m, c) = (self.modified_ms, self.created_ms);
        if self.is_dir { Entry::new(&self.name(), EntryKind::Dir, 0, m, c, rf_core::attrs::DIRECTORY) } else { Entry::new(&self.name(), EntryKind::File, self.size, m, c, 0) }
    }
}

/// The `response`s of a multi-status document. Properties count only from a `propstat` whose status is 2xx; elements are told
/// apart by their local name (the prefix is the server's choice).
fn parse_multistatus(xml: &str) -> Result<Vec<Res>, FsError> {
    use quick_xml::events::Event;
    let bad = || FsError::Other("The server's answer is not a WebDAV listing".into());
    let mut rd = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<Res> = None;
    // the propstat being read: its properties, and whether its status was 2xx
    let mut stat = Res::default();
    let mut stat_ok = true;
    let mut text = String::new();
    let mut path: Vec<String> = Vec::new();
    loop {
        match rd.read_event().map_err(|_| bad())? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_ascii_lowercase();
                match name.as_str() {
                    "response" => cur = Some(Res::default()),
                    "propstat" => {
                        stat = Res::default();
                        stat_ok = true;
                    }
                    _ => {}
                }
                text.clear();
                path.push(name);
            }
            Event::Empty(e) => {
                let name = e.local_name().as_ref().to_ascii_lowercase();
                if name == "collection" && path.last().is_some_and(|p| p == "resourcetype") {
                    stat.is_dir = true;
                }
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(c) => text.push_str(&c.xml10_content()),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    text.push(c);
                } else {
                    match &*r {
                        "amp" => text.push('&'),
                        "lt" => text.push('<'),
                        "gt" => text.push('>'),
                        "quot" => text.push('"'),
                        "apos" => text.push('\''),
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_ascii_lowercase();
                path.pop();
                let value = text.trim().to_string();
                match name.as_str() {
                    "href" if path.last().is_some_and(|p| p == "response") => {
                        if let Some(c) = cur.as_mut() {
                            if c.path.is_empty() {
                                let p = value.split_once("://").map_or(value.as_str(), |(_, r)| r.find('/').map_or("/", |i| &r[i..]));
                                c.path = unenc(p.split(['?', '#']).next().unwrap_or(""));
                            }
                        }
                    }
                    "status" if path.last().is_some_and(|p| p == "propstat") => {
                        // `HTTP/1.1 200 OK`
                        let code: u16 = value.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(200);
                        stat_ok = (200..300).contains(&code);
                    }
                    "getcontentlength" => stat.size = value.parse().unwrap_or(0),
                    "getlastmodified" => stat.modified_ms = chrono::DateTime::parse_from_rfc2822(&value).map_or(0, |d| d.timestamp_millis()),
                    "creationdate" => stat.created_ms = chrono::DateTime::parse_from_rfc3339(&value).map_or(0, |d| d.timestamp_millis()),
                    "quota-available-bytes" => stat.quota_available = value.parse().ok(),
                    "quota-used-bytes" => stat.quota_used = value.parse().ok(),
                    "collection" if path.last().is_some_and(|p| p == "resourcetype") => stat.is_dir = true,
                    "propstat" => {
                        if stat_ok && let Some(c) = cur.as_mut() {
                            c.is_dir |= stat.is_dir;
                            if stat.size > 0 {
                                c.size = stat.size;
                            }
                            if stat.modified_ms != 0 {
                                c.modified_ms = stat.modified_ms;
                            }
                            if stat.created_ms != 0 {
                                c.created_ms = stat.created_ms;
                            }
                            c.quota_available = c.quota_available.or(stat.quota_available);
                            c.quota_used = c.quota_used.or(stat.quota_used);
                        }
                    }
                    "response" => {
                        if let Some(c) = cur.take().filter(|c| !c.path.is_empty()) {
                            out.push(c);
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !xml.contains("response") && !xml.trim().is_empty() && out.is_empty() {
        return Err(bad());
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ the Fs

impl Fs for WebDavFs {
    fn caps(&self) -> Caps {
        Caps { write: true, free_space: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let t = target(&self.inner.cfg, dir)?;
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        let mut p = spath(dir.path());
        if !p.ends_with('/') {
            p.push('/');
        }
        let items = self.inner.propfind(&t, &p, 1, PROPS)?.ok_or(FsError::NotFound)?;
        // the folder itself: the answer whose path is the one asked for (else the first, when it is a folder)
        let me = trim_slash(&p).to_string();
        let own = items.iter().position(|r| trim_slash(&r.path) == me).or_else(|| items.first().filter(|r| r.is_dir).map(|_| 0));
        if own.is_some_and(|i| !items[i].is_dir) {
            return Err(FsError::Other("This is a file, not a folder".into()));
        }
        let mut entries = Vec::with_capacity(items.len());
        let mut left_out = 0;
        for (i, r) in items.iter().enumerate() {
            if Some(i) == own {
                continue;
            }
            let name = r.name();
            if leaf_vfs::names::address_problem(&name).is_some() {
                left_out += 1;
                continue;
            }
            entries.push(r.entry());
        }
        if left_out > 0 {
            log::warn!("webdav: {left_out} names in a listing cannot be used (empty, . or .., / or \\, NUL) and are left out");
        }
        let mut sent = 0;
        for part in entries.chunks(batch.max(1)) {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            sent += part.len();
            on_batch(part.to_vec());
        }
        Ok(sent)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        let t = target(&self.inner.cfg, p)?;
        self.inner.stat(&t, p.path())?.map(|r| r.meta()).ok_or(FsError::NotFound)
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let t = target(&self.inner.cfg, p)?;
        let mut h = vec![("Accept-Encoding", "identity".to_string())];
        if offset > 0 {
            h.push(("Range", format!("bytes={offset}-")));
        }
        let r = self.inner.call(&t, "GET", p.path(), &h, Payload::None)?;
        match r.status {
            200 => {
                let mut rd = r.body;
                // (a server that ignores ranges sends the whole file: the part before the offset is read and dropped)
                if offset > 0 {
                    std::io::copy(&mut (&mut rd).take(offset), &mut std::io::sink()).map_err(FsError::from)?;
                }
                Ok(Box::new(rd))
            }
            206 => {
                let starts = r.header("content-range").and_then(|v| v.strip_prefix("bytes ")).and_then(|v| v.split(['-', '/']).next()).and_then(|v| v.trim().parse::<u64>().ok());
                if starts.is_some_and(|s| s != offset) {
                    return Err(FsError::Other("The server sent another part of the file than asked for".into()));
                }
                Ok(Box::new(r.body))
            }
            // (nothing at or after the offset: the file is that long)
            416 => Ok(Box::new(std::io::empty())),
            s => Err(status_error(s)),
        }
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let t = target(&self.inner.cfg, p)?;
        let path = spath(p.path());
        let existing = self.inner.stat(&t, &path)?;
        if existing.as_ref().is_some_and(|r| r.is_dir) {
            return Err(FsError::Exists);
        }
        let (replace, in_place) = match o.mode {
            // (no append in WebDAV: a part sent before is sent again, from the start)
            WriteMode::At(_) => return Err(FsError::Unsupported),
            WriteMode::CreateNew if existing.is_some() => return Err(FsError::Exists),
            WriteMode::CreateNew => (false, false),
            WriteMode::Replace if is_part_name(p.name()) => {
                if existing.is_some() {
                    return Err(FsError::Exists);
                }
                (false, true)
            }
            WriteMode::Replace => (true, false),
        };
        // the bytes go to a name beside the file that nothing else uses; a part name already is one
        let (dir, name) = match path.trim_end_matches('/').rsplit_once('/') {
            Some((d, n)) => (if d.is_empty() { "/".to_string() } else { d.to_string() }, n.to_string()),
            None => ("/".to_string(), path.clone()),
        };
        let temp = if in_place { path.clone() } else { format!("{}/{}", dir.trim_end_matches('/'), side_name(&name, "leaf-up")) };
        Ok(Box::new(Writer { inner: self.inner.clone(), t, target: path, temp, in_place, replace, buf: Vec::new(), stream: None, done: false }))
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        if p.is_root() {
            return Err(FsError::Exists);
        }
        let t = target(&self.inner.cfg, p)?;
        let mut path = spath(p.path());
        if !path.ends_with('/') {
            path.push('/');
        }
        let r = self.inner.call(&t, "MKCOL", &path, &[], Payload::None)?;
        match r.status {
            200 | 201 => Ok(()),
            // a name that is taken (a file or a folder)
            405 => Err(FsError::Exists),
            s => Err(status_error(s)),
        }
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        if from.is_root() || to.is_root() {
            return Err(FsError::Denied);
        }
        let t = target(&self.inner.cfg, from)?;
        let src = self.inner.stat(&t, from.path())?.ok_or(FsError::NotFound)?;
        // never over anything unless the caller said replace, and then a file over a file only
        let overwrite = match self.inner.stat(&t, to.path())? {
            None => false,
            Some(d) if replace && !src.is_dir && !d.is_dir => true,
            Some(_) => return Err(FsError::Exists),
        };
        self.inner.move_to(&t, from.path(), to.path(), overwrite)
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() || p.path() == "/~" {
            return Err(FsError::Denied);
        }
        let t = target(&self.inner.cfg, p)?;
        let it = self.inner.stat(&t, p.path())?.ok_or(FsError::NotFound)?;
        if it.is_dir {
            let mut dir = spath(p.path());
            if !dir.ends_with('/') {
                dir.push('/');
            }
            if !recursive {
                let kids = self.inner.propfind(&t, &dir, 1, PROPS)?.unwrap_or_default();
                if kids.iter().any(|r| trim_slash(&r.path) != trim_slash(&dir)) {
                    return Err(FsError::NotEmpty);
                }
            }
            return self.inner.delete(&t, &dir);
        }
        self.inner.delete(&t, p.path())
    }

    fn set_meta(&self, _p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.modified_ms.is_none() && m.mode.is_none() { Ok(()) } else { Err(FsError::Unsupported) }
    }

    fn free_space(&self, p: &VPath) -> Result<Option<Space>, FsError> {
        let t = target(&self.inner.cfg, p)?;
        let mut path = spath(p.path());
        if !path.ends_with('/') {
            path.push('/');
        }
        // (a server without quotas answers 4xx/5xx or leaves the properties out: unknown)
        let items = match self.inner.propfind(&t, &path, 0, QUOTA) {
            Ok(Some(i)) => i,
            Ok(None) | Err(FsError::Unsupported | FsError::Denied | FsError::Other(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(items.first().and_then(|r| r.quota_available.map(|free| Space { free, total: r.quota_used.map(|u| u.saturating_add(free)) })))
    }
}

// ------------------------------------------------------------------------------------------------ writing

enum Msg {
    Data(Vec<u8>),
    End,
}

/// The body of a streamed `PUT`: what the writer sends. A writer that went away without finishing makes the body fail (the
/// request is not completed: the server drops the partial file).
struct ChanReader {
    rx: mpsc::Receiver<Msg>,
    cur: Vec<u8>,
    at: usize,
    ended: bool,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.cur.len() {
            if self.ended {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Msg::Data(d)) => {
                    self.cur = d;
                    self.at = 0;
                }
                Ok(Msg::End) => {
                    self.ended = true;
                    return Ok(0);
                }
                Err(_) => return Err(std::io::Error::other("the upload was cut")),
            }
        }
        let n = out.len().min(self.cur.len() - self.at);
        out[..n].copy_from_slice(&self.cur[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A big upload running: its channel and the thread that sends the request.
struct Stream {
    tx: mpsc::SyncSender<Msg>,
    thread: std::thread::JoinHandle<Result<(), FsError>>,
}

struct Writer {
    inner: Arc<Inner>,
    t: Target,
    /// Where the file goes in the end, and where the bytes go first.
    target: String,
    temp: String,
    /// `temp` is the name the caller gave (a part name): no `MOVE` after.
    in_place: bool,
    /// The caller said replace (the conflict dialog's answer): the `MOVE` may take the name of a file.
    replace: bool,
    buf: Vec<u8>,
    stream: Option<Stream>,
    /// `finish` ran (nothing is left for `Drop`).
    done: bool,
}

/// The status of a `PUT` as an error.
fn put_status(s: u16) -> Result<(), FsError> {
    match s {
        200 | 201 | 204 => Ok(()),
        s => Err(status_error(s)),
    }
}

impl Writer {
    fn start_stream(&mut self) -> Stream {
        let (tx, rx) = mpsc::sync_channel::<Msg>(8);
        let (inner, t, temp) = (self.inner.clone(), self.t.clone(), self.temp.clone());
        let thread = std::thread::spawn(move || {
            let mut body = ChanReader { rx, cur: Vec::new(), at: 0, ended: false };
            let r = inner.call(&t, "PUT", &temp, &[("If-None-Match", "*".into()), ("Content-Type", "application/octet-stream".into())], Payload::Stream(&mut body))?;
            put_status(r.status)
        });
        Stream { tx, thread }
    }

    fn send(&mut self, data: Vec<u8>) -> std::io::Result<()> {
        if self.stream.is_none() {
            let s = self.start_stream();
            self.stream = Some(s);
        }
        let s = self.stream.as_ref().expect("stream");
        s.tx.send(Msg::Data(data)).map_err(|_| std::io::Error::other("the upload broke off"))
    }

    /// Removes the temporary file (best effort, off this thread: the server may be slow).
    fn drop_temp(&self) {
        if self.in_place {
            return;
        }
        let (inner, t, temp) = (self.inner.clone(), self.t.clone(), self.temp.clone());
        std::thread::spawn(move || {
            let _ = inner.delete(&t, &temp);
        });
    }
}

impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.done {
            return Err(std::io::Error::other("the upload is finished"));
        }
        self.buf.extend_from_slice(data);
        if self.stream.is_some() || self.buf.len() > SMALL {
            if self.buf.len() >= 256 << 10 || self.stream.is_none() {
                let chunk = std::mem::take(&mut self.buf);
                self.send(chunk)?;
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl WriteSink for Writer {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        self.done = true;
        let uploaded = match self.stream.take() {
            Some(s) => {
                let last = std::mem::take(&mut self.buf);
                let sent = (!last.is_empty()).then(|| s.tx.send(Msg::Data(last))).transpose().and_then(|_| s.tx.send(Msg::End));
                match s.thread.join() {
                    Ok(r) if sent.is_ok() => r,
                    Ok(r) => r.and(Err(FsError::Other("The upload broke off".into()))),
                    Err(_) => Err(FsError::Other("The upload broke off".into())),
                }
            }
            None => self
                .inner
                .call(&self.t, "PUT", &self.temp, &[("If-None-Match", "*".into()), ("Content-Type", "application/octet-stream".into())], Payload::Bytes(&self.buf))
                .and_then(|r| put_status(r.status)),
        };
        if let Err(e) = uploaded {
            // (a 412 here is the name taken: nothing was stored; anything else may have left a part)
            if e != FsError::Exists {
                self.drop_temp();
            }
            return Err(e);
        }
        if self.in_place {
            return Ok(());
        }
        // the whole file is there: now it takes its name, over a file only when the caller said replace
        match self.inner.move_to(&self.t, &self.temp, &self.target, self.replace) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.drop_temp();
                Err(e)
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.done {
            // dropped without `finish`: the streamed request fails (no end was sent) and the temporary file goes
            self.stream.take();
            self.drop_temp();
        }
    }
}
