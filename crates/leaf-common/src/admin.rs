//! Administrator mode (W3): what leaf and its elevated helper (`leaf-helper`) say to each other, the same on every system.
//!
//! The helper is a separate small program that leaf starts only after the user clicked "Retry as administrator" (one UAC prompt
//! on Windows, `pkexec` on Linux). It does a FIXED list of file operations and nothing else: every request is one of the typed
//! [`Request`]s below, with whole paths that [`check_path`] accepts, and a name that [`check_name`] accepts where a new name is
//! made. There is no request that runs a program, joins strings into a path or names an operation by text.
//!
//! The wire is a sequence of frames: a 4-byte little-endian length (of what follows), one kind byte, then the payload. A
//! [`FRAME_MESSAGE`] payload is one JSON message ([`Request`] from leaf, [`Reply`] from the helper); a [`FRAME_DATA`] payload is raw
//! bytes (the contents of a file read with [`Request::ReadFile`], after its [`Reply::Reading`]). Requests are at most
//! [`MAX_REQUEST`] bytes; anything longer, unknown or malformed ends the conversation.
//!
//! The platform parts (the pipe or socket, who may connect, how a path is opened without following a swapped-in link) are in
//! `leaf-helper` and, for leaf's side, in the backends' `admin` modules (`rf-win`, `leaf-unix`).

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The version of this conversation; the helper refuses another.
pub const PROTOCOL: u32 = 4;
/// The helper's program (its file name without the extension), beside `leaf`.
pub const HELPER_EXE_STEM: &str = "leaf-helper";
/// The longest request frame the helper reads.
pub const MAX_REQUEST: usize = 1 << 20;
/// The longest reply frame leaf reads (a listing of a big folder).
pub const MAX_REPLY: usize = 64 << 20;
/// The largest file [`Request::ReadFile`] returns and [`Request::WriteFromPart`] writes (the editor's files).
pub const MAX_FILE: u64 = 256 << 20;
/// The helper ends after this long without a request (and with nothing running).
pub const IDLE_LIMIT: Duration = Duration::from_secs(5 * 60);
/// One frame of raw bytes carries at most this much.
pub const DATA_CHUNK: usize = 1 << 20;
/// The most items one listing returns (it comes in pages of [`PAGE_ITEMS`]).
pub const MAX_ITEMS: usize = 500_000;
pub const PAGE_ITEMS: usize = 4096;

pub const FRAME_MESSAGE: u8 = 1;
pub const FRAME_DATA: u8 = 2;

/// What to do with a name that already exists at a copy's destination (the copy engine's `Resolve`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Choice {
    Replace,
    Skip,
    KeepBoth,
}

impl From<Choice> for crate::fastcopy::Resolve {
    fn from(c: Choice) -> Self {
        match c {
            Choice::Replace => Self::Replace,
            Choice::Skip => Self::Skip,
            Choice::KeepBoth => Self::KeepBoth,
        }
    }
}

impl From<crate::fastcopy::Resolve> for Choice {
    fn from(r: crate::fastcopy::Resolve) -> Self {
        match r {
            crate::fastcopy::Resolve::Replace => Self::Replace,
            crate::fastcopy::Resolve::Skip => Self::Skip,
            crate::fastcopy::Resolve::KeepBoth => Self::KeepBoth,
        }
    }
}

/// A name that already exists at the destination (the copy engine's `Conflict`, with its times in milliseconds).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clash {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub src_dir: bool,
    pub dst_dir: bool,
    pub src_size: u64,
    pub dst_size: u64,
    pub src_ms: Option<i64>,
    pub dst_ms: Option<i64>,
}

fn to_ms(t: Option<std::time::SystemTime>) -> Option<i64> {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
}

fn from_ms(ms: Option<i64>) -> Option<std::time::SystemTime> {
    ms.filter(|&m| m >= 0).map(|m| std::time::UNIX_EPOCH + Duration::from_millis(m as u64))
}

impl From<&crate::fastcopy::Conflict> for Clash {
    fn from(c: &crate::fastcopy::Conflict) -> Self {
        Self {
            src: c.src.clone(),
            dst: c.dst.clone(),
            src_dir: c.src_dir,
            dst_dir: c.dst_dir,
            src_size: c.src_size,
            dst_size: c.dst_size,
            src_ms: to_ms(c.src_time),
            dst_ms: to_ms(c.dst_time),
        }
    }
}

impl From<&Clash> for crate::fastcopy::Conflict {
    fn from(c: &Clash) -> Self {
        Self {
            src: c.src.clone(),
            dst: c.dst.clone(),
            src_dir: c.src_dir,
            dst_dir: c.dst_dir,
            src_size: c.src_size,
            dst_size: c.dst_size,
            src_time: from_ms(c.src_ms),
            dst_time: from_ms(c.dst_ms),
        }
    }
}

/// One item of a folder listing or a stat (the fields of `rf_core::Entry` that a listing fills).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified_ms: i64,
    pub created_ms: i64,
    pub attrs: u32,
    pub link: bool,
    pub mode: u16,
    pub uid: u32,
}

impl From<&rf_core::Entry> for Item {
    fn from(e: &rf_core::Entry) -> Self {
        Self {
            name: e.name.to_string(),
            dir: e.is_dir(),
            size: e.size,
            modified_ms: e.modified_ms,
            created_ms: e.created_ms,
            attrs: e.attrs,
            link: e.link,
            mode: e.mode,
            uid: e.uid,
        }
    }
}

impl Item {
    pub fn to_entry(&self) -> rf_core::Entry {
        let kind = if self.dir { rf_core::EntryKind::Dir } else { rf_core::EntryKind::File };
        let mut e = rf_core::Entry::new(&self.name, kind, self.size, self.modified_ms, self.created_ms, self.attrs);
        e.link = self.link;
        e.mode = self.mode;
        e.uid = self.uid;
        e
    }
}

/// Everything leaf can ask the helper. The FIXED list: nothing else is understood.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Request {
    /// The first message, and only the first: the token from the helper's command line and the protocol version.
    Hello { token: String, protocol: u32 },
    /// The items of a folder.
    List { dir: PathBuf },
    /// One item (`None` in the reply: it is not there).
    Stat { path: PathBuf },
    /// A new folder `name` in `parent` (fails when the name is taken; no "(2)" is made up here).
    CreateFolder { parent: PathBuf, name: String },
    /// Copies `sources` into the folder `dest` (names that exist there are asked about with [`Reply::Ask`]).
    Copy { sources: Vec<PathBuf>, dest: PathBuf },
    /// Moves `sources` into the folder `dest` (asked like a copy).
    Move { sources: Vec<PathBuf>, dest: PathBuf },
    /// Gives `path` the name `new_name` in the same folder (never over an existing item).
    Rename { path: PathBuf, new_name: String },
    /// Moves `paths` to the Recycle Bin (the Trash).
    Recycle { paths: Vec<PathBuf> },
    /// Deletes `paths` for good. `asked_user` says that leaf asked the user and was told yes; the helper refuses without it.
    DeleteForGood { paths: Vec<PathBuf>, asked_user: bool },
    /// Puts the bytes of the part file `part` (written by leaf, in leaf's own folder) in place of the existing file `target`.
    WriteFromPart { part: PathBuf, target: PathBuf },
    /// The bytes of the file `path` (at most [`MAX_FILE`]).
    ReadFile { path: PathBuf },
    /// The answer to a [`Reply::Ask`] of the operation that is running (`None`: cancel it).
    Answer { choices: Option<Vec<Choice>> },
    /// Stop the operation that is running.
    Cancel,
    /// Leaf is done with the helper: it ends now.
    Bye,
    /// READ ONLY: the health, temperature, wear and power-on hours of one disk by its number, or of every disk (`None`), read
    /// through the storage API. Nothing is written and no command is passed to a drive. The number must be at most
    /// [`crate::drives::MAX_DISK_NUMBER`].
    DriveHealth { disk: Option<u32> },
}

impl Request {
    /// What the operation is called in the helper's log (never the paths).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::List { .. } => "list",
            Self::Stat { .. } => "stat",
            Self::CreateFolder { .. } => "create folder",
            Self::Copy { .. } => "copy",
            Self::Move { .. } => "move",
            Self::Rename { .. } => "rename",
            Self::Recycle { .. } => "recycle",
            Self::DeleteForGood { .. } => "delete",
            Self::WriteFromPart { .. } => "write",
            Self::ReadFile { .. } => "read",
            Self::Answer { .. } => "answer",
            Self::Cancel => "cancel",
            Self::Bye => "bye",
            Self::DriveHealth { .. } => "drive health",
        }
    }
}

/// Why the helper would not do a request (nothing was changed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The first message was not a matching [`Request::Hello`].
    NotGreeted,
    /// The protocol versions differ.
    Protocol,
    /// A path that [`check_path`] does not accept.
    Path(PathRefusal),
    /// A new name that [`check_name`] does not accept.
    Name,
    /// The path does not lead where it says (a link or junction on the way, or a short 8.3 name), or the item is a link where a
    /// file is needed.
    Link,
    /// A file larger than [`MAX_FILE`].
    TooBig,
    /// A permanent delete that leaf did not say it asked the user about.
    NotAsked,
    /// Another operation is running (one at a time).
    Busy,
    /// An answer or a cancel with nothing waiting for it, or a request with no paths.
    Unexpected,
    /// The item is not the kind the request needs (a folder where a file is written, a file where a folder is listed).
    WrongKind,
    /// The Recycle Bin is not used as administrator (a permanent delete can be asked instead).
    NoRecycle,
    /// A disk number above [`crate::drives::MAX_DISK_NUMBER`].
    Disk,
}

/// The helper's messages to leaf.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Reply {
    /// The greeting was accepted.
    Welcome { pid: u32 },
    /// Progress of the running operation, in the units of leaf's own trackers (`total`, `done`), bytes when known.
    Progress { total: u32, done: u32, current: Option<String>, bytes_done: u64, bytes_total: u64 },
    /// Names that already exist at the destination; the operation waits for a [`Request::Answer`].
    Ask { clashes: Vec<Clash> },
    /// One page of a listing; more pages or the last one ([`Reply::Listing`]) follow.
    Page { items: Vec<Item> },
    /// The last page of a listing ([`Client::call`] returns all pages together in it).
    Listing { items: Vec<Item> },
    Stat { item: Option<Item> },
    /// The answer to [`Request::DriveHealth`]: one entry per disk that answered (at most [`crate::drives::MAX_DISK_NUMBER`] + 1).
    Health { disks: Vec<crate::drives::DriveHealth> },
    Created { path: PathBuf },
    /// `len` bytes of the file follow as [`FRAME_DATA`] frames, then [`Reply::Done`].
    Reading { len: u64 },
    /// The operation ended: `ok` false = cancelled or not all done (as leaf's own engines answer `Ok(false)`). `pairs` = (source,
    /// result) of what was done, for leaf's undo list.
    /// `missing`: named items that were already gone (a delete counts them as done, a copy or move skipped them).
    /// `kept`: move sources that stayed (in use by another program, changed during the copy, with data a copy would lose);
    /// `partial`: copies without something of their source (alternate data streams, encryption).
    Done { ok: bool, pairs: Vec<(PathBuf, PathBuf)>, missing: u32, kept: u32, partial: u32 },
    /// Refused before anything was done.
    Refused { why: Refusal },
    /// Tried and failed. `denied`: the system said "access denied" even to the administrator. `detail` is the system's text,
    /// for the details line only.
    Failed { denied: bool, detail: String },
}

// ---------------------------------------------------------------------------------------------------------------- paths

/// Why a path is not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathRefusal {
    Empty,
    /// Not a whole path from a drive's root (`C:\…`) or from `/`.
    Relative,
    /// A `..` or `.` part.
    Dots,
    /// A network path, a device path (`\\?\`, `\\.\`), or a drive-relative one (`C:foo`).
    NotLocal,
    /// A part with characters Windows treats specially: `:` (an alternate data stream), `* ? " < > |`, control characters, a
    /// trailing dot or space, a device name (`CON`, `NUL`, `COM1`…), or an empty part (`a\\b`).
    BadPart,
    /// The root itself (a drive or `/`): no operation works on it as an item.
    Root,
    TooLong,
}

/// The longest path accepted (Windows' own limit for `\\?\` paths, in UTF-16 units; bytes on Linux).
const MAX_PATH_LEN: usize = 32_000;

/// Accepts only a whole, plain, local path: absolute, every part a normal name, no `.` or `..`, no network or device prefix, and on
/// Windows no part that Windows would read as something else (see [`PathRefusal::BadPart`]). Checked on the text as given, not on a
/// normalised form (`Path::components` quietly drops `.` parts and doubled separators; here they are refused).
///
/// This is the first check only: the helper then opens the path so that a link or junction swapped in later cannot redirect it.
pub fn check_path(p: &Path) -> Result<(), PathRefusal> {
    let Some(s) = p.to_str() else { return Err(PathRefusal::BadPart) };
    if s.is_empty() {
        return Err(PathRefusal::Empty);
    }
    if s.len() > MAX_PATH_LEN {
        return Err(PathRefusal::TooLong);
    }
    check_text(s, cfg!(windows))
}

/// [`check_path`] on the text, with Windows' rules or Unix' rules (a separate function so both sets are tested on every system).
fn check_text(s: &str, windows: bool) -> Result<(), PathRefusal> {
    let rest = if windows {
        let b = s.as_bytes();
        if s.starts_with(r"\\") || s.starts_with("//") || s.starts_with(r"\/") || s.starts_with(r"/\") {
            return Err(PathRefusal::NotLocal);
        }
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            if b.len() == 2 {
                return Err(PathRefusal::NotLocal);
            }
            if b[2] != b'\\' && b[2] != b'/' {
                return Err(PathRefusal::NotLocal);
            }
            &s[3..]
        } else {
            return Err(PathRefusal::Relative);
        }
    } else {
        match s.strip_prefix('/') {
            Some(r) => r,
            None => return Err(PathRefusal::Relative),
        }
    };
    if rest.is_empty() {
        return Err(PathRefusal::Root);
    }
    let parts: Vec<&str> = if windows { rest.split(['\\', '/']).collect() } else { rest.split('/').collect() };
    // (one trailing separator is tolerated: "C:\Program Files\" is the folder)
    let n = parts.len();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            if i + 1 == n && n > 1 {
                continue;
            }
            return Err(PathRefusal::BadPart);
        }
        if *part == "." || *part == ".." {
            return Err(PathRefusal::Dots);
        }
        if windows && !windows_part_ok(part) {
            return Err(PathRefusal::BadPart);
        }
        if !windows && part.contains('\0') {
            return Err(PathRefusal::BadPart);
        }
    }
    Ok(())
}

/// One part of a Windows path that means exactly its text.
fn windows_part_ok(part: &str) -> bool {
    if part.chars().any(|c| (c as u32) < 0x20 || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return false;
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return false;
    }
    // device names, with or without an extension ("nul.txt" is NUL)
    let stem = part.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if DEVICES.contains(&stem.as_str()) {
        return false;
    }
    // COM0-9 and LPT0-9, and with a superscript digit too ("COM¹")
    if (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.chars().count() == 4 {
        let last = stem.chars().last().unwrap_or(' ');
        if last.is_ascii_digit() || matches!(last, '¹' | '²' | '³') {
            return false;
        }
    }
    true
}

/// Accepts a new name for one item (a rename, a new folder): one plain part that [`check_path`] would accept inside a path, with
/// no separator in it.
pub fn check_name(name: &str) -> Result<(), PathRefusal> {
    if name.is_empty() {
        return Err(PathRefusal::Empty);
    }
    if name.len() > 255 * 3 {
        return Err(PathRefusal::TooLong);
    }
    if name.contains('/') || (cfg!(windows) && name.contains('\\')) {
        return Err(PathRefusal::BadPart);
    }
    check_path(&root().join(name)).map_err(|e| if e == PathRefusal::Root { PathRefusal::Empty } else { e })
}

fn root() -> PathBuf {
    if cfg!(windows) { PathBuf::from(r"C:\") } else { PathBuf::from("/") }
}

/// The parent folder and the last part of an accepted path (`None` for a root).
pub fn split(p: &Path) -> Option<(&Path, &std::ffi::OsStr)> {
    let name = p.file_name()?;
    let parent = p.parent()?;
    // (`file_name` of "C:\x\.." would be None already; a path that passed `check_path` has no such parts)
    if p.components().next_back().is_some_and(|c| matches!(c, Component::Normal(_))) { Some((parent, name)) } else { None }
}

/// The paths of a request (for the checks every request goes through).
pub fn paths_of(r: &Request) -> Vec<&Path> {
    match r {
        Request::List { dir } => vec![dir],
        Request::Stat { path } | Request::ReadFile { path } | Request::Rename { path, .. } => vec![path],
        Request::CreateFolder { parent, .. } => vec![parent],
        Request::Copy { sources, dest } | Request::Move { sources, dest } => sources.iter().map(|p| p.as_path()).chain(std::iter::once(dest.as_path())).collect(),
        Request::Recycle { paths } | Request::DeleteForGood { paths, .. } => paths.iter().map(|p| p.as_path()).collect(),
        Request::WriteFromPart { part, target } => vec![part, target],
        Request::Hello { .. } | Request::Answer { .. } | Request::Cancel | Request::Bye | Request::DriveHealth { .. } => Vec::new(),
    }
}

/// The syntax checks of a request that touches files, before anything is opened: every path through [`check_path`], every new
/// name through [`check_name`], lists not empty, a permanent delete only when leaf asked the user.
pub fn check_request(r: &Request) -> Result<(), Refusal> {
    for p in paths_of(r) {
        check_path(p).map_err(Refusal::Path)?;
    }
    match r {
        Request::CreateFolder { name, .. } => check_name(name).map_err(|_| Refusal::Name)?,
        Request::Rename { new_name, .. } => check_name(new_name).map_err(|_| Refusal::Name)?,
        Request::DeleteForGood { asked_user: false, .. } => return Err(Refusal::NotAsked),
        Request::DriveHealth { disk: Some(n) } if *n > crate::drives::MAX_DISK_NUMBER => return Err(Refusal::Disk),
        _ => {}
    }
    match r {
        Request::Copy { sources, .. } | Request::Move { sources, .. } if sources.is_empty() => Err(Refusal::Unexpected),
        Request::Recycle { paths } | Request::DeleteForGood { paths, .. } if paths.is_empty() => Err(Refusal::Unexpected),
        _ => Ok(()),
    }
}

/// Compares two tokens in time that does not depend on where they differ.
pub fn same_token(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 32 random bytes as 64 hex digits, from the bytes the system's random source gave (`rf-win`: BCryptGenRandom; `leaf-unix`:
/// getrandom).
pub fn token_text(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A token from a command line: exactly 64 lower-case hex digits.
pub fn token_ok(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ---------------------------------------------------------------------------------------------------------------- frames

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len() + 1).map_err(|_| std::io::Error::other("frame too long"))?;
    let mut head = [0u8; 5];
    head[..4].copy_from_slice(&len.to_le_bytes());
    head[4] = kind;
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

/// One frame: its kind and payload. A frame longer than `max`, or of length 0, is an error (the conversation ends).
pub fn read_frame(r: &mut impl Read, max: usize) -> std::io::Result<(u8, Vec<u8>)> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > max + 1 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame length out of range"));
    }
    let mut kind = [0u8; 1];
    r.read_exact(&mut kind)?;
    let mut body = vec![0u8; len - 1];
    r.read_exact(&mut body)?;
    Ok((kind[0], body))
}

pub fn send(w: &mut impl Write, m: &impl Serialize) -> std::io::Result<()> {
    let body = serde_json::to_vec(m).map_err(std::io::Error::other)?;
    write_frame(w, FRAME_MESSAGE, &body)
}

/// One message (a data frame here is an error).
pub fn recv<T: for<'de> Deserialize<'de>>(r: &mut impl Read, max: usize) -> std::io::Result<T> {
    let (kind, body) = read_frame(r, max)?;
    if kind != FRAME_MESSAGE {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "unexpected frame kind"));
    }
    serde_json::from_slice(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------------------------------------------- leaf's side

/// What went wrong talking to the helper, in words leaf can show (the system's text goes to the details line).
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("The administrator helper refused this ({0:?})")]
    Refused(Refusal),
    #[error("Access was denied even to the administrator")]
    Denied(String),
    #[error("The operation failed as administrator")]
    Failed(String),
    #[error("The administrator helper is not running any more")]
    Gone(#[source] std::io::Error),
    #[error("The administrator helper sent an answer leaf did not expect")]
    Confused,
}

/// Callbacks of one call: progress, and the question about names that exist (`None` from it cancels).
pub struct Calls<'a> {
    pub progress: &'a (dyn Fn(u32, u32, Option<String>, u64, u64) + Sync),
    pub ask: &'a (dyn Fn(&[Clash]) -> Option<Vec<Choice>> + Sync),
    pub cancel: &'a AtomicBool,
}

/// The conversation with a running helper, the same on every system: one reading end and one writing end (two pipes on Windows,
/// one socket on Linux), one call at a time. A cancel is written while a call waits for its answer.
pub struct Client {
    reader: Mutex<Box<dyn Read + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    broken: AtomicBool,
    /// One call at a time: a second caller waits for the first call's final reply (replies never cross).
    calls: Mutex<()>,
}

impl Client {
    /// Greets the helper with `token` and waits for its welcome. Returns the helper's process id.
    pub fn greet(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>, token: &str) -> Result<(Self, u32), HelperError> {
        let c = Self { reader: Mutex::new(reader), writer: Mutex::new(writer), broken: AtomicBool::new(false), calls: Mutex::new(()) };
        c.write(&Request::Hello { token: token.to_string(), protocol: PROTOCOL })?;
        match c.read()? {
            Reply::Welcome { pid } => Ok((c, pid)),
            Reply::Refused { why } => Err(HelperError::Refused(why)),
            _ => Err(HelperError::Confused),
        }
    }

    /// The pipe or socket failed once: the helper is gone for leaf.
    pub fn broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    fn write(&self, r: &Request) -> Result<(), HelperError> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        send(&mut *w, r).map_err(|e| {
            self.broken.store(true, Ordering::Relaxed);
            HelperError::Gone(e)
        })
    }

    fn read(&self) -> Result<Reply, HelperError> {
        let mut r = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        recv(&mut *r, MAX_REPLY).map_err(|e| {
            self.broken.store(true, Ordering::Relaxed);
            HelperError::Gone(e)
        })
    }

    /// Says goodbye (the helper ends).
    pub fn bye(&self) {
        let _ = self.write(&Request::Bye);
        self.broken.store(true, Ordering::Relaxed);
    }

    /// Sends `req` and returns its final reply ([`Reply::Done`], [`Reply::Listing`], …); progress and questions on the way go to
    /// `calls`. A set `calls.cancel` is passed on to the helper. A [`Request::ReadFile`] returns its bytes in `data`.
    pub fn call(&self, req: &Request, calls: &Calls, data: &mut Vec<u8>) -> Result<Reply, HelperError> {
        let _one = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        self.write(req)?;
        let finished = AtomicBool::new(false);
        std::thread::scope(|s| {
            // the cancel goes over the writing end while this thread waits on the reading end
            s.spawn(|| {
                while !finished.load(Ordering::Relaxed) {
                    if calls.cancel.load(Ordering::Relaxed) {
                        let _ = self.write(&Request::Cancel);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let r = self.answers(calls, data);
            finished.store(true, Ordering::Relaxed);
            r
        })
    }

    fn answers(&self, calls: &Calls, data: &mut Vec<u8>) -> Result<Reply, HelperError> {
        let mut pages: Vec<Item> = Vec::new();
        loop {
            match self.read()? {
                Reply::Page { items } => {
                    pages.extend(items);
                    if pages.len() > MAX_ITEMS {
                        self.broken.store(true, Ordering::Relaxed);
                        return Err(HelperError::Confused);
                    }
                }
                Reply::Listing { items } => {
                    pages.extend(items);
                    return Ok(Reply::Listing { items: pages });
                }
                Reply::Progress { total, done, current, bytes_done, bytes_total } => (calls.progress)(total, done, current, bytes_done, bytes_total),
                Reply::Ask { clashes } => {
                    let choices = (calls.ask)(&clashes);
                    self.write(&Request::Answer { choices })?;
                }
                Reply::Reading { len } => {
                    if len > MAX_FILE {
                        return Err(HelperError::Confused);
                    }
                    data.clear();
                    let mut r = self.reader.lock().unwrap_or_else(|e| e.into_inner());
                    while (data.len() as u64) < len {
                        let (kind, body) = read_frame(&mut *r, DATA_CHUNK).map_err(|e| {
                            self.broken.store(true, Ordering::Relaxed);
                            HelperError::Gone(e)
                        })?;
                        if kind != FRAME_DATA || data.len() as u64 + body.len() as u64 > len {
                            self.broken.store(true, Ordering::Relaxed);
                            return Err(HelperError::Confused);
                        }
                        data.extend_from_slice(&body);
                    }
                }
                Reply::Refused { why } => return Err(HelperError::Refused(why)),
                Reply::Failed { denied: true, detail } => return Err(HelperError::Denied(detail)),
                Reply::Failed { denied: false, detail } => return Err(HelperError::Failed(detail)),
                Reply::Welcome { .. } => return Err(HelperError::Confused),
                done => return Ok(done),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_path_validation_refuses_relative_dots_and_odd_parts() {
        // Windows' rules
        let w = |s: &str| check_text(s, true);
        assert_eq!(w(r"C:\Program Files\leaf"), Ok(()));
        assert_eq!(w(r"C:\Program Files\leaf\"), Ok(()));
        assert_eq!(w(r"Program Files\leaf"), Err(PathRefusal::Relative));
        assert_eq!(w(r"\Program Files"), Err(PathRefusal::Relative));
        assert_eq!(w(r"C:Program Files"), Err(PathRefusal::NotLocal));
        assert_eq!(w(r"C:\Program Files\..\Windows"), Err(PathRefusal::Dots));
        assert_eq!(w(r"C:\Program Files\.\leaf"), Err(PathRefusal::Dots));
        assert_eq!(w(r"\\server.example.com\share\x"), Err(PathRefusal::NotLocal));
        assert_eq!(w(r"\\?\C:\Windows"), Err(PathRefusal::NotLocal));
        assert_eq!(w(r"\\.\PhysicalDrive0"), Err(PathRefusal::NotLocal));
        assert_eq!(w(r"C:\x\file.txt:stream"), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\x\nul.txt"), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\x\COM1"), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\x\trailing."), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\x\trailing "), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\x\\y"), Err(PathRefusal::BadPart));
        assert_eq!(w(r"C:\"), Err(PathRefusal::Root));
        assert_eq!(w(r"C:\x\COM10"), Ok(()));
        // Unix rules
        let u = |s: &str| check_text(s, false);
        assert_eq!(u("/usr/local/share"), Ok(()));
        assert_eq!(u("usr/local"), Err(PathRefusal::Relative));
        assert_eq!(u("/usr/../etc/shadow"), Err(PathRefusal::Dots));
        assert_eq!(u("/usr/./local"), Err(PathRefusal::Dots));
        assert_eq!(u("/usr//local"), Err(PathRefusal::BadPart));
        assert_eq!(u("/"), Err(PathRefusal::Root));
        // names
        assert!(check_name("new folder").is_ok());
        assert!(check_name("..").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("").is_err());
        // a request with a bad path is refused before anything is opened
        let r = Request::Move { sources: vec![PathBuf::from("relative")], dest: root().join("x") };
        assert_eq!(check_request(&r), Err(Refusal::Path(PathRefusal::Relative)));
        let r = Request::DeleteForGood { paths: vec![root().join("x")], asked_user: false };
        assert_eq!(check_request(&r), Err(Refusal::NotAsked));
    }

    #[test]
    fn admin_frames_round_trip_and_refuse_oversize() {
        let mut buf = Vec::new();
        send(&mut buf, &Request::Stat { path: root().join("x") }).unwrap();
        let back: Request = recv(&mut buf.as_slice(), MAX_REQUEST).unwrap();
        assert!(matches!(back, Request::Stat { .. }));
        let mut big = Vec::new();
        write_frame(&mut big, FRAME_MESSAGE, &vec![b' '; 100]).unwrap();
        assert!(recv::<Request>(&mut big.as_slice(), 50).is_err());
        // an unknown operation is not understood
        let mut odd = Vec::new();
        write_frame(&mut odd, FRAME_MESSAGE, br#"{"Run":{"command":"x"}}"#).unwrap();
        assert!(recv::<Request>(&mut odd.as_slice(), MAX_REQUEST).is_err());
    }

    #[test]
    fn a_drive_health_request_takes_no_disk_or_a_small_disk_number_and_nothing_else() {
        assert_eq!(check_request(&Request::DriveHealth { disk: None }), Ok(()));
        assert_eq!(check_request(&Request::DriveHealth { disk: Some(0) }), Ok(()));
        assert_eq!(check_request(&Request::DriveHealth { disk: Some(crate::drives::MAX_DISK_NUMBER) }), Ok(()));
        assert_eq!(check_request(&Request::DriveHealth { disk: Some(crate::drives::MAX_DISK_NUMBER + 1) }), Err(Refusal::Disk));
        assert_eq!(check_request(&Request::DriveHealth { disk: Some(u32::MAX) }), Err(Refusal::Disk));
        // a text where a number is expected, a negative number and an extra field are not understood
        for bad in [r#"{"DriveHealth":{"disk":"C:"}}"#, r#"{"DriveHealth":{"disk":-1}}"#, r#"{"DriveHealth":{"disk":1,"path":"C:\\"}}"#] {
            let mut buf = Vec::new();
            write_frame(&mut buf, FRAME_MESSAGE, bad.as_bytes()).unwrap();
            assert!(recv::<Request>(&mut buf.as_slice(), MAX_REQUEST).is_err(), "{bad}");
        }
    }
}
