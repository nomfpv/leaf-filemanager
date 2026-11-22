//! One title to one Matroska file, losslessly: the clips are read one after the other, the chosen tracks' frames are written with
//! one continuous timeline (each clip's in time moved to where the one before ended, breaks inside a clip closed up), the
//! playlist's chapters with them.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::codec::{self, Codec};
use crate::disc::{Title, Track};
use crate::mkv::{Kind, Out, TrackDef, Writer};
use crate::mpls::StreamKind;
use crate::ts::{self, Clock, Demux, Pes};

#[derive(Debug)]
pub enum Error {
    Cancelled,
    /// The streams are encrypted: nothing was written.
    Protected,
    Io(std::io::Error),
    /// A track could not be set up (its first frames were not found) or a stream does not parse.
    Bad(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// How the work is steered from outside: cancel, pause, and progress as (bytes read, bytes in all).
pub struct Ctl<'a> {
    pub cancel: &'a AtomicBool,
    pub paused: &'a AtomicBool,
    pub progress: &'a dyn Fn(u64, u64),
}

impl Ctl<'_> {
    fn check(&self) -> Result<(), Error> {
        while self.paused.load(Ordering::Relaxed) && !self.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
        if self.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }
}

struct Lane {
    pid: u16,
    number: u64,
    codec: Codec,
    kind: StreamKind,
    clock: Clock,
    /// An audio frame waiting for its extension (same time stamp), or a PGS display set waiting for its end: (raw pts, time, bytes).
    pending: Option<(u64, i64, Vec<u8>)>,
    /// Video: the first key frame of this clip has been written (frames before it cannot be decoded).
    started: bool,
}

const READ: usize = 192 * 32 * 1024;
const PROBE_LIMIT: u64 = 96 << 20;

/// The header of each chosen track, from the first frames of the first clip.
fn track_defs(title: &Title, chosen: &[&Track], ctl: &Ctl) -> Result<Vec<TrackDef>, Error> {
    let first = &title.clips[0].path;
    let mut f = std::fs::File::open(first)?;
    let mut head = vec![0u8; READ];
    let n = read_full(&mut f, &mut head)?;
    head.truncate(n);
    if ts::looks_encrypted(&head) {
        return Err(Error::Protected);
    }
    let size = ts::packet_size(&head).ok_or(Error::Protected)?;
    let mut dmx = Demux::new(Some(chosen.iter().map(|t| t.pid).collect()));
    let mut seen: Vec<(u16, Vec<u8>)> = chosen.iter().map(|t| (t.pid, Vec::new())).collect();
    let mut defs: Vec<Option<TrackDef>> = vec![None; chosen.len()];
    let mut read = 0u64;
    let mut buf = head;
    let mut out = Vec::new();
    loop {
        for p in buf.chunks_exact(size) {
            dmx.feed(&p[size - ts::TS..], &mut out);
        }
        for pes in out.drain(..) {
            let Some(k) = chosen.iter().position(|t| t.pid == pes.pid) else { continue };
            if defs[k].is_some() {
                continue;
            }
            let t = chosen[k];
            let Some(c) = Codec::of(t.stream_type) else { continue };
            if c == Codec::TrueHd && (pes.stream_id_ext == 0x76 || pes.data.starts_with(&[0x0B, 0x77])) {
                continue;
            }
            let acc = &mut seen[k].1;
            if acc.len() < 8 << 20 {
                acc.extend_from_slice(&pes.data);
            }
            if let Some(mut d) = codec::track_def(c, acc, crate::mpls::video_size(t.format), (t.format, t.rate)) {
                d.lang = t.lang.clone();
                d.name = t.label();
                defs[k] = Some(d);
            }
        }
        if defs.iter().all(Option::is_some) {
            break;
        }
        read += buf.len() as u64;
        if read >= PROBE_LIMIT {
            break;
        }
        ctl.check()?;
        buf = vec![0u8; READ];
        let n = read_full(&mut f, &mut buf)?;
        if n == 0 {
            break;
        }
        buf.truncate(n - n % size);
    }
    let mut out_defs = Vec::with_capacity(chosen.len());
    for (k, d) in defs.into_iter().enumerate() {
        match d {
            Some(d) => out_defs.push(d),
            // a subtitle track that has nothing in the first minutes still gets its header
            None if Codec::of(chosen[k].stream_type) == Some(Codec::Pgs) => {
                let mut d = codec::track_def(Codec::Pgs, &[], None, (0, 0)).expect("PGS needs no first frame");
                d.lang = chosen[k].lang.clone();
                d.name = chosen[k].label();
                out_defs.push(d);
            }
            None => return Err(Error::Bad(format!("the start of the {} track was not found", chosen[k].label()))),
        }
    }
    Ok(out_defs)
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            k => got += k,
        }
    }
    Ok(got)
}

/// Writes `title`'s tracks `pids` (in this order; each must be [`Track::remuxable`]) into `out`. Gives the output back, finished.
pub fn remux<O: Out>(title: &Title, pids: &[u16], name: &str, out: O, ctl: &Ctl) -> Result<O, Error> {
    let chosen: Vec<&Track> = pids.iter().filter_map(|p| title.tracks.iter().find(|t| t.pid == *p)).collect();
    if chosen.is_empty() || chosen.iter().any(|t| !t.remuxable()) {
        return Err(Error::Bad("a chosen track can't be remuxed".into()));
    }
    let mut defs = track_defs(title, &chosen, ctl)?;
    // the first video and the first audio track play by default
    for kind in [Kind::Video, Kind::Audio] {
        if let Some(d) = defs.iter_mut().find(|d| d.kind == kind) {
            d.default = true;
        }
    }
    let chapters: Vec<(u64, String)> = title.chapters_ns.iter().enumerate().map(|(k, &t)| (t, format!("Chapter {:02}", k + 1))).collect();
    let mut w = Writer::new(out, name, &defs, title.duration_ns as f64 / 1e6, &chapters)?;
    let total: u64 = title.clips.iter().map(|c| c.size).sum::<u64>().max(1);
    let mut done = 0u64;
    // where the next clip starts on the output timeline, 90 kHz
    let mut at = 0i64;
    let mut out_pes = Vec::new();
    for clip in &title.clips {
        ctl.check()?;
        let mut lanes: Vec<Lane> = chosen
            .iter()
            .enumerate()
            .map(|(k, t)| Lane {
                pid: t.pid,
                number: k as u64 + 1,
                codec: Codec::of(t.stream_type).expect("checked"),
                kind: t.kind,
                clock: Clock::new(t.kind == StreamKind::Subtitle),
                pending: None,
                started: false,
            })
            .collect();
        let mut f = std::fs::File::open(&clip.path)?;
        let mut dmx = Demux::new(Some(pids.to_vec()));
        let mut size = 0usize;
        // the clip's own start (a loose file: its first time stamp) and the latest time seen in it
        let mut start: Option<i64> = clip.span.map(|(i, _)| i as i64);
        let end: Option<i64> = clip.span.map(|(_, o)| o as i64);
        let mut latest = i64::MIN;
        let mut buf = vec![0u8; READ];
        loop {
            ctl.check()?;
            let n = read_full(&mut f, &mut buf)?;
            if n == 0 {
                break;
            }
            if size == 0 {
                if ts::looks_encrypted(&buf[..n]) {
                    return Err(Error::Protected);
                }
                size = ts::packet_size(&buf[..n]).ok_or(Error::Protected)?;
            }
            for p in buf[..n - n % size].chunks_exact(size) {
                dmx.feed(&p[size - ts::TS..], &mut out_pes);
            }
            for pes in out_pes.drain(..) {
                take(&mut w, &mut lanes, pes, &mut start, end, at, &mut latest)?;
            }
            done += n as u64;
            (ctl.progress)(done.min(total), total);
        }
        dmx.flush(&mut out_pes);
        for pes in out_pes.drain(..) {
            take(&mut w, &mut lanes, pes, &mut start, end, at, &mut latest)?;
        }
        for l in &mut lanes {
            flush(&mut w, l)?;
        }
        let s = start.unwrap_or(0);
        at += match (clip.span, latest) {
            (Some((i, o)), _) => o as i64 - i as i64,
            (None, l) if l > i64::MIN => l - s + 3754,
            _ => 0,
        };
    }
    Ok(w.finish(at / 90)?)
}

fn flush<O: Out>(w: &mut Writer<O>, l: &mut Lane) -> Result<(), Error> {
    if let Some((_, t, data)) = l.pending.take()
        && !data.is_empty()
    {
        w.block(l.number, t / 90, true, &data)?;
    }
    Ok(())
}

/// One PES of a clip: onto the timeline, cut to the clip's in / out time, into frames.
fn take<O: Out>(w: &mut Writer<O>, lanes: &mut [Lane], pes: Pes, start: &mut Option<i64>, end: Option<i64>, at: i64, latest: &mut i64) -> Result<(), Error> {
    let Some(l) = lanes.iter_mut().find(|l| l.pid == pes.pid) else { return Ok(()) };
    if l.codec == Codec::TrueHd && pes.stream_id_ext == 0x76 {
        return Ok(());
    }
    let Some(raw) = pes.pts else {
        // no time of its own: part of the frame before (an extension)
        if let Some((_, _, d)) = &mut l.pending {
            d.extend_from_slice(&pes.data);
        }
        return Ok(());
    };
    let pts = l.clock.map(raw, pes.discontinuity);
    let dts = match pes.dts {
        Some(d) => pts - ((raw as i64 - d as i64).rem_euclid(1 << 33)),
        None => pts,
    };
    let s = *start.get_or_insert(pts);
    *latest = (*latest).max(pts);
    let t = pts - s + at;
    match l.kind {
        StreamKind::Video => {
            if end.is_some_and(|e| dts >= e) {
                return Ok(());
            }
            let (frame, key) = codec::video_frame(l.codec, &pes.data);
            if !l.started {
                // frames before the in time, or before the first key frame, are not shown
                if !key || pts < s - 4500 {
                    return Ok(());
                }
                l.started = true;
            }
            if !frame.is_empty() {
                w.block(l.number, t / 90, key, &frame)?;
            }
        }
        StreamKind::Audio | StreamKind::Subtitle => {
            if pts < s || end.is_some_and(|e| pts >= e) {
                return Ok(());
            }
            match l.codec {
                Codec::Pgs => {
                    let pend = l.pending.get_or_insert((raw, t, Vec::new()));
                    pend.2.extend_from_slice(&pes.data);
                    if codec::pgs_ends_display_set(&pes.data) {
                        flush(w, l)?;
                    }
                }
                Codec::TrueHd => {
                    // one frame per access unit (1/1200 s each)
                    for (k, unit) in codec::truehd_units(&pes.data).into_iter().enumerate() {
                        w.block(l.number, (t + k as i64 * 75) / 90, true, unit)?;
                    }
                }
                Codec::Lpcm => {
                    if let Some(f) = codec::lpcm_frame(&pes.data) {
                        w.block(l.number, t / 90, true, &f)?;
                    }
                }
                _ => {
                    // a core and its extension come with one time stamp: one frame
                    if l.pending.as_ref().is_some_and(|p| p.0 == raw) {
                        if let Some(p) = &mut l.pending {
                            p.2.extend_from_slice(&pes.data);
                        }
                    } else {
                        flush(w, l)?;
                        l.pending = Some((raw, t, pes.data));
                    }
                }
            }
        }
    }
    Ok(())
}
