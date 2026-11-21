//! Mullvad as a [`Provider`]: the public relay list, the choice of a server (a country and city, or the fastest one
//! measured by a WireGuard handshake), the resolver inside (plain or with Mullvad's DNS blocking), and the exit check
//! (`am.i.mullvad.net`). The endpoints are in `research\MULLVAD.md`; nothing else in leaf knows Mullvad's API.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::tunnel::{PrivateKey, TunnelConfig, base64_decode};
use crate::{BoxFuture, Exit, Prepared, Provider, Tunnel};

pub(crate) const API_HOST: &str = "api.mullvad.net";
/// WireGuard's port on every relay (it is in each relay's port ranges).
const WG_PORT: u16 = 51820;
const API_TIMEOUT: Duration = Duration::from_secs(20);
/// The relay list is read again after this long.
const RELAYS_FRESH: Duration = Duration::from_secs(3600);

/// One WireGuard server.
#[derive(Clone, Debug, PartialEq)]
pub struct Relay {
    /// `se-got-wg-001`
    pub hostname: String,
    pub country: String,
    /// `se`
    pub country_code: String,
    pub city: String,
    /// `got`
    pub city_code: String,
    pub ipv4: Ipv4Addr,
    pub public_key: [u8; 32],
    pub weight: u64,
    /// Mullvad's own hardware (not rented).
    pub owned: bool,
}

#[derive(Deserialize)]
struct RawList {
    locations: std::collections::HashMap<String, RawLocation>,
    wireguard: RawWireguard,
}

#[derive(Deserialize)]
struct RawLocation {
    country: String,
    city: String,
}

#[derive(Deserialize)]
struct RawWireguard {
    relays: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawRelay {
    hostname: String,
    location: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    owned: bool,
    ipv4_addr_in: Ipv4Addr,
    public_key: String,
    #[serde(default)]
    weight: u64,
}

/// The active relays of a relay list (`/app/v1/relays`); a malformed entry is left out, not the whole list.
pub fn parse_relays(json: &[u8]) -> Result<Vec<Relay>, String> {
    let raw: RawList = serde_json::from_slice(json).map_err(|e| format!("Mullvad's server list could not be read ({e})"))?;
    let mut out = Vec::new();
    for v in raw.wireguard.relays {
        let Ok(r) = serde_json::from_value::<RawRelay>(v) else { continue };
        if !r.active {
            continue;
        }
        let Some(key) = base64_decode(&r.public_key).and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok()) else { continue };
        let Some(loc) = raw.locations.get(&r.location) else { continue };
        let (cc, city_code) = r.location.split_once('-').unwrap_or((&r.location, ""));
        out.push(Relay {
            hostname: r.hostname,
            country: loc.country.clone(),
            country_code: cc.to_string(),
            city: loc.city.clone(),
            city_code: city_code.to_string(),
            ipv4: r.ipv4_addr_in,
            public_key: key,
            weight: r.weight.max(1),
            owned: r.owned,
        });
    }
    out.sort_by(|a, b| (&a.country, &a.city, &a.hostname).cmp(&(&b.country, &b.city, &b.hostname)));
    Ok(out)
}

static RELAYS: Mutex<Option<(Instant, Vec<Relay>)>> = Mutex::new(None);

/// Mullvad's WireGuard servers (asked once an hour; the list is public, no account is sent).
pub async fn relays() -> Result<Vec<Relay>, ApiError> {
    if let Some((at, list)) = RELAYS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && at.elapsed() < RELAYS_FRESH
    {
        return Ok(list.clone());
    }
    let a = api("GET", "/app/v1/relays", None, None).await?;
    if a.status != 200 {
        return Err(ApiError::from_answer(a.status, &a.body));
    }
    let list = parse_relays(&a.body).map_err(ApiError::Network)?;
    *RELAYS.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// The countries and their cities of `relays`: `(country, code, [(city, code)])`, sorted by name.
pub fn places(relays: &[Relay]) -> Vec<(String, String, Vec<(String, String)>)> {
    let mut out: Vec<(String, String, Vec<(String, String)>)> = Vec::new();
    for r in relays {
        match out.iter_mut().find(|(_, cc, _)| *cc == r.country_code) {
            Some((_, _, cities)) => {
                if !cities.iter().any(|(_, c)| *c == r.city_code) {
                    cities.push((r.city.clone(), r.city_code.clone()));
                }
            }
            None => out.push((r.country.clone(), r.country_code.clone(), vec![(r.city.clone(), r.city_code.clone())])),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, _, cities) in &mut out {
        cities.sort();
    }
    out
}

// ------------------------------------------------------------------------------------------------ errors

/// What Mullvad's API said, in leaf's words. Never carries the account number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// No such account number.
    InvalidAccount,
    /// The account has its five devices already.
    MaxDevices,
    /// This device was removed (on Mullvad's website, or by another copy).
    DeviceGone,
    /// The account's time ran out.
    Expired,
    /// No answer, or one that could not be read (the details).
    Network(String),
    /// Another refusal: the status and Mullvad's error code.
    Server(u16, String),
}

impl ApiError {
    pub(crate) fn from_answer(status: u16, body: &[u8]) -> ApiError {
        let code = serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string)).unwrap_or_default();
        match code.as_str() {
            "INVALID_ACCOUNT" | "INVALID_AUTH" => ApiError::InvalidAccount,
            "MAX_DEVICES_REACHED" => ApiError::MaxDevices,
            "DEVICE_NOT_FOUND" => ApiError::DeviceGone,
            _ if status == 401 || status == 403 => ApiError::InvalidAccount,
            // (the code is Mullvad's word for the error: letters and underscores only, so never anything the user typed)
            _ => ApiError::Server(status, code.chars().filter(|c| c.is_ascii_uppercase() || *c == '_').take(40).collect()),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::InvalidAccount => f.write_str("Mullvad does not know this account number. Check it on mullvad.net."),
            ApiError::MaxDevices => f.write_str(
                "This Mullvad account already has its five devices. Remove one on mullvad.net (Account > Devices), then sign in again.",
            ),
            ApiError::DeviceGone => f.write_str("This device was removed from the Mullvad account. Sign in again."),
            ApiError::Expired => f.write_str("The Mullvad account has no time left. Add time on mullvad.net."),
            ApiError::Network(d) => write!(f, "Mullvad's server could not be reached.\n{d}"),
            ApiError::Server(s, code) if code.is_empty() => write!(f, "Mullvad's server refused the request (HTTP {s})."),
            ApiError::Server(s, code) => write!(f, "Mullvad's server refused the request (HTTP {s}, {code})."),
        }
    }
}

impl std::error::Error for ApiError {}

/// One call to Mullvad's API, directly (not through the tunnel: it is used to set the tunnel up).
pub(crate) async fn api(method: &str, path: &str, bearer: Option<&str>, body: Option<&[u8]>) -> Result<crate::http::Answer, ApiError> {
    let tcp = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect((API_HOST, 443)))
        .await
        .map_err(|_| ApiError::Network("no answer".into()))?
        .map_err(|e| ApiError::Network(e.to_string()))?;
    crate::http::request(tcp, API_HOST, method, path, bearer, body, API_TIMEOUT).await.map_err(ApiError::Network)
}

// ------------------------------------------------------------------------------------------------ DNS

/// Mullvad's DNS blocking lists (the resolver inside the tunnel answers "no such name" for what they hold).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DnsBlocking {
    pub ads: bool,
    pub trackers: bool,
    pub malware: bool,
    pub adult: bool,
    pub gambling: bool,
    pub social: bool,
}

impl DnsBlocking {
    /// The resolver: `10.64.0.1` without blocking, `100.64.0.<lists>` with.
    pub fn resolver(&self) -> Ipv4Addr {
        let bits = [self.ads, self.trackers, self.malware, self.adult, self.gambling, self.social].iter().enumerate().fold(0u8, |acc, (i, on)| if *on { acc | (1 << i) } else { acc });
        if bits == 0 { Ipv4Addr::new(10, 64, 0, 1) } else { Ipv4Addr::new(100, 64, 0, bits) }
    }
}

// ------------------------------------------------------------------------------------------------ the provider

/// This PC's device on the Mullvad account: its key (kept in the keyring only) and the address Mullvad gave it.
#[derive(Clone)]
pub struct Device {
    pub id: String,
    /// Mullvad's name for it (two words, "happy seal").
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub key: PrivateKey,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device").field("name", &self.name).field("ipv4", &self.ipv4).finish_non_exhaustive()
    }
}

/// Where to connect: a country (code), a city in it, or anywhere; the fastest of those (measured) or one by Mullvad's weights.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub country: Option<String>,
    pub city: Option<String>,
    pub fastest: bool,
}

pub struct Mullvad {
    pub device: Device,
    pub place: Place,
    pub dns: DnsBlocking,
}

impl Provider for Mullvad {
    fn name(&self) -> &'static str {
        "Mullvad"
    }

    fn prepare(&self) -> BoxFuture<'_, Result<Prepared, String>> {
        Box::pin(async move {
            let all = relays().await.map_err(|e| e.to_string())?;
            let mut pool: Vec<Relay> = all
                .into_iter()
                .filter(|r| self.place.country.as_deref().is_none_or(|c| c.eq_ignore_ascii_case(&r.country_code)))
                .filter(|r| self.place.city.as_deref().is_none_or(|c| c.eq_ignore_ascii_case(&r.city_code)))
                .collect();
            if pool.is_empty() {
                return Err("Mullvad has no server in the chosen place right now. Choose another country or city.".to_string());
            }
            let relay = if self.place.fastest {
                match fastest(&pool, &self.device.key).await {
                    Some((r, rtt)) => {
                        log::info!("vpn: fastest server {} ({} ms)", r.hostname, rtt.as_millis());
                        r
                    }
                    // (none answered in time: Mullvad's own weighting, the tunnel then says what is wrong)
                    None => pick_weighted(&mut pool),
                }
            } else {
                pick_weighted(&mut pool)
            };
            Ok(Prepared {
                config: TunnelConfig {
                    private_key: self.device.key.clone(),
                    peer_public: relay.public_key,
                    endpoint: SocketAddr::new(IpAddr::V4(relay.ipv4), WG_PORT),
                    address: self.device.ipv4,
                    dns: self.dns.resolver(),
                },
                country: relay.country,
                city: relay.city,
                server: relay.hostname,
            })
        })
    }

    fn exit_check<'a>(&'a self, tunnel: &'a Tunnel) -> BoxFuture<'a, Result<Exit, String>> {
        Box::pin(async move {
            const HOST: &str = "am.i.mullvad.net";
            let ips = tunnel.resolve(HOST).await.map_err(|e| format!("the check's name could not be looked up through the VPN ({e})"))?;
            let mut last = String::from("no address");
            let mut answer = None;
            for ip in ips {
                match tunnel.connect(ip, 443, Duration::from_secs(10)).await {
                    Ok(s) => match crate::http::request(s, HOST, "GET", "/json", None, None, Duration::from_secs(10)).await {
                        Ok(a) if a.status == 200 => {
                            answer = Some(a);
                            break;
                        }
                        Ok(a) => last = format!("HTTP {}", a.status),
                        Err(e) => last = e,
                    },
                    Err(e) => last = e.to_string(),
                }
            }
            let a = answer.ok_or_else(|| format!("the check through the VPN did not answer ({last})"))?;
            #[derive(Deserialize)]
            struct Am {
                ip: Option<IpAddr>,
                country: Option<String>,
                city: Option<String>,
                mullvad_exit_ip: Option<bool>,
            }
            let am: Am = serde_json::from_slice(&a.body).map_err(|_| "the check's answer could not be read".to_string())?;
            // (only Mullvad's own word that the request came from one of its exits confirms it)
            if am.mullvad_exit_ip != Some(true) {
                return Err("the check says the traffic does not leave through Mullvad".to_string());
            }
            Ok(Exit { ip: am.ip, country: am.country, city: am.city })
        })
    }
}

/// One relay, by Mullvad's weights (a bigger server is picked more often).
fn pick_weighted(pool: &mut [Relay]) -> Relay {
    let total: u64 = pool.iter().map(|r| r.weight).sum();
    let mut b = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    let mut at = u64::from_le_bytes(b) % total.max(1);
    for r in pool.iter() {
        if at < r.weight {
            return r.clone();
        }
        at -= r.weight;
    }
    pool[0].clone()
}

/// The relay that answers a WireGuard handshake first, of up to 16 of `pool` (all at once, 1.5 s at most). The key must be
/// registered (a relay does not answer an unknown key).
pub async fn fastest(pool: &[Relay], key: &PrivateKey) -> Option<(Relay, Duration)> {
    let mut sample: Vec<Relay> = pool.to_vec();
    // (a spread over the cities, not the first 16 of one)
    if sample.len() > 16 {
        let step = sample.len() as f64 / 16.0;
        sample = (0..16).map(|i| sample[(i as f64 * step) as usize].clone()).collect();
    }
    let mut set = tokio::task::JoinSet::new();
    for r in sample {
        let key = key.clone();
        set.spawn(async move { handshake_time(&r, &key).await.map(|t| (r, t)) });
    }
    let mut best: Option<(Relay, Duration)> = None;
    while let Some(done) = set.join_next().await {
        if let Ok(Some((r, t))) = done
            && best.as_ref().is_none_or(|(_, b)| t < *b)
        {
            best = Some((r, t));
        }
    }
    best
}

async fn handshake_time(r: &Relay, key: &PrivateKey) -> Option<Duration> {
    use boringtun::noise::{Tunn, TunnResult};
    let udp = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    udp.connect(SocketAddr::new(IpAddr::V4(r.ipv4), WG_PORT)).await.ok()?;
    let mut tunn = Tunn::new(key.secret_for_probe(), boringtun::x25519::PublicKey::from(r.public_key), None, None, 1, None);
    let mut out = vec![0u8; 2048];
    let start = Instant::now();
    if let TunnResult::WriteToNetwork(b) = tunn.format_handshake_initiation(&mut out, false) {
        udp.send(b).await.ok()?;
    } else {
        return None;
    }
    let mut inb = vec![0u8; 2048];
    let n = tokio::time::timeout(Duration::from_millis(1500), udp.recv(&mut inb)).await.ok()?.ok()?;
    let t = start.elapsed();
    // only a real handshake answer counts (anything else, a cookie under load, is not a measure)
    let _ = tunn.decapsulate(None, &inb[..n], &mut out);
    tunn.time_since_last_handshake().map(|_| t)
}
