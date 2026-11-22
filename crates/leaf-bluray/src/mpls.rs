//! Blu-ray / AVCHD movie playlists (`PLAYLIST/*.mpls`, AVCHD `*.MPL`): the clips a title plays, each from an in time to an out
//! time (45 kHz), the stream table of the first play item (PIDs, coding types, languages) and the chapter marks.
//!
//! Only angle 1 is read (the clip named in the play item itself; the other angles' clips are skipped). Sub paths (picture in
//! picture, secondary audio) are not read.

/// One clip of a playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayItem {
    /// The clip's name without extension (`00055`): `STREAM/00055.m2ts`, `CLIPINF/00055.clpi`.
    pub clip: String,
    /// In and out time on the clip's clock, 45 kHz.
    pub in_time: u32,
    pub out_time: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
}

/// A row of the stream table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamEntry {
    pub kind: StreamKind,
    pub pid: u16,
    /// The MPEG-TS stream type (0x1B H.264, 0x83 TrueHD, 0x90 PGS, ...).
    pub coding: u8,
    /// ISO 639-2 (`eng`), empty for video.
    pub lang: String,
    /// Video: the format code (4 = 1080i, 6 = 1080p, 8 = 2160p, ...); audio: the channel layout code (1 mono, 3 stereo, 6 multi).
    pub format: u8,
    /// Video: the frame rate code; audio: the sample rate code (1 = 48 kHz, 4 = 96 kHz, 5 = 192 kHz).
    pub rate: u8,
}

/// A chapter (entry) mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The play item it is in.
    pub item: u16,
    /// On that clip's clock, 45 kHz.
    pub time: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Playlist {
    pub items: Vec<PlayItem>,
    pub streams: Vec<StreamEntry>,
    pub marks: Vec<Mark>,
}

impl Playlist {
    /// The time it plays, 45 kHz.
    pub fn duration_45k(&self) -> u64 {
        self.items.iter().map(|i| i.out_time.saturating_sub(i.in_time) as u64).sum()
    }

    /// Where each chapter starts, in nanoseconds from the start of the title; the first is always 0.
    pub fn chapters_ns(&self) -> Vec<u64> {
        let mut starts = Vec::with_capacity(self.items.len());
        let mut acc = 0u64;
        for i in &self.items {
            starts.push(acc);
            acc += i.out_time.saturating_sub(i.in_time) as u64;
        }
        let mut out: Vec<u64> = self
            .marks
            .iter()
            .filter_map(|m| {
                let item = self.items.get(m.item as usize)?;
                let at = starts[m.item as usize] + m.time.saturating_sub(item.in_time) as u64;
                (at < acc).then_some(at * 1_000_000_000 / 45_000)
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        // marks closer than a second to the one before are dropped (discs put a mark on every clip join)
        let mut kept: Vec<u64> = Vec::with_capacity(out.len());
        for t in out {
            if kept.last().is_none_or(|&l| t >= l + 1_000_000_000) {
                kept.push(t);
            }
        }
        if kept.first() != Some(&0) {
            if kept.first().is_some_and(|&f| f < 1_000_000_000) {
                kept[0] = 0;
            } else {
                kept.insert(0, 0);
            }
        }
        kept
    }

    /// The same clip with the same in and out time played twice: a loop or a decoy playlist, not a film.
    pub fn repeats_a_clip(&self) -> bool {
        self.items.iter().enumerate().any(|(k, a)| self.items[..k].contains(a))
    }
}

struct R<'a> {
    b: &'a [u8],
    pos: usize,
}

const DAMAGED: &str = "the playlist is damaged";

impl<'a> R<'a> {
    fn at(b: &'a [u8], pos: usize) -> Result<Self, String> {
        if pos > b.len() {
            return Err(DAMAGED.into());
        }
        Ok(Self { b, pos })
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.pos..self.pos + n).ok_or(DAMAGED)?;
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let s = self.bytes(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let s = self.bytes(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}

fn lang(b: &[u8]) -> String {
    let s: String = b.iter().take(3).filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase() as char).collect();
    if s.len() == 3 { s } else { String::new() }
}

/// Reads a playlist file's bytes.
pub fn parse(b: &[u8]) -> Result<Playlist, String> {
    if b.len() < 20 || &b[..4] != b"MPLS" {
        return Err("not a Blu-ray playlist".into());
    }
    let mut h = R::at(b, 8)?;
    let list_at = h.u32()? as usize;
    let mark_at = h.u32()? as usize;

    let mut r = R::at(b, list_at)?;
    let _len = r.u32()?;
    r.u16()?; // reserved
    let n_items = r.u16()?;
    let _n_sub = r.u16()?;
    let mut pl = Playlist::default();
    for k in 0..n_items {
        let start = r.pos;
        let len = r.u16()? as usize;
        let next = start + 2 + len;
        let clip = String::from_utf8_lossy(r.bytes(5)?).into_owned();
        r.bytes(4)?; // codec identifier ("M2TS")
        let flags = r.u16()?;
        let multi_angle = flags & 0x10 != 0;
        r.u8()?; // ref_to_STC_id
        let in_time = r.u32()?;
        let out_time = r.u32()?;
        r.bytes(8)?; // UO mask table
        r.u8()?; // random access flag
        r.u8()?; // still mode
        r.u16()?; // still time
        if multi_angle {
            let angles = r.u8()?;
            r.u8()?;
            for _ in 1..angles {
                r.bytes(10)?;
            }
        }
        if k == 0 {
            pl.streams = stn(&mut r)?;
        }
        pl.items.push(PlayItem { clip, in_time, out_time });
        r = R::at(b, next)?;
    }

    if mark_at > 0 && mark_at < b.len() {
        let mut m = R::at(b, mark_at)?;
        let _len = m.u32()?;
        let n = m.u16()?;
        for _ in 0..n {
            m.u8()?;
            let ty = m.u8()?;
            let item = m.u16()?;
            let time = m.u32()?;
            m.u16()?; // entry ES PID
            m.u32()?; // duration
            if ty == 1 {
                pl.marks.push(Mark { item, time });
            }
        }
    }
    Ok(pl)
}

/// The stream number table: primary video, primary audio and presentation graphics (the rest is not read).
fn stn(r: &mut R) -> Result<Vec<StreamEntry>, String> {
    let start = r.pos;
    let len = r.u16()? as usize;
    let end = start + 2 + len;
    r.u16()?; // reserved
    let n_video = r.u8()?;
    let n_audio = r.u8()?;
    let n_pg = r.u8()?;
    r.bytes(9)?; // IG, secondary audio, secondary video, PiP PG, reserved
    let mut out = Vec::new();
    for (kind, n) in [(StreamKind::Video, n_video), (StreamKind::Audio, n_audio), (StreamKind::Subtitle, n_pg)] {
        for _ in 0..n {
            // stream entry
            let e_start = r.pos;
            let e_len = r.u8()? as usize;
            let ty = r.u8()?;
            let pid = match ty {
                1 => r.u16()?,
                2 => {
                    r.u16()?;
                    r.u16()?
                }
                3 | 4 => {
                    r.u8()?;
                    r.u16()?
                }
                _ => 0,
            };
            *r = R::at(r.b, e_start + 1 + e_len)?;
            // stream attributes
            let a_start = r.pos;
            let a_len = r.u8()? as usize;
            let coding = r.u8()?;
            let (mut format, mut rate, mut language) = (0, 0, String::new());
            match coding {
                0x01 | 0x02 | 0x1B | 0x20 | 0x24 | 0xEA => {
                    let v = r.u8()?;
                    format = v >> 4;
                    rate = v & 0x0F;
                }
                0x03 | 0x04 | 0x80..=0x86 | 0xA1 | 0xA2 => {
                    let v = r.u8()?;
                    format = v >> 4;
                    rate = v & 0x0F;
                    language = lang(r.bytes(3)?);
                }
                0x90 | 0x91 => language = lang(r.bytes(3)?),
                0x92 => {
                    r.u8()?;
                    language = lang(r.bytes(3)?);
                }
                _ => {}
            }
            *r = R::at(r.b, a_start + 1 + a_len)?;
            if pid != 0 && ty == 1 {
                out.push(StreamEntry { kind, pid, coding, lang: language, format, rate });
            }
        }
    }
    *r = R::at(r.b, end)?;
    Ok(out)
}

/// Picture size of a video format code of the stream table.
pub fn video_size(format: u8) -> Option<(u32, u32)> {
    Some(match format {
        1 | 3 => (720, 480),
        2 | 7 => (720, 576),
        4 | 6 => (1920, 1080),
        5 => (1280, 720),
        8 => (3840, 2160),
        _ => return None,
    })
}

/// Sample rate of an audio rate code of the stream table.
pub fn sample_rate(rate: u8) -> u32 {
    match rate {
        4 | 14 => 96_000,
        5 | 12 => 192_000,
        _ => 48_000,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A made-up playlist: two clips, a video, two audio tracks (one on an angle-less entry), a PGS track and three marks.
    pub(crate) fn sample() -> Vec<u8> {
        let mut stn = Vec::new();
        stn.extend_from_slice(&[0, 0]); // reserved
        stn.extend_from_slice(&[1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // video 0x1011 H.264 1080p 23.976
        stn.extend_from_slice(&[9, 1, 0x10, 0x11, 0, 0, 0, 0, 0, 0]);
        stn.extend_from_slice(&[5, 0x1B, 0x61, 0, 0, 0]);
        // audio 0x1100 TrueHD eng, multi, 48k
        stn.extend_from_slice(&[9, 1, 0x11, 0x00, 0, 0, 0, 0, 0, 0]);
        stn.extend_from_slice(&[5, 0x83, 0x61, b'e', b'n', b'g']);
        // audio 0x1101 AC-3 deu
        stn.extend_from_slice(&[9, 1, 0x11, 0x01, 0, 0, 0, 0, 0, 0]);
        stn.extend_from_slice(&[5, 0x81, 0x31, b'd', b'e', b'u']);
        // PGS 0x1200 fra
        stn.extend_from_slice(&[9, 1, 0x12, 0x00, 0, 0, 0, 0, 0, 0]);
        stn.extend_from_slice(&[5, 0x90, b'f', b'r', b'a', 0]);
        let mut stn_full = (stn.len() as u16).to_be_bytes().to_vec();
        stn_full.extend_from_slice(&stn);

        let item = |clip: &str, inn: u32, out: u32, with_stn: bool| {
            let mut v = Vec::new();
            v.extend_from_slice(clip.as_bytes());
            v.extend_from_slice(b"M2TS");
            v.extend_from_slice(&[0, 1]); // connection condition 1, no angles
            v.push(0);
            v.extend_from_slice(&inn.to_be_bytes());
            v.extend_from_slice(&out.to_be_bytes());
            v.extend_from_slice(&[0; 8]);
            v.extend_from_slice(&[0, 0, 0, 0]);
            if with_stn {
                v.extend_from_slice(&stn_full);
            } else {
                v.extend_from_slice(&[0, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            }
            let mut out = (v.len() as u16).to_be_bytes().to_vec();
            out.extend_from_slice(&v);
            out
        };
        let mut list = vec![0, 0, 0, 2, 0, 0];
        list.extend(item("00010", 45_000, 45_000 * 61, true));
        list.extend(item("00011", 90_000, 90_000 + 45_000 * 30, false));
        let mut list_full = (list.len() as u32).to_be_bytes().to_vec();
        list_full.extend_from_slice(&list);

        let mut marks = Vec::new();
        let mark = |ty: u8, item: u16, t: u32| {
            let mut v = vec![0, ty];
            v.extend_from_slice(&item.to_be_bytes());
            v.extend_from_slice(&t.to_be_bytes());
            v.extend_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0]);
            v
        };
        marks.extend(mark(1, 0, 45_000));
        marks.extend(mark(1, 0, 45_000 * 31));
        marks.extend(mark(2, 0, 45_000 * 40)); // a link point, not a chapter
        marks.extend(mark(1, 1, 90_000));
        let mut marks_full = ((marks.len() + 2) as u32).to_be_bytes().to_vec();
        marks_full.extend_from_slice(&4u16.to_be_bytes());
        marks_full.extend_from_slice(&marks);

        let list_at = 40u32;
        let mark_at = list_at + list_full.len() as u32;
        let mut f = b"MPLS0200".to_vec();
        f.extend_from_slice(&list_at.to_be_bytes());
        f.extend_from_slice(&mark_at.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.resize(40, 0);
        f.extend(list_full);
        f.extend(marks_full);
        f
    }

    #[test]
    fn playlist_parsing_on_made_up_bytes() {
        let pl = parse(&sample()).unwrap();
        assert_eq!(pl.items.len(), 2);
        assert_eq!(pl.items[0], PlayItem { clip: "00010".into(), in_time: 45_000, out_time: 45_000 * 61 });
        assert_eq!(pl.items[1].clip, "00011");
        assert_eq!(pl.duration_45k(), 45_000 * 90);
        let pids: Vec<(StreamKind, u16, u8, &str)> = pl.streams.iter().map(|s| (s.kind, s.pid, s.coding, s.lang.as_str())).collect();
        assert_eq!(
            pids,
            [
                (StreamKind::Video, 0x1011, 0x1B, ""),
                (StreamKind::Audio, 0x1100, 0x83, "eng"),
                (StreamKind::Audio, 0x1101, 0x81, "deu"),
                (StreamKind::Subtitle, 0x1200, 0x90, "fra")
            ]
        );
        assert_eq!(video_size(pl.streams[0].format), Some((1920, 1080)));
        // chapters: 0 s, 30 s (the link point is not one), 60 s (the second clip's start)
        assert_eq!(pl.chapters_ns(), [0, 30_000_000_000, 60_000_000_000]);
        assert!(!pl.repeats_a_clip());
        assert!(parse(b"not a playlist at all").is_err());
        assert!(parse(&sample()[..60]).is_err());
    }
}
