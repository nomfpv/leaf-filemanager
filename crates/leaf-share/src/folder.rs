//! The friend's side of a sync: what the other PC may do in this PC's shared folder, and nothing more. Every path it sends is
//! checked before anything is touched: no `..`, no absolute path, no name this PC cannot store, no link or junction on the way (and
//! the result's folder, resolved by the system, must still lie inside the shared folder). A refused path is logged by its name and
//! answered with "access denied". A removal never deletes: the item is moved into the folder's own `.leaf-sync-trash/<time>/`. A
//! write only ever creates a new temporary file (`.<name>.<n>.leaf-part`), which becomes the real name by a rename that never
//! replaces anything.

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use leaf_vfs::{Fs as _, FsError, LocalFs, MetaChange, names};
use rf_nearby::kit;

use crate::wire::{ErrKind, WireEntry};

/// The folder in a shared folder's root that removed and replaced files go to (the same one leaf's sync jobs use).
pub const TRASH: &str = leaf_sync::TRASH_DIR;

pub struct Folder {
    root: PathBuf,
    /// The root as the system resolves it, for the last check of every path.
    canon: PathBuf,
}

fn is_link(m: &fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        // FILE_ATTRIBUTE_REPARSE_POINT: junctions and every other kind of link
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// A temporary name of the copy (`leaf_vfs::copy::part_name`): the only names a write may create.
pub fn is_part(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".leaf-part") && name.len() > ".leaf-part".len() + 1
}

fn refused(rel: &str, why: &str) -> FsError {
    log::warn!("share: refused a path from the other PC ({why}): {rel:?}");
    FsError::Denied
}

fn ms_of(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

fn entry_of(name: &str, m: &fs::Metadata) -> WireEntry {
    let link = is_link(m);
    WireEntry { name: name.to_string(), dir: m.is_dir() && !link, link, size: if m.is_dir() || link { 0 } else { m.len() }, mtime_ms: ms_of(m.modified()) }
}

pub fn err_kind(e: &FsError) -> (ErrKind, String) {
    let k = match e {
        FsError::NotFound => ErrKind::NotFound,
        FsError::Denied => ErrKind::Denied,
        FsError::Exists => ErrKind::Exists,
        FsError::NotEmpty => ErrKind::NotEmpty,
        FsError::Unsupported => ErrKind::Unsupported,
        _ => ErrKind::Other,
    };
    (k, e.to_string())
}

/// `YYYY-MM-DD_HHMMSS` (UTC).
pub(crate) fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}_{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

/// Year, month, day of a day number since 1970-01-01 (Howard Hinnant's civil_from_days).
pub(crate) fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl Folder {
    /// The shared folder at `root` (which must be a real folder, not a link to one).
    pub fn new(root: &Path) -> Result<Folder, FsError> {
        let m = fs::symlink_metadata(root)?;
        if is_link(&m) || !m.is_dir() {
            return Err(FsError::Other(format!("{} is not a folder (or is a link)", root.display())));
        }
        let canon = fs::canonicalize(root)?;
        Ok(Folder { root: root.to_path_buf(), canon })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The local path of `rel`, after every check. `final_link`: the last part may be a link (it is reported, never followed).
    pub fn resolve(&self, rel: &str, final_link: bool) -> Result<PathBuf, FsError> {
        if rel.is_empty() {
            return Ok(self.root.clone());
        }
        let parts = kit::safe_parts(rel).ok_or_else(|| refused(rel, "not a plain relative path"))?;
        if let Some(why) = parts.iter().find_map(|p| names::local_problem(p)) {
            return Err(refused(rel, why));
        }
        let mut cur = self.root.clone();
        for (i, p) in parts.iter().enumerate() {
            cur.push(p);
            let last = i + 1 == parts.len();
            match fs::symlink_metadata(&cur) {
                Ok(m) if is_link(&m) => {
                    if !(last && final_link) {
                        return Err(refused(rel, "a link on the way"));
                    }
                }
                Ok(m) if !last && !m.is_dir() => return Err(FsError::NotFound),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if !last {
                        return Err(FsError::NotFound);
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        // the system's own answer, last: the folder the item is in lies inside the shared folder
        if let Some(parent) = cur.parent() {
            match fs::canonicalize(parent) {
                Ok(cp) if cp.starts_with(&self.canon) => {}
                Ok(_) => return Err(refused(rel, "it leads out of the folder")),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(cur)
    }

    pub fn list(&self, rel: &str) -> Result<Vec<WireEntry>, FsError> {
        let dir = self.resolve(rel, false)?;
        let mut out = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let Some(name) = e.file_name().to_str().map(str::to_string) else {
                log::info!("share: a name that is not Unicode is left out in {}", dir.display());
                continue;
            };
            match fs::symlink_metadata(e.path()) {
                Ok(m) => out.push(entry_of(&name, &m)),
                Err(err) => log::info!("share: {name} could not be read: {err}"),
            }
        }
        Ok(out)
    }

    pub fn stat(&self, rel: &str) -> Result<WireEntry, FsError> {
        let p = self.resolve(rel, true)?;
        let m = fs::symlink_metadata(&p)?;
        Ok(entry_of(rel.rsplit('/').next().unwrap_or(""), &m))
    }

    /// The file, at `offset`, and how many bytes follow.
    pub fn open_read(&self, rel: &str, offset: u64) -> Result<(fs::File, u64), FsError> {
        let p = self.resolve(rel, false)?;
        let mut f = fs::File::open(&p)?;
        let m = f.metadata()?;
        if !m.is_file() {
            return Err(FsError::NotFound);
        }
        let len = m.len().saturating_sub(offset);
        if offset > 0 {
            f.seek(SeekFrom::Start(offset))?;
        }
        Ok((f, len))
    }

    /// A new temporary file to write into (never an existing file, never another name).
    pub fn create_part(&self, rel: &str) -> Result<(fs::File, PathBuf), FsError> {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if !is_part(name) {
            return Err(refused(rel, "only a temporary name can be written"));
        }
        let p = self.resolve(rel, false)?;
        let f = fs::OpenOptions::new().write(true).create_new(true).open(&p)?;
        Ok((f, p))
    }

    pub fn mkdir(&self, rel: &str) -> Result<(), FsError> {
        if rel.is_empty() {
            return Err(FsError::Exists);
        }
        Ok(fs::create_dir(self.resolve(rel, false)?)?)
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        if from.is_empty() || to.is_empty() {
            return Err(refused(from, "the folder itself"));
        }
        let (a, b) = (self.resolve(from, false)?, self.resolve(to, false)?);
        LocalFs.rename(&LocalFs::address(&a), &LocalFs::address(&b), false)
    }

    pub fn set_time(&self, rel: &str, mtime_ms: i64) -> Result<(), FsError> {
        let p = self.resolve(rel, false)?;
        LocalFs.set_meta(&LocalFs::address(&p), &MetaChange { modified_ms: Some(mtime_ms), mode: None })
    }

    pub fn hash(&self, rel: &str) -> Result<String, FsError> {
        let (mut f, _) = self.open_read(rel, 0)?;
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            ctx.update(&buf[..n]);
        }
        Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Takes `rel` out of the folder: into `.leaf-sync-trash/<time>/<rel>` (a new number when that is taken). Never a deletion,
    /// except for a temporary file of an unfinished copy. A folder with something in it only with `recursive`.
    pub fn remove(&self, rel: &str, recursive: bool) -> Result<(), FsError> {
        if rel.is_empty() {
            return Err(refused(rel, "the folder itself"));
        }
        if rel == TRASH || rel.starts_with(&format!("{TRASH}/")) {
            return Err(refused(rel, "inside the trash folder"));
        }
        let p = self.resolve(rel, true)?;
        let m = fs::symlink_metadata(&p)?;
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if m.is_file() && !is_link(&m) && is_part(name) {
            return Ok(fs::remove_file(&p)?);
        }
        if m.is_dir() && !is_link(&m) && !recursive && fs::read_dir(&p)?.next().is_some() {
            return Err(FsError::NotEmpty);
        }
        self.to_trash(rel, &p)
    }

    fn to_trash(&self, rel: &str, p: &Path) -> Result<(), FsError> {
        let in_trash = format!("{TRASH}/{}/{rel}", stamp(crate::now_ms()));
        let parts: Vec<&str> = in_trash.split('/').collect();
        let mut dir = String::new();
        for part in &parts[..parts.len() - 1] {
            dir = if dir.is_empty() { (*part).to_string() } else { format!("{dir}/{part}") };
            let d = self.resolve(&dir, false)?;
            match fs::create_dir(&d) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir() && !is_link(&m)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        let name = parts[parts.len() - 1];
        for n in 1..1000 {
            let cand = if n == 1 { name.to_string() } else { format!("{name} ({n})") };
            let to = self.resolve(&format!("{dir}/{cand}"), false)?;
            match LocalFs.rename(&LocalFs::address(p), &LocalFs::address(&to), false) {
                Ok(()) => return Ok(()),
                Err(FsError::Exists) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(FsError::Exists)
    }
}
