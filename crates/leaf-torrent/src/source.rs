//! What a torrent is started from (a magnet link or a .torrent file), and the trackers the engine is given for it. On the
//! tunnel route the engine never sees a UDP or an HTTPS tracker's address: each one is replaced by an address of
//! [`crate::gateway`], which asks the tracker through the tunnel (HTTPS there, because the engine's own certificate check
//! lets the system fetch revocation data outside the VPN). Anything that is not HTTP(S) or UDP is dropped there.

use librqbit::{Magnet, torrent_from_bytes};

use crate::Error;
use crate::gateway::GATEWAY_HOST;

#[derive(Clone, Debug)]
pub enum Source {
    Magnet(String),
    /// The bytes of a .torrent file.
    File(Vec<u8>),
}

/// A .torrent file larger than this is not one.
pub const MAX_TORRENT_BYTES: usize = 32 << 20;

/// A source, read.
#[derive(Clone, Debug)]
pub(crate) struct Parsed {
    /// 40 hex digits.
    pub hash: String,
    pub name: Option<String>,
    pub trackers: Vec<String>,
    /// The raw "info" dictionary (a file; a magnet link has none yet).
    pub info: Option<Vec<u8>>,
    /// The torrent says it must only use its own trackers.
    pub private: bool,
}

impl Parsed {
    pub fn of(source: &Source) -> Result<Parsed, Error> {
        match source {
            Source::Magnet(m) => {
                let magnet = Magnet::parse(m.trim()).map_err(|e| Error::NotTorrent(format!("{e:#}")))?;
                let id = magnet.as_id20().ok_or_else(|| Error::NotTorrent("the magnet link has no BitTorrent v1 hash".into()))?;
                Ok(Parsed { hash: id.as_string(), name: magnet.name.clone().filter(|n| !n.trim().is_empty()), trackers: magnet.trackers.clone(), info: None, private: false })
            }
            Source::File(bytes) => {
                if bytes.len() > MAX_TORRENT_BYTES {
                    return Err(Error::NotTorrent("the file is too large to be a .torrent file".into()));
                }
                let t = torrent_from_bytes(bytes).map_err(|e| Error::NotTorrent(format!("{e:#}")))?;
                let mut trackers: Vec<String> = Vec::new();
                for a in t.iter_announce() {
                    if let Ok(s) = std::str::from_utf8(a.as_ref())
                        && !trackers.iter().any(|x| x == s)
                    {
                        trackers.push(s.to_string());
                    }
                }
                Ok(Parsed {
                    hash: t.info_hash.as_string(),
                    name: t.info.data.name.as_ref().and_then(|n| std::str::from_utf8(n.as_ref()).ok()).map(str::to_string).filter(|n| !n.trim().is_empty()),
                    trackers,
                    info: Some(t.info.raw_bytes.as_ref().to_vec()),
                    private: t.info.data.private,
                })
            }
        }
    }
}

/// The name and the hash (40 hex digits) of a source, for a title before anything is downloaded.
pub fn describe(source: &Source) -> Result<(Option<String>, String), Error> {
    let p = Parsed::of(source)?;
    Ok((p.name, p.hash))
}

/// Public trackers asked as well on the tunnel route (not for a private torrent): there is no DHT there, and a magnet
/// link often names none. They learn the torrent's hash and the VPN's address, as any tracker of the torrent does.
const EXTRA_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
];

/// `udp://host:port/...` as the gateway's address for it; `None` for what cannot go through the tunnel (IPv6, no port, a
/// name with characters that do not belong in one).
fn through_gateway(udp: &str) -> Option<String> {
    let rest = udp.strip_prefix("udp://")?;
    let hostport = rest.split(['/', '?', '#']).next()?;
    let (host, port) = hostport.rsplit_once(':')?;
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    if host.is_empty() || host.len() > 253 || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    Some(format!("http://{GATEWAY_HOST}/udp/{}/{port}", host.to_ascii_lowercase()))
}

/// `https://host[:port]/path?query` as the gateway's address for it (the path and the query stay as they are: a private
/// tracker's passkey is in them); `None` for what cannot go through the tunnel.
fn through_gateway_tls(https: &str) -> Option<String> {
    let rest = &https[8..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (hostport, tail) = rest.split_at(end);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => (hostport, 443),
    };
    if host.is_empty() || host.len() > 253 || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    let tail = tail.split('#').next().unwrap_or("");
    let tail = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
    if tail.bytes().any(|b| b <= b' ' || b >= 0x7f) {
        return None;
    }
    Some(format!("http://{GATEWAY_HOST}/tls/{}/{port}{tail}", host.to_ascii_lowercase()))
}

fn scheme_is(url: &str, scheme: &str) -> bool {
    url.len() > scheme.len() && url[..scheme.len()].eq_ignore_ascii_case(scheme)
}

/// The trackers the engine gets. `tunnel`: plain HTTP ones as they are (the engine's HTTP goes through the SOCKS
/// entrance), UDP and HTTPS ones as gateway addresses, the rest dropped; never a `udp://` or `https://` address.
pub(crate) fn trackers_for(tunnel: bool, p: &Parsed) -> Vec<String> {
    if !tunnel {
        return p.trackers.clone();
    }
    let mut out: Vec<String> = Vec::new();
    let extra = if p.private { &[][..] } else { EXTRA_TRACKERS };
    for t in p.trackers.iter().map(String::as_str).chain(extra.iter().copied()) {
        let t = t.trim();
        let mapped = if scheme_is(t, "http://") {
            Some(t.to_string())
        } else if scheme_is(t, "https://") {
            through_gateway_tls(t)
        } else if scheme_is(t, "udp://") {
            through_gateway(&format!("udp://{}", &t[6..]))
        } else {
            None
        };
        if let Some(m) = mapped
            && !out.contains(&m)
        {
            out.push(m);
        }
    }
    out
}

fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A magnet link with exactly these trackers.
pub(crate) fn magnet_with(hash: &str, trackers: &[String]) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{hash}");
    for t in trackers {
        m.push_str("&tr=");
        m.push_str(&percent(t));
    }
    m
}

/// A .torrent file of the "info" dictionary with exactly these trackers (each a tier of its own).
pub(crate) fn torrent_with(info: &[u8], trackers: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(info.len() + 256);
    let text = |out: &mut Vec<u8>, s: &str| {
        out.extend_from_slice(format!("{}:", s.len()).as_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    out.push(b'd');
    if let Some(first) = trackers.first() {
        text(&mut out, "announce");
        text(&mut out, first);
        text(&mut out, "announce-list");
        out.push(b'l');
        for t in trackers {
            out.push(b'l');
            text(&mut out, t);
            out.push(b'e');
        }
        out.push(b'e');
    }
    text(&mut out, "info");
    out.extend_from_slice(info);
    out.push(b'e');
    out
}

/// A name from a torrent as one folder or file name: never a path, never a name the system refuses.
pub(crate) fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    let reserved = ["con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9"];
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_lowercase();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        String::new()
    } else if reserved.contains(&stem.as_str()) {
        format!("_{cleaned}")
    } else {
        cleaned.chars().take(180).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn parsed(trackers: &[&str], private: bool) -> Parsed {
        Parsed { hash: HASH.into(), name: None, trackers: trackers.iter().map(|s| s.to_string()).collect(), info: None, private }
    }

    #[test]
    fn no_udp_address_reaches_the_engine_on_the_tunnel_route() {
        let p = parsed(&["udp://tracker.example.com:6969/announce", "UDP://Other.Example.com:80", "https://example.com/announce", "wss://example.com/x", "udp://[2001:db8::1]:6969", "udp://bad host:1", "udp://noport.example.com"], false);
        let t = trackers_for(true, &p);
        assert!(t.iter().all(|t| t.starts_with("http://")), "{t:?}");
        assert!(t.contains(&format!("http://{GATEWAY_HOST}/udp/tracker.example.com/6969")));
        assert!(t.contains(&format!("http://{GATEWAY_HOST}/udp/other.example.com/80")));
        assert!(t.contains(&format!("http://{GATEWAY_HOST}/tls/example.com/443/announce")));
        assert!(!t.iter().any(|t| t.contains("2001") || t.contains("bad") || t.contains("noport") || t.contains("wss")));
        // (the public ones are added, each through the gateway)
        assert!(t.len() > 3);
        // direct: as the torrent has them
        assert_eq!(trackers_for(false, &p), p.trackers);
    }

    #[test]
    fn a_private_torrent_keeps_to_its_own_trackers() {
        let p = parsed(&["https://example.com/announce?passkey=1"], true);
        assert_eq!(trackers_for(true, &p), vec![format!("http://{GATEWAY_HOST}/tls/example.com/443/announce?passkey=1")]);
        let p = parsed(&["https://Example.com:8443", "http://example.com/a"], true);
        assert_eq!(trackers_for(true, &p), vec![format!("http://{GATEWAY_HOST}/tls/example.com/8443/"), "http://example.com/a".to_string()]);
    }

    #[test]
    fn a_magnet_link_is_read_and_written_with_other_trackers() {
        let src = Source::Magnet(format!("magnet:?xt=urn:btih:{HASH}&dn=Some+Name&tr=udp%3A%2F%2Ftracker.example.com%3A6969"));
        let p = Parsed::of(&src).unwrap();
        assert_eq!(p.hash, HASH);
        assert_eq!(p.trackers, vec!["udp://tracker.example.com:6969".to_string()]);
        let own = Parsed { private: true, ..p };
        let again = magnet_with(&own.hash, &trackers_for(true, &own));
        assert!(!again.contains("udp%3A") && !again.contains("udp:"), "{again}");
        let back = Parsed::of(&Source::Magnet(again)).unwrap();
        assert_eq!(back.hash, HASH);
        assert_eq!(back.trackers, vec![format!("http://{GATEWAY_HOST}/udp/tracker.example.com/6969")]);
    }

    #[test]
    fn a_torrent_file_keeps_its_hash_with_other_trackers() {
        let info = b"d6:lengthi5e4:name5:a.txt12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let first = torrent_with(info, &["udp://tracker.example.com:6969".to_string()]);
        let a = Parsed::of(&Source::File(first)).unwrap();
        assert_eq!(a.name.as_deref(), Some("a.txt"));
        assert_eq!(a.trackers, vec!["udp://tracker.example.com:6969".to_string()]);
        let second = torrent_with(a.info.as_ref().unwrap(), &trackers_for(true, &a));
        assert!(!second.windows(6).any(|w| w == b"udp://") && !second.windows(8).any(|w| w == b"https://"));
        let b = Parsed::of(&Source::File(second)).unwrap();
        assert_eq!(a.hash, b.hash);
        assert!(Parsed::of(&Source::File(b"not a torrent".to_vec())).is_err());
    }

    #[test]
    fn names_from_a_torrent_are_never_paths() {
        assert_eq!(safe_name("../../x"), ".._.._x");
        assert_eq!(safe_name("a:b|c."), "a_b_c");
        assert_eq!(safe_name(".."), "");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
    }
}
