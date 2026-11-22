//! A small Matroska writer: EBML header, segment info, tracks, chapters, clusters of SimpleBlocks and, when the output can be
//! written into again (a file, not a pipe), cues, a seek head and the real duration at the end.
//!
//! Time stamps are milliseconds (the default TimestampScale). A cluster holds up to a few seconds and starts again at a video key
//! frame, so it is kept in memory and written with its size known.

use std::io::{self, Write};

/// Where the file goes. `patch` writes over bytes already written (false: it cannot, a pipe).
pub trait Out: Write {
    fn patch(&mut self, pos: u64, bytes: &[u8]) -> io::Result<bool>;
}

impl Out for std::fs::File {
    fn patch(&mut self, pos: u64, bytes: &[u8]) -> io::Result<bool> {
        use std::io::{Seek, SeekFrom};
        let back = self.stream_position()?;
        self.seek(SeekFrom::Start(pos))?;
        self.write_all(bytes)?;
        self.seek(SeekFrom::Start(back))?;
        Ok(true)
    }
}

/// A pipe (ffmpeg's standard input): nothing can be written over.
pub struct Pipe<W: Write>(pub W);

impl<W: Write> Write for Pipe<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write> Out for Pipe<W> {
    fn patch(&mut self, _: u64, _: &[u8]) -> io::Result<bool> {
        Ok(false)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Subtitle,
}

#[derive(Clone, Debug)]
pub struct TrackDef {
    pub kind: Kind,
    pub codec_id: &'static str,
    pub private: Option<Vec<u8>>,
    /// ISO 639-2; empty: undetermined.
    pub lang: String,
    pub name: String,
    pub default: bool,
    /// Video: picture size.
    pub size: (u32, u32),
    /// Audio: sample rate, channels, bits per sample (0: not said).
    pub audio: (u32, u32, u32),
}

// element ids
const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POS: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CHAPTERS: u32 = 0x1043_A770;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const VOID: u32 = 0xEC;

fn id(out: &mut Vec<u8>, id: u32) {
    let b = id.to_be_bytes();
    let skip = b.iter().take_while(|&&x| x == 0).count();
    out.extend_from_slice(&b[skip..]);
}

fn size(out: &mut Vec<u8>, n: u64) {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let v = n | (1u64 << (7 * len));
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

fn size8(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&(n | (1u64 << 56)).to_be_bytes());
}

fn el(out: &mut Vec<u8>, i: u32, body: &[u8]) {
    id(out, i);
    size(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn uint(out: &mut Vec<u8>, i: u32, v: u64) {
    let b = v.to_be_bytes();
    let skip = b.iter().take_while(|&&x| x == 0).count().min(7);
    el(out, i, &b[skip..]);
}

fn float(out: &mut Vec<u8>, i: u32, v: f64) {
    el(out, i, &v.to_be_bytes());
}

fn text(out: &mut Vec<u8>, i: u32, s: &str) {
    el(out, i, s.as_bytes());
}

fn master(out: &mut Vec<u8>, i: u32, f: impl FnOnce(&mut Vec<u8>)) {
    let mut body = Vec::new();
    f(&mut body);
    el(out, i, &body);
}

/// A made-up but stable 64-bit id (track and chapter UIDs must not be 0).
fn uid(seed: u64) -> u64 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 31)) | 1
}

const SEEK_ROOM: usize = 160;

pub struct Writer<O: Out> {
    out: O,
    /// Bytes written so far.
    pos: u64,
    segment_size_at: u64,
    segment_data: u64,
    seek_at: u64,
    duration_at: u64,
    info_pos: u64,
    tracks_pos: u64,
    chapters_pos: Option<u64>,
    video_track: Option<u64>,
    cluster: Vec<u8>,
    cluster_ts: i64,
    cluster_open: bool,
    cues: Vec<(u64, u64, u64)>,
    max_ts: i64,
}

impl<O: Out> Writer<O> {
    /// Writes the header. `duration_ms`: what the playlist says (the real one is written at the end when it can be); chapters are
    /// (start in ns, name).
    pub fn new(mut out: O, title: &str, tracks: &[TrackDef], duration_ms: f64, chapters: &[(u64, String)]) -> io::Result<Self> {
        let mut h = Vec::new();
        master(&mut h, EBML, |b| {
            uint(b, 0x4286, 1);
            uint(b, 0x42F7, 1);
            uint(b, 0x42F2, 4);
            uint(b, 0x42F3, 8);
            text(b, 0x4282, "matroska");
            uint(b, 0x4287, 4);
            uint(b, 0x4285, 2);
        });
        id(&mut h, SEGMENT);
        let segment_size_at = h.len() as u64;
        h.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let segment_data = h.len() as u64;
        // room for the seek head, written at the end
        let seek_at = h.len() as u64;
        id(&mut h, VOID);
        size8(&mut h, (SEEK_ROOM - 9) as u64);
        h.resize(h.len() + SEEK_ROOM - 9, 0);

        let info_pos = h.len() as u64 - segment_data;
        let mut info = Vec::new();
        uint(&mut info, 0x2AD7B1, 1_000_000);
        let duration_in_info = info.len() + 3; // after the Duration id (2 bytes) and its size (1 byte)
        float(&mut info, 0x4489, duration_ms.max(0.0));
        text(&mut info, 0x4D80, "leaf");
        text(&mut info, 0x5741, "leaf");
        if !title.is_empty() {
            text(&mut info, 0x7BA9, title);
        }
        id(&mut h, INFO);
        size(&mut h, info.len() as u64);
        let duration_at = h.len() as u64 + duration_in_info as u64;
        h.extend_from_slice(&info);

        let tracks_pos = h.len() as u64 - segment_data;
        let mut video_track = None;
        master(&mut h, TRACKS, |b| {
            for (k, t) in tracks.iter().enumerate() {
                let n = k as u64 + 1;
                if t.kind == Kind::Video && video_track.is_none() {
                    video_track = Some(n);
                }
                master(b, 0xAE, |e| {
                    uint(e, 0xD7, n);
                    uint(e, 0x73C5, uid(n));
                    uint(e, 0x83, match t.kind {
                        Kind::Video => 1,
                        Kind::Audio => 2,
                        Kind::Subtitle => 0x11,
                    });
                    uint(e, 0x88, t.default as u64);
                    uint(e, 0x9C, 0);
                    text(e, 0x22B59C, if t.lang.is_empty() { "und" } else { &t.lang });
                    if !t.name.is_empty() {
                        text(e, 0x536E, &t.name);
                    }
                    text(e, 0x86, t.codec_id);
                    if let Some(p) = &t.private {
                        el(e, 0x63A2, p);
                    }
                    match t.kind {
                        Kind::Video => master(e, 0xE0, |v| {
                            uint(v, 0xB0, t.size.0 as u64);
                            uint(v, 0xBA, t.size.1 as u64);
                        }),
                        Kind::Audio => master(e, 0xE1, |a| {
                            float(a, 0xB5, t.audio.0 as f64);
                            uint(a, 0x9F, t.audio.1.max(1) as u64);
                            if t.audio.2 > 0 {
                                uint(a, 0x6264, t.audio.2 as u64);
                            }
                        }),
                        Kind::Subtitle => {}
                    }
                });
            }
        });

        let mut chapters_pos = None;
        if chapters.len() > 1 {
            chapters_pos = Some(h.len() as u64 - segment_data);
            master(&mut h, CHAPTERS, |b| {
                master(b, 0x45B9, |ed| {
                    uint(ed, 0x45BC, uid(1000));
                    for (k, (at, name)) in chapters.iter().enumerate() {
                        master(ed, 0xB6, |a| {
                            uint(a, 0x73C4, uid(2000 + k as u64));
                            uint(a, 0x91, *at);
                            master(a, 0x80, |d| {
                                text(d, 0x85, name);
                                text(d, 0x437C, "eng");
                            });
                        });
                    }
                });
            });
        }
        out.write_all(&h)?;
        Ok(Self {
            out,
            pos: h.len() as u64,
            segment_size_at,
            segment_data,
            seek_at,
            duration_at,
            info_pos,
            tracks_pos,
            chapters_pos,
            video_track,
            cluster: Vec::new(),
            cluster_ts: 0,
            cluster_open: false,
            cues: Vec::new(),
            max_ts: 0,
        })
    }

    fn close_cluster(&mut self) -> io::Result<()> {
        if !self.cluster_open {
            return Ok(());
        }
        let mut h = Vec::new();
        id(&mut h, CLUSTER);
        size(&mut h, self.cluster.len() as u64);
        self.out.write_all(&h)?;
        self.out.write_all(&self.cluster)?;
        self.pos += (h.len() + self.cluster.len()) as u64;
        self.cluster.clear();
        self.cluster_open = false;
        Ok(())
    }

    /// One frame of `track` (1-based, in the order given to [`Writer::new`]) at `ts_ms`.
    pub fn block(&mut self, track: u64, ts_ms: i64, key: bool, data: &[u8]) -> io::Result<()> {
        let ts_ms = ts_ms.max(0);
        let is_video = Some(track) == self.video_track;
        let rel = ts_ms - self.cluster_ts;
        let start_new = !self.cluster_open
            || (is_video && key && rel >= 1000)
            || (self.video_track.is_none() && rel >= 5000)
            || rel > 30_000
            || rel < -30_000
            || self.cluster.len() > 48 << 20;
        if start_new {
            self.close_cluster()?;
            self.cluster_open = true;
            self.cluster_ts = ts_ms;
            uint(&mut self.cluster, 0xE7, ts_ms as u64);
            if is_video && key {
                self.cues.push((ts_ms as u64, track, self.pos - self.segment_data));
            } else if self.video_track.is_none() {
                self.cues.push((ts_ms as u64, track, self.pos - self.segment_data));
            }
        }
        let rel = (ts_ms - self.cluster_ts).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        let mut b = Vec::with_capacity(4);
        size(&mut b, track);
        b.extend_from_slice(&rel.to_be_bytes());
        b.push(if key { 0x80 } else { 0 });
        id(&mut self.cluster, 0xA3);
        size(&mut self.cluster, (b.len() + data.len()) as u64);
        self.cluster.extend_from_slice(&b);
        self.cluster.extend_from_slice(data);
        self.max_ts = self.max_ts.max(ts_ms);
        Ok(())
    }

    /// Writes what is left; on a file also the cues, the seek head, the segment size and the duration. Gives the output back.
    pub fn finish(mut self, last_ms: i64) -> io::Result<O> {
        self.close_cluster()?;
        let mut cues_pos = None;
        if !self.cues.is_empty() {
            let mut c = Vec::new();
            master(&mut c, CUES, |b| {
                for &(t, track, at) in &self.cues {
                    master(b, 0xBB, |p| {
                        uint(p, 0xB3, t);
                        master(p, 0xB7, |q| {
                            uint(q, 0xF7, track);
                            uint(q, 0xF1, at);
                        });
                    });
                }
            });
            cues_pos = Some(self.pos - self.segment_data);
            self.out.write_all(&c)?;
            self.pos += c.len() as u64;
        }
        self.out.flush()?;
        let mut s = Vec::new();
        master(&mut s, SEEK_HEAD, |b| {
            let mut seek = |i: u32, at: u64| {
                master(b, SEEK, |e| {
                    let mut idb = Vec::new();
                    id(&mut idb, i);
                    el(e, SEEK_ID, &idb);
                    uint(e, SEEK_POS, at);
                });
            };
            seek(INFO, self.info_pos);
            seek(TRACKS, self.tracks_pos);
            if let Some(p) = self.chapters_pos {
                seek(CHAPTERS, p);
            }
            if let Some(p) = cues_pos {
                seek(CUES, p);
            }
        });
        if s.len() + 9 <= SEEK_ROOM {
            id(&mut s, VOID);
            let room = (SEEK_ROOM - s.len() - 8) as u64;
            size8(&mut s, room);
            s.resize(SEEK_ROOM, 0);
            if self.out.patch(self.seek_at, &s)? {
                let seg = self.pos - self.segment_data;
                self.out.patch(self.segment_size_at, &(seg | (1u64 << 56)).to_be_bytes())?;
                let d = last_ms.max(self.max_ts) as f64;
                self.out.patch(self.duration_at, &d.to_be_bytes())?;
            }
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

