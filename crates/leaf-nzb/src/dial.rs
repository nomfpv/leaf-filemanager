//! Who opens the NNTP connections. A [`Dialer`] gives a byte stream, or says "wait" (the VPN is not up and Usenet may only go
//! through it): a wait is never a failure, the job holds as "Waiting for the VPN" and goes on by itself.
//!
//! [`RouteDialer`] is the owner's rule in one place: with "Usenet only while the VPN is on", every connection (the downloads,
//! the Test button, retries, backup servers) goes through the tunnel and nowhere else, whatever the general download
//! settings say; otherwise the general "Downloads through VPN" setting and its kill switch decide.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub trait Conn: Read + Write + Send + 'static {
    fn set_read_timeout(&mut self, t: Option<Duration>);
    fn set_write_timeout(&mut self, t: Option<Duration>);
}

#[derive(Debug)]
pub enum DialError {
    /// Not now: the route is not there (the VPN is off, connecting or lost). The text says why, in leaf's words.
    Wait(String),
    Io(io::Error),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::Wait(w) => f.write_str(w),
            DialError::Io(e) => write!(f, "{e}"),
        }
    }
}

pub trait Dialer: Send + Sync + 'static {
    fn dial(&self, host: &str, port: u16, timeout: Duration) -> Result<Box<dyn Conn>, DialError>;
    /// Goes up whenever the route changes: a connection opened before is closed and opened again on the route of now.
    fn generation(&self) -> u64 {
        0
    }
    /// Set while the route this dialer must use is not there (the tunnel, when the rule says the VPN): a connection that
    /// fails now waits for it, it does not count as the server's failure. The words for the wait.
    fn wait_reason(&self) -> Option<String> {
        None
    }
}

/// A plain TCP connection made here.
pub struct DirectConn(pub TcpStream);

impl Read for DirectConn {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}

impl Write for DirectConn {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Conn for DirectConn {
    fn set_read_timeout(&mut self, t: Option<Duration>) {
        let _ = self.0.set_read_timeout(t);
    }
    fn set_write_timeout(&mut self, t: Option<Duration>) {
        let _ = self.0.set_write_timeout(t);
    }
}

pub fn connect_direct(host: &str, port: u16, timeout: Duration) -> io::Result<Box<dyn Conn>> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the server's name has no address");
    for a in (host.trim_start_matches('[').trim_end_matches(']'), port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(Box::new(DirectConn(s)));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Where leaf's VPN is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tunnel {
    Up,
    /// On its way (at start, a reconnect, a new place).
    Connecting,
    Down,
}

/// The network as the route rule needs it (leaf-vpn in the app, a fake in the tests).
pub trait Net: Send + Sync + 'static {
    fn tunnel(&self) -> Tunnel;
    /// A connection inside the tunnel only (never a direct one, not even for a LAN name).
    fn dial_tunnel(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Box<dyn Conn>>;
    fn dial_direct(&self, host: &str, port: u16, timeout: Duration) -> io::Result<Box<dyn Conn>>;
    /// The general kill switch: while the VPN is not up, what is set to use it waits.
    fn kill_switch(&self) -> bool;
    fn generation(&self) -> u64;
    /// The words for "waiting for the VPN" (leaf's, translated).
    fn waiting_text(&self) -> String;
}

/// The settings that decide the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// "Usenet only while the VPN is on" (on by default).
    pub vpn_only: bool,
    /// The general "Downloads through VPN" setting (used only when `vpn_only` is off).
    pub through_vpn: bool,
}

/// Where the route settings come from: read again at every dial, so a change applies to the next connection.
pub trait RouteSource: Send + Sync + 'static {
    fn route(&self) -> Route;
    /// Goes up whenever the route settings change (the job's connections are then closed and dialled again).
    fn generation(&self) -> u64 {
        0
    }
}

/// Fixed settings (the tests, the Test button).
impl RouteSource for Route {
    fn route(&self) -> Route {
        *self
    }
}

pub struct RouteDialer<N: Net, R: RouteSource = Route> {
    pub net: N,
    pub route: R,
}

impl<N: Net, R: RouteSource> Dialer for RouteDialer<N, R> {
    fn dial(&self, host: &str, port: u16, timeout: Duration) -> Result<Box<dyn Conn>, DialError> {
        let tunnel = self.net.tunnel();
        let route = self.route.route();
        if route.vpn_only {
            // the owner's rule: the tunnel or nothing; a failure inside an up tunnel is a network error like any other
            return match tunnel {
                Tunnel::Up => self.net.dial_tunnel(host, port, timeout).map_err(DialError::Io),
                Tunnel::Connecting | Tunnel::Down => Err(DialError::Wait(self.net.waiting_text())),
            };
        }
        if !route.through_vpn {
            return self.net.dial_direct(host, port, timeout).map_err(DialError::Io);
        }
        match tunnel {
            Tunnel::Up => self.net.dial_tunnel(host, port, timeout).map_err(DialError::Io),
            Tunnel::Connecting => Err(DialError::Wait(self.net.waiting_text())),
            Tunnel::Down if self.net.kill_switch() => Err(DialError::Wait(self.net.waiting_text())),
            Tunnel::Down => self.net.dial_direct(host, port, timeout).map_err(DialError::Io),
        }
    }

    fn generation(&self) -> u64 {
        // (two counters that only go up: their sum changes whenever either does)
        self.net.generation().wrapping_add(self.route.generation())
    }

    fn wait_reason(&self) -> Option<String> {
        let route = self.route.route();
        let tunnel = self.net.tunnel();
        let needs_tunnel = route.vpn_only || (route.through_vpn && (tunnel != Tunnel::Down || self.net.kill_switch()));
        (needs_tunnel && tunnel != Tunnel::Up).then(|| self.net.waiting_text())
    }
}
