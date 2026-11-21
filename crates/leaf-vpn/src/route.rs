//! Which addresses never go through the VPN: the local network (and this PC). A name is local when it is one of the names
//! a LAN uses (`localhost`, a single label, `.local`, `.lan`, `.home.arpa`, `.internal`); an address when it is private,
//! loopback, link-local or in the shared range (100.64.0.0/10: carrier NAT, Tailscale).

use std::net::IpAddr;

pub fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            v6.is_loopback() || v6.is_unspecified() || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xfe00) == 0xfc00
        }
    }
}

/// `host` as written in an address (a name, an IPv4 address, an IPv6 address with or without brackets).
pub fn is_lan_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    if let Ok(ip) = h.parse::<IpAddr>() {
        return is_lan_ip(ip);
    }
    // (the forms the system's resolver turns into an address by itself: `16843009`, `0x01010101`, `1.1`, `0177.0.0.1`)
    if let Some(ip) = inet_aton(&h) {
        return is_lan_ip(IpAddr::V4(ip));
    }
    h.is_empty() || h == "localhost" || !h.contains('.') || [".local", ".lan", ".home.arpa", ".internal", ".localhost"].iter().any(|s| h.ends_with(s))
}

/// An IPv4 address in the old `inet_aton` forms: one to four parts, each decimal, octal (a leading 0) or hex (`0x`); the last
/// part fills the bytes that are left.
pub fn inet_aton(s: &str) -> Option<std::net::Ipv4Addr> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in &parts {
        let v = if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            if h.is_empty() { Some(0) } else { u64::from_str_radix(h, 16).ok() }
        } else if p.len() > 1 && p.starts_with('0') {
            u64::from_str_radix(&p[1..], 8).ok()
        } else if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
            p.parse::<u64>().ok()
        } else {
            None
        }?;
        nums.push(v);
    }
    let (last, head) = nums.split_last()?;
    if head.iter().any(|&n| n > 255) {
        return None;
    }
    let room = 8 * (4 - head.len() as u32);
    if room < 64 && *last >= 1u64 << room {
        return None;
    }
    let mut v: u64 = 0;
    for (i, n) in head.iter().enumerate() {
        v |= n << (24 - 8 * i as u32);
    }
    Some(std::net::Ipv4Addr::from((v | last) as u32))
}
