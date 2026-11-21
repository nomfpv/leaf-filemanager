//! The HTTP side: one agent, the probe that finds the size, the range support and the validators, and ranged requests.

use std::io::Read;
use std::time::Duration;

use ureq::ResponseExt;

use crate::auth::Challenge;
use crate::{Error, Headers, Options, ProxyKind, ProxyMode};

/// Redirects followed by hand (with headers of the caller's own).
const MAX_REDIRECTS: usize = 10;

fn proxy_of(mode: &ProxyMode) -> Result<Option<ureq::Proxy>, Error> {
    match mode {
        ProxyMode::Direct => Ok(None),
        // (the environment's proxy variables, and on Windows the system's setting)
        ProxyMode::System => Ok(ureq::Proxy::try_from_env()),
        ProxyMode::Manual(m) => {
            let protocol = match m.kind {
                ProxyKind::Http => ureq::ProxyProtocol::Http,
                // (the proxy resolves the names: the address of the server never goes through this PC's resolver)
                ProxyKind::Socks5 => ureq::ProxyProtocol::Socks5h,
            };
            let mut b = ureq::Proxy::builder(protocol).host(&m.host).port(m.port);
            if let Some(u) = m.user.as_deref().filter(|u| !u.is_empty()) {
                b = b.username(u);
                if let Some(p) = m.password.as_deref() {
                    b = b.password(p);
                }
            }
            // (a proxy that cannot be set up is an error, never a download that goes direct)
            b.build().map(Some).map_err(|_| Error::Network("the proxy server's address is not valid".into()))
        }
    }
}

pub fn agent(o: &Options) -> Result<ureq::Agent, Error> {
    let ua = o.headers.get("user-agent").map_or_else(|| o.user_agent.clone(), str::to_string);
    let mut b = ureq::Agent::config_builder()
        .user_agent(ua)
        .timeout_connect(Some(o.connect_timeout))
        .timeout_per_read(Some(o.read_timeout))
        .http_status_as_error(false)
        .max_idle_connections_per_host(32)
        .proxy(proxy_of(&o.proxy)?);
    // headers of the caller's own: redirects are followed in `probe`, which drops Cookie and Authorization when one leaves the origin (scheme, host, port)
    if !o.headers.is_empty() {
        b = b.max_redirects(0);
    }
    Ok(b.build().into())
}

fn with_headers<B>(mut req: ureq::RequestBuilder<B>, h: &Headers, uri: &str) -> ureq::RequestBuilder<B> {
    // (the sign-in goes to the server that asked for it only; there it replaces an Authorization the browser sent)
    let signed = h.web_auth().filter(|a| *a.origin() == origin_of(uri));
    if let Some(a) = signed {
        req = req.header("Authorization", a.header(uri));
    }
    for (n, v) in h.iter().filter(|(n, _)| !n.eq_ignore_ascii_case("user-agent") && !(signed.is_some() && n.eq_ignore_ascii_case("authorization"))) {
        req = req.header(n, v);
    }
    req
}

/// `location` (a redirect's target) made absolute against `base` (RFC 3986 reference resolution, without dot segments).
pub(crate) fn resolve(base: &str, location: &str) -> String {
    let loc = location.trim();
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return loc.to_string();
    }
    let (scheme, rest) = base.split_once("://").unwrap_or(("https", base));
    if let Some(r) = loc.strip_prefix("//") {
        return format!("{scheme}://{r}");
    }
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..auth_end]);
    if loc.starts_with('/') {
        return format!("{origin}{loc}");
    }
    let after = &rest[auth_end..];
    let path = &after[..after.find(['?', '#']).unwrap_or(after.len())];
    let path = if path.is_empty() { "/" } else { path };
    // only a fragment: the same address; only a query: the same path with it
    if loc.is_empty() || loc.starts_with('#') {
        let no_frag = &after[..after.find('#').unwrap_or(after.len())];
        return format!("{origin}{}{loc}", if no_frag.is_empty() { "/" } else { no_frag });
    }
    if loc.starts_with('?') {
        return format!("{origin}{path}{loc}");
    }
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{origin}{dir}{loc}")
}

/// Scheme, host and port of an address (the port filled in for http and https), in lower case: Cookie and Authorization go only
/// where this is the same as the first address's (a redirect from https to http on the same host would send them in clear text).
pub(crate) fn origin_of(url: &str) -> (String, String, u16) {
    let (scheme, rest) = url.trim().split_once("://").unwrap_or(("", url));
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("").to_ascii_lowercase();
    // ([v6]:port, host:port, host)
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') && p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => (h.to_string(), p.parse().ok()),
        _ => (hostport.clone(), None),
    };
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    (scheme, host, port)
}

/// What the server told about the file.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    /// Where the address ended after redirects (the segments ask there).
    pub uri: String,
    /// Bytes, when known.
    pub len: Option<u64>,
    /// The server answers byte ranges (it said 206 to a range).
    pub ranges: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub disposition: Option<String>,
    /// The caller's headers as they are sent to `uri` (without Cookie and Authorization when a redirect left the first host).
    pub send: Headers,
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn total_of(content_range: &str) -> Option<u64> {
    content_range.rsplit('/').next()?.trim().parse().ok()
}

/// The error for a status worth trying again: 429 and 503 mean "fewer connections", the others are plain network trouble.
pub fn status_error(s: u16) -> Error {
    if matches!(s, 429 | 503) { Error::Busy(s) } else { Error::Network(format!("HTTP {s}")) }
}

/// Statuses worth trying again later.
pub fn retriable_status(s: u16) -> bool {
    matches!(s, 408 | 425 | 429) || s >= 500
}

pub fn map_err(e: ureq::Error) -> Error {
    use ureq::Error as E;
    match e {
        E::BadUri(_) | E::Http(_) | E::InvalidProxyUrl | E::TooManyRedirects | E::RedirectFailed | E::RequireHttpsOnly(_) => Error::BadUrl(e.to_string()),
        E::Tls(_) | E::Rustls(_) | E::Pem(_) => Error::Secure(e.to_string()),
        other => Error::Network(other.to_string()),
    }
}

/// The challenge a 401 carries, with the server that sent it (`at`).
fn challenge_of(resp: &ureq::http::Response<ureq::Body>, at: &str) -> Option<Challenge> {
    let lines: Vec<String> = resp.headers().get_all("www-authenticate").iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect();
    Challenge::parse(&lines).map(|mut c| {
        c.origin = origin_of(at);
        c
    })
}

/// One request for the size, the range support and the validators: a range of the first byte, so a server that ignores ranges is
/// told apart from one that honours them.
pub fn probe(agent: &ureq::Agent, url: &str, headers: &Headers) -> Result<Probe, Error> {
    let first = origin_of(url);
    let mut send = headers.clone();
    let mut at = url.to_string();
    let mut hops = 0;
    let mut stale = 0;
    let resp = loop {
        let resp = with_headers(agent.get(&at), &send, &at).header("Range", "bytes=0-0").call().map_err(map_err)?;
        let s = resp.status().as_u16();
        // a Digest nonce that has only expired: the new one is taken and the request made again
        if s == 401 && stale < 2 && send.web_auth().zip(challenge_of(&resp, &at)).is_some_and(|(a, c)| a.renew(&c)) {
            stale += 1;
            continue;
        }
        // (only with headers of the caller's own: otherwise the agent followed the redirects itself)
        if headers.is_empty() || !matches!(s, 301 | 302 | 303 | 307 | 308) {
            break resp;
        }
        let Some(loc) = header(&resp, "location") else { break resp };
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(Error::BadUrl("too many redirects".into()));
        }
        at = resolve(&at, &loc);
        if !crate::is_http_url(&at) {
            return Err(Error::BadUrl("a redirect to an address that is not http(s)".into()));
        }
        if origin_of(&at) != first {
            send = send.without_credentials();
        }
    };
    let status = resp.status().as_u16();
    let uri = if headers.is_empty() { resp.get_uri().to_string() } else { at };
    let mut p = Probe { uri, etag: header(&resp, "etag"), last_modified: header(&resp, "last-modified"), disposition: header(&resp, "content-disposition"), send, ..Probe::default() };
    match status {
        206 => {
            p.ranges = true;
            p.len = header(&resp, "content-range").as_deref().and_then(total_of);
            if p.len.is_none() {
                p.ranges = false;
            }
        }
        200 => p.len = header(&resp, "content-length").and_then(|v| v.parse().ok()),
        // an empty file has no first byte
        416 => {
            p.len = header(&resp, "content-range").as_deref().and_then(total_of).or(Some(0));
        }
        401 => return Err(challenge_of(&resp, &p.uri).map_or(Error::Http(401), Error::Auth)),
        s if retriable_status(s) => return Err(status_error(s)),
        s => return Err(Error::Http(s)),
    }
    // (the body of a 200 is not read: the connection is dropped)
    Ok(p)
}

/// The body of `bytes=start-(end-1)` of `uri`, or of the whole thing (`whole`). `Err(Changed)` when the server no longer has what
/// the probe saw.
pub fn open_range(agent: &ureq::Agent, uri: &str, start: u64, end: u64, whole: bool, p: &Probe) -> Result<Box<dyn Read + Send>, Error> {
    let mut stale = 0;
    let resp = loop {
        let mut req = with_headers(agent.get(uri), &p.send, uri);
        if !whole {
            req = req.header("Range", format!("bytes={start}-{}", end - 1));
            // (a file that changed meanwhile answers with the whole new file: the validator turns that into a 200)
            if let Some(v) = p.etag.as_deref().filter(|e| !e.starts_with("W/")).or(p.last_modified.as_deref()) {
                req = req.header("If-Range", v);
            }
        }
        let resp = req.call().map_err(map_err)?;
        if resp.status().as_u16() == 401 {
            let ch = challenge_of(&resp, uri);
            // (a Digest nonce lives a few minutes: the server says "stale" and the same sign-in is good with the new one; anything
            // else is asked again)
            if stale < 2 && p.send.web_auth().zip(ch.as_ref()).is_some_and(|(a, c)| a.renew(c)) {
                stale += 1;
                continue;
            }
            return Err(ch.map_or(Error::Http(401), Error::Auth));
        }
        break resp;
    };
    let status = resp.status().as_u16();
    match status {
        206 if !whole => {
            let ok = header(&resp, "content-range").and_then(|v| v.strip_prefix("bytes ").and_then(|r| r.split(['-', '/']).next().map(str::to_string))).and_then(|s| s.trim().parse::<u64>().ok());
            if ok != Some(start) {
                return Err(Error::Changed);
            }
        }
        200 if whole || (start == 0 && Some(end) == p.len) => {
            if p.len.is_some_and(|l| header(&resp, "content-length").and_then(|v| v.parse::<u64>().ok()).is_some_and(|n| n != l)) {
                return Err(Error::Changed);
            }
        }
        // the whole file for a range: from the same version (same validator, or the same length when it has none) that is a server
        // that stopped answering ranges; anything else is a file that changed
        200 | 416 => {
            let same = match (&p.etag, header(&resp, "etag")) {
                (Some(a), Some(b)) => *a == b,
                (None, None) => p.len.is_some() && header(&resp, "content-length").and_then(|v| v.parse::<u64>().ok()) == p.len,
                _ => false,
            };
            return Err(if status == 200 && same { Error::NoRanges } else { Error::Changed });
        }
        s if retriable_status(s) => return Err(status_error(s)),
        s => return Err(Error::Http(s)),
    }
    Ok(Box::new(resp.into_body().into_reader()))
}

pub fn sleep_checking(total: Duration, stop: &dyn Fn() -> bool) {
    let step = Duration::from_millis(100);
    let mut left = total;
    while !left.is_zero() && !stop() {
        let s = left.min(step);
        std::thread::sleep(s);
        left -= s;
    }
}
