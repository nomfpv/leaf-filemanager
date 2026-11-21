//! The connector seam: a download can have its connections opened by someone else (leaf's VPN) instead of a plain TCP socket.
//! [`Dialer::dial`] gives a byte stream, or `None` for "connect directly as usual"; an error refuses the connection (the VPN's
//! kill switch). TLS still runs here, over that stream, so certificates are checked the same way. A host the dialer takes is
//! not looked up on this PC (its name goes to the dialer as it is).

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport};

/// What a dialer returns to refuse a connection on purpose (not a network failure: it is not tried again).
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

pub fn refused(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, Refused(why.into()))
}

pub(crate) fn refusal_of(e: &io::Error) -> Option<String> {
    e.get_ref().and_then(|i| i.downcast_ref::<Refused>()).map(|r| r.0.clone())
}

/// A connection a dialer opened: blocking reads and writes with timeouts.
pub trait Conn: Read + Write + Send + Sync + 'static {
    fn set_read_timeout(&mut self, t: Option<Duration>);
    fn set_write_timeout(&mut self, t: Option<Duration>);
}

pub trait Dialer: Send + Sync + 'static {
    /// Whether connections to `host` are opened by [`Dialer::dial`] (then the name is not looked up on this PC).
    fn takes(&self, host: &str) -> bool;
    /// A connection to `host:port`; `Ok(None)`: connect directly. An error refuses it (nothing is opened instead).
    fn dial(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Option<Box<dyn Conn>>>;
}

#[derive(Clone)]
pub(crate) struct Shared(pub Arc<dyn Dialer>);

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dialer")
    }
}

fn host_port(uri: &ureq::http::Uri) -> (String, u16) {
    let host = uri.host().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_string();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("https") { 443 } else { 80 });
    (host, port)
}

/// The first link of the chain: the dialer's stream, or nothing (the TCP connector after it then connects directly).
#[derive(Debug)]
pub(crate) struct DialConnector(pub Shared);

impl Connector<()> for DialConnector {
    type Out = DialTransport;

    fn connect(&self, details: &ConnectionDetails, _: Option<()>) -> Result<Option<DialTransport>, ureq::Error> {
        let (host, port) = host_port(details.uri);
        let timeout = details.timeout.not_zero().map_or(Duration::from_secs(30), |d| *d);
        match self.0.0.dial(&host, port, timeout) {
            Ok(Some(conn)) => Ok(Some(DialTransport {
                conn,
                buffers: LazyBuffers::new(details.config.input_buffer_size(), details.config.output_buffer_size()),
                open: true,
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(ureq::Error::Io(e)),
        }
    }
}

pub(crate) struct DialTransport {
    conn: Box<dyn Conn>,
    buffers: LazyBuffers,
    open: bool,
}

impl std::fmt::Debug for DialTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DialTransport")
    }
}

fn io_error(e: io::Error, t: NextTimeout) -> ureq::Error {
    if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) { ureq::Error::Timeout(t.reason) } else { ureq::Error::Io(e) }
}

impl Transport for DialTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let t = timeout.for_write();
        self.conn.set_write_timeout(t.not_zero().map(|d| *d));
        let out = &self.buffers.output()[..amount];
        self.conn.write_all(out).and_then(|()| self.conn.flush()).map_err(|e| {
            self.open = false;
            io_error(e, t)
        })
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let t = timeout.for_read();
        self.conn.set_read_timeout(t.not_zero().map(|d| *d));
        let input = self.buffers.input_append_buf();
        let n = self.conn.read(input).map_err(|e| {
            self.open = false;
            io_error(e, t)
        })?;
        self.buffers.input_appended(n);
        if n == 0 {
            self.open = false;
        }
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        self.open
    }
}

/// Names the dialer takes are not looked up here: one placeholder address stands in (the dialer connects by name).
#[derive(Debug)]
pub(crate) struct DialResolver(pub Shared);

impl Resolver for DialResolver {
    fn resolve(&self, uri: &ureq::http::Uri, config: &ureq::config::Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let (host, port) = host_port(uri);
        if self.0.0.takes(&host) {
            let mut v = self.empty();
            v.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port));
            return Ok(v);
        }
        DefaultResolver::default().resolve(uri, config, timeout)
    }
}
