//! Editing a file on a server (H10): the app fetches it into its cache, opens it in the local editor and watches the copy;
//! every save comes back here.
//!
//! - Before anything is sent, the server's file is looked at: if its size or time differ from what was fetched (someone else
//!   changed it meanwhile), nothing is uploaded and the caller gets `SaveError::Changed` (the app offers "Save as copy on
//!   server" instead, a new name next to it; nothing is ever written over the other person's version).
//! - Otherwise the new version is written to a temporary name beside the file; at the end the version on the server becomes
//!   `name.leaf-bak` (one backup, replaced each time) and the new one takes the name (`SftpFs::open_write_keeping_backup`).
//!   A cut connection leaves the server's file as it was.
//! - The check and the final rename are separate requests: a change in that short moment is not seen (SFTP has no lock).

use std::io::Read;
use std::path::Path;

use leaf_vfs::{Fs, FsError, Kind, Meta, VPath, WriteOpts};

use crate::SftpFs;

/// What the server said about the file when it was fetched (or last saved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fetched {
    pub size: u64,
    pub modified_ms: i64,
}

impl Fetched {
    pub fn of(m: &Meta) -> Fetched {
        Fetched { size: m.size, modified_ms: m.modified_ms }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The file on the server is not the one that was fetched any more (what it is now); nothing was sent.
    Changed(Fetched),
    Fs(FsError),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Changed(_) => f.write_str("The file on the server changed since it was opened; your version was not uploaded"),
            SaveError::Fs(e) => e.fmt(f),
        }
    }
}

impl From<FsError> for SaveError {
    fn from(e: FsError) -> SaveError {
        SaveError::Fs(e)
    }
}

/// What to do with an editing copy that is there before a fetch (left from an earlier run of leaf).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leftover {
    /// No copy there: fetch.
    None,
    /// The copy has the server file's size and time: it IS the server's version, used as it is (no fetch).
    Same,
    /// The copy differs (an edit that was never sent): moved aside to this path in the same folder; then fetch.
    SetAside(std::path::PathBuf),
}

/// Before an editing copy is fetched to `local`: a copy that is there already and differs from the server's file (`server`)
/// is renamed to `name (unsent <time>).ext` beside it, so an edit not sent before leaf closed is never overwritten. Nothing
/// is truncated or removed here.
pub fn set_aside_unsent(local: &Path, server: &Meta) -> std::io::Result<Leftover> {
    let m = match std::fs::symlink_metadata(local) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Leftover::None),
        Err(e) => return Err(e),
    };
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let same = m.is_file() && m.len() == server.size && m.modified().is_ok_and(|t| secs(t) == server.modified_ms / 1000);
    if same {
        return Ok(Leftover::Same);
    }
    let dir = local.parent().ok_or_else(|| std::io::Error::other("no folder"))?;
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let when = chrono::Local::now().format("%Y-%m-%d %H%M%S");
    for n in 1..1000 {
        let cand = if n == 1 { format!("{stem} (unsent {when}){ext}") } else { format!("{stem} (unsent {when} {n}){ext}") };
        let to = dir.join(cand);
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        std::fs::rename(local, &to)?;
        return Ok(Leftover::SetAside(to));
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

/// The name of the one backup kept beside an edited file.
pub fn backup_name(name: &str) -> String {
    format!("{name}.leaf-bak")
}

fn backend(v: &VPath) -> Result<std::sync::Arc<SftpFs>, FsError> {
    if v.scheme() != "sftp" {
        return Err(FsError::Unsupported);
    }
    Ok(crate::sftp_backend())
}

/// Uploads the edited `local` over `v`, unless the server's file changed since `fetched`. Returns what the server says
/// about the new version (the next save compares with it). Blocks: a worker thread.
pub fn save_back(v: &VPath, local: &Path, fetched: &Fetched, cancelled: &dyn Fn() -> bool) -> Result<Fetched, SaveError> {
    save_back_with(&*backend(v)?, v, local, fetched, cancelled)
}

/// Uploads `local` next to `v` under a new name (`name (edited).ext`, `name (edited 2).ext`...), never over anything.
pub fn save_as_copy(v: &VPath, local: &Path, cancelled: &dyn Fn() -> bool) -> Result<VPath, FsError> {
    save_as_copy_with(&*backend(v)?, v, local, cancelled)
}

pub(crate) fn save_back_with(fs: &SftpFs, v: &VPath, local: &Path, fetched: &Fetched, cancelled: &dyn Fn() -> bool) -> Result<Fetched, SaveError> {
    let now = fs.stat(v)?;
    if now.kind != Kind::File {
        return Err(SaveError::Fs(FsError::Other("This is not a regular file on the server any more".into())));
    }
    if Fetched::of(&now) != *fetched {
        log::info!("remote: {} changed on the server since it was fetched; not uploaded", v.name());
        return Err(SaveError::Changed(Fetched::of(&now)));
    }
    let w = fs.open_write_keeping_backup(v, &backup_name(v.name()))?;
    send(local, w, cancelled)?;
    Ok(Fetched::of(&fs.stat(v)?))
}

pub(crate) fn save_as_copy_with(fs: &SftpFs, v: &VPath, local: &Path, cancelled: &dyn Fn() -> bool) -> Result<VPath, FsError> {
    let parent = v.parent().ok_or(FsError::Denied)?;
    let name = v.name();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1..1000 {
        let cand = if n == 1 { format!("{stem} (edited){ext}") } else { format!("{stem} (edited {n}){ext}") };
        let to = leaf_vfs::names::vchild(&parent, &cand).map_err(|why| FsError::Other(why.into()))?;
        match fs.open_write(&to, WriteOpts::CREATE_NEW) {
            Ok(w) => {
                send(local, w, cancelled)?;
                return Ok(to);
            }
            Err(FsError::Exists) => continue,
            Err(e) => return Err(e),
        }
    }
    Err(FsError::Exists)
}

/// The local file's bytes into `w`; only a finished upload takes the name (dropped on error or cancel).
fn send(local: &Path, mut w: Box<dyn leaf_vfs::WriteSink>, cancelled: &dyn Fn() -> bool) -> Result<(), FsError> {
    let mut f = std::fs::File::open(local)?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        w.write_all(&buf[..n])?;
    }
    w.finish()
}
