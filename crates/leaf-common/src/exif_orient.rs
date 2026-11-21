//! Turning a JPEG by 90 degrees without touching its picture data: only the EXIF orientation tag changes (every viewer honours it).
//! Pure bytes in, bytes out, so every backend can share it. A file with an Exif block that has an orientation tag gets that tag
//! patched in place; a file with no Exif block gets a minimal one; an Exif block with no orientation tag is refused (adding an entry
//! would mean rewriting the whole block).

use anyhow::{Result, bail};

/// Next EXIF orientation after a 90 degree turn (mirrored variants included).
pub fn next_orientation(cur: u16, clockwise: bool) -> u16 {
    // 1 -> 6 -> 3 -> 8 -> 1 (plain); 2 -> 7 -> 4 -> 5 -> 2 (mirrored)
    const CW: [(u16, u16); 8] = [(1, 6), (6, 3), (3, 8), (8, 1), (2, 7), (7, 4), (4, 5), (5, 2)];
    let cur = if (1..=8).contains(&cur) { cur } else { 1 };
    if clockwise {
        CW.iter().find(|(a, _)| *a == cur).map_or(6, |(_, b)| *b)
    } else {
        CW.iter().find(|(_, b)| *b == cur).map_or(8, |(a, _)| *a)
    }
}

/// The JPEG's bytes with its orientation turned 90 degrees.
pub fn rotate_jpeg(src: &[u8], clockwise: bool) -> Result<Vec<u8>> {
    if src.len() < 4 || src[0] != 0xFF || src[1] != 0xD8 {
        bail!("This is not a JPEG picture");
    }
    let mut pos = 2;
    let mut first_end = 2; // where a new APP1 goes: after SOI and a leading JFIF APP0
    let mut first = true;
    while pos + 4 <= src.len() && src[pos] == 0xFF {
        let marker = src[pos + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        let len = u16::from_be_bytes([src[pos + 2], src[pos + 3]]) as usize;
        if len < 2 || pos + 2 + len > src.len() {
            bail!("The JPEG's header is damaged");
        }
        let payload = pos + 4..pos + 2 + len;
        if marker == 0xE1 && src[payload.clone()].starts_with(b"Exif\0\0") {
            let mut out = src.to_vec();
            patch_exif(&mut out[payload.start + 6..payload.end], clockwise)?;
            return Ok(out);
        }
        if first && marker == 0xE0 {
            first_end = pos + 2 + len;
        }
        first = false;
        pos += 2 + len;
    }
    // no Exif block: a minimal one with the orientation tag
    let o = next_orientation(1, clockwise);
    let mut seg = vec![0xFF, 0xE1, 0, 34];
    seg.extend_from_slice(b"Exif\0\0MM\0\x2A\0\0\0\x08");
    seg.extend_from_slice(&[0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, (o >> 8) as u8, o as u8, 0, 0, 0, 0, 0, 0]);
    let mut out = Vec::with_capacity(src.len() + seg.len());
    out.extend_from_slice(&src[..first_end]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&src[first_end..]);
    Ok(out)
}

/// Patches the orientation entry of IFD0 inside a TIFF block.
fn patch_exif(tiff: &mut [u8], clockwise: bool) -> Result<()> {
    let le = match tiff.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => bail!("The picture's Exif block is damaged"),
    };
    let u16_at = |b: &[u8], o: usize| -> Option<u16> {
        let v = [*b.get(o)?, *b.get(o + 1)?];
        Some(if le { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) })
    };
    let u32_at = |b: &[u8], o: usize| -> Option<u32> {
        let v = [*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?];
        Some(if le { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) })
    };
    let damaged = || anyhow::anyhow!("The picture's Exif block is damaged");
    let ifd = u32_at(tiff, 4).ok_or_else(damaged)? as usize;
    let n = u16_at(tiff, ifd).ok_or_else(damaged)? as usize;
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        if u16_at(tiff, e).ok_or_else(damaged)? == 0x0112 {
            if u16_at(tiff, e + 2) != Some(3) {
                bail!("The picture's orientation tag has an unexpected form");
            }
            let cur = u16_at(tiff, e + 8).ok_or_else(damaged)?;
            let next = next_orientation(cur, clockwise);
            let bytes = if le { next.to_le_bytes() } else { next.to_be_bytes() };
            tiff[e + 8..e + 10].copy_from_slice(&bytes);
            return Ok(());
        }
    }
    bail!("This picture has no orientation tag to turn")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jpeg_without_exif_gets_one_and_turns_again_in_place() {
        // SOI, a tiny stand-in for the rest (an SOS marker starts the data), EOI
        let jpg = [0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 1, 2, 3, 0xFF, 0xD9];
        let a = rotate_jpeg(&jpg, true).unwrap();
        assert_eq!(a.len(), jpg.len() + 36);
        assert_eq!(&a[a.len() - 7..], &jpg[4..]);
        let b = rotate_jpeg(&a, true).unwrap();
        assert_eq!(b.len(), a.len());
        // orientation 1 -> 6 -> 3
        assert_eq!(a[2 + 4 + 6 + 8 + 2 + 8 + 1], 6);
        assert_eq!(b[2 + 4 + 6 + 8 + 2 + 8 + 1], 3);
        assert!(rotate_jpeg(b"GIF89a", true).is_err());
        assert_eq!(next_orientation(6, false), 1);
    }
}
