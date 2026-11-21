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
    h.is_empty() || h == "localhost" || !h.contains('.') || [".local", ".lan", ".home.arpa", ".internal", ".localhost"].iter().any(|s| h.ends_with(s))
}
