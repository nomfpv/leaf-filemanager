//! The HTTP side: one agent, the probe that finds the size, the range support and the validators, and ranged requests.

use std::io::Read;
use std::time::Duration;

use ureq::ResponseExt;

use crate::{Error, Headers, Options};

/// Redirects followed by hand (with headers of the caller's own).
const MAX_REDIRECTS: usize = 10;

pub fn agent(o: &Options) -> ureq::Agent {
    let ua = o.headers.get("user-agent").map_or_else(|| o.user_agent.clone(), str::to_string);
    let mut b = ureq::Agent::config_builder()
        .user_agent(ua)
        .timeout_connect(Some(o.connect_timeout))
        .timeout_per_read(Some(o.read_timeout))
        .http_status_as_error(false)
        .max_idle_connections_per_host(32);
    // headers of the caller's own: redirects are followed in `probe`, which drops Cookie and Authorization when one leaves the host
    if !o.headers.is_empty() {
        b = b.max_redirects(0);
    }
    b.build().into()
}

fn with_headers<B>(mut req: ureq::RequestBuilder<B>, h: &Headers) -> ureq::RequestBuilder<B> {
    for (n, v) in h.iter().filter(|(n, _)| !n.eq_ignore_ascii_case("user-agent")) {
        req = req.header(n, v);
    }
    req
}

/// `location` (a redirect's target) made absolute against `base`.
fn resolve(base: &str, location: &str) -> String {
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
    let path = &rest[auth_end..];
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{origin}{dir}{loc}")
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

/// One request for the size, the range support and the validators: a range of the first byte, so a server that ignores ranges is
/// told apart from one that honours them.
pub fn probe(agent: &ureq::Agent, url: &str, headers: &Headers) -> Result<Probe, Error> {
    let first_host = crate::host::host_of(url);
    let mut send = headers.clone();
    let mut at = url.to_string();
    let mut hops = 0;
    let resp = loop {
        let resp = with_headers(agent.get(&at), &send).header("Range", "bytes=0-0").call().map_err(map_err)?;
        let s = resp.status().as_u16();
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
        if crate::host::host_of(&at) != first_host {
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
        s if retriable_status(s) => return Err(status_error(s)),
        s => return Err(Error::Http(s)),
    }
    // (the body of a 200 is not read: the connection is dropped)
    Ok(p)
}

/// The body of `bytes=start-(end-1)` of `uri`, or of the whole thing (`whole`). `Err(Changed)` when the server no longer has what
/// the probe saw.
pub fn open_range(agent: &ureq::Agent, uri: &str, start: u64, end: u64, whole: bool, p: &Probe) -> Result<Box<dyn Read + Send>, Error> {
    let mut req = with_headers(agent.get(uri), &p.send);
    if !whole {
        req = req.header("Range", format!("bytes={start}-{}", end - 1));
        // (a file that changed meanwhile answers with the whole new file: the validator turns that into a 200)
        if let Some(v) = p.etag.as_deref().filter(|e| !e.starts_with("W/")).or(p.last_modified.as_deref()) {
            req = req.header("If-Range", v);
        }
    }
    let resp = req.call().map_err(map_err)?;
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
