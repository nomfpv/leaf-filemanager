//! A streaming MPEG transport stream reader: 188-byte packets, or the 192-byte packets of `.m2ts` / `.mts` with their 4-byte
//! arrival time in front. PAT and PMT give the streams; the PES packets of the wanted PIDs come out whole, with their PTS / DTS
//! (33-bit, 90 kHz, as written) and the stream id extension (Blu-ray puts an audio core and its extension in PES packets of their
//! own). Nothing is buffered beyond one PES per PID, so a 40 GB clip goes through in constant memory.
//!
//! [`Clock`] turns the raw time stamps of one stream into a timeline that never jumps: the 33-bit counter is unwrapped and a
//! discontinuity (a stream spliced from two recordings, a broken capture) is closed up.

use std::collections::HashMap;

pub const TS: usize = 188;

/// The packet size a file's first bytes show: 188, 192 (m2ts) or `None` (not a transport stream).
pub fn packet_size(head: &[u8]) -> Option<usize> {
    let synced = |start: usize, step: usize| (0..4).all(|k| head.get(start + k * step) == Some(&0x47));
    if synced(4, 192) {
        Some(192)
    } else if synced(0, 188) {
        Some(188)
    } else {
        None
    }
}

/// An encrypted m2ts (AACS): the 6144-byte aligned units are scrambled past their first 16 bytes, so the second packet's sync byte
/// is gone, and the copy permission bits of the first packet's header are set. Either one is enough to refuse.
pub fn looks_encrypted(head: &[u8]) -> bool {
    if head.len() < 192 * 8 {
        return false;
    }
    if head[0] >> 6 != 0 && head.get(4) == Some(&0x47) {
        return true;
    }
    packet_size(head).is_none()
}

/// A whole PES packet of one stream.
#[derive(Clone, Debug)]
pub struct Pes {
    pub pid: u16,
    pub pts: Option<u64>,
    pub dts: Option<u64>,
    /// The stream id (0xE0 video, 0xFD extended, ...) and, for 0xFD, the extension (0x71 core, 0x72 / 0x76 extension).
    pub stream_id: u8,
    pub stream_id_ext: u8,
    /// The adaptation field said the time base starts again here.
    pub discontinuity: bool,
    pub data: Vec<u8>,
}

/// A stream of the PMT: (PID, stream type, language from an ISO 639 descriptor).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PmtStream {
    pub pid: u16,
    pub stream_type: u8,
    pub lang: String,
}

struct Building {
    data: Vec<u8>,
    discontinuity: bool,
}

pub struct Demux {
    pmt_pids: Vec<u16>,
    streams: Vec<PmtStream>,
    /// `None`: every PID of the PMT.
    wanted: Option<Vec<u16>>,
    building: HashMap<u16, Building>,
    psi: HashMap<u16, Vec<u8>>,
}

impl Demux {
    pub fn new(wanted: Option<Vec<u16>>) -> Self {
        Self { pmt_pids: Vec::new(), streams: Vec::new(), wanted, building: HashMap::new(), psi: HashMap::new() }
    }

    /// The streams the PMT lists (empty until it has been seen).
    pub fn streams(&self) -> &[PmtStream] {
        &self.streams
    }

    fn wants(&self, pid: u16) -> bool {
        match &self.wanted {
            Some(w) => w.contains(&pid),
            None => self.streams.iter().any(|s| s.pid == pid),
        }
    }

    /// One 188-byte packet (a 192-byte one's last 188 bytes); finished PES packets are pushed to `out`.
    pub fn feed(&mut self, p: &[u8], out: &mut Vec<Pes>) {
        if p.len() < TS || p[0] != 0x47 {
            return;
        }
        let start = p[1] & 0x40 != 0;
        let pid = ((p[1] as u16 & 0x1F) << 8) | p[2] as u16;
        let afc = (p[3] >> 4) & 3;
        let mut off = 4;
        let mut disc = false;
        if afc & 2 != 0 {
            let len = p[4] as usize;
            if len > 0 && 5 < TS {
                disc = p[5] & 0x80 != 0;
            }
            off = 5 + len;
        }
        if afc & 1 == 0 || off >= TS {
            return;
        }
        let payload = &p[off..TS];
        if pid == 0 {
            self.section(pid, start, payload, |me, s| me.pat(s));
            return;
        }
        if self.pmt_pids.contains(&pid) {
            self.section(pid, start, payload, |me, s| me.pmt(s));
            return;
        }
        if !self.wants(pid) {
            return;
        }
        if start {
            if let Some(b) = self.building.remove(&pid) {
                if let Some(pes) = parse_pes(pid, b) {
                    out.push(pes);
                }
            }
            self.building.insert(pid, Building { data: payload.to_vec(), discontinuity: disc });
        } else if let Some(b) = self.building.get_mut(&pid) {
            b.data.extend_from_slice(payload);
            b.discontinuity |= disc;
        }
    }

    /// The PES packets still being put together (at the end of a clip).
    pub fn flush(&mut self, out: &mut Vec<Pes>) {
        let mut pids: Vec<u16> = self.building.keys().copied().collect();
        pids.sort_unstable();
        for pid in pids {
            if let Some(b) = self.building.remove(&pid)
                && let Some(pes) = parse_pes(pid, b)
            {
                out.push(pes);
            }
        }
    }

    fn section(&mut self, pid: u16, start: bool, payload: &[u8], then: impl FnOnce(&mut Self, &[u8])) {
        let buf = self.psi.entry(pid).or_default();
        if start {
            let ptr = payload[0] as usize;
            buf.clear();
            if let Some(s) = payload.get(1 + ptr..) {
                buf.extend_from_slice(s);
            }
        } else if !buf.is_empty() {
            buf.extend_from_slice(payload);
        }
        if buf.len() < 3 {
            return;
        }
        let len = (((buf[1] as usize) & 0x0F) << 8 | buf[2] as usize) + 3;
        if buf.len() >= len {
            let s = buf[..len].to_vec();
            buf.clear();
            then(self, &s);
        }
    }

    fn pat(&mut self, s: &[u8]) {
        if s.len() < 12 || s[0] != 0 {
            return;
        }
        let end = s.len() - 4;
        let mut i = 8;
        while i + 4 <= end {
            let program = u16::from_be_bytes([s[i], s[i + 1]]);
            let pid = ((s[i + 2] as u16 & 0x1F) << 8) | s[i + 3] as u16;
            if program != 0 && !self.pmt_pids.contains(&pid) {
                self.pmt_pids.push(pid);
            }
            i += 4;
        }
    }

    fn pmt(&mut self, s: &[u8]) {
        if s.len() < 16 || s[0] != 2 {
            return;
        }
        let end = s.len() - 4;
        let info_len = ((s[10] as usize & 0x0F) << 8) | s[11] as usize;
        let mut i = 12 + info_len;
        let mut found = Vec::new();
        while i + 5 <= end {
            let stream_type = s[i];
            let pid = ((s[i + 1] as u16 & 0x1F) << 8) | s[i + 2] as u16;
            let es_len = ((s[i + 3] as usize & 0x0F) << 8) | s[i + 4] as usize;
            let desc = s.get(i + 5..(i + 5 + es_len).min(end)).unwrap_or(&[]);
            let mut lang = String::new();
            let mut d = 0;
            while d + 2 <= desc.len() {
                let (tag, len) = (desc[d], desc[d + 1] as usize);
                if tag == 0x0A && len >= 3 && d + 5 <= desc.len() {
                    lang = desc[d + 2..d + 5].iter().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase() as char).collect();
                }
                d += 2 + len;
            }
            found.push(PmtStream { pid, stream_type, lang });
            i += 5 + es_len;
        }
        if !found.is_empty() {
            self.streams = found;
        }
    }
}

fn ts33(b: &[u8]) -> u64 {
    ((b[0] as u64 >> 1) & 7) << 30 | (b[1] as u64) << 22 | ((b[2] as u64) >> 1) << 15 | (b[3] as u64) << 7 | (b[4] as u64) >> 1
}

fn parse_pes(pid: u16, b: Building) -> Option<Pes> {
    let d = &b.data;
    if d.len() < 9 || d[..3] != [0, 0, 1] {
        return None;
    }
    let stream_id = d[3];
    // streams without the optional header (padding, private 2, ...)
    if matches!(stream_id, 0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF) {
        return None;
    }
    let pes_len = u16::from_be_bytes([d[4], d[5]]) as usize;
    let flags = d[7];
    let hdr_len = d[8] as usize;
    let body = 9 + hdr_len;
    if d.len() < body {
        return None;
    }
    let mut pts = None;
    let mut dts = None;
    if flags & 0x80 != 0 && hdr_len >= 5 {
        pts = Some(ts33(&d[9..14]));
        if flags & 0x40 != 0 && hdr_len >= 10 {
            dts = Some(ts33(&d[14..19]));
        }
    }
    let mut ext = 0;
    if stream_id == 0xFD && flags & 0x01 != 0 {
        // the stream id extension sits at the end of the PES extension; read it by walking the optional fields
        let mut i = 9;
        if flags & 0x80 != 0 {
            i += 5;
        }
        if flags & 0x40 != 0 {
            i += 5;
        }
        if flags & 0x20 != 0 {
            i += 6;
        }
        if flags & 0x10 != 0 {
            i += 3;
        }
        if flags & 0x04 != 0 {
            i += 1;
        }
        if flags & 0x02 != 0 {
            i += 2;
        }
        if let Some(&ef) = d.get(i) {
            let mut j = i + 1;
            if ef & 0x80 != 0 {
                j += 16;
            }
            if ef & 0x40 != 0 {
                j += 1 + d.get(j).copied().unwrap_or(0) as usize;
            }
            if ef & 0x20 != 0 {
                j += 2;
            }
            if ef & 0x10 != 0 {
                j += 2;
            }
            if ef & 0x01 != 0 && d.get(j).is_some_and(|l| l & 0x7F >= 1) {
                ext = d.get(j + 1).copied().unwrap_or(0) & 0x7F;
            }
        }
    }
    let end = if pes_len > 0 { (6 + pes_len).min(d.len()) } else { d.len() };
    if end < body {
        return None;
    }
    Some(Pes { pid, pts, dts, stream_id, stream_id_ext: ext, discontinuity: b.discontinuity, data: d[body..end].to_vec() })
}

const WRAP: u64 = 1 << 33;
/// More than this backwards, or [`FORWARD_GAP`] forwards, between two stamps of one stream is a break in the time base, not B-frame
/// reordering or a pause in the subtitles.
const BACKWARD_GAP: i64 = 90_000;
const FORWARD_GAP: i64 = 90_000 * 30;

/// One stream's timeline inside one clip.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    last_raw: Option<u64>,
    /// Added to the raw value (wraps and closed-up breaks).
    shift: i64,
    last_out: i64,
    /// The usual step between two stamps (a frame), used to close a break.
    step: i64,
    /// Stamps far ahead are breaks too (not for subtitles, which may pause for minutes).
    sparse: bool,
}

impl Clock {
    pub fn new(sparse: bool) -> Self {
        Self { sparse, ..Self::default() }
    }

    /// The raw 33-bit stamp on a continuous timeline (90 kHz; the first stamp keeps its value). `restart`: the stream said its time
    /// base starts again (discontinuity indicator).
    pub fn map(&mut self, raw: u64, restart: bool) -> i64 {
        let raw = raw & (WRAP - 1);
        let Some(last) = self.last_raw else {
            self.last_raw = Some(raw);
            self.last_out = raw as i64;
            return raw as i64;
        };
        let mut d = raw as i64 - last as i64;
        // the 33-bit counter went round
        if d < -(WRAP as i64) / 2 {
            self.shift += WRAP as i64;
            d += WRAP as i64;
        } else if d > (WRAP as i64) / 2 {
            self.shift -= WRAP as i64;
            d -= WRAP as i64;
        }
        let candidate = raw as i64 + self.shift;
        if restart || d < -BACKWARD_GAP || (!self.sparse && d > FORWARD_GAP) {
            // a break: carry on one step after the last stamp
            let target = self.last_out + self.step.max(1);
            self.shift += target - candidate;
        } else if d > 0 && d < 90_000 {
            self.step = d;
        }
        self.last_raw = Some(raw);
        let out = raw as i64 + self.shift;
        if out > self.last_out {
            self.last_out = out;
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn stamp(marker: u8, t: u64) -> [u8; 5] {
        [
            marker << 4 | ((t >> 29) as u8 & 0x0E) | 1,
            (t >> 22) as u8,
            ((t >> 14) as u8 & 0xFE) | 1,
            (t >> 7) as u8,
            ((t << 1) as u8) | 1,
        ]
    }

    /// m2ts packets: a PAT, a PMT with one video stream (PID 0x1011), then one small PES per picture at the given PTS (a DTS one
    /// frame earlier), `disc` marking the packets whose adaptation field says the time base restarts.
    pub(crate) fn stream(pts: &[(u64, bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut push = |pkt: [u8; 188]| {
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&pkt);
        };
        let psi = |pid: u16, body: &[u8]| {
            let mut p = [0xFFu8; 188];
            p[0] = 0x47;
            p[1] = 0x40 | (pid >> 8) as u8;
            p[2] = pid as u8;
            p[3] = 0x10;
            p[4] = 0;
            p[5..5 + body.len()].copy_from_slice(body);
            p
        };
        push(psi(0, &[0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00, 0, 0, 0, 0]));
        push(psi(0x100, &[0x02, 0xB0, 18, 0, 1, 0xC1, 0, 0, 0xF0, 0x11, 0xF0, 0, 0x1B, 0xF0, 0x11, 0xF0, 0, 0, 0, 0, 0]));
        for &(t, disc) in pts {
            let mut p = [0xFFu8; 188];
            p[0] = 0x47;
            p[1] = 0x40 | 0x10;
            p[2] = 0x11;
            p[3] = 0x30;
            p[4] = 1;
            p[5] = if disc { 0x80 } else { 0 };
            let mut pes = vec![0, 0, 1, 0xE0, 0, 19, 0x80, 0xC0, 10];
            pes.extend_from_slice(&stamp(3, t));
            pes.extend_from_slice(&stamp(1, t.wrapping_sub(3754) & (WRAP - 1)));
            pes.extend_from_slice(&[0, 0, 0, 1, 0x09, 0xF0]);
            p[6..6 + pes.len()].copy_from_slice(&pes);
            push(p);
        }
        out
    }

    #[test]
    fn a_ts_packet_stream_with_a_discontinuity_keeps_monotonic_timestamps() {
        // 10 frames, then the stamps start again near zero (a splice), then the 33-bit counter wraps
        let step = 3754u64;
        let mut stamps: Vec<(u64, bool)> = (0..10).map(|k| (900_000 + k * step, false)).collect();
        stamps.push((1_000, true));
        stamps.extend((1..10).map(|k| (1_000 + k * step, false)));
        // a break without the indicator: 40 s back
        stamps.extend((0..5).map(|k| ((1_000 + 9 * step as i64 - 3_600_000 + k * step as i64).rem_euclid(WRAP as i64) as u64, false)));
        stamps.extend((0..5).map(|k| ((WRAP - 2 * step + k * step) & (WRAP - 1), false)));
        let bytes = stream(&stamps);
        assert_eq!(packet_size(&bytes), Some(192));
        let mut dmx = Demux::new(None);
        let mut pes = Vec::new();
        for p in bytes.chunks(192) {
            dmx.feed(&p[4..], &mut pes);
        }
        dmx.flush(&mut pes);
        assert_eq!(dmx.streams(), [PmtStream { pid: 0x1011, stream_type: 0x1B, lang: String::new() }]);
        assert_eq!(pes.len(), stamps.len());
        let mut clock = Clock::new(false);
        let mapped: Vec<i64> = pes.iter().map(|p| clock.map(p.dts.unwrap(), p.discontinuity)).collect();
        for w in mapped.windows(2) {
            assert!(w[1] > w[0], "time goes forward: {mapped:?}");
            assert!(w[1] - w[0] <= step as i64, "no gap is left: {mapped:?}");
        }
        assert_eq!(&pes[0].data[..], &[0, 0, 0, 1, 0x09, 0xF0][..]);
    }
}
