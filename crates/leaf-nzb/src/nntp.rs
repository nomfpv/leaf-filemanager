//! A small blocking NNTP client (RFC 3977, AUTHINFO USER/PASS of RFC 4643): greeting, sign-in, `BODY` requests sent several
//! at a time (pipelined) and their answers read in order. TLS is rustls over the dialer's stream, so a connection through
//! the VPN is encrypted end to end like a direct one. The password is sent once per connection and never logged.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::dial::{Conn, DialError, Dialer};
use crate::server::Server;

#[derive(Debug)]
pub enum Error {
    /// The route is not there now (the VPN): try again later, nothing failed.
    Wait(String),
    /// The server refused the user name or password (481, 482, 502 at sign-in).
    Auth(String),
    /// The server is full (too many connections for the account: 400, 502 at the greeting).
    Busy(String),
    /// The secure channel could not be set up or the certificate was refused.
    Tls(String),
    Io(io::Error),
    /// An answer that is not NNTP (or not the answer to what was asked).
    Protocol(String),
    /// A line or an article far larger than asked for: the connection is closed.
    TooLarge,
    /// A message id that could carry further commands: never sent.
    BadId,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Wait(w) => f.write_str(w),
            Error::Auth(d) => write!(f, "sign-in refused: {d}"),
            Error::Busy(d) => write!(f, "server busy: {d}"),
            Error::Tls(d) => write!(f, "secure connection: {d}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Protocol(d) => write!(f, "unexpected answer: {d}"),
            Error::TooLarge => f.write_str("the server's answer is far larger than the article"),
            Error::BadId => f.write_str("an article's message id is not valid"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

enum Stream {
    Plain(Box<dyn Conn>),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Box<dyn Conn>>>),
}

impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(b),
            Stream::Tls(s) => s.read(b),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(b),
            Stream::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// The TLS setup for NNTPS: the certificate chain is checked against the system's root certificates, read once from the
/// local store (no download), by webpki: the chain to a root, the dates, the key usage and the host name. Nothing is
/// fetched during the handshake (no OCSP, CRL or AIA lookups, which on Windows the system verifier would make over the
/// normal network and DNS, outside the VPN); so revocation is not checked.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let found = rustls_native_certs::load_native_certs();
            let mut roots = rustls::RootCertStore::empty();
            let (added, _ignored) = roots.add_parsable_certificates(found.certs);
            if added == 0 {
                let why = found.errors.first().map(|e| e.to_string()).unwrap_or_else(|| "none found".into());
                return Err(format!("the system's certificates cannot be read ({why})"));
            }
            let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().map_err(|e| e.to_string())?;
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

/// What a `BODY` request gave.
#[derive(Debug)]
pub enum Body {
    /// The article's body, dot-unstuffed, lines ending in `\n`; and its size on the wire.
    Found(Vec<u8>, u64),
    /// 430 / 423: this server does not have it.
    Missing,
    /// Another 4xx / 5xx answer for this article: taken as missing here, counted by the caller.
    Refused(u16),
}

/// The longest line a server may send (an NNTP line is at most 512 bytes, a yEnc line a few hundred).
pub const MAX_LINE: usize = 64 << 10;
/// The largest article read whatever the NZB says.
pub const MAX_BODY: u64 = 32 << 20;

/// A message id leaf asks for: printable ASCII without space, `<` or `>` (RFC 3977 3.6), so it can never end the command
/// line or start another one.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 250 && id.bytes().all(|b| (0x21..0x7f).contains(&b) && b != b'<' && b != b'>')
}

/// `BODY <id>\r\n`, refused for an id that is not [`valid_id`] (the second guard after the NZB parser).
pub fn body_command(id: &str) -> Result<Vec<u8>, Error> {
    if !valid_id(id) {
        return Err(Error::BadId);
    }
    Ok(format!("BODY <{id}>\r\n").into_bytes())
}

/// The most an article of `bytes` (as the NZB says) may weigh on the wire.
fn body_limit(bytes: u64) -> u64 {
    if bytes == 0 { MAX_BODY } else { bytes.saturating_mul(2).clamp(64 << 10, MAX_BODY) }
}

pub struct Client {
    io: BufReader<Stream>,
    /// Bytes read since the last [`Client::take_read`].
    read: u64,
}

const TIMEOUT: Duration = Duration::from_secs(30);

impl Client {
    /// Connects through `dialer`, reads the greeting and signs in.
    pub fn connect(server: &Server, dialer: &dyn Dialer) -> Result<Client, Error> {
        let conn = dialer.dial(&server.host, server.port, Duration::from_secs(20)).map_err(|e| match e {
            DialError::Wait(w) => Error::Wait(w),
            DialError::Io(e) => Error::Io(e),
        })?;
        Self::over(conn, server)
    }

    /// The session over a stream that is already open (the dialer's, or a test's).
    pub fn over(mut conn: Box<dyn Conn>, server: &Server) -> Result<Client, Error> {
        conn.set_read_timeout(Some(TIMEOUT));
        conn.set_write_timeout(Some(TIMEOUT));
        let stream = if server.tls {
            let config = tls_config().map_err(Error::Tls)?;
            let name = rustls::pki_types::ServerName::try_from(server.host.trim_start_matches('[').trim_end_matches(']').to_string()).map_err(|e| Error::Tls(e.to_string()))?;
            let tls = rustls::ClientConnection::new(config, name).map_err(|e| Error::Tls(e.to_string()))?;
            Stream::Tls(Box::new(rustls::StreamOwned::new(tls, conn)))
        } else {
            Stream::Plain(conn)
        };
        let mut c = Client { io: BufReader::with_capacity(256 << 10, stream), read: 0 };
        let (code, text) = c.status().map_err(|e| match e {
            Error::Io(e) if e.get_ref().is_some_and(|i| i.is::<rustls::Error>()) => Error::Tls(e.to_string()),
            other => other,
        })?;
        match code {
            200 | 201 => {}
            400 | 502 => return Err(Error::Busy(text)),
            _ => return Err(Error::Protocol(format!("{code} {text}"))),
        }
        if !server.user.is_empty() {
            c.send(format!("AUTHINFO USER {}\r\n", server.user).as_bytes())?;
            let (code, text) = c.status()?;
            match code {
                281 => return Ok(c),
                381 => {}
                481 | 482 | 502 => return Err(Error::Auth(text)),
                _ => return Err(Error::Protocol(format!("{code} {text}"))),
            }
            let pw = server.password.as_ref().map(|p| p.expose()).unwrap_or("");
            let mut line = Vec::with_capacity(pw.len() + 16);
            line.extend_from_slice(b"AUTHINFO PASS ");
            line.extend_from_slice(pw.as_bytes());
            line.extend_from_slice(b"\r\n");
            let sent = c.send(&line);
            line.fill(0);
            sent?;
            let (code, text) = c.status()?;
            match code {
                281 => {}
                481 | 482 | 502 => return Err(Error::Auth(text)),
                _ => return Err(Error::Protocol(format!("{code} {text}"))),
            }
        }
        Ok(c)
    }

    fn send(&mut self, b: &[u8]) -> Result<(), Error> {
        let s = self.io.get_mut();
        s.write_all(b)?;
        s.flush()?;
        Ok(())
    }

    fn line(&mut self, buf: &mut Vec<u8>) -> Result<(), Error> {
        buf.clear();
        let n = (&mut self.io).take(MAX_LINE as u64).read_until(b'\n', buf)?;
        if n == 0 {
            return Err(Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection")));
        }
        self.read += n as u64;
        if !buf.ends_with(b"\n") {
            // (cut at the cap, or the connection ended inside a line)
            return Err(if n >= MAX_LINE { Error::TooLarge } else { Error::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection")) });
        }
        Ok(())
    }

    fn status(&mut self) -> Result<(u16, String), Error> {
        let mut buf = Vec::new();
        self.line(&mut buf)?;
        let text = String::from_utf8_lossy(&buf).trim_end().to_string();
        let code = text.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| Error::Protocol(text.chars().take(80).collect()))?;
        Ok((code, text.get(4..).unwrap_or("").to_string()))
    }

    /// The bytes read from the server since the last call (for the speed and the limit).
    pub fn take_read(&mut self) -> u64 {
        std::mem::take(&mut self.read)
    }

    /// Asks for the bodies of `reqs` (message id, the article's size as the NZB says) all at once and gives the answers in
    /// the same order (`each` is called as each arrives). Nothing is sent when one id is not valid.
    pub fn bodies(&mut self, reqs: &[(&str, u64)], mut each: impl FnMut(usize, Body, &mut Self)) -> Result<(), Error> {
        let mut out = Vec::new();
        for (id, _) in reqs {
            out.extend_from_slice(&body_command(id)?);
        }
        self.send(&out)?;
        for (i, (id, bytes)) in reqs.iter().enumerate() {
            let start = self.read;
            let (code, text) = self.status()?;
            let body = match code {
                222 => {
                    // `222 0 <id>`: the answer must be for the article asked (else the pipeline is out of step)
                    if let Some(got) = text.split_whitespace().nth(1)
                        && got.strip_prefix('<').and_then(|g| g.strip_suffix('>')) != Some(id)
                    {
                        return Err(Error::Protocol("the answer is for another article".into()));
                    }
                    Body::Found(self.multiline(body_limit(*bytes))?, 0)
                }
                430 | 423 => Body::Missing,
                400..=599 => Body::Refused(code),
                _ => return Err(Error::Protocol(format!("{code} {}", text.chars().take(80).collect::<String>()))),
            };
            let body = match body {
                Body::Found(b, _) => Body::Found(b, self.read - start),
                m => m,
            };
            each(i, body, self);
        }
        Ok(())
    }

    fn multiline(&mut self, limit: u64) -> Result<Vec<u8>, Error> {
        let mut out = Vec::with_capacity(limit.min(1 << 20) as usize);
        let mut buf = Vec::with_capacity(512);
        let start = self.read;
        loop {
            self.line(&mut buf)?;
            if self.read - start > limit {
                return Err(Error::TooLarge);
            }
            let l = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let l = l.strip_suffix(b"\r").unwrap_or(l);
            if l == b"." {
                return Ok(out);
            }
            out.extend_from_slice(l.strip_prefix(b".").filter(|_| l.starts_with(b"..")).unwrap_or(l));
            out.push(b'\n');
        }
    }

    /// Says goodbye (best effort).
    pub fn quit(mut self) {
        let _ = self.send(b"QUIT\r\n");
    }
}

/// The Test button: connects, signs in and leaves. `Ok` with the server's name for it, or leaf's reason.
pub fn test(server: &Server, dialer: &dyn Dialer) -> Result<(), Error> {
    let c = Client::connect(server, dialer)?;
    c.quit();
    Ok(())
}
