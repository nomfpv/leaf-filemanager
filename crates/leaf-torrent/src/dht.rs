//! Peers from the DHT on the tunnel route. The engine's own DHT needs UDP sockets of the PC, so it is off there; this is a
//! small one of leaf's that only asks: one lookup of a torrent's hash (BEP 5 `get_peers`) over a UDP socket inside the
//! tunnel, starting from the well-known routers, each answer naming closer nodes until peers come. leaf is never a node
//! others can find: it marks itself read-only (BEP 43), answers nothing and never announces itself. The gateway gives the
//! peers to the engine as a tracker's answer (`http://GATEWAY_HOST/dht`).

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

/// Where a lookup starts.
const ROUTERS: &[(&str, u16)] = &[("router.bittorrent.com", 6881), ("dht.transmissionbt.com", 6881), ("router.utorrent.com", 6881), ("dht.libtorrent.org", 25401)];
/// Nodes asked per round, rounds, and how long a round waits for answers.
const PER_ROUND: usize = 8;
const ROUNDS: usize = 14;
const ROUND_WAIT: Duration = Duration::from_millis(700);
/// Enough peers to stop asking.
const ENOUGH: usize = 200;

/// A bencoded value, borrowed from the datagram.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum B<'a> {
    Int(i64),
    Str(&'a [u8]),
    List(Vec<B<'a>>),
    Dict(Vec<(&'a [u8], B<'a>)>),
}

impl<'a> B<'a> {
    pub(crate) fn get(&self, key: &[u8]) -> Option<&B<'a>> {
        match self {
            B::Dict(d) => d.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub(crate) fn bytes(&self) -> Option<&'a [u8]> {
        match self {
            B::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// One value and what follows it. `depth`: a datagram nested deeper than a DHT message ever is, is not read.
pub(crate) fn decode(d: &[u8], depth: u8) -> Option<(B<'_>, &[u8])> {
    if depth > 8 {
        return None;
    }
    match *d.first()? {
        b'i' => {
            let end = d.iter().position(|b| *b == b'e')?;
            Some((B::Int(std::str::from_utf8(&d[1..end]).ok()?.parse().ok()?), &d[end + 1..]))
        }
        b'l' => {
            let (mut rest, mut items) = (&d[1..], Vec::new());
            while *rest.first()? != b'e' {
                let (v, r) = decode(rest, depth + 1)?;
                items.push(v);
                rest = r;
            }
            Some((B::List(items), &rest[1..]))
        }
        b'd' => {
            let (mut rest, mut items) = (&d[1..], Vec::new());
            while *rest.first()? != b'e' {
                let (B::Str(k), r) = decode(rest, depth + 1)? else { return None };
                let (v, r) = decode(r, depth + 1)?;
                items.push((k, v));
                rest = r;
            }
            Some((B::Dict(items), &rest[1..]))
        }
        b'0'..=b'9' => {
            let colon = d.iter().position(|b| *b == b':')?;
            let len: usize = std::str::from_utf8(&d[..colon]).ok()?.parse().ok()?;
            let end = colon.checked_add(1)?.checked_add(len)?;
            Some((B::Str(d.get(colon + 1..end)?), &d[end..]))
        }
        _ => None,
    }
}

pub(crate) fn query(me: &[u8; 20], hash: &[u8; 20], tx: [u8; 2]) -> Vec<u8> {
    let mut q = Vec::with_capacity(100);
    q.extend_from_slice(b"d1:ad2:id20:");
    q.extend_from_slice(me);
    q.extend_from_slice(b"9:info_hash20:");
    q.extend_from_slice(hash);
    q.extend_from_slice(b"e1:q9:get_peers2:roi1e1:t2:");
    q.extend_from_slice(&tx);
    q.extend_from_slice(b"1:y1:qe");
    q
}

/// What an answer to `tx` holds: peers (6 bytes each) and closer nodes (their id and address).
pub(crate) fn read_answer(d: &[u8], tx: [u8; 2]) -> Option<(Vec<[u8; 6]>, Vec<([u8; 20], SocketAddrV4)>)> {
    let (msg, _) = decode(d, 0)?;
    if msg.get(b"t")?.bytes()? != tx || msg.get(b"y")?.bytes()? != b"r" {
        return None;
    }
    let r = msg.get(b"r")?;
    let mut peers = Vec::new();
    if let Some(B::List(values)) = r.get(b"values") {
        peers.extend(values.iter().filter_map(|v| v.bytes().and_then(|b| <[u8; 6]>::try_from(b).ok())));
    }
    let mut nodes = Vec::new();
    for n in r.get(b"nodes").and_then(B::bytes).unwrap_or(&[]).chunks_exact(26) {
        let id: [u8; 20] = n[..20].try_into().ok()?;
        let addr = SocketAddrV4::new(Ipv4Addr::new(n[20], n[21], n[22], n[23]), u16::from_be_bytes([n[24], n[25]]));
        nodes.push((id, addr));
    }
    Some((peers, nodes))
}

fn public(addr: &SocketAddrV4) -> bool {
    addr.port() != 0 && !addr.ip().is_unspecified() && !addr.ip().is_broadcast() && !addr.ip().is_multicast() && !leaf_vpn::route::is_lan_ip(std::net::IpAddr::V4(*addr.ip()))
}

fn random20() -> [u8; 20] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 20];
    for c in out.chunks_mut(8) {
        let v = std::collections::hash_map::RandomState::new().build_hasher().finish().to_le_bytes();
        c.copy_from_slice(&v[..c.len()]);
    }
    out
}

fn distance(a: &[u8; 20], b: &[u8; 20]) -> [u8; 20] {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// The peers the DHT knows for `hash` (6 bytes each: address and port), asked through the tunnel.
pub(crate) async fn lookup(hash: [u8; 20]) -> Result<Vec<u8>, String> {
    let tunnel = leaf_vpn::tunnel().ok_or("the VPN is not connected")?;
    let sock = tunnel.udp().map_err(|e| e.to_string())?;
    // (a new id for every lookup: nothing ties two lookups together)
    let me = random20();
    let tx = [me[0], me[1]];
    let packet = query(&me, &hash, tx);
    // the nodes known, with their distance to the hash (the routers: as far as can be)
    let mut known: Vec<([u8; 20], SocketAddrV4)> = Vec::new();
    for (host, port) in ROUTERS {
        if let Ok(ip) = crate::gateway::public_ip(&tunnel, host).await {
            known.push(([0xff; 20], SocketAddrV4::new(ip, *port)));
        }
    }
    if known.is_empty() {
        return Err("the DHT's routers could not be found".into());
    }
    let mut asked: HashSet<SocketAddrV4> = HashSet::new();
    let mut peers: Vec<[u8; 6]> = Vec::new();
    let mut seen: HashSet<[u8; 6]> = HashSet::new();
    let mut buf = vec![0u8; 4096];
    for round in 0..ROUNDS {
        known.sort_by(|a, b| a.0.cmp(&b.0));
        let next: Vec<SocketAddrV4> = known.iter().map(|k| k.1).filter(|a| !asked.contains(a)).take(PER_ROUND).collect();
        if next.is_empty() {
            break;
        }
        for a in next {
            asked.insert(a);
            sock.send_to(&packet, a).map_err(|e| e.to_string())?;
        }
        // (the routers are slower to answer than the nodes they name)
        let until = tokio::time::Instant::now() + if round == 0 { ROUND_WAIT * 3 } else { ROUND_WAIT };
        while let Ok(got) = tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
            let (n, from) = got.map_err(|e| e.to_string())?;
            if !asked.contains(&from) {
                continue;
            }
            let Some((values, nodes)) = read_answer(&buf[..n], tx) else { continue };
            for v in values {
                if seen.insert(v) {
                    peers.push(v);
                }
            }
            for (id, addr) in nodes {
                if public(&addr) && known.len() < 600 && !known.iter().any(|k| k.1 == addr) {
                    known.push((distance(&id, &hash), addr));
                }
            }
        }
        if peers.len() >= ENOUGH {
            break;
        }
    }
    Ok(peers.into_iter().filter(|p| public(&SocketAddrV4::new(Ipv4Addr::new(p[0], p[1], p[2], p[3]), u16::from_be_bytes([p[4], p[5]])))).flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dht_question_and_its_answer() {
        let q = query(&[b'a'; 20], &[b'h'; 20], *b"tx");
        let (msg, rest) = decode(&q, 0).unwrap();
        assert!(rest.is_empty());
        assert_eq!(msg.get(b"q").unwrap().bytes(), Some(&b"get_peers"[..]));
        assert_eq!(msg.get(b"ro"), Some(&B::Int(1)));
        assert_eq!(msg.get(b"a").unwrap().get(b"info_hash").unwrap().bytes(), Some(&[b'h'; 20][..]));

        let mut a = b"d1:rd2:id20:nnnnnnnnnnnnnnnnnnnn5:nodes26:".to_vec();
        a.extend_from_slice(&[b'i'; 20]);
        a.extend_from_slice(&[192, 0, 2, 7, 0x1a, 0xe1]);
        a.extend_from_slice(b"6:valuesl6:\xc0\x00\x02\x01\x1a\xe13:badee1:t2:tx1:y1:re");
        let (peers, nodes) = read_answer(&a, *b"tx").unwrap();
        assert_eq!(peers, vec![[192, 0, 2, 1, 0x1a, 0xe1]]);
        assert_eq!(nodes, vec![([b'i'; 20], SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 7), 6881))]);
        // another question's answer, an error, rubbish
        assert!(read_answer(&a, *b"xx").is_none());
        assert!(read_answer(b"d1:eli201e1:xe1:t2:tx1:y1:ee", *b"tx").is_none());
        assert!(read_answer(b"d1:r", *b"tx").is_none());
        assert!(decode(b"99999999999999999999:x", 0).is_none());
    }
}
