//! After the download: unpack the archives (RAR incl. volumes, 7z, zip; leaf's archive engine, which never writes outside
//! the folder it is given), join split files (`name.ext.001`, `.002`, ...), clean up what is left over (par2, archive
//! volumes, samples), and place the result in its folder (never over an existing file or folder).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where the unpacked files go inside the job folder before they are placed.
pub const UNPACK_DIR: &str = "_unpack";

fn lower_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The first volume of each archive in `dir` (not the later volumes: rf-archive reads those itself).
pub fn archives(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = lower_name(p);
            if n.ends_with(".rar") {
                // name.part01.rar / name.part1.rar is the first; name.part02.rar is not
                return match n.rfind(".part").map(|i| &n[i + 5..n.len() - 4]) {
                    Some(num) if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) => num.trim_start_matches('0') == "1",
                    _ => true,
                };
            }
            n.ends_with(".7z") || n.ends_with(".7z.001") || n.ends_with(".zip") || n.ends_with(".zip.001")
        })
        .collect();
    v.sort();
    v
}

/// Whether `p` is a piece of an archive (any volume) or a par2 file: what cleanup removes after a good unpack.
pub fn is_leftover(p: &Path) -> bool {
    let n = lower_name(p);
    if n.ends_with(".par2") || n.ends_with(".rar") || n.ends_with(".7z") || n.ends_with(".zip") || n.ends_with(".sfv") || n.ends_with(".nzb") {
        return true;
    }
    // .r00 .. .r99, .s00 (old RAR volumes), .7z.002, .zip.002
    let ext = n.rsplit('.').next().unwrap_or("");
    if ext.len() == 3 && (ext.starts_with('r') || ext.starts_with('s')) && ext[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if ext.len() == 3 && ext.chars().all(|c| c.is_ascii_digit()) {
        let before = n[..n.len() - 4].rsplit('.').next().unwrap_or("");
        return matches!(before, "7z" | "zip" | "rar");
    }
    false
}

const VIDEO: &[&str] = &["mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "mpg", "mpeg", "webm", "vob"];
/// A sample clip is a short piece of the film: larger than this is never taken for one.
const SAMPLE_MAX: u64 = 300 << 20;

fn video_len(p: &Path) -> Option<u64> {
    let n = lower_name(p);
    let ext = n.rsplit_once('.')?.1;
    if !VIDEO.contains(&ext) {
        return None;
    }
    fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// A sample clip the poster added: a small video with `sample` as a word of its name, or a folder named `sample` /
/// `samples` holding nothing but small videos and small extras. The caller still checks that it is not the only video.
pub fn is_sample(p: &Path) -> bool {
    let n = lower_name(p);
    if p.is_dir() {
        return (n == "sample" || n == "samples")
            && fs::read_dir(p).is_ok_and(|rd| {
                rd.flatten().all(|e| {
                    let q = e.path();
                    match video_len(&q) {
                        Some(len) => len <= SAMPLE_MAX,
                        None => fs::metadata(&q).is_ok_and(|m| m.is_file() && m.len() <= 1 << 20),
                    }
                })
            });
    }
    let named = n.rsplit_once('.').map_or(n.as_str(), |(stem, _)| stem).split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "sample");
    named && video_len(p).is_some_and(|len| len <= SAMPLE_MAX)
}

/// The largest video in `dirs` (not inside folders), leaving out `but`.
fn largest_video(dirs: &[&Path], but: &Path) -> u64 {
    dirs.iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten().map(|e| e.path()))
        .filter(|p| p != but)
        .filter_map(|p| video_len(&p))
        .max()
        .unwrap_or(0)
}

/// The largest video in a folder (a sample folder: its clip).
fn largest_inside(dir: &Path) -> u64 {
    fs::read_dir(dir).map(|rd| rd.flatten().filter_map(|e| video_len(&e.path())).max().unwrap_or(0)).unwrap_or(0)
}

#[derive(Debug)]
pub enum UnpackError {
    /// The archive is protected and no password was given, or the given one is wrong.
    Password { wrong: bool },
    Failed(String),
    Cancelled,
}

/// Unpacks `archive` into `dir/_unpack`; a path in it that would leave that folder (`..`, a drive, an absolute path) stops
/// the unpack (rf-archive's rule).
pub fn unpack(archive: &Path, job_dir: &Path, password: Option<&str>, progress: &mut dyn FnMut(u64, u64), cancelled: &dyn Fn() -> bool) -> Result<PathBuf, UnpackError> {
    let out = job_dir.join(UNPACK_DIR);
    let opts = rf_archive::Options { password: password.map(str::to_string), ..Default::default() };
    match rf_archive::extract_with(archive, &out, &opts, &mut |p: rf_archive::Progress| progress(p.done_bytes, p.total_bytes), cancelled) {
        Ok(_) => Ok(out),
        Err(e) => {
            if let Some(pe) = e.downcast_ref::<rf_archive::PasswordError>() {
                return Err(UnpackError::Password { wrong: *pe == rf_archive::PasswordError::Wrong });
            }
            if e.downcast_ref::<rf_archive::Cancelled>().is_some() {
                return Err(UnpackError::Cancelled);
            }
            Err(UnpackError::Failed(format!("{e:#}")))
        }
    }
}

/// Joins `name.ext.001`, `.002`, ... into `name.ext` (archive volumes are left to the unpack step). Returns the joined files;
/// the pieces are removed only after the whole file is written.
pub fn join_split(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut joined = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return Ok(joined) };
    let firsts: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| lower_name(p).ends_with(".001") && !is_leftover(p)).collect();
    for first in firsts {
        let name = first.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let base = &name[..name.len() - 4];
        let target = dir.join(base);
        if target.exists() {
            continue;
        }
        let mut pieces = Vec::new();
        for n in 1..10_000 {
            let p = dir.join(format!("{base}.{n:03}"));
            if !p.exists() {
                break;
            }
            pieces.push(p);
        }
        let tmp = dir.join(format!("{base}{}", crate::engine::PART_EXT));
        {
            let mut out = fs::File::create(&tmp)?;
            for p in &pieces {
                io::copy(&mut fs::File::open(p)?, &mut out)?;
            }
            out.sync_all()?;
        }
        fs::rename(&tmp, &target)?;
        for p in &pieces {
            let _ = fs::remove_file(p);
        }
        joined.push(target);
    }
    Ok(joined)
}

/// Removes par2 files, archive volumes (only after they were unpacked: `unpacked`) and samples from the job folder.
pub fn cleanup(dir: &Path, unpacked: bool) {
    let unpack = dir.join(UNPACK_DIR);
    let both = [dir, unpack.as_path()];
    // a sample goes only when a larger video stays (never the only video of the job)
    let removable_sample = |p: &Path| {
        if !is_sample(p) {
            return false;
        }
        let own = if p.is_dir() { largest_inside(p) } else { video_len(p).unwrap_or(0) };
        largest_video(&both, p) > own
    };
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = lower_name(&p);
        if n.starts_with(".leaf-nzb") || n == UNPACK_DIR {
            continue;
        }
        let par = n.ends_with(".par2") || n.ends_with(".sfv");
        if par || (unpacked && is_leftover(&p)) {
            let _ = fs::remove_file(&p);
        } else if removable_sample(&p) {
            if p.is_dir() {
                let _ = fs::remove_dir_all(&p);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
    }
    if let Ok(rd) = fs::read_dir(&unpack) {
        for e in rd.flatten().filter(|e| removable_sample(&e.path())) {
            let p = e.path();
            let _ = if p.is_dir() { fs::remove_dir_all(&p) } else { fs::remove_file(&p) };
        }
    }
}

/// What to do when the folder a job goes to is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// A new name next to it (`name (2)`).
    KeepBoth,
    /// Leave the result in the incomplete folder.
    Skip,
}

/// Moves the job's result (the unpacked files and whatever else is left, not leaf's state) into `dest_dir/name`. The folder
/// is never merged with or written over an existing one: `taken` decides (asked only when the name is taken). Returns the
/// folder, or `None` when skipped.
pub fn place(job_dir: &Path, dest_dir: &Path, name: &str, taken: &dyn Fn(&Path) -> Taken) -> io::Result<Option<PathBuf>> {
    fs::create_dir_all(dest_dir)?;
    let mut target = dest_dir.join(name);
    if target.exists() {
        match taken(&target) {
            Taken::Skip => return Ok(None),
            Taken::KeepBoth => target = crate::engine::unique(dest_dir, name),
        }
    }
    // (a fresh folder, made now: nothing in it can be replaced)
    fs::create_dir(&target)?;
    let mut items: Vec<(PathBuf, PathBuf)> = Vec::new();
    let unpack = job_dir.join(UNPACK_DIR);
    for dir in [&unpack, &job_dir.to_path_buf()] {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let n = lower_name(&p);
            if dir == &job_dir.to_path_buf() && (n == UNPACK_DIR || n.starts_with(".leaf-nzb") || n.ends_with(crate::engine::PART_EXT)) {
                continue;
            }
            let file_name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            items.push((p, crate::engine::unique(&target, &file_name)));
        }
    }
    for (from, to) in items {
        // (`unique` is asked again: an earlier item of this list may have taken the name meanwhile)
        let to = if to.exists() { crate::engine::unique(&target, &to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) } else { to };
        move_item(&from, &to)?;
    }
    Ok(Some(target))
}

/// A rename, or a copy and delete across drives. `to` does not exist.
fn move_item(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)?.flatten() {
            move_item(&e.path(), &to.join(e.file_name()))?;
        }
        fs::remove_dir(from)
    } else {
        // (create_new: a file that appeared meanwhile is never written over)
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        io::copy(&mut fs::File::open(from)?, &mut out)?;
        out.sync_all()?;
        fs::remove_file(from)
    }
}
