//! Remove metadata: a cleaned COPY of a file, plus the list of what was found and what was removed.
//!
//! No UI here. The rules this crate keeps because it rewrites other people's files:
//! - the original is never opened for writing; the result is written to a temporary file next to the
//!   destination, READ BACK with the same reader that lists metadata, and only moved into place when
//!   nothing on the removal list is left in it. Anything else counts as a failure and the temporary file is deleted;
//! - a file that is cut off, malformed, encrypted or of a kind leaf has no safe writer for is REFUSED
//!   ([`ScrubError`]), never half-cleaned;
//! - what the removal cannot reach is said so ([`Inspection::notes`]), it is never left unmentioned.

mod gif;
mod heif;
mod image;
mod mp4;
mod office;
mod pdf;
mod streams;
mod svg;
#[cfg(test)]
mod tests;
mod tags;
mod tiff;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use streams::{list_streams, remove_streams};

/// The one line the result sheet always shows (the brief's fixed wording).
pub const NOT_REMOVED_LINE: &str = "Not removed: anything visible in the content itself, such as faces, text in the picture, watermarks or hidden marks from a printer or camera sensor";

/// What the user chose.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Keep the embedded colour profile of a picture (the picture looks the same). Default off in `Options::default()`
    /// means the profile is removed; the app's default is to keep it, so it passes `true` unless the user ticks it off.
    pub keep_color_profile: bool,
    /// Keep the cover art of an audio file (not possible in MP4 / MOV: it is removed there and the result says so).
    pub keep_cover_art: bool,
    /// Give the cleaned file the current time as its times instead of the original's.
    pub reset_times: bool,
}

/// One thing a file carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// "GPS Latitude", "Author", "Zone.Identifier stream" ...
    pub label: String,
    /// The value, shortened; empty when there is none worth showing.
    pub value: String,
    /// True when the run removes it; false for what is kept on purpose (colour profile, orientation).
    pub remove: bool,
}

impl Finding {
    pub(crate) fn new(label: impl Into<String>, value: impl Into<String>, remove: bool) -> Self {
        let mut value: String = value.into();
        value = value.replace(['\r', '\n', '\0'], " ").trim().to_string();
        if value.chars().count() > 80 {
            value = value.chars().take(79).collect::<String>() + "…";
        }
        Finding { label: label.into(), value, remove }
    }
}

/// What is in a file and what a run would do to it.
#[derive(Clone, Debug, Default)]
pub struct Inspection {
    /// "JPEG", "PDF", "Word document" ...
    pub format: String,
    pub findings: Vec<Finding>,
    /// What stays inside the file because the removal cannot reach it ("still inside: 3 comments by named authors").
    pub notes: Vec<String>,
    /// False when the file can be listed but leaf has no safe writer for it (TIFF, ...): a run refuses it.
    pub can_clean: bool,
    /// The refusal wording when `can_clean` is false.
    pub refusal: Option<String>,
}

impl Inspection {
    pub fn to_remove(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.remove)
    }
}

/// What a run did to one file.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The cleaned file.
    pub output: PathBuf,
    pub format: String,
    pub removed: Vec<Finding>,
    pub kept: Vec<Finding>,
    pub notes: Vec<String>,
}

/// Why a file was not cleaned. The text is leaf's wording; the technical reason is in [`ScrubError::details`].
#[derive(Debug)]
pub enum ScrubError {
    /// A kind of file leaf has no safe remover for.
    Unsupported(String),
    /// Cut off or damaged: cleaning it could damage it further.
    Malformed(String),
    /// Reading or writing failed.
    Io(io::Error),
    /// The cleaned file was read back and still holds something from the removal list (or no longer opens).
    Verify(String),
    /// Cancelled by the caller.
    Cancelled,
}

impl ScrubError {
    pub fn details(&self) -> String {
        match self {
            ScrubError::Unsupported(d) | ScrubError::Malformed(d) | ScrubError::Verify(d) => d.clone(),
            ScrubError::Io(e) => e.to_string(),
            ScrubError::Cancelled => String::new(),
        }
    }
}

impl std::fmt::Display for ScrubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScrubError::Unsupported(d) => write!(f, "Not supported: {d}"),
            ScrubError::Malformed(d) => write!(f, "Left untouched, the file looks damaged or cut off ({d})"),
            ScrubError::Io(e) => write!(f, "Could not read or write the file ({e})"),
            ScrubError::Verify(d) => write!(f, "Left untouched, the cleaned file did not pass the check ({d})"),
            ScrubError::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl std::error::Error for ScrubError {}

impl From<io::Error> for ScrubError {
    fn from(e: io::Error) -> Self {
        ScrubError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, ScrubError>;

/// The files bigger than this are refused for the formats that are read whole (pictures, PDF).
pub(crate) const MAX_IN_MEMORY: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Jpeg,
    Png,
    Webp,
    /// Recognised, but no safe writer: refused on a run (the kind decides the wording, see `refusal_text`).
    NoWriter(&'static str),
    /// HEIC / HEIF / AVIF, cleaned at container level.
    Heif(&'static str),
    Tiff,
    Gif,
    Svg,
    /// BMP / ICO: the format has no metadata fields.
    Bare(&'static str),
    /// zip (not Office), 7z, tar, gz, rar: not cleaned inside.
    Archive(&'static str),
    Pdf,
    Ooxml,
    Odf,
    Mp4,
    /// mp3, flac, ogg, opus, wav: tags through lofty.
    Tagged,
    /// Nothing leaf can read metadata from; only alternate streams / extended attributes apply.
    Plain,
}

fn read_head(path: &Path, n: usize) -> io::Result<Vec<u8>> {
    use io::Read;
    let mut buf = vec![0u8; n];
    let mut f = fs::File::open(path)?;
    let mut got = 0;
    while got < n {
        let k = f.read(&mut buf[got..])?;
        if k == 0 {
            break;
        }
        got += k;
    }
    buf.truncate(got);
    Ok(buf)
}

/// The kind of a file from its first bytes (the extension only decides among the audio kinds).
pub(crate) fn family_of(path: &Path) -> io::Result<(Family, &'static str)> {
    let h = read_head(path, 4096)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if h.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok((Family::Jpeg, "JPEG"));
    }
    if h.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok((Family::Png, "PNG"));
    }
    if h.len() >= 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"WEBP" {
        return Ok((Family::Webp, "WebP"));
    }
    const RAW_EXT: [&str; 25] = [
        "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "sr2", "srf", "orf", "rw2", "raf", "pef", "srw", "raw", "3fr", "x3f", "kdc", "dcr", "mrw",
        "erf", "mef", "mos", "iiq", "rwl",
    ];
    if h.starts_with(b"FUJIFILMCCD-RAW") || h.starts_with(b"IIRO") || h.starts_with(b"IIRS") || h.starts_with(b"IIU\0") || h.starts_with(b"MRM\0") || h.starts_with(b"II\x1a\0\0\0HEAPCCDR") {
        return Ok((Family::NoWriter("camera RAW"), "Camera RAW"));
    }
    if h.starts_with(b"II*\0") || h.starts_with(b"MM\0*") || h.starts_with(b"II+\0") || h.starts_with(b"MM\0+") {
        if RAW_EXT.contains(&ext.as_str()) {
            return Ok((Family::NoWriter("camera RAW"), "Camera RAW"));
        }
        return Ok((Family::Tiff, "TIFF"));
    }
    if h.starts_with(b"GIF8") {
        return Ok((Family::Gif, "GIF"));
    }
    if h.starts_with(b"BM") && ext == "bmp" {
        return Ok((Family::Bare("BMP"), "BMP"));
    }
    if (h.starts_with(&[0, 0, 1, 0]) || h.starts_with(&[0, 0, 2, 0])) && (ext == "ico" || ext == "cur") {
        return Ok((Family::Bare("ICO"), "ICO"));
    }
    if ext == "svg" && String::from_utf8_lossy(&h).contains("<svg") {
        return Ok((Family::Svg, "SVG"));
    }
    if h.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Ok((Family::NoWriter("Matroska / WebM"), "Matroska video"));
    }
    if h.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Ok((Family::Archive("7z archive"), "7z archive"));
    }
    if h.starts_with(&[0x1F, 0x8B]) {
        return Ok((Family::Archive("gzip archive"), "gzip archive"));
    }
    if h.starts_with(b"Rar!") {
        return Ok((Family::Archive("RAR archive"), "RAR archive"));
    }
    if h.len() > 262 && &h[257..262] == b"ustar" {
        return Ok((Family::Archive("tar archive"), "tar archive"));
    }
    if h.len() >= 12 && &h[0..4] == b"RIFF" && &h[8..12] == b"AVI " {
        return Ok((Family::NoWriter("AVI"), "AVI video"));
    }
    if h.starts_with(b"%PDF") || h.windows(5).take(1024).any(|w| w == b"%PDF-") && ext == "pdf" {
        return Ok((Family::Pdf, "PDF"));
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        let brand = &h[8..12];
        if brand == b"crx " {
            return Ok((Family::NoWriter("camera RAW"), "Camera RAW"));
        }
        let heif: [&[u8]; 9] = [b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1", b"avif", b"avis", b"heim"];
        if heif.iter().any(|b| *b == brand) {
            let name = if brand.starts_with(b"avi") { "AVIF" } else { "HEIC" };
            return Ok((Family::Heif(name), name));
        }
        return Ok((Family::Mp4, "MP4 / MOV / M4A"));
    }
    if h.len() >= 12 && (&h[4..8] == b"moov" || &h[4..8] == b"mdat" || &h[4..8] == b"free" || &h[4..8] == b"wide") {
        return Ok((Family::Mp4, "QuickTime movie"));
    }
    if h.starts_with(b"PK\x03\x04") {
        let kind = office::sniff(path);
        match kind {
            Some(office::Kind::Ooxml(name)) => return Ok((Family::Ooxml, name)),
            Some(office::Kind::Odf(name)) => return Ok((Family::Odf, name)),
            None => return Ok((Family::Archive("ZIP archive"), "ZIP archive")),
        }
    }
    let tagged_ext = ["mp3", "flac", "ogg", "oga", "opus", "wav", "wv", "ape"];
    let looks_audio = h.starts_with(b"ID3")
        || h.starts_with(b"fLaC")
        || h.starts_with(b"OggS")
        || (h.starts_with(b"RIFF") && h.len() >= 12 && &h[8..12] == b"WAVE")
        || (h.len() >= 2 && h[0] == 0xFF && (h[1] & 0xE0) == 0xE0 && ext == "mp3");
    if looks_audio || (tagged_ext.contains(&ext.as_str()) && ext != "wv" && ext != "ape") {
        let name = match ext.as_str() {
            "mp3" => "MP3",
            "flac" => "FLAC",
            "ogg" | "oga" => "Ogg",
            "opus" => "Opus",
            "wav" => "WAV",
            _ => {
                if h.starts_with(b"fLaC") {
                    "FLAC"
                } else if h.starts_with(b"OggS") {
                    "Ogg"
                } else if h.starts_with(b"ID3") {
                    "MP3"
                } else {
                    "WAV"
                }
            }
        };
        return Ok((Family::Tagged, name));
    }
    Ok((Family::Plain, "file"))
}

/// What is in `path`, without changing anything ("Show metadata").
pub fn inspect(path: &Path, opts: &Options) -> Result<Inspection> {
    let (fam, name) = family_of(path)?;
    let mut out = match fam {
        Family::Jpeg | Family::Png | Family::Webp => image::inspect(path, fam, opts)?,
        Family::NoWriter(kind) => image::inspect_unwritable(path, kind),
        Family::Heif(kind) => heif::inspect(path, kind, opts)?,
        Family::Tiff => tiff::inspect(path, opts)?,
        Family::Gif => gif::inspect(path, opts)?,
        Family::Svg => svg::inspect(path, opts)?,
        Family::Bare(kind) => Inspection {
            format: kind.to_string(),
            can_clean: true,
            notes: vec![format!("Nothing found: {kind} files have no metadata fields.")],
            ..Default::default()
        },
        Family::Archive(kind) => Inspection {
            format: kind.to_string(),
            can_clean: false,
            notes: vec!["An archive keeps the metadata of the files inside it.".into()],
            refusal: Some("an archive keeps the metadata of the files inside it: clean the files before packing them".into()),
            ..Default::default()
        },
        Family::Pdf => pdf::inspect(path)?,
        Family::Ooxml | Family::Odf => office::inspect(path)?,
        Family::Mp4 => mp4::inspect(path)?,
        Family::Tagged => tags::inspect(path, opts)?,
        Family::Plain => Inspection {
            format: name.to_string(),
            can_clean: true,
            notes: vec!["The contents of this kind of file were not examined: leaf cannot read metadata from it.".into()],
            ..Default::default()
        },
    };
    if out.format.is_empty() {
        out.format = name.to_string();
    }
    for s in streams::list_streams(path) {
        out.findings.push(Finding::new(s.label, s.value, true));
    }
    if fam == Family::Plain && !out.findings.iter().any(|f| f.remove) {
        out.can_clean = false;
        out.refusal = Some("this kind of file has no metadata leaf can read, and it has no alternate streams or extended attributes".into());
    }
    Ok(out)
}

/// "Formats leaf cannot clean are refused and left untouched": the second fixed line of the result sheet.
pub const REFUSED_LINE: &str = "Formats leaf cannot clean are refused and left untouched: check the list above.";

/// Writes the cleaned copy of `src` to `dst` (which must not exist) and checks it. On any error nothing is left at `dst`.
pub fn clean_to(src: &Path, dst: &Path, opts: &Options, cancel: &dyn Fn() -> bool) -> Result<Outcome> {
    let (fam, name) = family_of(src)?;
    let before = inspect(src, opts)?;
    if !before.can_clean {
        return Err(ScrubError::Unsupported(
            before.refusal.clone().unwrap_or_else(|| format!("leaf cannot clean {name} files safely")),
        ));
    }
    if dst.exists() {
        return Err(ScrubError::Io(io::Error::new(io::ErrorKind::AlreadyExists, "the target name is taken")));
    }
    let dir = dst.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".leafscrub-{}",
        dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    let _ = fs::remove_file(&tmp);
    let res = (|| -> Result<Vec<String>> {
        if cancel() {
            return Err(ScrubError::Cancelled);
        }
        let mut extra = Vec::new();
        match fam {
            Family::Jpeg | Family::Png | Family::Webp => image::produce(src, &tmp, fam, opts)?,
            Family::Heif(_) => heif::produce(src, &tmp, opts)?,
            Family::Tiff => tiff::produce(src, &tmp, opts)?,
            Family::Gif => gif::produce(src, &tmp, opts)?,
            Family::Svg => svg::produce(src, &tmp, opts)?,
            Family::Bare(_) => copy_without_attrs(src, &tmp)?,
            Family::Archive(k) => return Err(ScrubError::Unsupported(format!("a {k} keeps the metadata of the files inside it"))),
            Family::Pdf => pdf::produce(src, &tmp)?,
            Family::Ooxml | Family::Odf => office::produce(src, &tmp)?,
            Family::Mp4 => extra.extend(mp4::produce(src, &tmp, opts)?),
            Family::Tagged => tags::produce(src, &tmp, opts)?,
            Family::Plain => {
                // Nothing to rewrite: a plain copy carries no alternate stream and no extended attribute.
                copy_without_attrs(src, &tmp)?;
            }
            Family::NoWriter(k) => return Err(ScrubError::Unsupported(format!("leaf cannot clean {k} files safely"))),
        }
        if cancel() {
            return Err(ScrubError::Cancelled);
        }
        // Read it back: the same reader that lists metadata must find nothing from the removal list.
        verify(&tmp, opts, fam)?;
        Ok(extra)
    })();
    let extra = match res {
        Ok(e) => e,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // Times: the copy keeps the original's modified time unless the user ticked "reset".
    if opts.reset_times {
        // a fresh file already has "now" as its times
    } else if let Ok(m) = fs::metadata(src).and_then(|m| m.modified()) {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&tmp) {
            let _ = f.set_modified(m);
        }
    }
    if let Err(e) = fs::rename(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    let mut notes = before.notes.clone();
    notes.extend(extra);
    let (removed, kept): (Vec<_>, Vec<_>) = before.findings.into_iter().partition(|f| f.remove);
    Ok(Outcome { output: dst.to_path_buf(), format: before.format, removed, kept, notes })
}

fn verify(tmp: &Path, opts: &Options, fam: Family) -> Result<()> {
    let after = inspect(tmp, opts).map_err(|e| ScrubError::Verify(format!("the cleaned file no longer opens: {e}")))?;
    if let Some(f) = after.to_remove().next() {
        let what = if f.value.is_empty() { f.label.clone() } else { format!("{}: {}", f.label, f.value) };
        return Err(ScrubError::Verify(format!("{what} is still in it")));
    }
    let _ = fam;
    Ok(())
}

/// Copies the bytes of `src` to `dst` and nothing else.
pub(crate) fn copy_without_attrs(src: &Path, dst: &Path) -> io::Result<()> {
    let mut r = fs::File::open(src)?;
    let mut w = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    io::copy(&mut r, &mut w)?;
    w.sync_all()
}

/// `name.clean.ext`, then `name.clean (2).ext` ... in the folder of `src`.
pub fn clean_copy_name(src: &Path) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let first = dir.join(format!("{stem}.clean{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2.. {
        let p = dir.join(format!("{stem}.clean ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    unreachable!()
}

/// `file-1.jpg`, `file-2.jpg` ... the first free one in `dir`; `index` is where the count starts.
pub fn neutral_name(dir: &Path, ext: &str, index: usize) -> PathBuf {
    let ext = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let mut n = index.max(1);
    loop {
        let p = dir.join(format!("file-{n}{ext}"));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// "Replace the originals": the cleaned file is written beside `src` first (and checked), then `put_away` must move the original
/// out of its place (leaf: into the Recycle Bin / Trash, where it stays recoverable), and only then the cleaned file takes `target`
/// (`src` itself, or a neutral name in the same folder). If anything fails before the original is gone, nothing is changed; if
/// the last step fails the original is in the Trash and the cleaned file is kept under a `.clean` name, and the error says so.
pub fn replace(
    src: &Path,
    target: &Path,
    opts: &Options,
    cancel: &dyn Fn() -> bool,
    put_away: &dyn Fn(&Path) -> std::result::Result<(), String>,
) -> Result<Outcome> {
    let dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut n = 0u32;
    let beside = loop {
        let p = dir.join(format!(".{name}.{}-{n}.leaf-scrub", std::process::id()));
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    // (the extension stays last so the file is read as what it is: the temporary name ends in the real one)
    let beside = match src.extension() {
        Some(e) => beside.with_file_name(format!("{}.{}", beside.file_name().unwrap().to_string_lossy(), e.to_string_lossy())),
        None => beside,
    };
    let mut out = clean_to(src, &beside, opts, cancel)?;
    if cancel() {
        let _ = fs::remove_file(&beside);
        return Err(ScrubError::Cancelled);
    }
    if let Err(e) = put_away(src) {
        let _ = fs::remove_file(&beside);
        return Err(ScrubError::Io(io::Error::other(format!("the original could not be moved to the Trash, nothing was changed ({e})"))));
    }
    // a put-away that returned without moving the file kept the original nowhere: never replaced
    if fs::symlink_metadata(src).is_ok() {
        let _ = fs::remove_file(&beside);
        return Err(ScrubError::Io(io::Error::other("the original did not reach the Trash, nothing was changed")));
    }
    match fs::rename(&beside, target) {
        Ok(()) => {
            out.output = target.to_path_buf();
            Ok(out)
        }
        Err(e) => {
            let keep = clean_copy_name(src);
            let kept = fs::rename(&beside, &keep).is_ok();
            Err(ScrubError::Io(io::Error::other(format!(
                "the cleaned file could not be put in place ({e}); the original is in the Trash{}",
                if kept { format!(" and the cleaned file is saved as {}", keep.display()) } else { String::new() }
            ))))
        }
    }
}
