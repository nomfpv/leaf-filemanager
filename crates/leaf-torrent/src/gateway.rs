//! UDP and HTTPS trackers through the tunnel. The engine can only be given a proxy for TCP: its own UDP tracker client
//! would use the PC's network, and its HTTPS certificate check lets the system fetch revocation data outside the VPN. So
//! on the tunnel route it never gets a `udp://` or `https://` address. It gets `http://GATEWAY_HOST/udp/<host>/<port>` or
//! `http://GATEWAY_HOST/tls/<host>/<port>/<path>` instead, a name the private SOCKS entrance answers here, inside leaf:
//!
//! - `udp`: the engine's HTTP announce is read, the same announce is made to the UDP tracker (BEP 15) over a UDP socket
//!   inside the tunnel, and the tracker's peers go back as an HTTP tracker's answer.
//! - `tls`: the announce is passed on over TLS on a connection inside the tunnel; the certificate is checked against the
//!   system's root certificates read from the local store (nothing is fetched for the check).
//!
//! The tracker's name is looked up inside the tunnel too. Without the tunnel there is no answer.

use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The name the engine's HTTP client asks for (`.invalid` never exists outside).
pub const GATEWAY_HOST: &str = "udp-trackers.leaf.invalid";

/// What the SOCKS entrance calls for a connection to [`GATEWAY_HOST`].
pub(crate) fn own(rt: tokio::runtime::Handle) -> leaf_vpn::socks::Own {
    std::sync::Arc::new(move |_port| {
        let (ours, theirs) = tokio::io::duplex(16 << 10);
        rt.spawn(serve(ours));
        Box::new(theirs)
    })
}

async fn serve(mut s: tokio::io::DuplexStream) {
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    let read = async {
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut buf).await.ok().filter(|n| *n > 0)?;
            head.extend_from_slice(&buf[..n]);
            if head.len() > 16 << 10 {
                return None;
            }
        }
        Some(())
    };
    if tokio::time::timeout(Duration::from_secs(10), read).await.ok().flatten().is_none() {
        return;
    }
    let body = if let Some(hash) = dht_target(&head) {
        // (the DHT is only asked, through the tunnel; with no peers yet it is asked again sooner)
        match tokio::time::timeout(Duration::from_secs(25), crate::dht::lookup(hash)).await.unwrap_or_else(|_| Err("the DHT did not answer".into())) {
            Ok(peers) => Answer { interval: if peers.is_empty() { 120 } else { 600 }, peers }.body(),
            Err(why) => {
                log::debug!("torrent: DHT: {why}");
                failure(&why)
            }
        }
    } else if let Some(t) = TlsTarget::parse(&head) {
        match tokio::time::timeout(Duration::from_secs(45), relay_tls(&t)).await.unwrap_or_else(|_| Err("the tracker did not answer".into())) {
            Ok(b) => b,
            Err(why) => {
                log::debug!("torrent: HTTPS tracker {}:{}: {why}", t.host, t.port);
                failure(&why)
            }
        }
    } else {
        match Request::parse(&head) {
            Some(req) => match announce(&req).await {
                Ok(a) => a.body(),
                Err(why) => {
                    log::debug!("torrent: UDP tracker {}:{}: {why}", req.host, req.port);
                    failure(&why)
                }
            },
            None => failure("not an announce"),
        }
    };
    let mut out = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    let _ = s.write_all(&out).await;
    let _ = s.shutdown().await;
}

fn failure(why: &str) -> Vec<u8> {
    format!("d14:failure reason{}:{why}e", why.len()).into_bytes()
}

/// The engine's HTTP announce.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub host: String,
    pub port: u16,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
    pub listen_port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    /// BEP 15: 0 none, 1 completed, 2 started, 3 stopped.
    pub event: u32,
    pub key: u32,
}

fn unpercent(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        match b[i] {
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

impl Request {
    /// `GET /<host>/<port>?info_hash=..&peer_id=..&port=..&uploaded=..&downloaded=..&left=..[&event=..][&key=..] HTTP/1.1`
    pub(crate) fn parse(head: &[u8]) -> Option<Request> {
        let text = std::str::from_utf8(head).ok()?;
        let line = text.lines().next()?;
        let mut parts = line.split(' ');
        if parts.next()? != "GET" {
            return None;
        }
        let target = parts.next()?;
        // (a proxy-style absolute address is taken too)
        let target = target.strip_prefix("http://").map(|t| t.find('/').map_or("/", |i| &t[i..])).unwrap_or(target);
        let (path, query) = target.split_once('?')?;
        let mut segs = path.strip_prefix("/udp/")?.trim_end_matches('/').split('/');
        let host = segs.next().filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))?.to_string();
        let port: u16 = segs.next()?.parse().ok().filter(|p| *p != 0)?;
        if segs.next().is_some() {
            return None;
        }
        let mut req = Request { host, port, info_hash: [0; 20], peer_id: [0; 20], listen_port: 0, uploaded: 0, downloaded: 0, left: 0, event: 0, key: 0 };
        let (mut has_hash, mut has_peer) = (false, false);
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let num = || std::str::from_utf8(&unpercent(v)).ok().and_then(|n| n.parse::<u64>().ok());
            match k {
                "info_hash" => {
                    req.info_hash = unpercent(v).try_into().ok()?;
                    has_hash = true;
                }
                "peer_id" => {
                    req.peer_id = unpercent(v).try_into().ok()?;
                    has_peer = true;
                }
                "port" => req.listen_port = num()? as u16,
                "uploaded" => req.uploaded = num()?,
                "downloaded" => req.downloaded = num()?,
                "left" => req.left = num()?,
                "key" => req.key = num().unwrap_or(0) as u32,
                "event" => {
                    req.event = match v {
                        "completed" => 1,
                        "started" => 2,
                        "stopped" => 3,
                        _ => 0,
                    }
                }
                _ => {}
            }
        }
        (has_hash && has_peer).then_some(req)
    }
}

/// `GET /dht?info_hash=.. HTTP/1.1`: the hash to look up in the DHT.
pub(crate) fn dht_target(head: &[u8]) -> Option<[u8; 20]> {
    let text = std::str::from_utf8(head).ok()?;
    let mut parts = text.lines().next()?.split(' ');
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let target = target.strip_prefix("http://").map(|t| t.find('/').map_or("/", |i| &t[i..])).unwrap_or(target);
    let query = target.strip_prefix("/dht?")?;
    query.split('&').find_map(|p| p.strip_prefix("info_hash=")).and_then(|v| unpercent(v).try_into().ok())
}

/// An HTTPS tracker's announce to pass on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TlsTarget {
    pub host: String,
    pub port: u16,
    /// The path and the query, as the tracker gets them.
    pub path: String,
    /// The engine's own `User-Agent`, passed on.
    pub agent: Option<String>,
}

impl TlsTarget {
    /// `GET /tls/<host>/<port>/<path>?<query> HTTP/1.1`
    pub(crate) fn parse(head: &[u8]) -> Option<TlsTarget> {
        let text = std::str::from_utf8(head).ok()?;
        let mut lines = text.lines();
        let mut parts = lines.next()?.split(' ');
        if parts.next()? != "GET" {
            return None;
        }
        let target = parts.next()?;
        let target = target.strip_prefix("http://").map(|t| t.find('/').map_or("/", |i| &t[i..])).unwrap_or(target);
        let mut it = target.strip_prefix("/tls/")?.splitn(3, '/');
        let host = it.next().filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))?.to_string();
        let port: u16 = it.next()?.parse().ok().filter(|p| *p != 0)?;
        let path = format!("/{}", it.next().unwrap_or(""));
        if path.bytes().any(|b| b <= b' ' || b >= 0x7f) {
            return None;
        }
        let agent = lines.find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("user-agent")).map(|(_, v)| v.trim().to_string())).filter(|a| a.bytes().all(|b| (b' '..0x7f).contains(&b)));
        Some(TlsTarget { host, port, path, agent })
    }
}

fn tls_config() -> Result<std::sync::Arc<rustls::ClientConfig>, String> {
    static CONFIG: std::sync::OnceLock<Result<std::sync::Arc<rustls::ClientConfig>, String>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            for c in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(c);
            }
            if roots.is_empty() {
                return Err("the system has no root certificates to check a tracker with".to_string());
            }
            let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().map_err(|e| e.to_string())?.with_root_certificates(roots).with_no_client_auth();
            Ok(std::sync::Arc::new(config))
        })
        .clone()
}

/// A tracker's public address, looked up inside the tunnel (one that points to this PC or the local network is not asked
/// through the VPN, and never directly).
pub(crate) async fn public_ip(tunnel: &leaf_vpn::Tunnel, host: &str) -> Result<Ipv4Addr, String> {
    let ips = tunnel.resolve(host).await.map_err(|e| e.to_string())?;
    ips.into_iter().find(|ip| !ip.is_unspecified() && !leaf_vpn::route::is_lan_ip(IpAddr::V4(*ip))).ok_or_else(|| "the tracker's name has no public address".to_string())
}

/// The body of an HTTP answer (`200` only), a chunked one put together.
pub(crate) fn http_body(answer: &[u8]) -> Result<Vec<u8>, String> {
    let split = answer.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the tracker's answer is not HTTP")?;
    let head = String::from_utf8_lossy(&answer[..split]).to_ascii_lowercase();
    let status = head.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("");
    if status != "200" {
        return Err(format!("the tracker answered {}", status.chars().filter(char::is_ascii_digit).take(3).collect::<String>()));
    }
    let body = &answer[split + 4..];
    if !head.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        return Ok(body.to_vec());
    }
    let (mut out, mut rest) = (Vec::new(), body);
    loop {
        let line = rest.windows(2).position(|w| w == b"\r\n").ok_or("the tracker's answer is cut off")?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..line]).split(';').next().unwrap_or("").trim(), 16).map_err(|_| "the tracker's answer is not HTTP")?;
        rest = &rest[line + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size {
            return Err("the tracker's answer is cut off".into());
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

/// The announce passed on to the HTTPS tracker over a connection inside the tunnel; the tracker's answer.
async fn relay_tls(t: &TlsTarget) -> Result<Vec<u8>, String> {
    let tunnel = leaf_vpn::tunnel().ok_or("the VPN is not connected")?;
    let ip = public_ip(&tunnel, &t.host).await?;
    let tcp = tunnel.connect(ip, t.port, Duration::from_secs(15)).await.map_err(|e| e.to_string())?;
    let name = rustls::pki_types::ServerName::try_from(t.host.clone()).map_err(|e| e.to_string())?;
    let mut s = tokio_rustls::TlsConnector::from(tls_config()?).connect(name, tcp).await.map_err(|e| format!("the tracker's certificate or connection was refused ({e})"))?;
    let host = if t.port == 443 { t.host.clone() } else { format!("{}:{}", t.host, t.port) };
    let agent = t.agent.as_deref().map(|a| format!("User-Agent: {a}\r\n")).unwrap_or_default();
    let request = format!("GET {} HTTP/1.1\r\nHost: {host}\r\n{agent}Accept: */*\r\nConnection: close\r\n\r\n", t.path);
    s.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                answer.extend_from_slice(&buf[..n]);
                if answer.len() > 1 << 20 {
                    return Err("the tracker's answer is too large".into());
                }
            }
            // (a server that closes without TLS's goodbye: what came is the answer, checked as HTTP below)
            Err(_) if !answer.is_empty() => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    http_body(&answer)
}

/// A UDP tracker's answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    pub interval: u32,
    /// 6 bytes each: the address and the port.
    pub peers: Vec<u8>,
}

impl Answer {
    /// As an HTTP tracker's answer (compact peers).
    pub(crate) fn body(&self) -> Vec<u8> {
        let mut out = format!("d8:intervali{}e5:peers{}:", self.interval.clamp(60, 3600), self.peers.len()).into_bytes();
        out.extend_from_slice(&self.peers);
        out.push(b'e');
        out
    }
}

fn random() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish() as u32
}

pub(crate) fn connect_packet(txid: u32) -> [u8; 16] {
    let mut p = [0u8; 16];
    p[..8].copy_from_slice(&0x0417_2710_1980u64.to_be_bytes());
    p[12..].copy_from_slice(&txid.to_be_bytes());
    p
}

pub(crate) fn announce_packet(conn: u64, txid: u32, r: &Request) -> Vec<u8> {
    let mut p = Vec::with_capacity(98);
    p.extend_from_slice(&conn.to_be_bytes());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&txid.to_be_bytes());
    p.extend_from_slice(&r.info_hash);
    p.extend_from_slice(&r.peer_id);
    p.extend_from_slice(&r.downloaded.to_be_bytes());
    p.extend_from_slice(&r.left.to_be_bytes());
    p.extend_from_slice(&r.uploaded.to_be_bytes());
    p.extend_from_slice(&r.event.to_be_bytes());
    // (the address: 0, the tracker takes the one the datagram comes from, which is the VPN's)
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&r.key.to_be_bytes());
    p.extend_from_slice(&(-1i32).to_be_bytes());
    p.extend_from_slice(&r.listen_port.to_be_bytes());
    p
}

/// The answer to an announce with `txid`; `None` when the datagram is something else.
pub(crate) fn read_announce(d: &[u8], txid: u32) -> Option<Result<Answer, String>> {
    if d.len() < 8 || d[4..8] != txid.to_be_bytes() {
        return None;
    }
    match u32::from_be_bytes(d[..4].try_into().ok()?) {
        1 if d.len() >= 20 => {
            let peers = &d[20..];
            Some(Ok(Answer { interval: u32::from_be_bytes(d[8..12].try_into().ok()?), peers: peers[..peers.len() / 6 * 6].to_vec() }))
        }
        3 => Some(Err(String::from_utf8_lossy(&d[8..]).chars().filter(|c| !c.is_control()).take(120).collect())),
        _ => None,
    }
}

/// One request and its answer from `to`: sent again twice when nothing comes (UDP loses datagrams).
async fn ask(sock: &leaf_vpn::UdpPort, to: SocketAddrV4, packet: &[u8], mut take: impl FnMut(&[u8]) -> bool) -> Result<(), String> {
    let mut buf = vec![0u8; 8192];
    for attempt in 0..3u64 {
        sock.send_to(packet, to).map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3 + 2 * attempt);
        loop {
            match tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) => {
                    if from == to && take(&buf[..n]) {
                        return Ok(());
                    }
                }
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => break,
            }
        }
    }
    Err("the tracker did not answer".into())
}

async fn announce(r: &Request) -> Result<Answer, String> {
    let tunnel = leaf_vpn::tunnel().ok_or("the VPN is not connected")?;
    let ip = public_ip(&tunnel, &r.host).await?;
    let to = SocketAddrV4::new(ip, r.port);
    let sock = tunnel.udp().map_err(|e| e.to_string())?;
    let txid = random();
    let mut conn = None;
    ask(&sock, to, &connect_packet(txid), |d| {
        if d.len() >= 16 && d[..4] == [0, 0, 0, 0] && d[4..8] == txid.to_be_bytes() {
            conn = d[8..16].try_into().ok().map(u64::from_be_bytes);
        }
        conn.is_some()
    })
    .await?;
    let conn = conn.ok_or("the tracker did not answer")?;
    let txid = random();
    let mut answer = None;
    ask(&sock, to, &announce_packet(conn, txid, r), |d| {
        answer = read_announce(d, txid);
        answer.is_some()
    })
    .await?;
    answer.ok_or("the tracker did not answer")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engines_announce_is_read() {
        let head = b"GET /udp/tracker.example.com/6969?info_hash=%01%02%03%04%05%06%07%08%09%0A%0B%0C%0D%0E%0F%10%11%12%13%14&peer_id=-rQ0001-abcdefghijkl&event=started&port=4240&uploaded=1&downloaded=2&left=3&compact=1&no_peer_id=0&key=77 HTTP/1.1\r\nhost: x\r\n\r\n";
        let r = Request::parse(head).unwrap();
        assert_eq!((r.host.as_str(), r.port), ("tracker.example.com", 6969));
        assert_eq!(r.info_hash, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]);
        assert_eq!(&r.peer_id, b"-rQ0001-abcdefghijkl");
        assert_eq!((r.listen_port, r.uploaded, r.downloaded, r.left, r.event, r.key), (4240, 1, 2, 3, 2, 77));
        // not an announce, a path that names more than a tracker, a hash of the wrong length
        assert!(Request::parse(b"GET / HTTP/1.1\r\n\r\n").is_none());
        assert!(Request::parse(b"GET /udp/a.example.com/80/x?info_hash=1&peer_id=2 HTTP/1.1\r\n\r\n").is_none());
        assert!(Request::parse(b"GET /udp/a.example.com/80?info_hash=abc&peer_id=-rQ0001-abcdefghijkl HTTP/1.1\r\n\r\n").is_none());
        assert!(Request::parse(b"GET /udp/a_b/80?info_hash=abc HTTP/1.1\r\n\r\n").is_none());
        assert!(Request::parse(b"GET /a.example.com/80?info_hash=aaaaaaaaaaaaaaaaaaaa&peer_id=bbbbbbbbbbbbbbbbbbbb HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn the_dht_address_gives_the_hash() {
        assert_eq!(dht_target(b"GET /dht?info_hash=aaaaaaaaaaaaaaaaaaa%62&peer_id=x HTTP/1.1\r\n\r\n"), Some(*b"aaaaaaaaaaaaaaaaaaab"));
        assert_eq!(dht_target(b"GET /dht?info_hash=short HTTP/1.1\r\n\r\n"), None);
        assert_eq!(dht_target(b"GET /udp/dht/80?info_hash=aaaaaaaaaaaaaaaaaaaa HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn an_https_announce_is_passed_on_as_it_is() {
        let t = TlsTarget::parse(b"GET /tls/tracker.example.com/8443/a/announce?info_hash=%01&passkey=k HTTP/1.1\r\nhost: x\r\nUser-Agent: rqbit 9\r\n\r\n").unwrap();
        assert_eq!(t, TlsTarget { host: "tracker.example.com".into(), port: 8443, path: "/a/announce?info_hash=%01&passkey=k".into(), agent: Some("rqbit 9".into()) });
        assert!(TlsTarget::parse(b"GET /udp/tracker.example.com/80?x HTTP/1.1\r\n\r\n").is_none());
        assert!(TlsTarget::parse(b"GET /tls/bad_host/443/a HTTP/1.1\r\n\r\n").is_none());
        assert_eq!(http_body(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nde").unwrap(), b"de");
        assert_eq!(http_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nd\r\n1\r\ne\r\n0\r\n\r\n").unwrap(), b"de");
        assert!(http_body(b"HTTP/1.1 403 Forbidden\r\n\r\nno").is_err());
    }

    #[test]
    fn the_udp_packets_and_the_answer() {
        let head = b"GET /udp/t.example.com/80?info_hash=aaaaaaaaaaaaaaaaaaaa&peer_id=bbbbbbbbbbbbbbbbbbbb&port=1&uploaded=0&downloaded=0&left=9 HTTP/1.1\r\n\r\n";
        let r = Request::parse(head).unwrap();
        let c = connect_packet(7);
        assert_eq!(&c[..8], &[0, 0, 4, 0x17, 0x27, 0x10, 0x19, 0x80]);
        assert_eq!(&c[8..], &[0, 0, 0, 0, 0, 0, 0, 7]);
        let a = announce_packet(5, 9, &r);
        assert_eq!(a.len(), 98);
        assert_eq!(&a[16..36], b"aaaaaaaaaaaaaaaaaaaa");
        assert_eq!(&a[64..72], &9u64.to_be_bytes());
        // an answer: action 1, the id, interval 1800, leechers, seeders, one peer and a cut one
        let mut d = vec![0, 0, 0, 1, 0, 0, 0, 9, 0, 0, 7, 8, 0, 0, 0, 1, 0, 0, 0, 2, 192, 0, 2, 1, 0x1a, 0xe1, 1, 2];
        let got = read_announce(&d, 9).unwrap().unwrap();
        assert_eq!(got, Answer { interval: 1800, peers: vec![192, 0, 2, 1, 0x1a, 0xe1] });
        assert_eq!(got.body(), b"d8:intervali1800e5:peers6:\xc0\x00\x02\x01\x1a\xe1e");
        // another id: not ours
        assert!(read_announce(&d, 8).is_none());
        // an error from the tracker
        d[3] = 3;
        assert!(read_announce(&d, 9).unwrap().is_err());
    }
}
