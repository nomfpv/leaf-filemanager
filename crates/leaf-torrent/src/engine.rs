//! The engine (librqbit) as leaf runs it: one session at a time, made for the route of now. On the tunnel route the
//! session has no way out but the private SOCKS entrance into leaf's tunnel; when the route changes (the rule was turned
//! off or on, or the general setting's tunnel came or went) the session is stopped and a new one is made, and every
//! running torrent is put into it again (what is on disk is checked and kept).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig, ListenerOptions, PeerConnectionOptions, Session, SessionOptions, TorrentStatsState};

use crate::source::{Parsed, magnet_with, safe_name, torrent_with, trackers_for};
use crate::{Control, Done, Error, Go, Host, Progress, Spec, Step};

/// Connections the SOCKS entrance keeps at once (each holds a socket and its buffers in the tunnel's stack).
const TUNNEL_CONNECTIONS: usize = 400;
/// Peers per torrent through the tunnel, and on the PC's own network.
const TUNNEL_PEERS: usize = 120;
const DIRECT_PEERS: usize = 300;
/// A peer that does not answer in this time is left for the next one.
const PEER_CONNECT: Duration = Duration::from_secs(8);

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        tokio::runtime::Builder::new_multi_thread().worker_threads(workers).thread_name("leaf-torrent").enable_all().build().expect("the torrent engine's runtime")
    })
}

struct Live {
    tunnel: bool,
    no_upload: bool,
    session: Arc<Session>,
    /// The SOCKS entrance (the tunnel route): stopped when this goes.
    _entrance: Option<leaf_vpn::socks::Private>,
    generation: u64,
}

static ENGINE: tokio::sync::Mutex<Option<Live>> = tokio::sync::Mutex::const_new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static JOBS: AtomicUsize = AtomicUsize::new(0);

/// The tests' network: no DHT on the direct route, and these peers for every torrent (a seeder on 127.0.0.1).
#[cfg(test)]
static TEST_PEERS: std::sync::Mutex<Option<Vec<std::net::SocketAddr>>> = std::sync::Mutex::new(None);

fn test_peers() -> Option<Vec<std::net::SocketAddr>> {
    #[cfg(test)]
    {
        TEST_PEERS.lock().unwrap().clone()
    }
    #[cfg(not(test))]
    {
        None
    }
}

fn limit_of(bytes: u64) -> Option<NonZeroU32> {
    NonZeroU32::new(bytes.min(u64::from(u32::MAX)) as u32)
}

async fn make(tunnel: bool, limit: u64) -> Result<Live, Error> {
    // (the engine's HTTPS client takes the process's default provider; an error only says one is already there)
    let _ = rustls::crypto::ring::default_provider().install_default();
    let fail = |what: &str, e: String| Error::Engine(format!("{what}: {e}"));
    let peer_opts = Some(PeerConnectionOptions { connect_timeout: Some(PEER_CONNECT), ..Default::default() });
    let mut opts = SessionOptions {
        // (nothing of the engine's own is written anywhere: leaf keeps the list of torrents)
        persistence: None,
        fastresume: false,
        // never announced on the local network, on either route
        disable_local_service_discovery: true,
        // (the engine's own client name and peer id are kept: "leaf" would single out its few users among a torrent's peers)
        client_name_and_version: None,
        ..Default::default()
    };
    opts.ratelimits.download_bps = limit_of(limit);
    // "Never upload": no bitfield, no "have", every request for a piece refused
    let no_upload = crate::no_upload();
    opts.disable_upload = no_upload;
    let entrance = if tunnel {
        let rt = tokio::runtime::Handle::current();
        let entrance = leaf_vpn::socks::start_private(&rt, TUNNEL_CONNECTIONS, Some((crate::gateway::GATEWAY_HOST.to_string(), crate::gateway::own(rt.clone())))).map_err(|e| fail("the entrance to the VPN could not be opened", e.to_string()))?;
        // `socks5h`: names (the HTTP trackers') are looked up by the entrance, inside the tunnel
        let url = format!("socks5h://{}:{}@127.0.0.1:{}", entrance.credentials.user, entrance.credentials.password.as_str(), entrance.port);
        // the proxy and nothing else: no TCP of the engine's own, no DHT, no listener (so no uTP and no port forwarding),
        // no IPv6 (the tunnel carries IPv4)
        opts.connect = Some(ConnectionOptions { proxy_url: Some(url), enable_tcp: false, peer_opts });
        opts.dht = None;
        opts.listen = None;
        opts.ipv4_only = true;
        opts.peer_limit = Some(TUNNEL_PEERS);
        Some(entrance)
    } else {
        opts.connect = Some(ConnectionOptions { proxy_url: None, enable_tcp: true, peer_opts });
        // (the DHT's table is not kept on disk; no port is opened on the router)
        opts.dht = test_peers().is_none().then(|| DhtSessionConfig { bootstrap_addrs: None, port: None, persistence: None });
        opts.listen = Some(ListenerOptions { enable_upnp_port_forwarding: false, ..Default::default() });
        opts.peer_limit = Some(DIRECT_PEERS);
        None
    };
    let session = Session::new_with_opts(std::env::temp_dir(), opts).await.map_err(|e| fail("the torrent engine could not start", format!("{e:#}")))?;
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    log::info!("torrent: the engine runs {}", if tunnel { "through the VPN only" } else { "on this PC's own network" });
    Ok(Live { tunnel, no_upload, session, _entrance: entrance, generation })
}

async fn stop(live: Live) {
    // (so a job that still holds the old session sees at once that it is not the one of now)
    GENERATION.fetch_add(1, Ordering::AcqRel);
    live.session.stop().await;
    log::info!("torrent: the engine stopped");
}

/// The session for the route, made (and the one for the other route stopped first) when it is not the one running.
async fn session_for(tunnel: bool, limit: u64) -> Result<(Arc<Session>, u64), Error> {
    let mut g = ENGINE.lock().await;
    if let Some(live) = g.as_ref()
        && live.tunnel == tunnel
        && live.no_upload == crate::no_upload()
        && GENERATION.load(Ordering::Acquire) == live.generation
    {
        return Ok((live.session.clone(), live.generation));
    }
    if let Some(old) = g.take() {
        stop(old).await;
    }
    let live = make(tunnel, limit).await?;
    let out = (live.session.clone(), live.generation);
    *g = Some(live);
    Ok(out)
}

/// A setting the session is made with changed: the jobs put their torrents into a new one.
pub(crate) fn restart() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

struct JobGuard;

impl Drop for JobGuard {
    fn drop(&mut self) {
        if JOBS.fetch_sub(1, Ordering::AcqRel) == 1 {
            // the last torrent went: the engine stops (nothing runs, nothing listens, nothing is connected)
            runtime().spawn(async {
                let mut g = ENGINE.lock().await;
                if JOBS.load(Ordering::Acquire) == 0
                    && let Some(old) = g.take()
                {
                    stop(old).await;
                }
            });
        }
    }
}

pub(crate) fn run(spec: &Spec, ctl: &Control, host: &dyn Host) -> Result<Done, Error> {
    let parsed = Parsed::of(&spec.source)?;
    JOBS.fetch_add(1, Ordering::AcqRel);
    let _guard = JobGuard;
    runtime().block_on(job(spec, &parsed, ctl, host))
}

const TICK: Duration = Duration::from_millis(400);

enum Round {
    Done(Done),
    /// The route or the session changed: from the start, on the route of now.
    Again,
}

/// Why a round stops before its end, looked at on every tick.
fn stop_reason(ctl: &Control, tunnel: bool, generation: u64) -> Option<Result<Round, Error>> {
    if ctl.cancel.load(Ordering::Relaxed) {
        return Some(Err(Error::Cancelled));
    }
    if GENERATION.load(Ordering::Acquire) != generation {
        return Some(Ok(Round::Again));
    }
    match crate::go_now() {
        // (the route this session was made for is away for now: nothing can be sent, the torrent waits in place)
        Go::Wait => None,
        Go::Tunnel if tunnel => None,
        Go::Direct if !tunnel => None,
        _ => Some(Ok(Round::Again)),
    }
}

async fn job(spec: &Spec, parsed: &Parsed, ctl: &Control, host: &dyn Host) -> Result<Done, Error> {
    // (asked once, also when the route changes and the torrent is put into the engine again)
    let mut chosen: Option<Vec<usize>> = spec.only.clone();
    let mut seen = Progress { step: Step::WaitingForVpn, name: parsed.name.clone(), done_bytes: 0, total_bytes: 0, peers: 0, tunnel: true };
    loop {
        let tunnel = loop {
            if ctl.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            match crate::go_now() {
                Go::Tunnel => break true,
                Go::Direct => break false,
                Go::Wait => {
                    seen.step = Step::WaitingForVpn;
                    seen.peers = 0;
                    host.progress(&seen);
                    tokio::time::sleep(TICK).await;
                }
            }
        };
        seen.tunnel = tunnel;
        let (session, generation) = session_for(tunnel, ctl.limit.load(Ordering::Relaxed)).await?;
        match round(&session, generation, tunnel, spec, parsed, ctl, host, &mut seen, &mut chosen).await? {
            Round::Done(d) => return Ok(d),
            Round::Again => continue,
        }
    }
}

/// The torrent as the engine is given it on this route.
fn add_of(tunnel: bool, parsed: &Parsed, info: Option<&[u8]>) -> AddTorrent<'static> {
    let trackers = trackers_for(tunnel, parsed);
    match info.or(parsed.info.as_deref()) {
        Some(info) => AddTorrent::from_bytes(torrent_with(info, &trackers)),
        None => AddTorrent::from_url(magnet_with(&parsed.hash, &trackers)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn round(session: &Arc<Session>, generation: u64, tunnel: bool, spec: &Spec, parsed: &Parsed, ctl: &Control, host: &dyn Host, seen: &mut Progress, chosen: &mut Option<Vec<usize>>) -> Result<Round, Error> {
    let engine_err = |e: anyhow::Error| Error::Engine(format!("{e:#}"));
    // 1. what the torrent holds (a magnet link: asked from the first peers, which can take a while)
    seen.step = if parsed.info.is_some() { Step::Checking } else { Step::Metadata };
    host.progress(seen);
    let listed = {
        let add = session.add_torrent(add_of(tunnel, parsed, None), Some(AddTorrentOptions { list_only: true, initial_peers: test_peers(), ..Default::default() }));
        tokio::pin!(add);
        loop {
            tokio::select! {
                r = &mut add => break r.map_err(engine_err)?,
                _ = tokio::time::sleep(TICK) => {
                    if let Some(out) = stop_reason(ctl, tunnel, generation) {
                        return out;
                    }
                    seen.step = if crate::go_now() == Go::Wait { Step::WaitingForVpn } else if parsed.info.is_some() { Step::Checking } else { Step::Metadata };
                    host.progress(seen);
                }
            }
        }
    };
    let (info_bytes, files, total, name, peers) = match listed {
        AddTorrentResponse::ListOnly(l) => {
            let files: Vec<(PathBuf, u64)> = l.info.iter_file_details().map(|f| (f.filename.to_pathbuf(), f.len)).collect();
            let total: u64 = files.iter().map(|f| f.1).sum();
            let name = l.info.name().map(|n| n.to_string()).or_else(|| parsed.name.clone()).unwrap_or_default();
            // (the engine's bytes hold the trackers it was given; only the "info" dictionary is taken from them)
            let info = librqbit::torrent_from_bytes(&l.torrent_bytes).map(|t| t.info.raw_bytes.as_ref().to_vec()).map_err(|e| Error::Engine(format!("{e:#}")))?;
            (info, files, total, name, l.seen_peers)
        }
        _ => return Err(Error::Duplicate),
    };
    let name = safe_name(&name);
    let name = if name.is_empty() { format!("torrent-{}", &parsed.hash[..12]) } else { name };
    // a single file goes straight into the folder; several get a folder named after the torrent
    let (folder, result) = match files.as_slice() {
        [(only, _)] => (spec.dest.clone(), spec.dest.join(only)),
        _ => (spec.dest.join(&name), spec.dest.join(&name)),
    };
    seen.name = Some(name.clone());
    // several files: which of them (asked once; the answer may wait for the user, so not on a thread the engine needs)
    if files.len() > 1 && chosen.is_none() {
        let list: Vec<(String, u64)> = files.iter().map(|(p, len)| (p.to_string_lossy().replace('\\', "/"), *len)).collect();
        let picked = tokio::task::block_in_place(|| host.choose(&name, &list)).map(|mut v| {
            v.retain(|i| *i < list.len());
            v.sort_unstable();
            v.dedup();
            v
        });
        match picked {
            Some(v) if !v.is_empty() => *chosen = Some(v),
            _ => return Err(Error::Cancelled),
        }
        if let Some(out) = stop_reason(ctl, tunnel, generation) {
            return out;
        }
    }
    let only = chosen.clone().filter(|c| files.len() > 1 && c.len() < files.len());
    let total = match &only {
        Some(c) => c.iter().map(|i| files[*i].1).sum(),
        None => total,
    };
    seen.total_bytes = total;
    // 2. into the engine for real
    let opts = AddTorrentOptions {
        output_folder: Some(folder.to_string_lossy().into_owned()),
        // (a download that was started before goes on from what is there: the pieces on disk are checked first)
        overwrite: true,
        only_files: only,
        initial_peers: test_peers().or((!peers.is_empty()).then_some(peers)),
        paused: ctl.paused.load(Ordering::Relaxed),
        ..Default::default()
    };
    let (id, handle) = match session.add_torrent(add_of(tunnel, parsed, Some(&info_bytes)), Some(opts)).await.map_err(engine_err)? {
        AddTorrentResponse::Added(id, h) => (id, h),
        _ => return Err(Error::Duplicate),
    };
    let leave = |session: Arc<Session>| async move {
        // (out of the engine, the files stay)
        let _ = session.delete(id.into(), false).await;
    };
    // 3. until the last piece is checked
    let mut paused = handle.is_paused();
    let mut limit = ctl.limit.load(Ordering::Relaxed);
    loop {
        tokio::time::sleep(TICK).await;
        if let Some(out) = stop_reason(ctl, tunnel, generation) {
            leave(session.clone()).await;
            return out;
        }
        let want_paused = ctl.paused.load(Ordering::Relaxed);
        if want_paused != paused {
            let r = if want_paused { session.pause(&handle).await } else { session.unpause(&handle).await };
            if let Err(e) = r {
                log::debug!("torrent: pause or resume: {e:#}");
            }
            paused = want_paused;
        }
        let want_limit = ctl.limit.load(Ordering::Relaxed);
        if want_limit != limit {
            limit = want_limit;
            session.ratelimits.set_download_bps(limit_of(limit));
        }
        let stats = handle.stats();
        if let Some(e) = stats.error.as_ref().filter(|_| matches!(stats.state, TorrentStatsState::Error)) {
            let e = e.clone();
            leave(session.clone()).await;
            return Err(Error::Engine(e));
        }
        if stats.finished && stats.total_bytes > 0 {
            // complete: it leaves the engine now, so nothing is uploaded after this
            leave(session.clone()).await;
            return Ok(Round::Done(Done { name, path: result, bytes: stats.total_bytes }));
        }
        seen.done_bytes = stats.progress_bytes;
        seen.total_bytes = if stats.total_bytes > 0 { stats.total_bytes } else { total };
        seen.done_bytes = seen.done_bytes.min(seen.total_bytes);
        seen.peers = stats.live.as_ref().map_or(0, |l| l.snapshot.peer_stats.live as usize);
        seen.step = if paused {
            Step::Paused
        } else if crate::go_now() == Go::Wait {
            Step::WaitingForVpn
        } else if matches!(stats.state, TorrentStatsState::Initializing { .. }) {
            Step::Checking
        } else {
            Step::Downloading
        };
        host.progress(seen);
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::atomic::AtomicBool;

    use sha1::Digest as _;

    use super::*;
    use crate::{Route, Source};

    struct Quiet(AtomicBool);

    impl Host for Quiet {
        fn progress(&self, p: &Progress) {
            if p.step == Step::WaitingForVpn {
                self.0.store(true, Ordering::Relaxed);
            }
        }
    }

    /// A whole download from a seeder on this PC (no tracker, no DHT, nothing outside 127.0.0.1): the rule first holds it
    /// while there is no VPN, then, turned off, lets it go; the file arrives complete and the engine stops after it.
    #[test]
    fn a_torrent_waits_for_the_vpn_and_downloads_once_the_rule_is_off() {
        let dir = std::env::temp_dir().join(format!("leaf-torrent-test-{}", std::process::id()));
        let (seed, dest) = (dir.join("seed"), dir.join("dest"));
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        std::fs::write(seed.join("data.bin"), &data).unwrap();
        let mut info = format!("d6:lengthi{}e4:name8:data.bin12:piece lengthi65536e6:pieces{}:", data.len(), data.len().div_ceil(65536) * 20).into_bytes();
        for piece in data.chunks(65536) {
            info.extend_from_slice(&sha1::Sha1::digest(piece));
        }
        info.push(b'e');
        let torrent = torrent_with(&info, &[]);

        // the seeder: a plain session that listens on 127.0.0.1 and has the whole file
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let seeder = runtime().block_on(async {
            let opts = SessionOptions {
                dht: None,
                disable_local_service_discovery: true,
                ipv4_only: true,
                listen: Some(ListenerOptions { listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)), ipv4_only: true, ..Default::default() }),
                ..Default::default()
            };
            let s = Session::new_with_opts(seed.clone(), opts).await.unwrap();
            let h = s.add_torrent(AddTorrent::from_bytes(torrent.clone()), Some(AddTorrentOptions { overwrite: true, output_folder: Some(seed.to_string_lossy().into_owned()), ..Default::default() })).await.unwrap().into_handle().unwrap();
            h.wait_until_initialized().await.unwrap();
            assert!(h.stats().finished, "the seeder has the file");
            s
        });
        *TEST_PEERS.lock().unwrap() = Some(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))]);

        let spec = Spec { source: Source::File(torrent), dest: dest.clone(), only: None };
        let ctl = Control::default();
        let host = Quiet(AtomicBool::new(false));
        // the rule is on and there is no VPN: nothing starts; turning the rule off lets it go
        assert!(crate::route().vpn_only);
        let done = std::thread::scope(|sc| {
            let job = sc.spawn(|| run(&spec, &ctl, &host));
            std::thread::sleep(Duration::from_millis(1500));
            assert!(host.0.load(Ordering::Relaxed), "it waits for the VPN");
            assert!(!dest.join("data.bin").exists(), "nothing was made while waiting");
            assert!(runtime().block_on(async { ENGINE.lock().await.is_none() }), "no engine runs while waiting");
            crate::set_route(Route { vpn_only: false, through_vpn: false });
            job.join().unwrap()
        });
        crate::set_route(Route { vpn_only: true, through_vpn: false });
        let done = done.unwrap();
        assert_eq!(done.path, dest.join("data.bin"));
        assert_eq!(std::fs::read(&done.path).unwrap(), data);
        // the last job went: the engine stops
        std::thread::sleep(Duration::from_millis(2500));
        assert!(runtime().block_on(async { ENGINE.lock().await.is_none() }));
        runtime().block_on(seeder.stop());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
