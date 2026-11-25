//! BitTorrent downloads for leaf: a magnet link or a .torrent file becomes a download that leaf's Status Center shows
//! (rf-app's `torrent.rs` is the face). The engine is librqbit; this crate is what makes it leaf's:
//!
//! - **The owner's rule** ([`Route`], [`decide`]): with "Torrents only while the VPN is on" (on by default) everything a
//!   torrent sends goes through leaf's VPN and nowhere else, or it waits. On that route the engine has no socket of its own
//!   to the outside: peers and HTTP trackers go through a private SOCKS entrance into the tunnel (names are looked up
//!   inside it), UDP and HTTPS trackers through [`gateway`], the DHT is only asked, by leaf itself through the tunnel
//!   (`dht`), and the engine's own DHT, the listener, uTP, port forwarding, local discovery and IPv6 are off, because none
//!   of them can be put into the tunnel.
//! - Only when the rule is turned off does the engine get the PC's own network (then with DHT and a listener), or the
//!   general "Downloads through VPN" setting and its kill switch decide.
//! - Nothing is uploaded after a torrent is complete: it leaves the engine the moment its last piece is checked.
//! - Nothing runs while no torrent does: the runtime starts with the first one, the engine stops with the last.

mod dht;
mod engine;
pub mod gateway;
pub mod source;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use source::{Source, describe};

/// The settings that decide the route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// "Torrents only while the VPN is on" (on by default).
    pub vpn_only: bool,
    /// The general "Downloads through VPN" setting (used only when `vpn_only` is off).
    pub through_vpn: bool,
}

/// Where leaf's VPN is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tunnel {
    Up,
    /// On its way (at start, a reconnect, a new place).
    Connecting,
    Down,
}

/// What a torrent does now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Go {
    /// Everything through the tunnel.
    Tunnel,
    /// The PC's own network.
    Direct,
    /// Nothing: the route it must use is not there.
    Wait,
}

/// The rule in one place. `kill`: the general kill switch.
pub fn decide(route: Route, tunnel: Tunnel, kill: bool) -> Go {
    if route.vpn_only {
        // the owner's rule: the tunnel or nothing
        return if tunnel == Tunnel::Up { Go::Tunnel } else { Go::Wait };
    }
    if !route.through_vpn {
        return Go::Direct;
    }
    match tunnel {
        Tunnel::Up => Go::Tunnel,
        Tunnel::Connecting => Go::Wait,
        Tunnel::Down if kill => Go::Wait,
        Tunnel::Down => Go::Direct,
    }
}

static VPN_ONLY: AtomicBool = AtomicBool::new(true);
static THROUGH_VPN: AtomicBool = AtomicBool::new(false);

/// Takes the settings' route: running torrents follow it at once (the engine is stopped and started again on the new
/// route; nothing is sent in between).
pub fn set_route(route: Route) {
    VPN_ONLY.store(route.vpn_only, Ordering::Release);
    THROUGH_VPN.store(route.through_vpn, Ordering::Release);
}

pub fn route() -> Route {
    Route { vpn_only: VPN_ONLY.load(Ordering::Acquire), through_vpn: THROUGH_VPN.load(Ordering::Acquire) }
}

fn tunnel_now() -> Tunnel {
    if leaf_vpn::tunnel().is_some() {
        Tunnel::Up
    } else if matches!(leaf_vpn::state(), leaf_vpn::State::Connecting { .. }) {
        Tunnel::Connecting
    } else {
        Tunnel::Down
    }
}

/// What the rule says right now.
pub fn go_now() -> Go {
    decide(route(), tunnel_now(), leaf_vpn::kill_switch())
}

/// One download.
pub struct Spec {
    pub source: Source,
    /// The folder the result goes into: a single file straight into it, several files into a folder named after the torrent.
    pub dest: PathBuf,
    /// The files to download, by their place in the torrent, when that was chosen before (a download carried on); else
    /// [`Host::choose`] is asked.
    pub only: Option<Vec<usize>>,
}

/// The job's switches, read while it runs.
#[derive(Clone, Default)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    /// Bytes per second for all torrents together (0: no limit).
    pub limit: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The route is not there (the VPN is off, connecting or lost).
    WaitingForVpn,
    /// A magnet link: the list of files is asked from the first peers.
    Metadata,
    /// What is already on disk is checked (a resumed download).
    Checking,
    Downloading,
    Paused,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub step: Step,
    /// The torrent's name, once known.
    pub name: Option<String>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Peers connected now.
    pub peers: usize,
    /// Through the tunnel (else the PC's own network).
    pub tunnel: bool,
}

pub trait Host: Sync {
    fn progress(&self, p: &Progress);
    /// A torrent with several files: which of them to download, by their place in `files` (path and size). `None`: none,
    /// the download is cancelled. May wait for the user.
    fn choose(&self, _name: &str, files: &[(String, u64)]) -> Option<Vec<usize>> {
        Some((0..files.len()).collect())
    }
}

#[derive(Debug)]
pub struct Done {
    pub name: String,
    /// The file, or the folder of a torrent with several files.
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum Error {
    Cancelled,
    /// Not a magnet link or .torrent file leaf can read (the detail).
    NotTorrent(String),
    /// The same torrent is already downloading.
    Duplicate,
    /// The engine could not start or gave up (the detail).
    Engine(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelled"),
            Error::NotTorrent(d) => write!(f, "not a torrent: {d}"),
            Error::Duplicate => f.write_str("this torrent is already downloading"),
            Error::Engine(d) => f.write_str(d),
        }
    }
}

impl std::error::Error for Error {}

/// Downloads `spec` and returns when it is complete, cancelled or failed. Blocks the calling thread (a job's own thread);
/// a wait for the VPN is never a failure.
pub fn run(spec: &Spec, ctl: &Control, host: &dyn Host) -> Result<Done, Error> {
    engine::run(spec, ctl, host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule() {
        let only = Route { vpn_only: true, through_vpn: false };
        // the tunnel or nothing, whatever the kill switch says
        for kill in [false, true] {
            assert_eq!(decide(only, Tunnel::Up, kill), Go::Tunnel);
            assert_eq!(decide(only, Tunnel::Connecting, kill), Go::Wait);
            assert_eq!(decide(only, Tunnel::Down, kill), Go::Wait);
            // (the general setting changes nothing while the rule is on)
            assert_eq!(decide(Route { vpn_only: true, through_vpn: true }, Tunnel::Down, kill), Go::Wait);
        }
        let off = Route { vpn_only: false, through_vpn: false };
        assert_eq!(decide(off, Tunnel::Up, true), Go::Direct);
        assert_eq!(decide(off, Tunnel::Down, true), Go::Direct);
        let general = Route { vpn_only: false, through_vpn: true };
        assert_eq!(decide(general, Tunnel::Up, false), Go::Tunnel);
        assert_eq!(decide(general, Tunnel::Connecting, false), Go::Wait);
        assert_eq!(decide(general, Tunnel::Down, true), Go::Wait);
        assert_eq!(decide(general, Tunnel::Down, false), Go::Direct);
    }

}
