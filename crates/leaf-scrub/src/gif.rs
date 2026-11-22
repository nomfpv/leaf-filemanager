//! GIF: comment extensions, XMP and every application extension except the animation loop, and anything after the trailer are
//! left out. The frames (image descriptors, colour tables, LZW data) and the timing extensions are copied byte for byte.

use crate::{Finding, Inspection, Options, Result, ScrubError};
use std::fs;
use std::path::Path;

fn bad<T>(w: &str) -> Result<T> {
    Err(ScrubError::Malformed(w.into()))
}

/// The position after the sub-blocks that start at `pos` (up to and including the zero block), and their joined data.
fn sub_blocks(d: &[u8], mut pos: usize) -> Result<(usize, Vec<u8>)> {
    let mut data = Vec::new();
    loop {
        let s = *d.get(pos).ok_or_else(|| ScrubError::Malformed("the GIF is cut off".into()))? as usize;
        pos += 1;
        if s == 0 {
            return Ok((pos, data));
        }
        data.extend_from_slice(d.get(pos..pos + s).ok_or_else(|| ScrubError::Malformed("the GIF is cut off".into()))?);
        pos += s;
    }
}

fn process(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>, Vec<String>)> {
    if d.len() < 14 || !(d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a")) {
        return bad("no GIF header");
    }
    let mut found = Vec::new();
    let mut notes = Vec::new();
    let mut out = Vec::with_capacity(d.len());
    let mut pos = 13usize;
    if d[10] & 0x80 != 0 {
        pos += 3 << ((d[10] & 7) + 1);
    }
    if pos > d.len() {
        return bad("the GIF is cut off");
    }
    out.extend_from_slice(&d[..pos]);
    let mut frames = 0;
    let mut ended = false;
    while pos < d.len() {
        match d[pos] {
            0x3B => {
                out.push(0x3B);
                pos += 1;
                ended = true;
                break;
            }
            0x2C => {
                let start = pos;
                let packed = *d.get(pos + 9).ok_or_else(|| ScrubError::Malformed("the GIF is cut off".into()))?;
                pos += 10;
                if packed & 0x80 != 0 {
                    pos += 3 << ((packed & 7) + 1);
                }
                pos += 1; // LZW minimum code size
                if pos > d.len() {
                    return bad("the GIF is cut off");
                }
                pos = sub_blocks(d, pos)?.0;
                out.extend_from_slice(&d[start..pos]);
                frames += 1;
            }
            0x21 => {
                let start = pos;
                let label = *d.get(pos + 1).ok_or_else(|| ScrubError::Malformed("the GIF is cut off".into()))?;
                let (end, data) = sub_blocks(d, pos + 2)?;
                let keep = match label {
                    0xF9 => true,
                    0x01 => {
                        notes.push("The picture has a text overlay (plain text extension): it is part of what the picture shows, so it stays.".into());
                        true
                    }
                    0xFE => {
                        found.push(Finding::new("Comment", String::from_utf8_lossy(&data), true));
                        false
                    }
                    0xFF => {
                        let id = &data[..data.len().min(11)];
                        if id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0" {
                            true
                        } else if id == b"ICCRGBG1012" {
                            found.push(Finding::new("Colour profile", "", !opts.keep_color_profile));
                            opts.keep_color_profile
                        } else if id == b"XMP DataXMP" {
                            found.push(Finding::new("XMP", format!("{} bytes", data.len()), true));
                            false
                        } else {
                            found.push(Finding::new("Application extension", String::from_utf8_lossy(id), true));
                            false
                        }
                    }
                    other => {
                        found.push(Finding::new(format!("Extension (0x{other:02X})"), format!("{} bytes", data.len()), true));
                        false
                    }
                };
                // the graphic control byte pair 0x21 0xF9 sits before a frame: kept as is
                if keep {
                    out.extend_from_slice(&d[start..end]);
                }
                pos = end;
            }
            _ => return bad("unexpected data between the GIF blocks"),
        }
    }
    if !ended || frames == 0 {
        return bad("the GIF has no end marker or no picture");
    }
    if pos < d.len() {
        found.push(Finding::new("Data after the end of the picture", format!("{} bytes", d.len() - pos), true));
    }
    Ok((out, found, notes))
}

/// A GIF held in memory, cleaned (for pictures inside Office files).
pub(crate) fn clean_bytes(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    let (out, found, _) = process(d, opts)?;
    Ok((out, found))
}

pub(crate) fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let d = crate::image::read_all(path)?;
    let (_, findings, mut notes) = process(&d, opts)?;
    notes.push("The picture frames are copied byte for byte.".into());
    Ok(Inspection { format: "GIF".into(), findings, notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let d = crate::image::read_all(src)?;
    let (out, _, _) = process(&d, opts)?;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, &out)?;
    f.sync_all()?;
    Ok(())
}
