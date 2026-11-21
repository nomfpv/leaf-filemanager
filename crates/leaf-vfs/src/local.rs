//! `file://`: the local disk behind the `Fs` interface, thin over `leaf_sys` (its listing, its no-replace rename, its free
//! space, its folder watch). Used by the generic copy and by the shared tests. The app's own folder listing does NOT come
//! through here: local folders keep their direct path (`leaf_sys::list_dir` called by the folder view).
//!
//! Addresses: `file:///C:/data/x` is `C:\data\x`, `file://server/share/x` is `\\server\share\x` (on Linux `file:///home/x` is
//! `/home/x`).

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::{Caps, Entry, Fs, FsError, Kind, Meta, MetaChange, Space, VPath, WatchCallback, WatchEvent, WatchGuard, WriteMode, WriteOpts, WriteSink};

pub struct LocalFs;

impl LocalFs {
    /// The disk path of an address.
    pub fn native(p: &VPath) -> PathBuf {
        if !p.authority().is_empty() && !p.authority().eq_ignore_ascii_case("localhost") {
            return PathBuf::from(format!(r"\\{}{}", p.authority(), p.path().replace('/', "\\")));
        }
        native_path(p.path())
    }

    /// The address of a disk path (`C:\data` → `file:///C:/data`).
    pub fn address(path: &std::path::Path) -> VPath {
        let s = path.to_string_lossy().replace('\\', "/");
        let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
        if let Some(unc) = s.strip_prefix("//") {
            let (server, rest) = unc.split_once('/').unwrap_or((unc, ""));
            return VPath::new("file", server, rest).unwrap_or_else(|_| VPath::new("file", "", "/").expect("root"));
        }
        VPath::new("file", "", &s).unwrap_or_else(|_| VPath::new("file", "", "/").expect("root"))
    }
}

#[cfg(windows)]
fn native_path(path: &str) -> PathBuf {
    // "/C:/data" → "C:\data"; "/C:" → "C:\"
    let t = path.trim_start_matches('/');
    let mut s = t.replace('/', "\\");
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    PathBuf::from(s)
}

#[cfg(not(windows))]
fn native_path(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn split(p: &VPath) -> Result<(PathBuf, String), FsError> {
    let par = p.parent().ok_or(FsError::Denied)?;
    Ok((LocalFs::native(&par), p.name().to_string()))
}

impl Fs for LocalFs {
    fn caps(&self) -> Caps {
        Caps { write: true, watch: true, set_times: true, links: true, free_space: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let path = LocalFs::native(dir);
        let n = leaf_sys::list_dir(&path, batch.max(1), cancelled, on_batch).map_err(list_err)?;
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        Ok(n)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        if p.is_root() || p.parent().is_some_and(|par| par.is_root()) && cfg!(windows) && p.name().ends_with(':') {
            let m = fs::metadata(LocalFs::native(p))?;
            return Ok(Meta { kind: if m.is_dir() { Kind::Dir } else { Kind::File }, size: 0, modified_ms: 0, created_ms: 0, mode: None });
        }
        let (dir, name) = split(p)?;
        let e = leaf_sys::stat_entry(&dir, &name).map_err(list_err)?.ok_or(FsError::NotFound)?;
        Ok(Meta::from_entry(&e))
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let mut f = fs::File::open(LocalFs::native(p))?;
        if offset > 0 {
            f.seek(SeekFrom::Start(offset))?;
        }
        Ok(Box::new(std::io::BufReader::with_capacity(256 * 1024, f)))
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        let path = LocalFs::native(p);
        if path.is_dir() {
            return Err(FsError::Exists);
        }
        let mut opts = fs::OpenOptions::new();
        opts.write(true);
        match o.mode {
            WriteMode::CreateNew => opts.create_new(true),
            WriteMode::Replace => opts.create(true).truncate(true),
            WriteMode::At(_) => &mut opts,
        };
        let mut f = opts.open(&path)?;
        if let WriteMode::At(off) = o.mode {
            if off > f.metadata()?.len() {
                return Err(FsError::Other("the offset is past the end of the file".into()));
            }
            f.set_len(off)?;
            f.seek(SeekFrom::Start(off))?;
        }
        Ok(Box::new(LocalSink(std::io::BufWriter::with_capacity(256 * 1024, f))))
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        Ok(fs::create_dir(LocalFs::native(p))?)
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if to.starts_with(from) && to != from {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        let (a, b) = (LocalFs::native(from), LocalFs::native(to));
        if replace {
            if fs::symlink_metadata(&b).is_ok_and(|m| m.is_dir()) {
                return Err(FsError::Exists);
            }
            if fs::symlink_metadata(&a)?.is_dir() && b.exists() {
                return Err(FsError::Exists);
            }
            return Ok(fs::rename(&a, &b)?);
        }
        Ok(leaf_sys::fileops::rename_no_replace(&a, &b)?)
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let path = LocalFs::native(p);
        let m = fs::symlink_metadata(&path)?;
        if m.file_type().is_symlink() {
            // the link itself, never what it points to (a folder link is removed like an empty folder)
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if m.file_attributes() & 0x10 != 0 {
                    return Ok(fs::remove_dir(&path)?);
                }
            }
            return Ok(fs::remove_file(&path)?);
        }
        if m.is_dir() {
            return Ok(if recursive { fs::remove_dir_all(&path)? } else { fs::remove_dir(&path)? });
        }
        Ok(fs::remove_file(&path)?)
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.mode.is_some() {
            return Err(FsError::Unsupported);
        }
        let Some(ms) = m.modified_ms else { return Ok(()) };
        let path = LocalFs::native(p);
        let mut o = fs::OpenOptions::new();
        o.write(true);
        #[cfg(windows)]
        {
            // FILE_FLAG_BACKUP_SEMANTICS: folders open too
            use std::os::windows::fs::OpenOptionsExt;
            o.custom_flags(0x0200_0000);
        }
        let f = o.open(&path)?;
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0) as u64);
        Ok(f.set_modified(t)?)
    }

    fn watch(&self, dir: &VPath, on: WatchCallback) -> Result<WatchGuard, FsError> {
        let w = leaf_sys::watch::watch_dir_changes(&LocalFs::native(dir), std::time::Duration::from_millis(150), move |c| {
            on(WatchEvent { names: c.names, rescan: c.rescan })
        })
        .map_err(|e| FsError::Other(format!("{e:#}")))?;
        Ok(Box::new(w))
    }

    fn free_space(&self, p: &VPath) -> Result<Option<Space>, FsError> {
        let free = leaf_sys::fileattr::free_space(&LocalFs::native(p));
        Ok((free > 0).then_some(Space { free, total: None }))
    }
}

fn list_err(e: leaf_sys::ListError) -> FsError {
    use leaf_sys::ListError as L;
    match e {
        L::NotFound => FsError::NotFound,
        L::AccessDenied => FsError::Denied,
        L::NotReady | L::NetworkPath => FsError::Offline(e.to_string()),
        L::Cancelled => FsError::Cancelled,
        L::Unsupported => FsError::Unsupported,
        other => FsError::Other(other.to_string()),
    }
}

struct LocalSink(std::io::BufWriter<fs::File>);

impl Write for LocalSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl WriteSink for LocalSink {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        self.0.flush()?;
        Ok(())
    }
}
