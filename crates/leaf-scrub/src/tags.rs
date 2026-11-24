//! mp3, FLAC, Ogg / Opus, WAV: the tags (ID3, Vorbis comments, APE, RIFF INFO) and the cover art, removed with lofty's writers,
//! which move the audio data as it is (no re-encoding). The result is read back and the audio properties must be unchanged.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagExt, TagType};
use std::path::Path;

fn read(path: &Path) -> Result<lofty::file::TaggedFile> {
    Probe::open(path)
        .map_err(|e| ScrubError::Malformed(format!("the file could not be read: {e}")))?
        .options(ParseOptions::new().read_cover_art(true))
        .guess_file_type()
        .map_err(ScrubError::Io)?
        .read()
        .map_err(|e| ScrubError::Malformed(format!("the audio could not be read: {e}")))
}

pub(crate) fn tag_findings(tags: &[Tag], keep_cover: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in tags {
        for item in t.items() {
            let value = match item.value() {
                ItemValue::Text(s) | ItemValue::Locator(s) => s.clone(),
                ItemValue::Binary(b) => format!("{} bytes", b.len()),
            };
            out.push(Finding::new(format!("{:?}", item.key()), value, true));
        }
        for p in t.pictures() {
            let mime = p.mime_type().map(|m| m.to_string()).unwrap_or_else(|| "picture".into());
            out.push(Finding::new("Cover art", format!("{mime}, {} bytes", p.data().len()), !keep_cover));
        }
    }
    out
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let f = read(path)?;
    let mut findings = tag_findings(f.tags(), opts.keep_cover_art);
    findings.extend(extra(path, false)?);
    Ok(Inspection {
        format: format!("{:?} audio", f.file_type()).replace(" audio", ""),
        findings,
        notes: vec!["Still inside: the encoder name some programs write into the audio stream itself is not removed.".into()],
        can_clean: true,
        refusal: None,
    })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let before = read(src)?;
    let props = before.properties();
    let sig = (props.duration(), props.sample_rate(), props.channels());
    crate::copy_without_attrs(src, dst)?;
    let wo = WriteOptions::default();
    for t in before.tags() {
        let ty: TagType = t.tag_type();
        if opts.keep_cover_art && t.picture_count() > 0 {
            let mut keep = Tag::new(ty);
            for p in t.pictures() {
                keep.push_picture(p.clone());
            }
            keep.save_to_path(dst, wo).map_err(|e| ScrubError::Verify(format!("a tag could not be rewritten: {e}")))?;
        } else {
            ty.remove_from_path(dst, wo).map_err(|e| ScrubError::Verify(format!("a tag could not be removed: {e}")))?;
        }
    }
    // what the tag library does not handle: WAV production chunks, FLAC application blocks, tags at the end of an mp3
    extra(dst, true)?;
    let after = read(dst)?;
    let p = after.properties();
    if (p.duration(), p.sample_rate(), p.channels()) != sig {
        return Err(ScrubError::Verify("the audio changed while the tags were removed".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------- beyond the tag library

/// WAV chunks a player needs; every other chunk (bext, iXML, _PMX / XMP, cart, axml, LIST, id3, padding ...) goes.
const WAV_KEEP: [&[u8; 4]; 10] = [b"fmt ", b"data", b"fact", b"cue ", b"smpl", b"inst", b"plst", b"ds64", b"wavl", b"slnt"];

fn printable(b: &[u8]) -> String {
    b.iter().take(200).map(|c| if (0x20..0x7F).contains(c) { *c as char } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lists (and with `apply`, removes in place) what the tag library leaves: WAV chunks off the list above, FLAC APPLICATION
/// blocks (overwritten with zeros and turned into padding), ID3v1 / Lyrics3 / APE tags at the end of an mp3.
pub(crate) fn extra(path: &Path, apply: bool) -> Result<Vec<Finding>> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).write(apply).open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 12];
    let got = f.read(&mut head)?;
    let mut found = Vec::new();
    if got == 12 && (&head[0..4] == b"RIFF" || &head[0..4] == b"RF64") && &head[8..12] == b"WAVE" {
        let mut pos = 12u64;
        let mut keep: Vec<(u64, u64)> = Vec::new();
        let mut drop = 0;
        while pos + 8 <= len {
            let mut h = [0u8; 8];
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut h)?;
            let id: [u8; 4] = h[0..4].try_into().unwrap();
            let size = u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64;
            let total = (8 + size + (size & 1)).min(len - pos);
            if WAV_KEEP.iter().any(|k| **k == id) {
                keep.push((pos, total));
            } else {
                drop += 1;
                let mut b = vec![0u8; size.min(512) as usize];
                let _ = f.read(&mut b)?;
                let listed_by_tags = &id == b"id3 " || &id == b"ID3 " || (&id == b"LIST" && b.starts_with(b"INFO"));
                if !listed_by_tags {
                    found.push(Finding::new(format!("Audio chunk ({})", String::from_utf8_lossy(&id).trim()), printable(&b), true));
                }
            }
            pos += total;
        }
        if apply && drop > 0 {
            if &head[0..4] == b"RF64" {
                return Err(ScrubError::Unsupported("the RF64 file has chunks leaf would have to remove, and it cannot rewrite RF64 sizes".into()));
            }
            // a new file of the kept chunks, copied as they are, then in place of the old one
            let tmp = path.with_extension("leafwav");
            let written = (|| -> Result<()> {
                let mut w = std::io::BufWriter::new(crate::access::create_temp(path, &tmp)?);
                let body: u64 = 4 + keep.iter().map(|k| k.1).sum::<u64>();
                w.write_all(b"RIFF")?;
                w.write_all(&u32::try_from(body).map_err(|_| ScrubError::Unsupported("the WAV is too big".into()))?.to_le_bytes())?;
                w.write_all(b"WAVE")?;
                for (at, n) in &keep {
                    f.seek(SeekFrom::Start(*at))?;
                    std::io::copy(&mut (&mut f).take(*n), &mut w)?;
                }
                w.flush()?;
                w.get_ref().sync_all()?;
                Ok(())
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            drop_and_replace(f, &tmp, path)?;
        }
        return Ok(found);
    }
    if got >= 4 && &head[0..4] == b"fLaC" {
        let mut pos = 4u64;
        loop {
            let mut h = [0u8; 4];
            f.seek(SeekFrom::Start(pos))?;
            if f.read(&mut h)? < 4 {
                break;
            }
            let (last, ty) = (h[0] & 0x80 != 0, h[0] & 0x7F);
            let size = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
            if ty == 2 {
                let mut b = vec![0u8; size.min(200) as usize];
                let _ = f.read(&mut b)?;
                found.push(Finding::new(format!("FLAC application block ({})", printable(&b[..b.len().min(4)])), printable(&b[b.len().min(4)..]), true));
                if apply {
                    f.seek(SeekFrom::Start(pos))?;
                    f.write_all(&[(h[0] & 0x80) | 1])?;
                    f.seek(SeekFrom::Start(pos + 4))?;
                    let zeros = vec![0u8; size.min(1 << 20) as usize];
                    let mut left = size;
                    while left > 0 {
                        let k = left.min(zeros.len() as u64) as usize;
                        f.write_all(&zeros[..k])?;
                        left -= k as u64;
                    }
                }
            }
            pos += 4 + size;
            if last || pos >= len {
                break;
            }
        }
        if apply {
            f.sync_all()?;
        }
        return Ok(found);
    }
    let is_mp3 = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) || head.starts_with(b"ID3") || (head[0] == 0xFF && head[1] & 0xE0 == 0xE0);
    if !is_mp3 {
        return Ok(found);
    }
    // tags at the end, newest last: ID3v1, Lyrics3, APE, in any order
    let mut end = len;
    for _ in 0..6 {
        let tail_len = end.min(64 * 1024);
        let mut t = vec![0u8; tail_len as usize];
        f.seek(SeekFrom::Start(end - tail_len))?;
        f.read_exact(&mut t)?;
        let n = t.len();
        if n >= 128 && &t[n - 128..n - 125] == b"TAG" {
            found.push(Finding::new("ID3v1 tag at the end", printable(&t[n - 125..n - 30]), true));
            end -= 128;
        } else if n >= 15 && &t[n - 9..] == b"LYRICS200" {
            let size: u64 = std::str::from_utf8(&t[n - 15..n - 9]).ok().and_then(|x| x.parse().ok()).unwrap_or(0);
            if size + 15 > end {
                break;
            }
            found.push(Finding::new("Lyrics3 tag", "", true));
            end -= size + 15;
        } else if n >= 9 && &t[n - 9..] == b"LYRICSEND" {
            match t.windows(11).rposition(|w| w == b"LYRICSBEGIN") {
                Some(i) => {
                    found.push(Finding::new("Lyrics3 tag", "", true));
                    end -= (n - i) as u64;
                }
                None => break,
            }
        } else if n >= 32 && &t[n - 32..n - 24] == b"APETAGEX" {
            let size = u32::from_le_bytes(t[n - 20..n - 16].try_into().unwrap()) as u64;
            let flags = u32::from_le_bytes(t[n - 12..n - 8].try_into().unwrap());
            let total = size + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
            if total > end {
                break;
            }
            found.push(Finding::new("APE tag", format!("{total} bytes"), true));
            end -= total;
        } else {
            break;
        }
    }
    if apply && end < len {
        f.set_len(end)?;
        f.sync_all()?;
    }
    Ok(found)
}

fn drop_and_replace(f: std::fs::File, tmp: &Path, path: &Path) -> Result<()> {
    drop(f);
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e.into());
    }
    Ok(())
}
