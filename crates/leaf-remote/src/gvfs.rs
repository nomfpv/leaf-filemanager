//! `smb://host/share/dir` and `nfs://host/export/dir` on Linux, through the desktop's gvfs: the place is mounted with `gio mount`
//! (an `smb://` share that wants a login gets leaf's own password dialog, or the password saved in the keyring, answered on
//! gio's input: `smb::mount_with_login`) and its files are read and written through the folder gvfs
//! makes under `$XDG_RUNTIME_DIR/gvfs`. The address stays the `smb://` form everywhere; only this file knows the disk path.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;

use leaf_vfs::{Caps, Entry, Fs, FsError, LocalFs, Meta, MetaChange, Space, VPath, WriteOpts, WriteSink};

#[derive(Default)]
pub struct GvfsFs {
    /// Addresses already resolved to their gvfs folder (a folder below one of them is joined, not asked again).
    known: Mutex<HashMap<VPath, PathBuf>>,
}

impl GvfsFs {
    /// The gvfs folder of `v`, mounting its place first when needed.
    fn resolve(&self, v: &VPath) -> Result<PathBuf, FsError> {
        {
            let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
            let mut up = Some(v.clone());
            while let Some(a) = up {
                if let Some(base) = known.get(&a) {
                    let rest = v.path().strip_prefix(a.path()).unwrap_or("").trim_start_matches('/');
                    let p = if rest.is_empty() { base.clone() } else { base.join(rest) };
                    // (a mount that went away: the folder is gone, ask again)
                    if base.exists() {
                        return Ok(p);
                    }
                    break;
                }
                up = a.parent();
            }
        }
        let uri = format!("{}://{}{}", v.scheme(), v.authority(), v.path());
        let p = match leaf_sys::network::mount_path(&uri) {
            Ok(p) => p,
            // a share that wants a login: leaf's own dialog (or the saved password), answered on gio's input
            Err(e) if v.scheme() == "smb" && e.kind() == std::io::ErrorKind::PermissionDenied => crate::smb::mount_with_login(v, &uri)?,
            Err(e) => return Err(mount_error(e)),
        };
        self.known.lock().unwrap_or_else(|e| e.into_inner()).insert(v.clone(), p.clone());
        Ok(p)
    }

    fn local(&self, v: &VPath) -> Result<VPath, FsError> {
        Ok(LocalFs::address(&self.resolve(v)?))
    }
}

/// leaf's wording for what `gio` said; its own text goes after the colon.
fn mount_error(e: std::io::Error) -> FsError {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::Unsupported => FsError::Other("Network places need gio (gvfs), which is not installed on this system".into()),
        K::PermissionDenied => FsError::Auth(e.to_string()),
        _ => FsError::Offline(e.to_string()),
    }
}

impl Fs for GvfsFs {
    fn caps(&self) -> Caps {
        Caps { write: true, free_space: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        LocalFs.list(&self.local(dir)?, batch, cancelled, on_batch)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
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
