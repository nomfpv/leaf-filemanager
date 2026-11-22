//! `RcloneFs` (B6): any storage `rclone` can reach, behind `leaf-vfs`, as `rclone://<remote>/<path>`. Only registered when the
//! `rclone` program is installed. leaf starts it with argument lists (never a shell) and reads its JSON (`lsjson`); the
//! remotes come from `rclone listremotes`. leaf never reads, copies or stores rclone's config or its secrets: rclone does the
//! logins itself, from its own config file. WebDAV (X9) is reached this way: `rclone config` makes a `webdav` remote.
//!
//! Folders: `lsjson`; files: `cat --offset` to read, a temp file then `copyto` to write; `mkdir`, `moveto`, `deletefile`,
//! `rmdir` and `purge` for the rest.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, VPath, WriteMode, WriteOpts, WriteSink};

/// The first argument of every run: rclone must never ask for the config password on a terminal that is not there.
const GLOBAL: &str = "--ask-password=false";

/// The `rclone` program on the PATH, when there is one.
pub fn find_exe() -> Option<PathBuf> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| {
        let names: &[&str] = if cfg!(windows) { &["rclone.exe"] } else { &["rclone"] };
        std::env::split_paths(&std::env::var_os("PATH")?).find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
    })
    .clone()
}

fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.arg(GLOBAL).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// The remotes rclone has configured (`rclone listremotes`), names without the colon. Blocks: a worker thread.
pub fn list_remotes() -> Vec<String> {
    let Some(exe) = find_exe() else { return Vec::new() };
    let Ok(out) = leaf_proc::output_helper(command(&exe).arg("listremotes"), Some(std::time::Duration::from_secs(30))) else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().strip_suffix(':')).filter(|n| valid_remote(n)).map(str::to_string).collect()
}

/// The remotes, read once in the background the first time this is asked (empty until that is done; ask again later).
pub fn remotes_cached() -> Vec<String> {
    static CELL: OnceLock<std::sync::Mutex<Option<Vec<String>>>> = OnceLock::new();
    let cell = CELL.get_or_init(|| {
        let m = std::sync::Mutex::new(None);
        std::thread::Builder::new()
            .name("rclone-remotes".into())
            .spawn(|| {
                let r = list_remotes();
                if let Some(c) = CELL.get() {
                    *c.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                }
            })
            .ok();
        m
    });
    cell.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// A remote's name as leaf passes it on: not empty, no colon or control character, and not starting with `-` (it would be read
/// as an option).
fn valid_remote(n: &str) -> bool {
    !n.is_empty() && !n.starts_with('-') && !n.contains(':') && !n.chars().any(char::is_control)
}

/// `remote:path` for rclone (the root is `remote:`).
fn spec(v: &VPath) -> Result<String, FsError> {
    let remote = v.authority();
    if !valid_remote(remote) {
        return Err(FsError::Other("The rclone remote's name is not valid".into()));
    }
    Ok(format!("{remote}:{}", v.path().trim_start_matches('/')))
}

/// The argument list that deletes one file (`rclone deletefile remote:path`); a root or a bad remote name is refused here.
pub(crate) fn deletefile_args(v: &VPath) -> Result<Vec<String>, FsError> {
    if v.is_root() {
        return Err(FsError::Denied);
    }
    Ok(vec![GLOBAL.into(), "deletefile".into(), spec(v)?])
}

pub struct RcloneFs {
    exe: PathBuf,
}

impl RcloneFs {
    pub fn new(exe: PathBuf) -> RcloneFs {
        RcloneFs { exe }
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, FsError> {
        let out = leaf_proc::output_helper(command(&self.exe).args(args), None).map_err(|e| FsError::Other(format!("rclone could not be started: {e}")))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        Err(failure(out.status.code(), &String::from_utf8_lossy(&out.stderr)))
    }

    fn run_full(&self, args: &[String]) -> Result<Vec<u8>, FsError> {
        let refs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
        self.run(&refs)
    }

    fn exists(&self, v: &VPath) -> Result<bool, FsError> {
        match self.stat(v) {
            Ok(_) => Ok(true),
            Err(FsError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// rclone's exit codes: 3 directory not found, 4 file not found; the rest is the last line of what it said.
fn failure(code: Option<i32>, stderr: &str) -> FsError {
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("rclone failed").trim().to_string();
    let low = last.to_ascii_lowercase();
    match code {
        Some(3 | 4) => FsError::NotFound,
        _ if low.contains("not found") && !low.contains("section") => FsError::NotFound,
        _ if low.contains("permission denied") || low.contains("access denied") || low.contains("forbidden") => FsError::Denied,
        _ if low.contains("directory not empty") => FsError::NotEmpty,
        _ if low.contains("couldn't connect") || low.contains("no such host") || low.contains("i/o timeout") || low.contains("network is unreachable") => FsError::Offline(last),
        _ => FsError::Other(last),
    }
}

#[derive(serde::Deserialize)]
struct Item {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Size", default)]
    size: i64,
    #[serde(rename = "ModTime", default)]
    mod_time: String,
    #[serde(rename = "IsDir", default)]
    is_dir: bool,
}

/// An RFC 3339 time (`2026-11-21T16:46:22.123456789Z`, or with an offset) as Unix milliseconds; 0 when it cannot be read.
fn parse_time(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s).map_or(0, |t| t.timestamp_millis())
}

fn entry_of(i: &Item) -> Entry {
    let m = parse_time(&i.mod_time);
    if i.is_dir {
        Entry::new(&i.name, EntryKind::Dir, 0, m, m, rf_core::attrs::DIRECTORY)
    } else {
        Entry::new(&i.name, EntryKind::File, i.size.max(0) as u64, m, m, 0)
    }
}

/// What `lsjson` printed for a folder, as entries; the names that cannot be one part of an address are left out.
fn parse_list(json: &[u8]) -> Result<Vec<Entry>, FsError> {
    let items: Vec<Item> = serde_json::from_slice(json).map_err(|e| FsError::Other(format!("rclone's listing could not be read: {e}")))?;
    Ok(items.iter().filter(|i| leaf_vfs::names::address_problem(&i.name).is_none()).map(entry_of).collect())
}

impl Fs for RcloneFs {
    fn caps(&self) -> Caps {
        Caps { write: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let s = spec(dir)?;
        let out = self.run(&["lsjson", "--no-mimetype", &s])?;
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        let all = parse_list(&out)?;
        let n = all.len();
        for part in all.chunks(batch.max(1)) {
            on_batch(part.to_vec());
        }
        Ok(n)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        let s = spec(p)?;
        if p.is_root() {
            self.run(&["lsjson", "--no-mimetype", "--max-depth", "1", "--files-only", &s])?;
            return Ok(Meta { kind: Kind::Dir, size: 0, modified_ms: 0, created_ms: 0, mode: None });
        }
        let out = self.run(&["lsjson", "--stat", "--no-mimetype", &s])?;
        let i: Item = serde_json::from_slice(&out).map_err(|_| FsError::NotFound)?;
        let m = parse_time(&i.mod_time);
        Ok(Meta { kind: if i.is_dir { Kind::Dir } else { Kind::File }, size: i.size.max(0) as u64, modified_ms: m, created_ms: 0, mode: None })
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let s = spec(p)?;
        let mut child = leaf_proc::spawn_helper(
            command(&self.exe).args(["cat", "--offset", &offset.to_string(), &s]).stdout(Stdio::piped()).stderr(Stdio::null()),
        )
        .map_err(|e| FsError::Other(format!("rclone could not be started: {e}")))?;
        let stdout = child.stdout.take().ok_or_else(|| FsError::Other("rclone gave no output".into()))?;
        Ok(Box::new(CatReader { child, stdout }))
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        let s = spec(p)?;
        match o.mode {
            WriteMode::At(_) => return Err(FsError::Unsupported),
            WriteMode::CreateNew if self.exists(p)? => return Err(FsError::Exists),
            _ => {}
        }
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::temp_dir().join(format!("leaf-rclone-{}-{}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        Ok(Box::new(TempSink { file: Some(std::io::BufWriter::new(file)), tmp, exe: self.exe.clone(), spec: s }))
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        let s = spec(p)?;
        if self.exists(p)? {
            return Err(FsError::Exists);
        }
        self.run(&["mkdir", &s]).map(|_| ())
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if to.starts_with(from) && to != from {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        if self.exists(to)? && (!replace || self.stat(to)?.is_dir()) {
            return Err(FsError::Exists);
        }
        self.run(&["moveto", &spec(from)?, &spec(to)?]).map(|_| ())
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() {
            return Err(FsError::Denied);
        }
        match self.stat(p)?.kind {
            Kind::Dir if recursive => self.run(&["purge", &spec(p)?]).map(|_| ()),
            Kind::Dir => self.run(&["rmdir", &spec(p)?]).map(|_| ()),
            _ => self.run_full(&deletefile_args(p)?).map(|_| ()),
        }
    }

    fn set_meta(&self, _p: &VPath, _m: &MetaChange) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
}

/// The bytes `rclone cat` prints; dropping it stops rclone.
struct CatReader {
    child: Child,
    stdout: std::process::ChildStdout,
}

impl Read for CatReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for CatReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A file written to a temp file first and handed to `rclone copyto` when it is finished; dropped before that it leaves nothing.
struct TempSink {
    file: Option<std::io::BufWriter<std::fs::File>>,
    tmp: PathBuf,
    exe: PathBuf,
    spec: String,
}

impl Write for TempSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.file.as_mut().map_or(Ok(0), |f| f.write(b))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

impl WriteSink for TempSink {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
        }
        let out = leaf_proc::output_helper(command(&self.exe).arg("copyto").arg(&self.tmp).arg(&self.spec), None).map_err(|e| FsError::Other(format!("rclone could not be started: {e}")))?;
        if out.status.success() { Ok(()) } else { Err(failure(out.status.code(), &String::from_utf8_lossy(&out.stderr))) }
    }
}

impl Drop for TempSink {
    fn drop(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_is_an_argument_list_for_one_file() {
        let v = VPath::parse("rclone://gdrive/My Docs/a; rm -rf x.txt").unwrap();
        assert_eq!(deletefile_args(&v).unwrap(), vec!["--ask-password=false", "deletefile", "gdrive:My Docs/a; rm -rf x.txt"]);
        // the root and a remote name that would be read as an option are refused
        assert_eq!(deletefile_args(&VPath::parse("rclone://gdrive/").unwrap()), Err(FsError::Denied));
        assert!(deletefile_args(&VPath::parse("rclone://-v/a").unwrap()).is_err());
    }

    #[test]
    fn reads_a_listing() {
        let json = br#"[{"Path":"a","Name":"a","Size":-1,"ModTime":"2026-11-21T16:46:22.5Z","IsDir":true},{"Path":"f.txt","Name":"f.txt","Size":12,"ModTime":"2026-11-21T16:46:22Z","IsDir":false}]"#;
        let e = parse_list(json).unwrap();
        assert_eq!((&*e[0].name, e[0].is_dir()), ("a", true));
        assert_eq!((&*e[1].name, e[1].size), ("f.txt", 12));
    }
}
