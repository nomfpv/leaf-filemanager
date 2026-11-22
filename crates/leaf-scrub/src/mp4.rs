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

fn rename_free(c: &Child<'_>, what: &str, patches: &mut Vec<Patch>, found: &mut Vec<Finding>) {
    found.push(Finding::new(what, format!("{} bytes", c.body.len() + c.hdr as usize), true));
    patches.push((c.off + 4, b"free".to_vec()));
}

fn walk_trak(body: &[u8], base: u64, patches: &mut Vec<Patch>, found: &mut Vec<Finding>) -> Result<()> {
    for c in children(body, base)? {
        match &c.ty {
            b"tkhd" => zero_times(&c, "Track", patches, found),
            b"udta" | b"meta" => rename_free(&c, "Track tags and device information", patches, found),
            b"mdia" => {
                for m in children(c.body, c.off + c.hdr)? {
                    match &m.ty {
                        b"mdhd" => zero_times(&m, "Media", patches, found),
                        b"hdlr" if m.body.len() > 24 => {
                            let name = &m.body[24..];
                            if name.iter().any(|b| *b != 0) {
                                let txt: String = name.iter().filter(|b| **b >= 0x20 && **b < 0x7F).map(|b| *b as char).collect();
                                found.push(Finding::new("Handler / encoder name", txt, true));
                                patches.push((m.off + m.hdr + 24, vec![0u8; name.len()]));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn plan(path: &Path) -> Result<(Vec<Patch>, Vec<Finding>)> {
    let mut f = fs::File::open(path)?;
    let flen = f.metadata()?.len();
    let mut patches = Vec::new();
    let mut found = Vec::new();
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
                        b"trak" => walk_trak(c.body, c.off + c.hdr, &mut patches, &mut found)?,
                        b"uuid" if c.body.len() >= 16 && c.body[..16] == XMP_UUID => rename_free(&c, "XMP", &mut patches, &mut found),
                        _ => {}
                    }
                }
            }
            b"udta" | b"meta" => rename_free(
                &Child { off: pos, hdr, ty, body: &[] },
                "Tags and device information",
                &mut patches,
                &mut found,
            ),
            b"uuid" => {
                let mut u = [0u8; 16];
                f.seek(SeekFrom::Start(pos + hdr))?;
                if size - hdr >= 16 && f.read_exact(&mut u).is_ok() && u == XMP_UUID {
                    patches.push((pos + 4, b"free".to_vec()));
                    found.push(Finding::new("XMP", format!("{size} bytes"), true));
                }
            }
            _ => {}
        }
        pos += size;
    }
    if !saw_moov {
        return bad("no movie header");
    }
    Ok((patches, found))
}

pub(crate) fn inspect(path: &Path) -> Result<Inspection> {
    let (_, mut findings) = plan(path)?;
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
        ],
        can_clean: true,
        refusal: None,
    })
}

pub(crate) fn produce(src: &Path, dst: &Path, _opts: &Options) -> Result<Vec<String>> {
    let (patches, _) = plan(src)?;
    crate::copy_without_attrs(src, dst)?;
    let mut f = fs::OpenOptions::new().write(true).open(dst)?;
    for (off, bytes) in &patches {
        f.seek(SeekFrom::Start(*off))?;
        f.write_all(bytes)?;
    }
    f.sync_all()?;
    drop(f);
    // the same walk over the result: nothing left to patch, and the file is the same size
    let (left, _) = plan(dst)?;
    if !left.is_empty() {
        return Err(ScrubError::Verify("tags or times are still in the file".into()));
    }
    if fs::metadata(dst)?.len() != fs::metadata(src)?.len() {
        return Err(ScrubError::Verify("the size changed".into()));
    }
    Ok(vec![])
}
