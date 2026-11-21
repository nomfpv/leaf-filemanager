//! One HTTPS request over a stream that is already open (a direct TCP connection, or one through the tunnel): HTTP/1.0 with
//! `Connection: close`, so the answer is the rest of the stream (no chunks, no keep-alive). Certificates are checked against
//! the system's store (rustls with ring, as leaf-remote's FTPS has it). Small JSON answers only.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest answer read (the relay list is about 0.7 MB).
const MAX_BODY: usize = 16 << 20;

pub(crate) struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
}

fn tls() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CFG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| format!("the system's certificates cannot be read ({e})"))?;
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("TLS could not be set up ({e})"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Ok(Arc::new(cfg))
    })
    .clone()
}

/// `method path` on `host` over `stream`. `bearer` is sent as `Authorization` (never logged); `body` as JSON.
pub(crate) async fn request<S>(stream: S, host: &str, method: &str, path: &str, bearer: Option<&str>, body: Option<&[u8]>, timeout: Duration) -> Result<Answer, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let work = async {
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| "a bad server name".to_string())?;
        let mut tls = tokio_rustls::TlsConnector::from(tls()?).connect(name, stream).await.map_err(|e| format!("the secure connection failed ({e})"))?;
        let mut head = format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: leaf\r\nAccept: application/json\r\nConnection: close\r\n");
        if let Some(t) = bearer {
            head.push_str("Authorization: Bearer ");
            head.push_str(t);
            head.push_str("\r\n");
        }
        let body = body.unwrap_or_default();
        if !body.is_empty() || matches!(method, "POST" | "PUT") {
            head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut req = zeroize::Zeroizing::new(head.into_bytes());
        req.extend_from_slice(body);
        tls.write_all(&req).await.map_err(|e| format!("the request could not be sent ({e})"))?;
        tls.flush().await.map_err(|e| format!("the request could not be sent ({e})"))?;
        let mut raw = Vec::new();
        let mut chunk = vec![0u8; 64 << 10];
        loop {
            match tls.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&chunk[..n]);
                    if raw.len() > MAX_BODY {
                        return Err("the answer is too large".to_string());
                    }
                }
                // (servers that close without TLS's close_notify: what came is the answer)
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => break,
                Err(e) => return Err(format!("the answer broke off ({e})")),
            }
        }
        parse(&raw)
    };
    tokio::time::timeout(timeout, work).await.map_err(|_| "no answer in time".to_string())?
}

fn parse(raw: &[u8]) -> Result<Answer, String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the answer has no header")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or("the answer has no status")?;
    let mut body = raw[split + 4..].to_vec();
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    if chunked {
        body = unchunk(&body).ok_or("the answer's chunks are broken")?;
    }
    Ok(Answer { status, body })
}

fn unchunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = b.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&b[..end]).ok()?.split(';').next()?.trim(), 16).ok()?;
        b = &b[end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(b.get(..size)?);
        b = b.get(size + 2..)?;
    }
}
