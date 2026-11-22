//! Finding the movie on a disc: where its `BDMV` folder is (a Blu-ray, an AVCHD camcorder card, a folder copied from either), the
//! titles its playlists make (duplicates and loops left out, the longest first), their tracks and chapters, and whether the streams
//! can be read at all (an AACS-encrypted disc cannot; it is refused, never decrypted).

use std::path::{Path, PathBuf};

use crate::mpls::{self, StreamKind};
use crate::ts;

/// What the converter was pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A `BDMV` folder (its `PLAYLIST`, `CLIPINF`, `STREAM`).
    Disc(PathBuf),
    /// Loose `.m2ts` / `.mts` files, joined in this order.
    Files(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Nothing here a Blu-ray or AVCHD converter can read.
    NotADisc,
    /// The streams are encrypted (or do not parse): leaf converts unprotected discs only.
    Protected,
    Io(String),
}

fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.exists() {
        return Some(direct);
    }
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name)))
}

fn is_bdmv(dir: &Path) -> bool {
    child_ci(dir, "PLAYLIST").is_some_and(|p| p.is_dir()) && child_ci(dir, "STREAM").is_some_and(|p| p.is_dir())
}

/// The `BDMV` folder of a disc root, a `BDMV` folder itself, an AVCHD card (`PRIVATE/AVCHD/BDMV`) or an `AVCHD` folder.
pub fn find_bdmv(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    if dir.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("BDMV")) && is_bdmv(dir) {
        return Some(dir.to_path_buf());
    }
    let candidates = [vec!["BDMV"], vec!["PRIVATE", "AVCHD", "BDMV"], vec!["AVCHD", "BDMV"]];
    for c in candidates {
        let mut p = dir.to_path_buf();
        let mut ok = true;
        for part in c {
            match child_ci(&p, part) {
                Some(n) => p = n,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && is_bdmv(&p) {
            return Some(p);
        }
    }
    None
}

pub fn is_stream_file(p: &Path) -> bool {
    p.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("m2ts") || e.eq_ignore_ascii_case("mts")
    })
}

/// A disc image the system can mount (`.iso`, `.img`).
pub fn is_image(p: &Path) -> bool {
    p.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("iso") || e.eq_ignore_ascii_case("img")
    })
}

/// What a selection is, without reading any stream (cheap enough for a menu).
pub fn locate(paths: &[PathBuf]) -> Option<Source> {
    if let [one] = paths
        && one.is_dir()
    {
        return find_bdmv(one).map(Source::Disc);
    }
    if !paths.is_empty() && paths.iter().all(|p| is_stream_file(p) && p.is_file()) {
        let mut v = paths.to_vec();
        v.sort_by(|a, b| natural(&a.to_string_lossy(), &b.to_string_lossy()));
        return Some(Source::Files(v));
    }
    None
}

fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut num: Option<u64> = None;
        for c in s.chars() {
            if let Some(d) = c.to_digit(10) {
                num = Some(num.unwrap_or(0).saturating_mul(10).saturating_add(d as u64));
            } else {
                if let Some(n) = num.take() {
                    out.push((std::mem::take(&mut text), n));
                }
                text.extend(c.to_lowercase());
            }
        }
        out.push((text, num.unwrap_or(0)));
        out
    };
    key(a).cmp(&key(b))
}

/// One clip of a title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    pub path: PathBuf,
    /// In and out time on the clip's clock, 90 kHz; `None` for a loose file (all of it).
    pub span: Option<(u64, u64)>,
    pub size: u64,
}

/// A track of a title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub pid: u16,
    pub kind: StreamKind,
    pub stream_type: u8,
    pub lang: String,
    /// The stream table's format and rate codes (0 when there is no playlist).
    pub format: u8,
    pub rate: u8,
}

impl Track {
    /// leaf remuxes it itself (else only ffmpeg can).
    pub fn remuxable(&self) -> bool {
        crate::codec::Codec::of(self.stream_type).is_some()
    }

    /// "English · Dolby TrueHD".
    pub fn label(&self) -> String {
        let codec = crate::codec::type_name(self.stream_type);
        if self.lang.is_empty() { codec.to_string() } else { format!("{} · {codec}", language_name(&self.lang)) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title {
    /// The playlist's file name (`00800.mpls`), or the first file's name.
    pub name: String,
    pub clips: Vec<Clip>,
    /// 0 when unknown (loose files: known after the conversion).
    pub duration_ns: u64,
    pub chapters_ns: Vec<u64>,
    pub tracks: Vec<Track>,
    /// The clips' files together (an upper bound of what the remux writes).
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Disc {
    /// The disc's name (its folder or volume), for the output's name.
    pub name: String,
    /// Longest first; the first is the main title.
    pub titles: Vec<Title>,
}

impl Disc {
    /// The titles almost as long as the main one (a director's cut beside the cinema version): the user chooses when there are
    /// several.
    pub fn long_titles(&self) -> Vec<&Title> {
        let Some(main) = self.titles.first() else { return Vec::new() };
        self.titles.iter().filter(|t| t.duration_ns * 10 >= main.duration_ns * 7).take(6).collect()
    }
}

const PROBE: usize = 4 << 20;

fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, OpenError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| OpenError::Io(e.to_string()))?;
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) => return Err(OpenError::Io(e.to_string())),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// The streams of a clip's PMT, from its first megabytes. `Protected` when they cannot be read.
pub fn probe(path: &Path) -> Result<Vec<ts::PmtStream>, OpenError> {
    let head = read_head(path, PROBE)?;
    if ts::looks_encrypted(&head) {
        return Err(OpenError::Protected);
    }
    let Some(size) = ts::packet_size(&head) else { return Err(OpenError::Protected) };
    let skip = size - ts::TS;
    let mut dmx = ts::Demux::new(Some(Vec::new()));
    let mut sink = Vec::new();
    for p in head.chunks_exact(size) {
        dmx.feed(&p[skip..], &mut sink);
        if !dmx.streams().is_empty() {
            break;
        }
    }
    Ok(dmx.streams().to_vec())
}

fn kind_of(stream_type: u8) -> Option<StreamKind> {
    Some(match stream_type {
        0x01 | 0x02 | 0x1B | 0x24 | 0xEA => StreamKind::Video,
        0x03 | 0x04 | 0x0F | 0x11 | 0x80..=0x87 | 0xA1 | 0xA2 => StreamKind::Audio,
        0x90 | 0x92 => StreamKind::Subtitle,
        _ => return None,
    })
}

fn find_clip(stream_dir: &Path, clip: &str) -> Option<PathBuf> {
    ["m2ts", "M2TS", "mts", "MTS"].iter().map(|e| stream_dir.join(format!("{clip}.{e}"))).find(|p| p.is_file())
}

/// Reads the titles of a source (a few megabytes of the first clip of each title are read for its tracks).
pub fn open(src: &Source) -> Result<Disc, OpenError> {
    match src {
        Source::Files(files) => {
            let tracks = tracks_from_pmt(&probe(&files[0])?);
            let clips: Vec<Clip> = files.iter().map(|p| Clip { path: p.clone(), span: None, size: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) }).collect();
            let name = files[0].file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let size = clips.iter().map(|c| c.size).sum();
            Ok(Disc { name: name.clone(), titles: vec![Title { name, clips, duration_ns: 0, chapters_ns: vec![0], tracks, size }] })
        }
        Source::Disc(bdmv) => open_bdmv(bdmv),
    }
}

fn tracks_from_pmt(pmt: &[ts::PmtStream]) -> Vec<Track> {
    pmt.iter()
        .filter_map(|s| Some(Track { pid: s.pid, kind: kind_of(s.stream_type)?, stream_type: s.stream_type, lang: s.lang.clone(), format: 0, rate: 0 }))
        .collect()
}

fn disc_name(bdmv: &Path) -> String {
    // the folder above BDMV (a disc's root has no name: its volume label is not read here, the caller may know it)
    let mut p = bdmv.parent();
    while let Some(d) = p {
        if let Some(n) = d.file_name() {
            let n = n.to_string_lossy();
            if !n.eq_ignore_ascii_case("AVCHD") && !n.eq_ignore_ascii_case("PRIVATE") {
                return n.into_owned();
            }
        } else {
            break;
        }
        p = d.parent();
    }
    "Blu-ray".to_string()
}

fn open_bdmv(bdmv: &Path) -> Result<Disc, OpenError> {
    let list_dir = child_ci(bdmv, "PLAYLIST").ok_or(OpenError::NotADisc)?;
    let stream_dir = child_ci(bdmv, "STREAM").ok_or(OpenError::NotADisc)?;
    let mut names: Vec<PathBuf> = std::fs::read_dir(&list_dir)
        .map_err(|e| OpenError::Io(e.to_string()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mpls") || e.eq_ignore_ascii_case("mpl")))
        .collect();
    names.sort();
    let mut seen: Vec<Vec<(String, u32, u32)>> = Vec::new();
    let mut titles = Vec::new();
    let mut probed: Vec<(PathBuf, Vec<ts::PmtStream>)> = Vec::new();
    for path in names {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(pl) = mpls::parse(&bytes) else { continue };
        if pl.items.is_empty() || pl.repeats_a_clip() {
            continue;
        }
        let key: Vec<(String, u32, u32)> = pl.items.iter().map(|i| (i.clip.clone(), i.in_time, i.out_time)).collect();
        if seen.contains(&key) {
            continue;
        }
        let Some(clips) = pl
            .items
            .iter()
            .map(|i| {
                let p = find_clip(&stream_dir, &i.clip)?;
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                Some(Clip { path: p, span: Some((i.in_time as u64 * 2, i.out_time as u64 * 2)), size })
            })
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        seen.push(key);
        let duration_ns = pl.duration_45k() * 1_000_000_000 / 45_000;
        titles.push((path, pl, clips, duration_ns));
    }
    if titles.is_empty() {
        return Err(OpenError::NotADisc);
    }
    titles.sort_by(|a, b| b.3.cmp(&a.3));
    // short ones (menus, logos, trailers) only when there is nothing else
    let longest = titles[0].3;
    titles.retain(|t| t.3 >= 60_000_000_000 || longest < 60_000_000_000);
    titles.truncate(30);
    let mut out = Vec::new();
    for (path, pl, clips, duration_ns) in titles {
        let first = &clips[0].path;
        let pmt = match probed.iter().find(|(p, _)| p == first) {
            Some((_, s)) => s.clone(),
            None => {
                let s = probe(first)?;
                probed.push((first.clone(), s.clone()));
                s
            }
        };
        let tracks: Vec<Track> = if pl.streams.is_empty() {
            tracks_from_pmt(&pmt)
        } else {
            pl.streams
                .iter()
                .filter(|s| pmt.is_empty() || pmt.iter().any(|p| p.pid == s.pid))
                .map(|s| Track { pid: s.pid, kind: s.kind, stream_type: s.coding, lang: s.lang.clone(), format: s.format, rate: s.rate })
                .collect()
        };
        let size = clips.iter().map(|c| c.size).sum();
        out.push(Title {
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            clips,
            duration_ns,
            chapters_ns: pl.chapters_ns(),
            tracks,
            size,
        });
    }
    Ok(Disc { name: disc_name(bdmv), titles: out })
}

/// The English name of an ISO 639-2 code, or the code in capitals.
pub fn language_name(code: &str) -> String {
    let n = match code {
        "eng" => "English",
        "deu" | "ger" => "German",
        "fra" | "fre" => "French",
        "spa" => "Spanish",
        "ita" => "Italian",
        "jpn" => "Japanese",
        "nld" | "dut" => "Dutch",
        "por" => "Portuguese",
        "rus" => "Russian",
        "tur" => "Turkish",
        "zho" | "chi" => "Chinese",
        "kor" => "Korean",
        "pol" => "Polish",
        "swe" => "Swedish",
        "dan" => "Danish",
        "fin" => "Finnish",
        "nor" => "Norwegian",
        "ces" | "cze" => "Czech",
        "hun" => "Hungarian",
        "ell" | "gre" => "Greek",
        "ara" => "Arabic",
        "heb" => "Hebrew",
        "hin" => "Hindi",
        "tha" => "Thai",
        "ukr" => "Ukrainian",
        "und" | "" => "Unknown",
        other => return other.to_uppercase(),
    };
    n.to_string()
}
