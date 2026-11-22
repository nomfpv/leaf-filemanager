//! Tokens a session used (PLAN F5, D1, D9, D12), summed from the `usage` of its assistant records. A read of the whole file: call off the
//! UI thread, and keep the answer until the file's modification time changes. Records with the same message id repeat the usage (one per
//! content block): the last of each id counts once. Lines that do not parse are skipped. "Tokens" here are the new ones (input, output
//! and what was written to the cache); cache reads are counted apart because they repeat the whole context on every turn.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    /// New tokens: input, output and cache writes (no cache reads).
    pub fn new_tokens(&self) -> u64 {
        self.input + self.output + self.cache_write
    }
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
    }
}

/// One answer of the model: when, and what it used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turn {
    pub at_ms: i64,
    pub tokens: Tokens,
}

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    id: Option<String>,
    usage: Option<U>,
}

#[derive(Deserialize)]
struct U {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

/// Every answer of the session file, oldest first (one turn per message id).
pub fn read(path: &Path, cancel: &AtomicBool) -> Vec<Turn> {
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Turn> = Vec::new();
    crate::transcript::each_line(path, cancel, |_, line| {
        if !line.contains("\"usage\"") {
            return true;
        }
        let Ok(r) = serde_json::from_str::<Rec>(line) else { return true };
        if r.kind.as_deref() != Some("assistant") {
            return true;
        }
        let (Some(Msg { id, usage: Some(u) }), Some(ts)) = (r.message, r.timestamp) else { return true };
        let Some(at_ms) = parse_ms(&ts) else { return true };
        let turn = Turn {
            at_ms,
            tokens: Tokens {
                input: u.input_tokens.unwrap_or(0),
                output: u.output_tokens.unwrap_or(0),
                cache_write: u.cache_creation_input_tokens.unwrap_or(0),
                cache_read: u.cache_read_input_tokens.unwrap_or(0),
            },
        };
        match id.map(|id| (by_id.get(&id).copied(), id)) {
            Some((Some(ix), _)) => out[ix] = turn,
            Some((None, id)) => {
                by_id.insert(id, out.len());
                out.push(turn);
            }
            None => out.push(turn),
        }
        true
    });
    out
}

/// "2026-11-21T10:11:12.345Z" to milliseconds since the Unix epoch (UTC; any other form: `None`).
pub fn parse_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let ms = s.get(19..).and_then(|r| r.strip_prefix('.')).map(|r| r.chars().take_while(char::is_ascii_digit).take(3).collect::<String>()).filter(|f| !f.is_empty()).map_or(0, |f| f.parse::<i64>().unwrap_or(0) * 10i64.pow(3 - f.len() as u32));
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + se * 1000 + ms)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_timestamp_becomes_milliseconds() {
        assert_eq!(super::parse_ms("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(super::parse_ms("2000-03-01T00:00:00Z"), Some(951_868_800_000));
        assert_eq!(super::parse_ms("nonsense"), None);
    }
}
