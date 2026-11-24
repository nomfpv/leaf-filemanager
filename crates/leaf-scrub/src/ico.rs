//! ICO / CUR: an icon file is a directory of pictures. A picture is either a bitmap (no metadata fields) or a whole PNG file, and
//! a PNG can carry text, Exif, XMP and time chunks like any other. The PNG pictures go through the PNG remover, and the file is
//! written again: the same directory with new offsets and sizes, then the pictures one after the other in the directory's order.
//! Bytes that no directory entry points at are not copied. A bitmap picture with a colour profile or bytes after its end, and
//! pictures that overlap, refuse the file.

use crate::{Family, Finding, Inspection, Options, Result, ScrubError};
use std::fs;
use std::path::Path;

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn le16(d: &[u8], p: usize) -> usize {
    u16::from_le_bytes([d[p], d[p + 1]]) as usize
}

fn le32(d: &[u8], p: usize) -> usize {
    u32::from_le_bytes([d[p], d[p + 1], d[p + 2], d[p + 3]]) as usize
}

/// The pictures of the directory: (start, length) in the file. Anything that does not add up refuses the file.
pub(crate) fn entries(d: &[u8]) -> Result<Vec<(usize, usize)>> {
    let bad = |w: &str| ScrubError::Malformed(w.into());
    if d.len() < 6 || le16(d, 0) != 0 || !matches!(le16(d, 2), 1 | 2) {
        return Err(bad("the icon directory is damaged"));
    }
    let n = le16(d, 4);
    let dir_end = 6 + 16 * n;
    if n == 0 || dir_end > d.len() {
        return Err(bad("the icon directory is empty or cut off"));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = 6 + 16 * i;
        let (len, at) = (le32(d, e + 8), le32(d, e + 12));
        if len == 0 || at < dir_end || at.checked_add(len).is_none_or(|end| end > d.len()) {
            return Err(bad("a picture of the icon lies outside the file"));
        }
        out.push((at, len));
    }
    // each picture once: entries that share or overlap bytes would be written out again and again (65,535 entries on one big
    // range is gigabytes)
    let mut spans = out.clone();
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].0 + w[0].1 > w[1].0) || spans.iter().map(|s| s.1).sum::<usize>() > d.len() {
        return Err(ScrubError::Unsupported("the pictures of the icon overlap each other, leaf does not clean such a file".into()));
    }
    Ok(out)
}

/// A bitmap picture of an icon (a header, the colours, the picture, the transparency mask). Refused when it carries what a
/// bitmap can carry besides the picture: an embedded or linked colour profile (a linked one is a file path), or bytes after the
/// mask.
fn check_bitmap(pic: &[u8]) -> Result<()> {
    let refuse = |w: &str| Err(ScrubError::Unsupported(w.into()));
    if pic.len() < 40 {
        return refuse("a picture of the icon is cut off");
    }
    let header = le32(pic, 0);
    if !matches!(header, 40 | 108 | 124) {
        return refuse("a picture of the icon is of a kind leaf does not know");
    }
    let width = u32::from_le_bytes(pic[4..8].try_into().unwrap()) as i32;
    // the height counts the picture and the mask
    let height = (u32::from_le_bytes(pic[8..12].try_into().unwrap()) as i32).unsigned_abs() as usize / 2;
    let bits = u16::from_le_bytes([pic[14], pic[15]]) as usize;
    let compression = le32(pic, 16);
    let colours = le32(pic, 32);
    if width <= 0 || width > 1 << 16 || height > 1 << 16 || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) || !matches!(compression, 0 | 3) {
        return refuse("a picture of the icon is of a kind leaf does not know");
    }
    // LCS_PROFILE_EMBEDDED ('MBED') and LCS_PROFILE_LINKED ('LINK')
    if header == 124 && matches!(le32(pic, 56), 0x4D42_4544 | 0x4C49_4E4B) {
        return refuse("a picture of the icon carries a colour profile, leaf does not clean such an icon");
    }
    let width = width as usize;
    let masks = if header == 40 && compression == 3 { 12 } else { 0 };
    let palette = if bits <= 8 { if colours == 0 { 1 << bits } else { colours } } else { colours } * 4;
    let picture = (width * bits).div_ceil(32) * 4 * height;
    let mask = width.div_ceil(32) * 4 * height;
    if pic.len() > header + masks + palette + picture + mask {
        return refuse("a picture of the icon has data after its end, leaf does not clean such an icon");
    }
    Ok(())
}

/// The cleaned file and what was found.
fn plan(d: &[u8], opts: &Options) -> Result<(Vec<u8>, Vec<Finding>)> {
    let list = entries(d)?;
    let dir_end = 6 + 16 * list.len();
    let mut out = d[..dir_end].to_vec();
    let mut found = Vec::new();
    for (i, (at, len)) in list.iter().enumerate() {
        let pic = &d[*at..*at + *len];
        let data = if pic.starts_with(PNG_SIG) {
            let (clean, f) = crate::image::process(pic, Family::Png, opts)?;
            let named = |l: &str| if list.len() == 1 { l.to_string() } else { format!("Picture {}: {l}", i + 1) };
            found.extend(f.into_iter().map(|x| Finding { label: named(&x.label), ..x }));
            clean
        } else {
            check_bitmap(pic)?;
            pic.to_vec()
        };
        let e = 6 + 16 * i;
        let (len, at) = (u32::try_from(data.len()), u32::try_from(out.len()));
        let (Ok(len), Ok(at)) = (len, at) else { return Err(ScrubError::Unsupported("the icon is too big".into())) };
        out[e + 8..e + 12].copy_from_slice(&len.to_le_bytes());
        out[e + 12..e + 16].copy_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&data);
    }
    // bytes between or after the pictures: nothing a viewer reads, but they can hold anything
    let mut spans: Vec<(usize, usize)> = list.iter().map(|(a, n)| (*a, a + n)).collect();
    spans.sort_unstable();
    let (mut pos, mut loose) = (dir_end, 0usize);
    for (s, e) in spans.into_iter().chain([(d.len(), d.len())]) {
        if s > pos && d[pos..s].iter().any(|c| *c != 0) {
            loose += s - pos;
        }
        pos = pos.max(e);
    }
    if loose > 0 {
        found.push(Finding::new("Data outside the icon's pictures", format!("{loose} bytes"), true));
    }
    Ok((out, found))
}

pub(crate) fn inspect(path: &Path, name: &'static str, opts: &Options) -> Result<Inspection> {
    let d = crate::image::read_all(path)?;
    let (_, findings) = plan(&d, opts)?;
    let notes = if findings.is_empty() { vec![format!("Nothing found: the pictures of this {name} file carry no metadata.")] } else { Vec::new() };
    Ok(Inspection { format: name.into(), findings, notes, can_clean: true, refusal: None })
}

pub(crate) fn produce(src: &Path, dst: &Path, opts: &Options) -> Result<()> {
    let d = crate::image::read_all(src)?;
    let (out, _) = plan(&d, opts)?;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    std::io::Write::write_all(&mut f, &out)?;
    f.sync_all()?;
    Ok(())
}
