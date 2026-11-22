//! `smb://host/share/dir` and `nfs://host/export/dir` on Linux, through the desktop's own mount: gvfs (`gio mount`, when the gvfs
//! backend of the scheme is installed) or, on KDE, kio-fuse (see `leaf_sys::network::mount_path`, which picks). An `smb://` share
//! that wants a login gets leaf's own password dialog or the password saved in the keyring (gvfs route, `smb::mount_with_login`),
//! and its files are read and written through the folder the mount makes. The address stays the `smb://` form everywhere; only
//! this file knows the disk path. The server's own top level is made up here: `smb://host/` lists the shares (`smbclient -L`), and
//! `nfs://host/` lists the exports (`showmount -e`) as folders that lead down to each export, which is what gets mounted.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, LocalFs, Meta, MetaChange, Space, VPath, WriteOpts, WriteSink};

#[derive(Default)]
pub struct GvfsFs {
    /// Addresses already resolved to their gvfs folder (a folder below one of them is joined, not asked again).
    known: Mutex<HashMap<VPath, PathBuf>>,
    /// The exports each NFS server listed, and when (a minute: moving down the made-up folders asks once).
    exports: Mutex<HashMap<String, (Instant, Vec<String>)>>,
}

/// What an NFS path is, given the server's exports.
#[derive(Debug, PartialEq, Eq)]
enum NfsPlace {
    /// Inside this export (its first `n` path segments): mount it, the rest is a folder in it.
    Inside(usize),
    /// Above the exports: a made-up folder holding these names.
    Above(Vec<String>),
    /// Nothing the server exports.
    Nowhere,
}

/// Where `segments` (the path of an `nfs://` address) lies among `exports` (full paths such as `/volume1/data`): in the longest
/// export that starts it, else above some exports (the names that lead on to them), else nowhere.
fn nfs_place(exports: &[String], segments: &[&str]) -> NfsPlace {
    let split = |e: &str| e.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let mut inside = None;
    let mut names: Vec<String> = Vec::new();
    for e in exports {
        let es = split(e);
        if es.len() <= segments.len() && es.iter().zip(segments).all(|(a, b)| a == b) {
            inside = inside.max(Some(es.len()));
        } else if es.len() > segments.len() && es.iter().zip(segments).all(|(a, b)| a == b) {
            names.push(es[segments.len()].clone());
        }
    }
    names.sort();
    names.dedup();
    match inside {
        Some(n) => NfsPlace::Inside(n),
        None if !names.is_empty() => NfsPlace::Above(names),
        None => NfsPlace::Nowhere,
    }
}

fn dir_entries(names: &[String]) -> Vec<Entry> {
    names.iter().map(|n| Entry::new(n, EntryKind::Dir, 0, 0, 0, 0x10)).collect()
}

impl GvfsFs {
    /// The folder of `v` below an address that is mounted already.
    fn known_folder(&self, v: &VPath) -> Option<PathBuf> {
        let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        let mut up = Some(v.clone());
        while let Some(a) = up {
            if let Some(base) = known.get(&a) {
                let rest = v.path().strip_prefix(a.path()).unwrap_or("").trim_start_matches('/');
                // (a mount that went away: the folder is gone, ask again)
                if base.exists() {
                    return Some(if rest.is_empty() { base.clone() } else { base.join(rest) });
                }
                return None;
            }
            up = a.parent();
        }
        None
    }

    /// The exports `host` lists (`None`: the list cannot be had; an `Err` says why, in leaf's words, when the address is the
    /// server's top).
    fn exports_of(&self, v: &VPath) -> Result<Option<Vec<String>>, FsError> {
        let host = v.host().to_string();
        if let Some((at, list)) = self.exports.lock().unwrap_or_else(|e| e.into_inner()).get(&host) {
            if at.elapsed() < Duration::from_secs(60) {
                return Ok(Some(list.clone()));
            }
        }
        match leaf_sys::network::nfs_exports(&host) {
            Ok(list) if !list.is_empty() => {
                self.exports.lock().unwrap_or_else(|e| e.into_inner()).insert(host, (Instant::now(), list.clone()));
                Ok(Some(list))
            }
            // (the server lists nothing, or this system cannot ask: an address that names its export still mounts as typed)
            r => {
                if cfg!(unix) && v.path().trim_matches('/').is_empty() {
                    let why = match r {
                        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => "this computer cannot list them (the nfs-utils package provides showmount)".to_string(),
                        Err(e) => e.to_string(),
                        Ok(_) => "the server lists none".to_string(),
                    };
                    return Err(FsError::Other(format!("The exports of this NFS server cannot be listed: {why}. Type the export after the server, for example nfs://nas.example.com/volume1/data")));
                }
                Ok(None)
            }
        }
    }

    /// The folders leaf makes up itself: the shares of `smb://host/` and the folders above an NFS server's exports. `None`: a real
    /// place (or one that cannot be told, which the mount then answers).
    fn made_up(&self, v: &VPath) -> Result<Option<Vec<Entry>>, FsError> {
        if self.known_folder(v).is_some() {
            return Ok(None);
        }
        match v.scheme() {
            "smb" if v.path().trim_matches('/').is_empty() => match leaf_sys::network::shares(v.host()) {
                Ok(list) => Ok(Some(dir_entries(&list.into_iter().map(|s| s.name).collect::<Vec<_>>()))),
                // (no smbclient, or a server that wants a login before it lists: the desktop's own browsing may still do it)
                Err(e) if matches!(e.kind(), std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput) => Ok(None),
                Err(e) => Err(FsError::Offline(e.to_string())),
            },
            "nfs" => {
                let Some(exports) = self.exports_of(v)? else { return Ok(None) };
                let segments: Vec<&str> = v.segments().collect();
                match nfs_place(&exports, &segments) {
                    NfsPlace::Above(names) => Ok(Some(dir_entries(&names))),
                    NfsPlace::Inside(_) => Ok(None),
                    NfsPlace::Nowhere => Err(FsError::Other(format!("This NFS server does not export {}. Its exports: {}", v.path(), exports.join(", ")))),
                }
            }
            _ => Ok(None),
        }
    }

    /// The gvfs folder of `v`, mounting its place first when needed.
    fn resolve(&self, v: &VPath) -> Result<PathBuf, FsError> {
        if let Some(p) = self.known_folder(v) {
            return Ok(p);
        }
        // an NFS address is mounted at its export, not at the folder typed below it
        let mut at = v.clone();
        if v.scheme() == "nfs" {
            if let Some(exports) = self.exports_of(v)? {
                let segments: Vec<&str> = v.segments().collect();
                match nfs_place(&exports, &segments) {
                    NfsPlace::Inside(n) => at = v.with_path(&format!("/{}", segments[..n].join("/"))),
                    _ => return Err(FsError::Other("This is a list of NFS exports, not a folder".into())),
                }
            }
        }
        let uri = format!("{}://{}{}", at.scheme(), at.authority(), at.path());
        let p = match leaf_sys::network::mount_path(&uri) {
            Ok(p) => p,
            // a share that wants a login: leaf's own dialog (or the saved password), answered on gio's input
            #[cfg(unix)]
            Err(e) if v.scheme() == "smb" && e.kind() == std::io::ErrorKind::PermissionDenied => crate::smb::mount_with_login(v, &uri)?,
            Err(e) => return Err(mount_error(e)),
        };
        self.known.lock().unwrap_or_else(|e| e.into_inner()).insert(at.clone(), p.clone());
        let rest = v.path().strip_prefix(at.path()).unwrap_or("").trim_start_matches('/');
        Ok(if rest.is_empty() { p } else { p.join(rest) })
    }

    fn local(&self, v: &VPath) -> Result<VPath, FsError> {
        Ok(LocalFs::address(&self.resolve(v)?))
    }
}

/// leaf's wording for what the mount said; the system's own text goes after the colon.
fn mount_error(e: std::io::Error) -> FsError {
    use std::io::ErrorKind as K;
    match e.kind() {
        // (Windows: leaf's own sentence about the Client for NFS feature; Linux: which packages are missing)
        K::Unsupported if cfg!(windows) || e.get_ref().is_some() => FsError::Other(e.to_string()),
        K::Unsupported => FsError::Other("Network places need gio (gvfs), which is not installed on this system".into()),
        // (leaf's own sentence, with what to do)
        K::InvalidInput => FsError::Other(e.to_string()),
        K::PermissionDenied => FsError::Auth(e.to_string()),
        _ => FsError::Offline(e.to_string()),
    }
}

impl Fs for GvfsFs {
    fn caps(&self) -> Caps {
        Caps { write: true, free_space: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        if let Some(entries) = self.made_up(dir)? {
            let n = entries.len();
            on_batch(entries);
            return Ok(n);
        }
        LocalFs.list(&self.local(dir)?, batch, cancelled, on_batch)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        if self.made_up(p)?.is_some() {
            return Ok(Meta { kind: Kind::Dir, size: 0, modified_ms: 0, created_ms: 0, mode: None });
        }
        LocalFs.stat(&self.local(p)?)
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        LocalFs.open_read(&self.local(p)?, offset)
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        LocalFs.open_write(&self.local(p)?, o)
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        LocalFs.mkdir(&self.local(p)?)
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        LocalFs.rename(&self.local(from)?, &self.local(to)?, replace)
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        LocalFs.remove(&self.local(p)?, recursive)
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        LocalFs.set_meta(&self.local(p)?, m)
    }

    fn free_space(&self, p: &VPath) -> Result<Option<Space>, FsError> {
        LocalFs.free_space(&self.local(p)?)
    }
}
