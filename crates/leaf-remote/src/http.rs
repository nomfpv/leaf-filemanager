//! A small blocking HTTP/1.1 client for WebDAV (`webdav.rs`): any method (the HTTP libraries in the tree refuse `PROPFIND`,
//! `MKCOL`, `MOVE` and `COPY`), one request at a time on a connection, `Content-Length` or chunked bodies, connections kept for
//! the next request. TLS is rustls with the certificate check of `ftp.rs` (the system's store, plus what the user pinned).
//!
//! Nothing here knows about sign-ins or paths: the caller gives the whole header list and the request target. A header value with
//! a line break is refused. Bodies are read within what the response says (`Content-Length`, chunks, or the end of the
//! connection) and the socket's timeouts apply to every read and write.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leaf_vfs::FsError;

/// Connections kept for the next request, per server.
const POOL_MAX: usize = 4;
/// A kept connection idle for longer than this is dropped (servers close them after a short while anyway).
const IDLE_MAX: Duration = Duration::from_secs(20);
/// The head of an answer (status line and headers) may be this long.
const HEAD_MAX: usize = 64 << 10;
/// An unread rest of an answer this small is read and dropped so the connection can be used again.
const DRAIN_MAX: u64 = 32 << 10;
/// Interim (1xx) answers and trailer lines a server may send before the answer is taken as broken.
const INTERIM_MAX: usize = 10;
const TRAILERS_MAX: usize = 100;

pub(crate) trait Duplex: Read + Write + Send {}
impl<T: Read + Write + Send> Duplex for T {}

/// One connection.
pub(crate) struct Link {
    io: BufReader<Box<dyn Duplex>>,
    /// The same socket, for changing the read limit of one request.
    sock: TcpStream,
    idle_since: Instant,
}

/// Where a server is and how to reach it.
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    /// `None`: plain http.
    pub tls: Option<Arc<rustls::ClientConfig>>,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl Endpoint {
    fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let default = if self.tls.is_some() { 443 } else { 80 };
        if self.port == default { host } else { format!("{host}:{}", self.port) }
    }
}

/// The connections of one server that are open and free.
#[derive(Default)]
pub(crate) struct Pool {
    idle: Mutex<Vec<Link>>,
}

impl Pool {
    fn take(&self) -> Option<Link> {
        let mut g = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|l| l.idle_since.elapsed() < IDLE_MAX);
        g.pop()
    }
    fn put(&self, mut l: Link) {
        l.idle_since = Instant::now();
        let mut g = self.idle.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() < POOL_MAX {
            g.push(l);
        }
    }
    /// Drops every kept connection (a certificate was trusted: new ones are made with the new check).
    pub fn clear(&self) {
        self.idle.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// What a request sends as its body.
pub(crate) enum Payload<'a> {
    None,
    /// Held in memory: sent with its length, and can be sent again.
    Bytes(&'a [u8]),
    /// Streamed (chunked): sent once.
    Stream(&'a mut dyn Read),
}

/// Why a request did not get an answer.
pub(crate) enum Fail {
    /// The connection failed or broke (also a refused certificate: the caller asks the check).
    Io(std::io::Error),
    /// Like `Io`, and the request cannot have been acted on: a kept connection that turned out dead. Only this is sent again.
    Stale(std::io::Error),
    Fs(FsError),
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Fail {
        Fail::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Length(u64),
    /// The bytes left in the current chunk.
    Chunked(u64),
    /// Until the server closes the connection.
    Close,
    Done,
}

/// An answer's body, read as it arrives. When it was read to its end on a connection that may be used again, the connection goes
/// back to the pool; dropped before that it is closed (a small rest is read first).
pub(crate) struct Body {
    link: Option<Link>,
    mode: Mode,
    /// The connection may serve another request after this body.
    keep: bool,
    pool: Arc<Pool>,
}

impl Body {
    fn finish(&mut self) {
        self.mode = Mode::Done;
        if let Some(l) = self.link.take()
            && self.keep
        {
            self.pool.put(l);
        }
    }

    fn line(&mut self) -> std::io::Result<String> {
        let l = self.link.as_mut().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;
        read_line(&mut l.io)
    }
}

fn read_line(io: &mut BufReader<Box<dyn Duplex>>) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let n = (&mut *io).take(16 << 10).read_until(b'\n', &mut buf)?;
    if n == 0 || !buf.ends_with(b"\n") {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string())
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            match self.mode {
                Mode::Done => return Ok(0),
                Mode::Length(0) => {
                    self.finish();
                    return Ok(0);
                }
                Mode::Length(n) => {
                    let io = &mut self.link.as_mut().ok_or(std::io::ErrorKind::UnexpectedEof)?.io;
                    let max = out.len().min(n.min(usize::MAX as u64) as usize);
                    let got = io.read(&mut out[..max])?;
                    if got == 0 {
                        self.link = None;
                        self.mode = Mode::Done;
                        return Err(std::io::ErrorKind::UnexpectedEof.into());
                    }
                    self.mode = Mode::Length(n - got as u64);
                    if n == got as u64 {
                        self.finish();
                    }
                    return Ok(got);
                }
                Mode::Chunked(0) => {
                    // the next chunk's size (a chunk's end CRLF was read with the chunk)
                    let line = self.line()?;
                    let size = u64::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| std::io::Error::other("the server's chunked answer is broken"))?;
                    if size == 0 {
                        // (trailers, up to the empty line)
                        let mut n = 0;
                        while !self.line()?.is_empty() {
                            n += 1;
                            if n > TRAILERS_MAX {
                                self.link = None;
                                self.mode = Mode::Done;
                                return Err(std::io::Error::other("the server's answer has too many trailer lines"));
                            }
                        }
                        self.finish();
                        return Ok(0);
                    }
                    self.mode = Mode::Chunked(size);
                }
                Mode::Chunked(n) => {
                    let io = &mut self.link.as_mut().ok_or(std::io::ErrorKind::UnexpectedEof)?.io;
                    let max = out.len().min(n.min(usize::MAX as u64) as usize);
                    let got = io.read(&mut out[..max])?;
                    if got == 0 {
                        self.link = None;
                        self.mode = Mode::Done;
                        return Err(std::io::ErrorKind::UnexpectedEof.into());
                    }
                    self.mode = Mode::Chunked(n - got as u64);
                    if n == got as u64 {
                        // the CRLF after the chunk
                        self.line()?;
                    }
                    return Ok(got);
                }
                Mode::Close => {
                    let io = &mut self.link.as_mut().ok_or(std::io::ErrorKind::UnexpectedEof)?.io;
                    let got = io.read(out)?;
                    if got == 0 {
                        self.keep = false;
                        self.finish();
                    }
                    return Ok(got);
                }
            }
        }
    }
}

impl Drop for Body {
    fn drop(&mut self) {
        // a small rest of a body is read and dropped, so the connection can serve the next request
        if let Mode::Length(n) = self.mode
            && n <= DRAIN_MAX
            && self.keep
            && let Some(l) = self.link.as_mut()
        {
            let mut rest = vec![0; n as usize];
            if l.io.read_exact(&mut rest).is_ok() {
                self.finish();
            }
        }
    }
}

/// An answer: the status, the headers (names in lower case), and the body still to be read.
pub(crate) struct Reply {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Body,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    pub fn headers_of(&self, name: &str) -> Vec<String> {
        self.headers.iter().filter(|(k, _)| k == name).map(|(_, v)| v.clone()).collect()
    }
    /// The whole body as text, `Err` when it is longer than `limit` bytes.
    pub fn text(mut self, limit: u64) -> std::io::Result<String> {
        let mut buf = Vec::new();
        (&mut self.body).take(limit + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > limit {
            return Err(std::io::Error::other("the answer is too long"));
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

fn connect(ep: &Endpoint) -> Result<Link, Fail> {
    log::info!("webdav: connecting to {}:{} ({})", ep.host, ep.port, if ep.tls.is_some() { "TLS" } else { "not encrypted" });
    let addrs: Vec<SocketAddr> = (ep.host.as_str(), ep.port).to_socket_addrs().map_err(|_| Fail::Fs(FsError::Offline("the server's name has no address".into())))?.collect();
    let mut last = None;
    let mut tcp = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, ep.connect_timeout) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some(mut tcp) = tcp else {
        return Err(Fail::Fs(FsError::Offline(match last {
            Some(e) if e.kind() == std::io::ErrorKind::TimedOut => "no answer".into(),
            Some(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => "the server does not take connections on this port".into(),
            Some(e) => format!("the connection failed ({e})"),
            None => "no address".into(),
        })));
    };
    tcp.set_read_timeout(Some(ep.io_timeout))?;
    tcp.set_write_timeout(Some(ep.io_timeout))?;
    let _ = tcp.set_nodelay(true);
    let sock = tcp.try_clone()?;
    let io: Box<dyn Duplex> = match &ep.tls {
        None => Box::new(tcp),
        Some(cfg) => {
            let name = rustls::pki_types::ServerName::try_from(ep.host.clone()).map_err(|_| Fail::Fs(FsError::Other("The server's name cannot be used for a secure connection".into())))?;
            let mut conn = rustls::ClientConnection::new(cfg.clone(), name).map_err(|e| Fail::Fs(FsError::Other(format!("TLS could not be set up ({e})"))))?;
            // (the handshake here, so a refused certificate is this connection's error)
            while conn.is_handshaking() {
                conn.complete_io(&mut tcp)?;
            }
            Box::new(rustls::StreamOwned::new(conn, tcp))
        }
    };
    Ok(Link { io: BufReader::new(io), sock, idle_since: Instant::now() })
}

/// Sends one request and reads the head of the answer. A kept connection that turns out dead (the server closed it meanwhile) is
/// given up for a new one, once; the body of a `Stream` payload is never sent twice.
pub(crate) fn send(pool: &Arc<Pool>, ep: &Endpoint, method: &str, target: &str, headers: &[(String, String)], payload: &mut Payload) -> Result<Reply, Fail> {
    if !matches!(payload, Payload::Stream(_))
        && let Some(link) = pool.take()
    {
        match exchange(link, pool, ep, method, target, headers, payload) {
            Err(Fail::Stale(e)) => log::debug!("webdav: a kept connection was dead ({e}); a new one is made"),
            r => return r.map_err(unstale),
        }
    }
    exchange(connect(ep)?, pool, ep, method, target, headers, payload).map_err(unstale)
}

fn unstale(f: Fail) -> Fail {
    match f {
        Fail::Stale(e) => Fail::Io(e),
        f => f,
    }
}

/// A write that failed (the request was not all sent) or an answer that never began (closed or reset before one byte): the server
/// cannot have acted on the request. A timeout is neither: the server may be busy with it.
fn dead_kind(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(e.kind(), UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe | NotConnected)
}

/// How long a COPY or MOVE (a folder is done by the server before it answers) may take to answer.
fn read_limit(ep: &Endpoint, method: &str) -> Duration {
    if matches!(method, "COPY" | "MOVE") { ep.io_timeout.saturating_mul(10).max(Duration::from_secs(300)) } else { ep.io_timeout }
}

fn exchange(mut link: Link, pool: &Arc<Pool>, ep: &Endpoint, method: &str, target: &str, headers: &[(String, String)], payload: &mut Payload) -> Result<Reply, Fail> {
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: {}\r\nUser-Agent: leaf/{}\r\nAccept-Encoding: identity\r\n", ep.host_header(), env!("CARGO_PKG_VERSION"));
    for (k, v) in headers {
        if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
            return Err(Fail::Fs(FsError::Other("A request header cannot be used".into())));
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    match payload {
        Payload::None if !matches!(method, "GET" | "HEAD" | "DELETE") => head.push_str("Content-Length: 0\r\n"),
        Payload::None => {}
        Payload::Bytes(b) => head.push_str(&format!("Content-Length: {}\r\n", b.len())),
        Payload::Stream(_) => head.push_str("Transfer-Encoding: chunked\r\n"),
    }
    head.push_str("\r\n");
    let long = read_limit(ep, method);
    if long != ep.io_timeout {
        let _ = link.sock.set_read_timeout(Some(long));
    }
    let sent = (|| -> Result<(), Fail> {
        let w = link.io.get_mut();
        w.write_all(head.as_bytes())?;
        match payload {
            Payload::None => {}
            Payload::Bytes(b) => w.write_all(b)?,
            Payload::Stream(r) => {
                let mut buf = vec![0; 64 << 10];
                loop {
                    // (a stream that fails is not ended: the server drops the unfinished request)
                    let n = r.read(&mut buf).map_err(|_| Fail::Fs(FsError::Other("The upload broke off".into())))?;
                    if n == 0 {
                        break;
                    }
                    w.write_all(format!("{n:x}\r\n").as_bytes())?;
                    w.write_all(&buf[..n])?;
                    w.write_all(b"\r\n")?;
                }
                w.write_all(b"0\r\n\r\n")?;
            }
        }
        w.flush()?;
        Ok(())
    })();
    if let Err(f) = sent {
        return Err(match f {
            Fail::Io(e) if dead_kind(&e) => Fail::Stale(e),
            f => f,
        });
    }
    // the head of the answer (interim 1xx answers are skipped); one that never begins is a dead connection
    match link.io.fill_buf() {
        Ok(b) if b.is_empty() => return Err(Fail::Stale(std::io::ErrorKind::UnexpectedEof.into())),
        Err(e) if dead_kind(&e) => return Err(Fail::Stale(e)),
        Err(e) => return Err(Fail::Io(e)),
        Ok(_) => {}
    }
    let mut interim = 0;
    let (status, headers, http11) = loop {
        let line = read_line(&mut link.io)?;
        let mut parts = line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        let status: u16 = parts.next().and_then(|s| s.parse().ok()).filter(|_| version.starts_with("HTTP/1.")).ok_or_else(|| Fail::Fs(FsError::Other("The server did not answer in HTTP".into())))?;
        let mut hs = Vec::new();
        let mut size = line.len();
        loop {
            let l = read_line(&mut link.io)?;
            size += l.len();
            if l.is_empty() {
                break;
            }
            if size > HEAD_MAX || hs.len() > 200 {
                return Err(Fail::Fs(FsError::Other("The server's answer has too long a head".into())));
            }
            if let Some((k, v)) = l.split_once(':') {
                hs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        if status == 101 {
            return Err(Fail::Fs(FsError::Other("The server switched protocols".into())));
        }
        if status >= 200 {
            break (status, hs, version == "HTTP/1.1");
        }
        interim += 1;
        if interim > INTERIM_MAX {
            return Err(Fail::Fs(FsError::Other("The server sent too many interim answers".into())));
        }
    };
    if long != ep.io_timeout {
        let _ = link.sock.set_read_timeout(Some(ep.io_timeout));
    }
    let has = |name: &str, token: &str| headers.iter().filter(|(k, _)| k == name).any(|(_, v)| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)));
    // (several lengths that differ, or one that is not plain digits (`+5`), is an answer that cannot be framed: an error)
    let mut lengths: Vec<Option<u64>> = Vec::new();
    for (_, v) in headers.iter().filter(|(k, _)| k == "content-length") {
        for part in v.split(',') {
            let p = part.trim();
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Fail::Fs(FsError::Other("The server sent a length that cannot be used".into())));
            }
            lengths.push(Some(p.parse::<u64>().map_err(|_| Fail::Fs(FsError::Other("The server sent a length that cannot be used".into())))?));
        }
    }
    if lengths.windows(2).any(|w| w[0] != w[1]) {
        return Err(Fail::Fs(FsError::Other("The server sent two different lengths".into())));
    }
    if lengths.is_empty() {
        lengths.push(None);
    }
    let mode = if method == "HEAD" || status == 204 || status == 304 {
        Mode::Length(0)
    } else if has("transfer-encoding", "chunked") {
        Mode::Chunked(0)
    } else if let Some(n) = lengths[0] {
        Mode::Length(n)
    } else {
        Mode::Close
    };
    let keep = http11 && !has("connection", "close") && mode != Mode::Close;
    let mut body = Body { link: Some(link), mode, keep, pool: pool.clone() };
    if mode == Mode::Length(0) {
        body.finish();
    }
    Ok(Reply { status, headers, body })
}
