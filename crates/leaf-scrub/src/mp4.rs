//! MP4, MOV, M4A: the boxes that hold tags, location and device information (`udta`, `meta`, the XMP `uuid`) are renamed to
//! `free` IN PLACE, the creation / modification times of the movie, tracks and media are zeroed, and the handler names are
//! emptied. Nothing changes size, so no offset in the file moves and the audio and video data is not touched.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const XMP_UUID: [u8; 16] = [0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF, 0xAC];
const MAX_MOOV: u64 = 512 << 20;

type Patch = (u64, Vec<u8>);

struct Child<'a> {
    off: u64,
    hdr: u64,
    ty: [u8; 4],
    body: &'a [u8],
}

fn bad<T>(w: &str) -> Result<T> {
    Err(ScrubError::Malformed(w.into()))
}

/// The boxes of an in-memory range; `base` is the file offset of `buf[0]`.
fn children(buf: &[u8], base: u64) -> Result<Vec<Child<'_>>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < buf.len() {
        if buf.len() - p < 8 {
            return bad("a box header is cut off");
        }
        let s32 = u32::from_be_bytes(buf[p..p + 4].try_into().unwrap()) as u64;
        let ty: [u8; 4] = buf[p + 4..p + 8].try_into().unwrap();
        let (hdr, size) = match s32 {
            0 => (8, (buf.len() - p) as u64),
            1 => {
                if buf.len() - p < 16 {
                    return bad("a box header is cut off");
                }
                (16, u64::from_be_bytes(buf[p + 8..p + 16].try_into().unwrap()))
            }
            n => (8, n),
        };
        if size < hdr || p as u64 + size > buf.len() as u64 {
            return bad("a box runs past its parent");
        }
        out.push(Child { off: base + p as u64, hdr, ty, body: &buf[p + hdr as usize..p + size as usize] });
        p += size as usize;
    }
    Ok(out)
}

fn mac_date(secs: u64) -> String {
    if secs == 0 {
        return String::new();
    }
    let unix = secs as i64 - 2_082_844_800;
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

/// Zero the creation and modification time of a mvhd / tkhd / mdhd box; reports a finding when one was set.
fn zero_times(c: &Child<'_>, what: &str, patches: &mut Vec<Patch>, found: &mut Vec<Finding>) {
    if c.body.len() < 4 {
        return;
    }
    let (len, v1) = if c.body[0] == 1 { (16usize, true) } else { (8usize, false) };
    if c.body.len() < 4 + len {
        return;
    }
    let times = &c.body[4..4 + len];
    if times.iter().all(|b| *b == 0) {
        return;
    }
    let first = if v1 { u64::from_be_bytes(times[0..8].try_into().unwrap()) } else { u32::from_be_bytes(times[0..4].try_into().unwrap()) as u64 };
    found.push(Finding::new(format!("{what} creation and modification time"), mac_date(first), true));
    patches.push((c.off + c.hdr + 4, vec![0u8; len]));
}

fn odd_label(ty: &[u8; 4]) -> String {
    match ty {
        b"uuid" => "Vendor data box (uuid: can hold the camera model and serial)".to_string(),
        b"prft" => "Producer time stamp (prft)".to_string(),
        other => format!("Box leaf does not know ({})", String::from_utf8_lossy(other).trim()),
    }
}

fn rename_free(c: &Child<'_>, what: &str, patches: &mut Vec<Patch>, found: &mut Vec<Finding>) {
    found.push(Finding::new(what, format!("{} bytes", c.body.len() + c.hdr as usize), true));
    patches.push((c.off + 4, b"free".to_vec()));
    // the old content goes too: a renamed box that still holds the tags would only hide them
    if c.body.iter().any(|b| *b != 0) {
        patches.push((c.off + c.hdr, vec![0u8; c.body.len()]));
    }
}

fn be32(b: &[u8], p: usize) -> Result<u64> {
    b.get(p..p + 4).map(|x| u32::from_be_bytes(x.try_into().unwrap()) as u64).ok_or_else(|| ScrubError::Malformed("a sample table is cut off".into()))
}

fn be64(b: &[u8], p: usize) -> Result<u64> {
    b.get(p..p + 8).map(|x| u64::from_be_bytes(x.try_into().unwrap())).ok_or_else(|| ScrubError::Malformed("a sample table is cut off".into()))
}

/// A table of `count` entries of `size` bytes after `skip` header bytes must fit in its box (a count read from the file never
/// drives a loop or an allocation past the box it is in).
fn table(body: &[u8], skip: usize, size: usize) -> Result<u64> {
    let count = be32(body, skip - 4)?;
    if count.checked_mul(size as u64).is_none_or(|n| n > (body.len() - skip.min(body.len())) as u64) {
        return bad("a sample table says it has more entries than it holds");
    }
    Ok(count)
}

/// One track as far as the cleaning needs it.
#[derive(Default)]
struct Track {
    handler: [u8; 4],
    /// the type of the first sample entry (`avc1`, `mp4a`, `gpmd`, `tmcd` ...)
    entry: [u8; 4],
    /// the file position of the flags byte of `tkhd` that holds "enabled", and its value
    enabled_at: Option<(u64, u8)>,
    /// every chunk of samples: (file position, length)
    chunks: Vec<(u64, u64)>,
    /// the sample data is in this file (`dref` entries are self-contained)
    self_contained: bool,
    /// the tables to find the samples were there and consistent
    tables: bool,
}

/// The chunks of a sample table (`stbl`): positions from `stco` / `co64`, how many samples each holds from `stsc`, their
/// sizes from `stsz`. `None` when a table is missing or uses a form leaf does not read (`stz2`).
fn chunks_of(stbl: &[u8], base: u64) -> Result<Option<Vec<(u64, u64)>>> {
    let (mut offs, mut stsc, mut stsz): (Option<Vec<u64>>, Option<&[u8]>, Option<&[u8]>) = (None, None, None);
    for c in children(stbl, base)? {
        match &c.ty {
            b"stco" => {
                let n = table(c.body, 8, 4)?;
                offs = Some((0..n as usize).map(|i| be32(c.body, 8 + 4 * i)).collect::<Result<_>>()?);
            }
            b"co64" => {
                let n = table(c.body, 8, 8)?;
                offs = Some((0..n as usize).map(|i| be64(c.body, 8 + 8 * i)).collect::<Result<_>>()?);
            }
            b"stsc" => stsc = Some(c.body),
            b"stsz" => stsz = Some(c.body),
            _ => {}
        }
    }
    let (Some(offs), Some(stsc), Some(stsz)) = (offs, stsc, stsz) else { return Ok(None) };
    let runs = table(stsc, 8, 12)? as usize;
    let uniform = be32(stsz, 4)?;
    let samples = be32(stsz, 8)?;
    if uniform == 0 {
        table(stsz, 12, 4)?;
    }
    let mut out = Vec::with_capacity(offs.len());
    let mut next_sample = 0u64;
    let mut run = 0usize;
    for (i, off) in offs.iter().enumerate() {
        let chunk = i as u64 + 1;
        // the last run that starts at or before this chunk (the chunks go up, so the run only moves forward)
        while run + 1 < runs && be32(stsc, 8 + 12 * (run + 1))? <= chunk {
            run += 1;
        }
        let per = if runs > 0 && be32(stsc, 8 + 12 * run)? <= chunk { be32(stsc, 8 + 12 * run + 4)? } else { 0 };
        if next_sample.checked_add(per).is_none_or(|n| n > samples) {
            return bad("the sample tables of a track do not agree");
        }
        let len = if uniform != 0 {
            uniform.checked_mul(per).ok_or_else(|| ScrubError::Malformed("a chunk is too big".into()))?
        } else {
            let mut sum = 0u64;
            for k in next_sample..next_sample + per {
                sum += be32(stsz, 12 + 4 * k as usize)?;
            }
            sum
        };
        next_sample += per;
        out.push((*off, len));
    }
    Ok(Some(out))
}

fn walk_trak(body: &[u8], base: u64, patches: &mut Vec<Patch>, found: &mut Vec<Finding>) -> Result<Track> {
    let mut t = Track { self_contained: true, ..Default::default() };
    for c in children(body, base)? {
        match &c.ty {
            b"tkhd" => {
                zero_times(&c, "Track", patches, found);
                if c.body.len() >= 4 {
                    t.enabled_at = Some((c.off + c.hdr + 3, c.body[3]));
                }
            }
            b"udta" | b"meta" => rename_free(&c, "Track tags and device information", patches, found),
            b"free" | b"skip" if c.body.iter().any(|b| *b != 0) => {
                found.push(Finding::new("Leftover data in padding", format!("{} bytes", c.body.len()), true));
                patches.push((c.off + c.hdr, vec![0u8; c.body.len()]));
            }
            b"mdia" => {
                for m in children(c.body, c.off + c.hdr)? {
                    match &m.ty {
                        b"mdhd" => zero_times(&m, "Media", patches, found),
                        b"hdlr" if m.body.len() >= 12 => {
                            t.handler = m.body[8..12].try_into().unwrap();
                            if m.body.len() > 24 {
                                let name = &m.body[24..];
                                if name.iter().any(|b| *b != 0) {
                                    let txt: String = name.iter().filter(|b| **b >= 0x20 && **b < 0x7F).map(|b| *b as char).collect();
                                    found.push(Finding::new("Handler / encoder name", txt, true));
                                    patches.push((m.off + m.hdr + 24, vec![0u8; name.len()]));
                                }
                            }
                        }
                        b"minf" => {
                            for n in children(m.body, m.off + m.hdr)? {
                                match &n.ty {
                                    b"dinf" => {
                                        for dr in children(n.body, n.off + n.hdr)?.into_iter().filter(|x| &x.ty == b"dref") {
                                            if dr.body.len() < 8 {
                                                return bad("a data reference is cut off");
                                            }
                                            for e in children(&dr.body[8..], dr.off + dr.hdr + 8)? {
                                                if e.body.len() < 4 || e.body[3] & 1 == 0 {
                                                    t.self_contained = false;
                                                }
                                            }
                                        }
                                    }
                                    b"stbl" => {
                                        let kids = children(n.body, n.off + n.hdr)?;
                                        if let Some(sd) = kids.iter().find(|k| &k.ty == b"stsd") {
                                            if sd.body.len() >= 16 {
                                                t.entry = sd.body[12..16].try_into().unwrap();
                                            }
                                        }
                                        if let Some(ch) = chunks_of(n.body, n.off + n.hdr)? {
                                            t.chunks = ch;
                                            t.tables = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(t)
}

const SUBTITLE_HANDLERS: [&[u8; 4]; 4] = [b"sbtl", b"subt", b"text", b"clcp"];

/// What a track that is not picture or sound holds, in words.
fn track_kind(t: &Track) -> String {
    let e = &t.entry;
    let what = match (e, &t.handler) {
        (b"gpmd", _) => "GoPro telemetry: GPS route and sensors",
        (b"djmd", _) | (b"dbgi", _) => "DJI telemetry: GPS and flight data",
        (b"camm", _) | (_, b"camm") => "camera motion and GPS (camm)",
        (b"rtmd", _) => "Sony real-time metadata",
        (b"mett", _) | (b"metx", _) | (b"mebx", _) | (_, b"meta") | (_, b"mdta") => "timed metadata (can hold GPS and device data)",
        (b"tmcd", _) | (_, b"tmcd") => "timecode (recording time)",
        (_, h) if SUBTITLE_HANDLERS.contains(&h) => "text / subtitles (can hold coordinates)",
        (_, b"hint") => "streaming hints",
        _ => "data",
    };
    let code = |b: &[u8; 4]| String::from_utf8_lossy(b).trim().to_string();
    format!("{what}; handler {}, {}", code(&t.handler), code(&t.entry))
}

/// Tracks that are not picture or sound (and not a subtitle track the user keeps): their samples are overwritten with zeros in
/// place and the track is disabled; each is named. Refused when that cannot be done safely.
#[allow(clippy::too_many_arguments)]
fn plan_tracks(
    f: &mut fs::File,
    tracks: &[Track],
    mdats: &[(u64, u64)],
    fragmented: bool,
    opts: &Options,
    patches: &mut Vec<Patch>,
    zero_runs: &mut Vec<(u64, u64)>,
    found: &mut Vec<Finding>,
) -> Result<Vec<(u64, u64)>> {
    let keeps = |t: &Track| &t.handler == b"vide" || &t.handler == b"soun" || (opts.keep_subtitles && SUBTITLE_HANDLERS.contains(&&t.handler));
    let mut kept: Vec<(u64, u64)> = Vec::new();
    for t in tracks.iter().filter(|t| keeps(t)) {
        kept.extend(t.chunks.iter().map(|(a, n)| (*a, a.saturating_add(*n))));
        if SUBTITLE_HANDLERS.contains(&&t.handler) {
            found.push(Finding::new("Subtitle track (kept as you chose)", track_kind(t), false));
        }
    }
    // as disjoint ranges in order, so one search tells whether a range touches any of them
    kept.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(kept.len());
    for (s, e) in kept {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let kept = merged;
    for t in tracks.iter().filter(|t| !keeps(t)) {
        let kind = track_kind(t);
        if fragmented {
            return Err(ScrubError::Unsupported(format!("the video is stored in fragments and has a track that is not picture or sound ({kind}): leaf cannot empty it safely")));
        }
        if !t.self_contained {
            return Err(ScrubError::Unsupported(format!("a track ({kind}) keeps its data in another file")));
        }
        if !t.tables {
            return Err(ScrubError::Unsupported(format!("a track ({kind}) has sample tables leaf cannot read, so it cannot be emptied")));
        }
        let mut dirty = 0usize;
        for (a, n) in &t.chunks {
            let end = a.checked_add(*n).ok_or_else(|| ScrubError::Malformed("a chunk is out of range".into()))?;
            if *n == 0 {
                continue;
            }
            if !mdats.iter().any(|(s, e)| *a >= *s && end <= *e) {
                return Err(ScrubError::Unsupported(format!("the samples of a track ({kind}) are not inside the media data, leaf cannot empty them safely")));
            }
            // never a byte of a picture or sound sample
            let i = kept.partition_point(|k| k.1 <= *a);
            if kept.get(i).is_some_and(|(s, _)| *s < end) {
                return Err(ScrubError::Unsupported(format!("the samples of a track ({kind}) share bytes with the picture or sound")));
            }
            let mut buf = vec![0u8; (*n).min(1 << 20) as usize];
            let mut at = *a;
            let mut nonzero = false;
            while at < end && !nonzero {
                let k = ((end - at) as usize).min(buf.len());
                f.seek(SeekFrom::Start(at))?;
                f.read_exact(&mut buf[..k])?;
                nonzero = buf[..k].iter().any(|b| *b != 0);
                at += k as u64;
            }
            if nonzero {
                zero_runs.push((*a, *n));
                dirty += 1;
            }
        }
        let enabled = t.enabled_at.filter(|(_, fl)| fl & 1 != 0);
        if let Some((at, fl)) = enabled {
            patches.push((at, vec![fl & !1]));
        }
        if dirty > 0 || enabled.is_some() {
            found.push(Finding::new(format!("Track that is not picture or sound ({kind})"), format!("{} chunks of samples emptied, track disabled", t.chunks.len()), true));
        }
    }
    Ok(kept)
}

/// What a run writes: byte patches, and runs of zeros (file position, length) over the samples of emptied tracks.
type PlanOut = (Vec<Patch>, Vec<(u64, u64)>, Vec<Finding>);

fn plan(path: &Path, opts: &Options) -> Result<PlanOut> {
    let mut f = fs::File::open(path)?;
    let flen = f.metadata()?.len();
    let mut patches = Vec::new();
    let mut found = Vec::new();
    let mut tracks: Vec<Track> = Vec::new();
    let mut mdats: Vec<(u64, u64)> = Vec::new();
    let mut fragmented = false;
    // top-level boxes that are not on the list of what a player needs: (position, header, size, type)
    let mut odd: Vec<(u64, u64, u64, [u8; 4])> = Vec::new();
    let mut pos = 0u64;
    let mut saw_moov = false;
    while pos < flen {
        if flen - pos < 8 {
            return bad("a box header is cut off");
        }
        let mut h = [0u8; 16];
        f.seek(SeekFrom::Start(pos))?;
        let want = (flen - pos).min(16) as usize;
        f.read_exact(&mut h[..want])?;
        let s32 = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        let ty: [u8; 4] = h[4..8].try_into().unwrap();
        let (hdr, size) = match s32 {
            0 => (8, flen - pos),
            1 => {
                if want < 16 {
                    return bad("a box header is cut off");
                }
                (16, u64::from_be_bytes(h[8..16].try_into().unwrap()))
            }
            n => (8, n),
        };
        if size < hdr || pos + size > flen {
            return bad("a box runs past the end of the file, it is cut off");
        }
        if matches!(&ty, b"udta" | b"meta" | b"uuid" | b"free" | b"skip") && size - hdr > MAX_MOOV {
            return Err(ScrubError::Unsupported("a box that may hold tags is bigger than 512 MB".into()));
        }
        match &ty {
            b"moov" => {
                saw_moov = true;
                if size - hdr > MAX_MOOV {
                    return Err(ScrubError::Unsupported("the movie header is bigger than 512 MB".into()));
                }
                let mut body = vec![0u8; (size - hdr) as usize];
                f.seek(SeekFrom::Start(pos + hdr))?;
                f.read_exact(&mut body)?;
                for c in children(&body, pos + hdr)? {
                    match &c.ty {
                        b"mvhd" => zero_times(&c, "Movie", &mut patches, &mut found),
                        b"udta" | b"meta" => rename_free(&c, "Tags, location and device information", &mut patches, &mut found),
                        b"trak" => tracks.push(walk_trak(c.body, c.off + c.hdr, &mut patches, &mut found)?),
                        b"mvex" => fragmented = true,
                        b"uuid" if c.body.len() >= 16 && c.body[..16] == XMP_UUID => rename_free(&c, "XMP", &mut patches, &mut found),
                        b"iods" => {}
                        b"free" | b"skip" => {
                            if c.body.iter().any(|b| *b != 0) {
                                found.push(Finding::new("Leftover data in padding", format!("{} bytes", c.body.len()), true));
                                patches.push((c.off + c.hdr, vec![0u8; c.body.len()]));
                            }
                        }
                        b"cmov" => return Err(ScrubError::Unsupported("the movie header is compressed, leaf cannot read it".into())),
                        other => rename_free(&c, &odd_label(other), &mut patches, &mut found),
                    }
                }
            }
            b"udta" | b"meta" => {
                let mut body = vec![0u8; (size - hdr) as usize];
                f.seek(SeekFrom::Start(pos + hdr))?;
                f.read_exact(&mut body)?;
                rename_free(&Child { off: pos, hdr, ty, body: &body }, "Tags and device information", &mut patches, &mut found);
            }
            b"uuid" => {
                let mut u = [0u8; 16];
                f.seek(SeekFrom::Start(pos + hdr))?;
                if size - hdr >= 16 && f.read_exact(&mut u).is_ok() && u == XMP_UUID {
                    let mut body = vec![0u8; (size - hdr) as usize];
                    f.seek(SeekFrom::Start(pos + hdr))?;
                    f.read_exact(&mut body)?;
                    rename_free(&Child { off: pos, hdr, ty, body: &body }, "XMP", &mut patches, &mut found);
                } else {
                    odd.push((pos, hdr, size, ty));
                }
            }
            b"free" | b"skip" => {
                let mut body = vec![0u8; (size - hdr) as usize];
                f.seek(SeekFrom::Start(pos + hdr))?;
                f.read_exact(&mut body)?;
                if body.iter().any(|b| *b != 0) {
                    found.push(Finding::new("Leftover data in padding", format!("{} bytes", body.len()), true));
                    patches.push((pos + hdr, vec![0u8; body.len()]));
                }
            }
            b"mdat" => mdats.push((pos + hdr, pos + size)),
            b"moof" => fragmented = true,
            b"ftyp" | b"wide" | b"mfra" | b"sidx" | b"ssix" | b"styp" | b"pdin" | b"emsg" => {}
            _ => odd.push((pos, hdr, size, ty)),
        }
        pos += size;
    }
    if !saw_moov {
        return bad("no movie header");
    }
    let mut zero_runs = Vec::new();
    let kept = plan_tracks(&mut f, &tracks, &mdats, fragmented, opts, &mut patches, &mut zero_runs, &mut found)?;
    // a vendor box (camera model, serial, owner), a producer time stamp or any box leaf does not know: renamed to `free` and
    // emptied, unless a picture or sound sample lies in it (then the file is refused)
    for (at, hdr, size, ty) in odd {
        let (a, end) = (at, at + size);
        let i = kept.partition_point(|k| k.1 <= a);
        if kept.get(i).is_some_and(|(s, _)| *s < end) {
            return Err(ScrubError::Unsupported(format!("a box leaf does not know ({}) holds picture or sound data", String::from_utf8_lossy(&ty))));
        }
        found.push(Finding::new(odd_label(&ty), format!("{size} bytes"), true));
        patches.push((at + 4, b"free".to_vec()));
        zero_runs.push((at + hdr, size - hdr));
    }
    Ok((patches, zero_runs, found))
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let (_, _, mut findings) = plan(path, opts)?;
    // the individual tags, when lofty can read them (only for the list: the removal does not depend on it)
    use lofty::file::TaggedFileExt;
    if let Ok(p) = lofty::probe::Probe::open(path) {
        if let Ok(p) = p.guess_file_type() {
            if let Ok(t) = p.read() {
                findings.extend(crate::tags::tag_findings(t.tags(), false));
            }
        }
    }
    Ok(Inspection {
        format: "MP4 / MOV".into(),
        findings,
        notes: vec![
            "Still inside: what the camera or program wrote into the video or audio stream itself (an encoder name in the first frames, for example) is not removed.".into(),
            "Cover art cannot be kept in MP4 and MOV files, it is removed with the tags.".into(),
            "Tracks that are not picture or sound (telemetry and GPS, timecode, text and subtitles unless you keep them, timed metadata) are emptied with zeros and disabled; each one is named above.".into(),
        ],
        can_clean: true,
        refusal: None,
    })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<Vec<String>> {
    let (patches, zero_runs, _) = plan(src, opts)?;
    crate::copy_without_attrs(src, dst)?;
    let mut f = fs::OpenOptions::new().write(true).open(dst)?;
    let zeros = vec![0u8; 1 << 20];
    for (off, len) in &zero_runs {
        f.seek(SeekFrom::Start(*off))?;
        let mut left = *len;
        while left > 0 {
            let k = left.min(zeros.len() as u64) as usize;
            f.write_all(&zeros[..k])?;
            left -= k as u64;
        }
    }
    for (off, bytes) in &patches {
        f.seek(SeekFrom::Start(*off))?;
        f.write_all(bytes)?;
    }
    f.sync_all()?;
    drop(f);
    // the same walk over the result: nothing left to patch, and the file is the same size
    let (left, runs, _) = plan(dst, opts)?;
    if !left.is_empty() || !runs.is_empty() {
        return Err(ScrubError::Verify("tags or times are still in the file".into()));
    }
    if fs::metadata(dst)?.len() != fs::metadata(src)?.len() {
        return Err(ScrubError::Verify("the size changed".into()));
    }
    Ok(vec![])
}
