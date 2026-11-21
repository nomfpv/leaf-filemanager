//! How two leafs reach each other, behind two small traits so another way (one through relays, for people without a VPN between
//! their PCs) can be added later without touching the share protocol. The one there is now: QUIC on rf-nearby's identity (TLS 1.3,
//! both sides prove their certificate), to an address the user typed or the invite carried (a WireGuard address, a LAN address).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use rf_nearby::kit::{self, Identity};
use tokio::io::{AsyncRead, AsyncWrite};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// The ALPN of the share protocol: a Nearby endpoint (or anything else) is turned away in the handshake.
pub const ALPN: &[u8] = b"leaf-share/1";
const CONNECT: Duration = Duration::from_secs(10);

/// One connection with the other PC.
pub trait Link: Send + Sync {
    /// The SHA-256 of the certificate the other side proved.
    fn peer_fingerprint(&self) -> String;
    /// The six-digit code drawn from this connection's TLS session (the same on both sides, never on a connection through a
    /// device in the middle).
    fn pairing_code(&self) -> Option<String>;
    /// Where the other side is, `host:port`.
    fn remote_address(&self) -> String;
    /// A new two-way stream.
    fn open(&self) -> BoxFut<'_, anyhow::Result<(SendHalf, RecvHalf)>>;
    /// The next stream the other side opened.
    fn accept(&self) -> BoxFut<'_, anyhow::Result<(SendHalf, RecvHalf)>>;
    /// Ends the connection without a word to the other side's protocol.
    fn close(&self);
    fn is_closed(&self) -> bool;
    /// Resolves when the connection is gone.
    fn closed(&self) -> BoxFut<'_, ()>;
}

/// Where connections come from and go to.
pub trait Transport: Send + Sync {
    fn connect<'a>(&'a self, address: &'a str) -> BoxFut<'a, anyhow::Result<Arc<dyn Link>>>;
    /// The next incoming connection (its handshake done); `None` once the transport is closed.
    fn accept(&self) -> BoxFut<'_, Option<anyhow::Result<Arc<dyn Link>>>>;
    fn local_port(&self) -> u16;
    fn close(&self);
}

// ------------------------------------------------------------------------------------------------------------------------- QUIC

pub struct Quic {
    endpoint: quinn::Endpoint,
    client: quinn::ClientConfig,
}

impl Quic {
    /// Listens on `port` on every interface (or on 127.0.0.1 only, for tests). Must be called inside the runtime.
    pub fn bind(me: &Identity, port: u16, local_only: bool) -> anyhow::Result<Quic> {
        let server = kit::server_config_for(me, ALPN)?;
        let client = kit::client_config_for(me, ALPN)?;
        let at = SocketAddr::from((if local_only { [127, 0, 0, 1] } else { [0, 0, 0, 0] }, port));
        let endpoint = quinn::Endpoint::server(server, at).with_context(|| format!("port {port} could not be used"))?;
        Ok(Quic { endpoint, client })
    }
}

struct QuicLink(quinn::Connection);

impl Link for QuicLink {
    fn peer_fingerprint(&self) -> String {
        kit::peer_fingerprint(&self.0).unwrap_or_default()
    }
    fn pairing_code(&self) -> Option<String> {
        kit::pairing_code(&self.0)
    }
    fn remote_address(&self) -> String {
        self.0.remote_address().to_string()
    }
    fn open(&self) -> BoxFut<'_, anyhow::Result<(SendHalf, RecvHalf)>> {
        Box::pin(async move {
            let (s, r) = self.0.open_bi().await?;
            Ok((Box::new(s) as SendHalf, Box::new(r) as RecvHalf))
        })
    }
    fn accept(&self) -> BoxFut<'_, anyhow::Result<(SendHalf, RecvHalf)>> {
        Box::pin(async move {
            let (s, r) = self.0.accept_bi().await?;
            Ok((Box::new(s) as SendHalf, Box::new(r) as RecvHalf))
        })
    }
    fn close(&self) {
        self.0.close(0u32.into(), b"");
    }
    fn is_closed(&self) -> bool {
        self.0.close_reason().is_some()
    }
    fn closed(&self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            self.0.closed().await;
        })
    }
}

impl Transport for Quic {
    fn connect<'a>(&'a self, address: &'a str) -> BoxFut<'a, anyhow::Result<Arc<dyn Link>>> {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::time::timeout(CONNECT, tokio::net::lookup_host(address))
                .await
                .map_err(|_| anyhow!("the name {address} could not be looked up in time"))?
                .with_context(|| format!("{address} is not an address"))?
                .collect();
            let mut last = anyhow!("{address} has no address");
            // IPv4 first (the endpoint listens on IPv4)
            let mut addrs = addrs;
            addrs.sort_by_key(|a| a.is_ipv6());
            for a in addrs {
                let attempt = async { anyhow::Ok(self.endpoint.connect_with(self.client.clone(), a, "leaf")?.await?) };
                match tokio::time::timeout(CONNECT, attempt).await {
                    Ok(Ok(c)) => return Ok(Arc::new(QuicLink(c)) as Arc<dyn Link>),
                    Ok(Err(e)) => last = e,
                    Err(_) => last = anyhow!("{a} did not answer"),
                }
            }
            Err(last)
        })
    }

    fn accept(&self) -> BoxFut<'_, Option<anyhow::Result<Arc<dyn Link>>>> {
        Box::pin(async move {
            let incoming = self.endpoint.accept().await?;
            Some(match tokio::time::timeout(CONNECT, incoming).await {
                Ok(Ok(c)) => Ok(Arc::new(QuicLink(c)) as Arc<dyn Link>),
                Ok(Err(e)) => Err(e.into()),
                Err(_) => Err(anyhow!("a handshake did not finish")),
            })
        })
    }

    fn local_port(&self) -> u16 {
        self.endpoint.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    fn close(&self) {
        self.endpoint.close(0u32.into(), b"");
    }
}

/// The addresses of this PC a friend may reach it at, the likely one first: a WireGuard interface, then private LAN addresses,
/// then the rest (never loopback). For the invite's prefilled address.
pub fn own_addresses() -> Vec<String> {
    let mut v: Vec<(u8, String)> = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if i.is_loopback() {
            continue;
        }
        let if_addrs::IfAddr::V4(a) = &i.addr else { continue };
        let name = i.name.to_lowercase();
        let rank = if name.contains("wg") || name.contains("wireguard") || name.contains("tailscale") {
            0
        } else if a.ip.is_private() {
            1
        } else if a.ip.is_link_local() {
            3
        } else {
            2
        };
        v.push((rank, a.ip.to_string()));
    }
    v.sort();
    v.into_iter().map(|(_, a)| a).collect()
}
