//! leaf-vfs: one interface for the places that are not local folders (PLAN 2.2, X6).
//!
//! The local folder listing and the Windows copy engine do NOT go through here: local folders keep their direct path
//! (`leaf_sys::list_dir`, `fastcopy`). This crate is for everything else: `mem://` (tests and the demo), and later `sftp://`,
//! `ftp://`, archives. `LocalFs` exists so the generic copy (`copy`) and the shared semantics tests can use a real disk.
//!
//! Every call blocks and is made from a worker thread, never from the UI thread. There is no async runtime here; a backend
//! that needs one (SSH) keeps it inside itself.

pub mod copy;
mod local;
mod mem;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};

pub use local::LocalFs;
pub use mem::MemFs;
pub use rf_core::vpath::{VPath, VPathError};
pub use rf_core::{Entry, EntryKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    Denied,
    /// The target is there already (mkdir, rename without replace, a create-new write).
    Exists,
    /// A folder with something in it, removed without `recursive`.
    NotEmpty,
    /// The backend cannot do this (or the scheme has no backend).
    Unsupported,
    /// The server cannot be reached (the text says what happened).
    Offline(String),
    /// The server refused the login or the key.
    Auth(String),
    Cancelled,
    Other(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::NotFound => f.write_str("Not found"),
            FsError::Denied => f.write_str("Access denied"),
            FsError::Exists => f.write_str("Something with this name is there already"),
            FsError::NotEmpty => f.write_str("The folder is not empty"),
            FsError::Unsupported => f.write_str("Not supported here"),
            FsError::Offline(m) => write!(f, "The server cannot be reached: {m}"),
            FsError::Auth(m) => write!(f, "The server refused the login: {m}"),
            FsError::Cancelled => f.write_str("Cancelled"),
            FsError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> FsError {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => FsError::NotFound,
            K::PermissionDenied => FsError::Denied,
            K::AlreadyExists => FsError::Exists,
            K::DirectoryNotEmpty => FsError::NotEmpty,
            K::Unsupported => FsError::Unsupported,
            K::Interrupted => FsError::Cancelled,
            _ => match e.raw_os_error() {
                // ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS, ERROR_DIR_NOT_EMPTY, ERROR_PATH_NOT_FOUND (Windows)
                #[cfg(windows)]
                Some(80 | 183) => FsError::Exists,
                #[cfg(windows)]
                Some(145) => FsError::NotEmpty,
                #[cfg(windows)]
                Some(3) => FsError::NotFound,
                _ => FsError::Other(e.to_string()),
            },
        }
    }
}

/// What a backend can do (the app greys out what is missing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub write: bool,
    pub watch: bool,
    pub trash: bool,
    /// Unix mode bits through `set_meta`.
    pub permissions: bool,
    /// Modification times through `set_meta` (the copy keeps them when this is set).
    pub set_times: bool,
    /// Symbolic links are shown as links.
    pub links: bool,
    pub hash: bool,
    pub server_copy: bool,
    pub free_space: bool,
    pub exec: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link (or a junction): never followed by `stat`, `list` or the copy.
    Link,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    /// Unix time in milliseconds (0 = unknown).
    pub modified_ms: i64,
    pub created_ms: i64,
    /// Unix mode bits where the backend has them.
    pub mode: Option<u32>,
}

impl Meta {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
    /// The same values an `Entry` of a listing carries.
    pub fn from_entry(e: &Entry) -> Meta {
        let kind = if e.link || e.is_symlink() {
            Kind::Link
        } else if e.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        Meta { kind, size: e.size, modified_ms: e.modified_ms, created_ms: e.created_ms, mode: None }
    }
}

/// What `set_meta` changes (`None`: left as it is).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaChange {
    pub modified_ms: Option<i64>,
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Fails with `Exists` when the file is there.
    CreateNew,
    /// Creates the file, or empties the one that is there.
    Replace,
    /// Writes into an existing file from this byte on (resuming a transfer); the rest of the file is cut off.
    At(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOpts {
    pub mode: WriteMode,
}

impl WriteOpts {
    pub const CREATE_NEW: WriteOpts = WriteOpts { mode: WriteMode::CreateNew };
    pub const REPLACE: WriteOpts = WriteOpts { mode: WriteMode::Replace };
}

/// A file being written. `finish` says whether everything reached the target; dropping it without `finish` leaves what was
/// written so far (the copy removes such a partial file itself).
pub trait WriteSink: Write + Send {
    fn finish(self: Box<Self>) -> Result<(), FsError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub free: u64,
    pub total: Option<u64>,
}

/// What changed in a watched folder: the names, or `rescan` when they cannot be told.
#[derive(Debug, Clone, Default)]
pub struct WatchEvent {
    pub names: Vec<String>,
    pub rescan: bool,
}

pub type WatchCallback = Box<dyn Fn(WatchEvent) + Send + Sync + 'static>;
/// Keeps a watch alive; drop it to stop.
pub type WatchGuard = Box<dyn Send>;

/// One kind of place. All calls block (worker threads only). Paths are absolute in the backend's own space: the scheme and
/// authority of the `VPath` pick the server, its path the place on it.
pub trait Fs: Send + Sync {
    fn caps(&self) -> Caps;

    /// Lists `dir`, `batch` entries at a time, polling `cancelled` between batches (then `Err(Cancelled)`). Links are listed
    /// as links (`Entry::link`), not followed. Returns how many entries were sent.
    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError>;

    /// What `p` is, without following a link.
    fn stat(&self, p: &VPath) -> Result<Meta, FsError>;

    /// The file's bytes from `offset` on.
    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError>;

    /// Writes the file (its folder must exist).
    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError>;

    /// One folder; its parent must exist. `Exists` when something has the name.
    fn mkdir(&self, p: &VPath) -> Result<(), FsError>;

    /// Moves `from` to `to` on the same server. Never replaces anything unless `replace`, and even then only a file: an
    /// existing folder at `to` is `Exists`.
    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError>;

    /// Removes a file, a link (never what it points to) or a folder; a folder with something in it only with `recursive`.
    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError>;

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError>;

    fn watch(&self, _dir: &VPath, _on: WatchCallback) -> Result<WatchGuard, FsError> {
        Err(FsError::Unsupported)
    }

    fn free_space(&self, _p: &VPath) -> Result<Option<Space>, FsError> {
        Ok(None)
    }
}

/// Makes the backend of one scheme. Called once, on the first use of the scheme.
pub type Factory = fn() -> Arc<dyn Fs>;

struct Registry {
    factories: Vec<(String, Factory)>,
    live: HashMap<String, Arc<dyn Fs>>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        let factories: Vec<(String, Factory)> = vec![
            ("mem".into(), || Arc::new(MemFs::with_demo()) as Arc<dyn Fs>),
            ("file".into(), || Arc::new(LocalFs) as Arc<dyn Fs>),
        ];
        Mutex::new(Registry { factories, live: HashMap::new() })
    })
}

/// Adds (or replaces) the backend of a scheme; it is created on its first use.
pub fn register(scheme: &str, f: Factory) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let scheme = scheme.to_ascii_lowercase();
    r.live.remove(&scheme);
    r.factories.retain(|(s, _)| *s != scheme);
    r.factories.push((scheme, f));
}

/// True when some backend serves this scheme.
pub fn has_backend(scheme: &str) -> bool {
    let r = registry().lock().unwrap_or_else(|e| e.into_inner());
    r.factories.iter().any(|(s, _)| s.eq_ignore_ascii_case(scheme))
}

/// The backend of a scheme, created on first use. `Unsupported` for a scheme nobody serves.
pub fn fs_for(scheme: &str) -> Result<Arc<dyn Fs>, FsError> {
    let scheme = scheme.to_ascii_lowercase();
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = r.live.get(&scheme) {
        return Ok(f.clone());
    }
    let make = r.factories.iter().find(|(s, _)| *s == scheme).map(|(_, f)| *f).ok_or(FsError::Unsupported)?;
    let fs = make();
    r.live.insert(scheme, fs.clone());
    Ok(fs)
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}
