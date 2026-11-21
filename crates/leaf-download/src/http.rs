//! The HTTP side: one agent, the probe that finds the size, the range support and the validators, and ranged requests.

use std::io::Read;
use std::time::Duration;

use ureq::ResponseExt;

use crate::{Error, Options};

pub fn agent(o: &Options) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(o.user_agent.clone())
        .timeout_connect(Some(o.connect_timeout))
        .timeout_per_read(Some(o.read_timeout))
        .http_status_as_error(false)
        .max_idle_connections_per_host(32)
        .build()
        .into()
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
}

fn header(resp: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn total_of(content_range: &str) -> Option<u64> {
    content_range.rsplit('/').next()?.trim().parse().ok()
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
pub fn probe(agent: &ureq::Agent, url: &str) -> Result<Probe, Error> {
    let resp = agent.get(url).header("Range", "bytes=0-0").call().map_err(map_err)?;
    let status = resp.status().as_u16();
    let uri = resp.get_uri().to_string();
    let mut p = Probe { uri, etag: header(&resp, "etag"), last_modified: header(&resp, "last-modified"), disposition: header(&resp, "content-disposition"), ..Probe::default() };
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
        s if retriable_status(s) => return Err(Error::Network(format!("HTTP {s}"))),
        s => return Err(Error::Http(s)),
    }
    // (the body of a 200 is not read: the connection is dropped)
    Ok(p)
}

/// The body of `bytes=start-(end-1)` of `uri`, or of the whole thing (`whole`). `Err(Changed)` when the server no longer has what
/// the probe saw.
pub fn open_range(agent: &ureq::Agent, uri: &str, start: u64, end: u64, whole: bool, p: &Probe) -> Result<Box<dyn Read + Send>, Error> {
    let mut req = agent.get(uri);
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
        200 | 416 => return Err(Error::Changed),
        s if retriable_status(s) => return Err(Error::Network(format!("HTTP {s}"))),
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
