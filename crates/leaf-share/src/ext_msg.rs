//! The side channel an extension of the app can use between two linked leafs (see `docs/extensions.md`, "Peer channel").
//!
//! Wire format: one more control-stream notice, `{"t":"ext","kind":<u16>,"hex":"<lower-case hex of the payload>"}`, in the same
//! length-prefixed JSON frames as every other notice. Nothing else is added to the protocol: no stream, no file, no setting.
//!
//! Limits, all enforced on the receiving side before anything is acted on:
//! - a control frame is at most [`MAX_NOTICE_FRAME`] bytes (read before its buffer is allocated, so a large frame is refused unread
//!   and the connection ends);
//! - the payload is at most [`MAX_PAYLOAD`] bytes: a longer or odd-length or non-hex text is dropped without being decoded;
//! - at most [`BURST`] messages at once and [`PER_SEC`] a second on average, per link; the rest are dropped;
//! - dropped unless the user allowed extension messages globally and for this share (both off by default).
//!
//! A message only reaches the app's extension code as bytes; it never touches files or settings by itself.

use std::time::Instant;

use crate::wire::Notice;

/// The largest payload of one message.
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// The largest control frame: the payload as hex plus the JSON around it.
pub const MAX_NOTICE_FRAME: usize = 2 * MAX_PAYLOAD + 256;
/// Messages allowed at once.
pub const BURST: u32 = 5;
/// Messages allowed per second, on average.
pub const PER_SEC: f64 = 3.0;

/// The notice for a message, or why it cannot be sent.
pub fn encode(kind: u16, payload: &[u8]) -> Result<Notice, String> {
    if payload.len() > MAX_PAYLOAD {
        return Err(format!("A message is at most {} bytes", MAX_PAYLOAD));
    }
    let mut hex = String::with_capacity(payload.len() * 2);
    for b in payload {
        hex.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(b & 15), 16).unwrap_or('0'));
    }
    Ok(Notice::Ext { kind, hex })
}

/// The payload of a received message; `None` (drop it) when it is too long, of odd length or not hex. The length is looked at
/// before the bytes are allocated.
pub fn decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() > 2 * MAX_PAYLOAD || hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// A token bucket: [`BURST`] at once, [`PER_SEC`] refilled each second.
pub struct Limiter {
    tokens: f64,
    at: Instant,
}

impl Default for Limiter {
    fn default() -> Self {
        Self { tokens: f64::from(BURST), at: Instant::now() }
    }
}

impl Limiter {
    /// Whether one more message may pass at `now`.
    pub fn allow(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + dt * PER_SEC).min(f64::from(BURST));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn limits_hold_and_malformed_frames_are_dropped() {
        // a round trip, and the largest payload
        let Notice::Ext { kind, hex } = encode(7, &[0, 1, 254, 255]).unwrap() else { panic!("not an ext notice") };
        assert_eq!((kind, decode(&hex).unwrap()), (7, vec![0, 1, 254, 255]));
        let big = vec![9u8; MAX_PAYLOAD];
        let Notice::Ext { hex, .. } = encode(1, &big).unwrap() else { panic!() };
        assert_eq!(decode(&hex).unwrap().len(), MAX_PAYLOAD);
        // one byte more is refused on both sides, before anything is allocated for it
        assert!(encode(1, &vec![0u8; MAX_PAYLOAD + 1]).is_err());
        assert!(decode(&"00".repeat(MAX_PAYLOAD + 1)).is_none());
        // odd length and non-hex are dropped
        assert!(decode("abc").is_none());
        assert!(decode("zz").is_none());
        // the rate: the burst passes, the next is dropped, and a second later three more pass
        let mut l = Limiter::default();
        let t0 = l.at;
        assert_eq!((0..BURST).filter(|_| l.allow(t0)).count(), BURST as usize);
        assert!(!l.allow(t0));
        let later = t0 + Duration::from_secs(1);
        assert_eq!((0..10).filter(|_| l.allow(later)).count(), 3);
        // a frame bigger than the control-frame limit is refused by the reader
        let big_frame = (MAX_NOTICE_FRAME as u32 + 1).to_be_bytes();
        let mut r: &[u8] = &big_frame;
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let got: anyhow::Result<Notice> = rt.block_on(crate::wire::read_frame_max(&mut r, MAX_NOTICE_FRAME));
        assert!(got.is_err());
    }

    /// A notice of a kind this leaf does not know is skipped and the next one is still read; the connection is not dropped.
    #[test]
    fn unknown_notice_is_skipped() {
        let mut bytes = Vec::new();
        for body in [r#"{"t":"from-the-future","x":1}"#, r#"{"t":"ext","kind":3}"#, r#"{"t":"changed"}"#] {
            bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
            bytes.extend_from_slice(body.as_bytes());
        }
        let mut r: &[u8] = &bytes;
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let got: Vec<Notice> = (0..3).map(|_| rt.block_on(crate::wire::read_notice(&mut r, MAX_NOTICE_FRAME)).unwrap()).collect();
        assert_eq!(got, vec![Notice::Unknown, Notice::Unknown, Notice::Changed]);
    }
}
