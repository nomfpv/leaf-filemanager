//! Checkpoints of a folder, and putting a folder back the way a checkpoint saw it.
//!
//! **In a git repository** a checkpoint is a commit under a hidden ref, `refs/leaf/checkpoints/<work tree key>/<id>`, made from
//! leaf's OWN index file (`<git dir>/leaf-checkpoint-index`, through `GIT_INDEX_FILE`): `add -A`, `write-tree`, `commit-tree`,
//! `update-ref`. The work tree, the real index, HEAD, the branches and the stash are never touched; untracked files are in it,
//! ignored ones are not. Every git call here reads attributes from the empty tree and turns `core.autocrlf` off, so no line-end
//! conversion or filter runs: the blobs are the files' exact bytes, and a restore writes them back as they were (`cat-file --batch`).
//!
//! **Anywhere else** a checkpoint is a copy under leaf's data folder (`<data>/checkpoints/<folder key>/<id>/files`), made with
//! `reflink-copy` (a copy-on-write clone where the file system has them, else a plain copy; never a hard link, as an editor that
//! rewrites a file in place would change the checkpoint too). [`estimate`] tells the size first; [`create`] takes a cap.
//!
//! A restore ([`restore`]) first takes an automatic checkpoint of the state it is about to change, so taking it back is one more
//! restore. It writes the files that differ or are gone; a file added since the checkpoint is only removed when it is named in
//! `remove` (the user ticked "also remove files added since"), and then it goes to the Trash through the caller's `trash`.
//!
//! Everything here blocks (git, file copies): call it off the UI thread. git is run by argument list, never through a shell.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::gitread::{self, DiffLine, FileDiff, LineKind};

/// The refs every checkpoint of a repository lives under.
pub const REF_ROOT: &str = "refs/leaf/checkpoints/";
/// leaf's own index file inside the (per work tree) git dir.
const INDEX_NAME: &str = "leaf-checkpoint-index";
/// The file that describes a copy checkpoint.
const META_NAME: &str = "checkpoint.txt";
/// How long one git call may take (an `add -A` of a big tree hashes every file once).
const LONG: Duration = Duration::from_secs(900);

/// A failure, in leaf's words; `details` holds what git or the file system said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub details: String,
}

impl Error {
    fn new(message: impl Into<String>, details: impl Into<String>) -> Self {
        Self { message: message.into(), details: details.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.details.is_empty() { write!(f, "{}", self.message) } else { write!(f, "{} ({})", self.message, self.details) }
    }
}

/// Where the checkpoints of a folder are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Store {
    /// The folder is in a git repository: the whole work tree is checkpointed, as hidden refs of that repository.
    Git {
        /// The top of the work tree.
        root: PathBuf,
        /// The git dir of this work tree (a linked worktree has its own).
        git_dir: PathBuf,
        /// Names this work tree under [`REF_ROOT`], so linked worktrees keep their checkpoints apart.
        key: String,
        /// The empty tree of the repository's hash (the attribute source).
        empty_tree: String,
    },
    /// Copies of the folder under `base`.
    Copy { folder: PathBuf, base: PathBuf },
}

impl Store {
    /// The folder the checkpoints are of (a repository: its top).
    pub fn folder(&self) -> &Path {
        match self {
            Store::Git { root, .. } => root,
            Store::Copy { folder, .. } => folder,
        }
    }

    pub fn is_git(&self) -> bool {
        matches!(self, Store::Git { .. })
    }
}

/// One checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// The time it was taken in ms, with a suffix when two were taken in the same ms ("1700000000000", "1700000000000-1").
    pub id: String,
    pub time_ms: i64,
    pub note: String,
    /// Taken by leaf itself (before a restore), not asked for.
    pub auto: bool,
    pub files: u64,
    pub size: u64,
    /// The commit (git) or the checkpoint's folder (copy).
    pub handle: String,
}

/// How a file differs between now and the checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Change {
    /// In both, with other content: the restore writes the checkpoint's.
    Changed,
    /// In the checkpoint, gone now: the restore brings it back.
    Back,
    /// Not in the checkpoint (added since): left alone unless the user asks to remove such files (they go to the Trash).
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Src {
    /// A blob of the repository with its mode (100644, 100755, 120000).
    Blob { oid: String, mode: u32 },
    /// A file of a copy checkpoint.
    File(PathBuf),
    /// Nothing (an added file).
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    /// Relative to the store's folder, with `/`.
    pub rel: String,
    pub change: Change,
    src: Src,
}

/// What a restore of one checkpoint would do, file by file (sorted by path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<PlanItem>,
    /// Paths git gave whose name is not valid Unicode (shown with replacement characters): never written or removed.
    pub unreadable: Vec<String>,
    /// git: the tree of the state now and the checkpoint's commit, for the diff of one file.
    now_tree: Option<String>,
    cp_tree: Option<String>,
    /// copy: the checkpoint's files.
    cp_files: Option<PathBuf>,
}

impl Plan {
    pub fn count(&self, c: Change) -> usize {
        self.items.iter().filter(|i| i.change == c).count()
    }
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The checkpoint of the state before the restore: restoring it is "Undo restore".
    pub auto: Checkpoint,
    /// Files written with the checkpoint's content.
    pub written: Vec<String>,
    /// Of those, the ones that did not exist before (an undo removes these).
    pub back: Vec<String>,
    /// Files added since that went to the Trash.
    pub trashed: Vec<String>,
    /// Files that could not be put back: path and why.
    pub failed: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------------------------------------------- the store

/// A short stable name of a path (FNV-1a of its text; case folded on Windows, where paths are not case-sensitive).
fn key_of(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Looks like a bare repository (git's own files, no work tree).
fn looks_bare(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// The store of `folder`: its repository when it is in one, else copies under `data_dir/checkpoints`. A folder that is in a
/// repository git cannot be run for (git missing, a repository owned by another user, git's own folder, a bare repository) is an
/// error: copying it would copy and later overwrite `.git` itself.
pub fn store_for(folder: &Path, data_dir: &Path) -> Result<Store, Error> {
    if folder.components().any(|c| c.as_os_str() == ".git") || looks_bare(folder) {
        return Err(Error::new("This is git's own folder of a repository: checkpoints are taken of the files of a repository, not of it", ""));
    }
    if crate::gitignore::repo_root(folder).is_none() {
        return Ok(Store::Copy { folder: folder.to_path_buf(), base: data_dir.join("checkpoints").join(key_of(folder)) });
    }
    git_store(folder).map_err(|e| Error::new("This folder is in a git repository, but git could not be run for it", e.details))
}

fn git_store(folder: &Path) -> Result<Store, Error> {
    let t = text(run(with_args(plain_git(folder), &["rev-parse", "--show-toplevel", "--absolute-git-dir"]), &[0], Duration::from_secs(10))?);
    let mut lines = t.lines();
    let root = PathBuf::from(lines.next().unwrap_or("").trim());
    let git_dir = PathBuf::from(lines.next().unwrap_or("").trim());
    let empty_tree = text(run(with_args(plain_git(&root), &["hash-object", "-t", "tree", "--stdin"]), &[0], Duration::from_secs(10))?);
    if root.as_os_str().is_empty() || git_dir.as_os_str().is_empty() || empty_tree.is_empty() {
        return Err(Error::new("git failed", "no work tree"));
    }
    Ok(Store::Git { key: key_of(&root), root, git_dir, empty_tree })
}

// ---------------------------------------------------------------------------------------------------------------- running git

fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// No repository, work tree or index of leaf's own environment reaches git: it finds them from the folder alone.
fn clean_env(c: &mut Command) {
    for v in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE", "GIT_INDEX_FILE"] {
        c.env_remove(v);
    }
}

/// git in `dir`, nothing configured beyond no prompt and no window.
fn plain_git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    clean_env(&mut c);
    c.arg("-C").arg(dir).arg("--no-pager");
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0");
    no_window(&mut c);
    c
}

fn with_args(mut c: Command, args: &[&str]) -> Command {
    c.args(args);
    c
}

/// git for a checkpoint store: no attributes (so no filter and no line-end change), leaf's own index when `own_index`, and a fixed
/// author so `commit-tree` works without a configured name.
fn store_git(root: &Path, git_dir: &Path, empty_tree: &str, own_index: bool) -> Command {
    let mut c = Command::new("git");
    clean_env(&mut c);
    c.arg("-C").arg(root).args(["--no-pager", "--attr-source", empty_tree]);
    for kv in ["core.autocrlf=false", "core.safecrlf=false", "core.fsmonitor=false", "core.quotepath=false", "gc.auto=0", "maintenance.auto=false", "diff.external="] {
        c.args(["-c", kv]);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "leaf")
        .env("GIT_AUTHOR_EMAIL", "leaf@localhost")
        .env("GIT_COMMITTER_NAME", "leaf")
        .env("GIT_COMMITTER_EMAIL", "leaf@localhost");
    if own_index {
        c.env("GIT_INDEX_FILE", git_dir.join(INDEX_NAME));
    }
    no_window(&mut c);
    c
}

/// Runs `c`: its output when it ends with one of `ok`, else an error with git's own words.
fn run(mut c: Command, ok: &[i32], timeout: Duration) -> Result<Vec<u8>, Error> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| Error::new("git could not be started", e.to_string()))?;
    let mut out = child.stdout.take().ok_or_else(|| Error::new("git could not be started", ""))?;
    let mut err = child.stderr.take().ok_or_else(|| Error::new("git could not be started", ""))?;
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = err.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if b.len() < 16 * 1024 {
                b.extend_from_slice(&chunk[..n]);
            }
        }
        b
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::new("git took too long", ""));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(Error::new("git failed", e.to_string())),
        }
    };
    let out = t_out.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).trim().to_string();
    match status.code() {
        Some(code) if ok.contains(&code) => Ok(out),
        _ => Err(Error::new("git failed", err)),
    }
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).trim().to_string()
}

/// Reads blobs one after another through one `git cat-file --batch`.
struct Blobs {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
}

impl Blobs {
    fn open(root: &Path, git_dir: &Path, empty_tree: &str) -> Result<Self, Error> {
        let mut c = store_git(root, git_dir, empty_tree, false);
        c.args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| Error::new("git could not be started", e.to_string()))?;
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().ok_or_else(|| Error::new("git could not be started", ""))?);
        Ok(Self { child, stdin, out })
    }

    fn get(&mut self, oid: &str) -> Result<Vec<u8>, String> {
        let w = self.stdin.as_mut().ok_or("git stopped")?;
        writeln!(w, "{oid}").and_then(|_| w.flush()).map_err(|e| e.to_string())?;
        let mut head = String::new();
        self.out.read_line(&mut head).map_err(|e| e.to_string())?;
        let f: Vec<&str> = head.split_whitespace().collect();
        if f.len() != 3 || f[1] != "blob" {
            return Err(format!("not in the repository: {}", head.trim()));
        }
        let size: usize = f[2].parse().map_err(|_| "a bad answer from git".to_string())?;
        let mut buf = vec![0u8; size];
        self.out.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let mut nl = [0u8; 1];
        self.out.read_exact(&mut nl).map_err(|e| e.to_string())?;
        Ok(buf)
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------------------------------------------- metadata

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn one_line(s: &str) -> String {
    s.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

fn meta_text(time_ms: i64, note: &str, auto: bool, files: u64, size: u64) -> String {
    format!("leaf checkpoint\n\ntime: {time_ms}\nnote: {}\nauto: {}\nfiles: {files}\nsize: {size}\n", one_line(note), u8::from(auto))
}

/// Reads [`meta_text`] back (unknown lines are skipped).
fn parse_meta(id: &str, handle: String, t: &str) -> Checkpoint {
    let mut cp = Checkpoint { id: id.to_string(), time_ms: 0, note: String::new(), auto: false, files: 0, size: 0, handle };
    for l in t.lines() {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim();
        match k.trim() {
            "time" => cp.time_ms = v.parse().unwrap_or(0),
            "note" => cp.note = v.to_string(),
            "auto" => cp.auto = v == "1",
            "files" => cp.files = v.parse().unwrap_or(0),
            "size" => cp.size = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    if cp.time_ms == 0 {
        cp.time_ms = id.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(0);
    }
    cp
}

fn fresh_id(taken: &[Checkpoint]) -> (String, i64) {
    let t = now_ms();
    let mut id = t.to_string();
    let mut n = 1;
    while taken.iter().any(|c| c.id == id) {
        id = format!("{t}-{n}");
        n += 1;
    }
    (id, t)
}

// ---------------------------------------------------------------------------------------------------------------- walking

/// Every regular file under `dir` (relative, with `/`), with its size. Links are not followed and not listed; `skip` (leaf's own
/// checkpoint folder, should it be inside) is left out.
fn walk(dir: &Path, skip: Option<&Path>) -> Result<BTreeMap<String, (PathBuf, u64)>, Error> {
    let skip_key = skip.map(path_key);
    let skip_name = skip.and_then(Path::file_name).map(|n| n.to_string_lossy().to_lowercase());
    let mut out = BTreeMap::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, rel)) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| Error::new(format!("The folder {} could not be read", d.display()), e.to_string()))?;
        for e in rd.flatten() {
            let p = e.path();
            // (the name first, cheaply; then the real paths, as the file system spells them)
            if skip_name.as_deref().is_some_and(|n| e.file_name().to_string_lossy().to_lowercase() == n) && skip_key.as_deref() == Some(path_key(&p).as_str()) {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            // (git's own folder is never copied, compared or written, wherever it is)
            if e.file_name() == ".git" {
                continue;
            }
            let name = name_text(&e.file_name());
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if ft.is_symlink() {
                continue;
            } else if ft.is_dir() {
                stack.push((p, r));
            } else if ft.is_file() {
                let len = e.metadata().map_or(0, |m| m.len());
                out.insert(r, (p, len));
            }
        }
    }
    Ok(out)
}

/// A file name as text that keeps every byte: valid text as it is; a `\u{FFFD}` in a name, and bytes that are not UTF-8, as
/// `\u{FFFD}x<hex bytes>;`. [`name_from_text`] gives the exact name back, so a copy checkpoint never renames a file.
fn name_text(n: &std::ffi::OsStr) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        fn escape(out: &mut String, b: &[u8]) {
            out.push('\u{FFFD}');
            out.push('x');
            for x in b {
                out.push_str(&format!("{x:02x}"));
            }
            out.push(';');
        }
        fn valid(out: &mut String, t: &str) {
            for c in t.chars() {
                if c == '\u{FFFD}' {
                    escape(out, "\u{FFFD}".as_bytes());
                } else {
                    out.push(c);
                }
            }
        }
        let mut out = String::new();
        let mut rest = n.as_bytes();
        loop {
            match std::str::from_utf8(rest) {
                Ok(t) => {
                    valid(&mut out, t);
                    return out;
                }
                Err(e) => {
                    let (good, bad) = rest.split_at(e.valid_up_to());
                    valid(&mut out, std::str::from_utf8(good).unwrap_or_default());
                    let n = e.error_len().unwrap_or(bad.len()).max(1);
                    escape(&mut out, &bad[..n]);
                    rest = &bad[n..];
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        n.to_string_lossy().into_owned()
    }
}

/// The exact name (or `/`-separated relative path) a [`name_text`] text stands for.
fn name_from_text(t: &str) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut out: Vec<u8> = Vec::new();
        let mut it = t.char_indices().peekable();
        while let Some((i, c)) = it.next() {
            if c == '\u{FFFD}'
                && let Some(end) = t[i..].find(';').map(|e| e + i)
                && let Some(hex) = t[i + c.len_utf8()..end].strip_prefix('x')
                && hex.len() % 2 == 0
                && hex.bytes().all(|b| b.is_ascii_hexdigit())
            {
                out.extend((0..hex.len()).step_by(2).filter_map(|k| u8::from_str_radix(&hex[k..k + 2], 16).ok()));
                while it.peek().is_some_and(|(j, _)| *j <= end) {
                    it.next();
                }
                continue;
            }
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
        std::ffi::OsString::from_vec(out)
    }
    #[cfg(not(unix))]
    {
        t.into()
    }
}

/// A path as the file system resolves it (links and `..` followed), with `/`, without the `\\?\` prefix, case folded on Windows:
/// two spellings of one folder give the same text.
fn path_key(p: &Path) -> String {
    let real = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = real.to_string_lossy();
    let s = s.strip_prefix(r"\\?\UNC\").map(|r| format!("//{r}")).unwrap_or_else(|| s.strip_prefix(r"\\?\").unwrap_or(&s).to_string());
    let mut s = s.replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn copy_skip(folder: &Path, base: &Path) -> Option<PathBuf> {
    // (leaf's checkpoints folder, `<data>/checkpoints`, when the folder holds it)
    let top = base.parent()?;
    let (t, f) = (path_key(top), path_key(folder));
    (t == f || t.starts_with(&format!("{}/", f.trim_end_matches('/')))).then(|| top.to_path_buf())
}

/// What a checkpoint would add: (files, bytes). Outside a repository the whole folder (it is copied); in one the files git does not
/// track and does not ignore (what is tracked is in the repository already; git keeps each content once).
pub fn estimate(store: &Store) -> Result<(u64, u64), Error> {
    match store {
        Store::Git { root, .. } => {
            let raw = run(with_args(plain_git(root), &["ls-files", "--others", "--exclude-standard", "-z"]), &[0], LONG)?;
            let mut n = 0;
            let mut bytes = 0;
            for rel in raw.split(|&b| b == 0).filter(|r| !r.is_empty()) {
                n += 1;
                bytes += std::fs::symlink_metadata(root.join(String::from_utf8_lossy(rel).as_ref())).map_or(0, |m| m.len());
            }
            Ok((n, bytes))
        }
        Store::Copy { folder, base } => {
            let files = walk(folder, copy_skip(folder, base).as_deref())?;
            Ok((files.len() as u64, files.values().map(|(_, s)| *s).sum()))
        }
    }
}

fn too_big(size: u64, cap: u64) -> Error {
    Error::new("The folder is bigger than the checkpoint size limit", format!("{size} > {cap} bytes"))
}

fn same_content(a: &Path, b: &Path) -> bool {
    let (Ok(fa), Ok(fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
    let (mut ra, mut rb) = (BufReader::with_capacity(64 * 1024, fa), BufReader::with_capacity(64 * 1024, fb));
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let na = match read_full(&mut ra, &mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        let nb = match read_full(&mut rb, &mut bb) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

// ---------------------------------------------------------------------------------------------------------------- list, create, delete

/// The checkpoints of the store, the newest first.
pub fn list(store: &Store) -> Result<Vec<Checkpoint>, Error> {
    let mut out = match store {
        Store::Git { root, git_dir, key, empty_tree } => {
            let prefix = format!("{REF_ROOT}{key}/");
            let raw = run(
                with_args(store_git(root, git_dir, empty_tree, false), &["for-each-ref", "--format=%(refname)%00%(objectname)%00%(contents)%01", &prefix]),
                &[0],
                Duration::from_secs(30),
            )?;
            let t = String::from_utf8_lossy(&raw).into_owned();
            t.split('\u{1}')
                .filter_map(|rec| {
                    let rec = rec.trim_start_matches(['\r', '\n']);
                    let mut f = rec.splitn(3, '\0');
                    let name = f.next()?.strip_prefix(&prefix)?.to_string();
                    let oid = f.next()?.to_string();
                    Some(parse_meta(&name, oid, f.next().unwrap_or("")))
                })
                .collect::<Vec<_>>()
        }
        Store::Copy { base, .. } => {
            let mut v = Vec::new();
            if let Ok(rd) = std::fs::read_dir(base) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".partial") || !e.path().is_dir() {
                        continue;
                    }
                    let Ok(meta) = std::fs::read_to_string(e.path().join(META_NAME)) else { continue };
                    v.push(parse_meta(&name, e.path().to_string_lossy().into_owned(), &meta));
                }
            }
            v
        }
    };
    out.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then_with(|| b.id.cmp(&a.id)));
    Ok(out)
}

/// The tree of the work tree as it is now, through leaf's own index (made again from nothing once should it be damaged).
fn snapshot_tree(root: &Path, git_dir: &Path, empty_tree: &str) -> Result<String, Error> {
    let index = git_dir.join(INDEX_NAME);
    // a lock older than any git call may last is left by a leaf that was stopped mid-way: nothing holds it any more
    let lock = git_dir.join(format!("{INDEX_NAME}.lock"));
    if std::fs::metadata(&lock).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > LONG)) {
        let _ = std::fs::remove_file(&lock);
    }
    let once = || -> Result<String, Error> {
        run(with_args(store_git(root, git_dir, empty_tree, true), &["add", "-A", "--", ":/"]), &[0], LONG)
            .map_err(|e| Error::new("The files could not be read into the checkpoint", e.details))?;
        let tree = text(run(with_args(store_git(root, git_dir, empty_tree, true), &["write-tree"]), &[0], LONG).map_err(|e| Error::new("The checkpoint could not be written", e.details))?);
        if tree.is_empty() {
            return Err(Error::new("The checkpoint could not be written", "write-tree gave nothing"));
        }
        Ok(tree)
    };
    // leaf's own index can be made again from nothing: once, should it be damaged
    once().or_else(|_| {
        let _ = std::fs::remove_file(&index);
        once()
    })
}

/// (files, bytes) of a tree: its blobs, not counting submodules.
fn tree_size(root: &Path, git_dir: &Path, empty_tree: &str, tree: &str) -> (u64, u64) {
    let Ok(raw) = run(with_args(store_git(root, git_dir, empty_tree, false), &["ls-tree", "-r", "-l", "-z", tree]), &[0], LONG) else { return (0, 0) };
    let mut n = 0;
    let mut bytes = 0;
    for rec in raw.split(|&b| b == 0) {
        let rec = String::from_utf8_lossy(rec);
        let Some((head, _)) = rec.split_once('\t') else { continue };
        let f: Vec<&str> = head.split_whitespace().collect();
        if f.len() == 4 && f[1] == "blob" {
            n += 1;
            bytes += f[3].parse::<u64>().unwrap_or(0);
        }
    }
    (n, bytes)
}

/// Takes a checkpoint of the store's folder. `cap` (bytes) refuses a copy checkpoint bigger than that (a repository's is not capped:
/// git keeps each content once).
pub fn create(store: &Store, note: &str, auto: bool, cap: Option<u64>) -> Result<Checkpoint, Error> {
    let taken = list(store)?;
    let (id, time_ms) = fresh_id(&taken);
    match store {
        Store::Git { root, git_dir, key, empty_tree } => {
            if let Some(c) = cap {
                let (_, size) = estimate(store)?;
                if size > c {
                    return Err(too_big(size, c));
                }
            }
            let tree = snapshot_tree(root, git_dir, empty_tree)?;
            let (files, size) = tree_size(root, git_dir, empty_tree, &tree);
            let msg = meta_text(time_ms, note, auto, files, size);
            let commit = text(run(with_args(store_git(root, git_dir, empty_tree, false), &["commit-tree", &tree, "-m", &msg]), &[0], LONG)?);
            if commit.is_empty() {
                return Err(Error::new("The checkpoint could not be written", "commit-tree gave nothing"));
            }
            let name = format!("{REF_ROOT}{key}/{id}");
            run(with_args(store_git(root, git_dir, empty_tree, false), &["update-ref", &name, &commit, ""]), &[0], Duration::from_secs(30))
                .map_err(|e| Error::new("The checkpoint could not be written", e.details))?;
            Ok(Checkpoint { id, time_ms, note: one_line(note), auto, files, size, handle: commit })
        }
        Store::Copy { folder, base } => {
            let files = walk(folder, copy_skip(folder, base).as_deref())?;
            let size: u64 = files.values().map(|(_, s)| *s).sum();
            if let Some(c) = cap.filter(|c| size > *c) {
                return Err(too_big(size, c));
            }
            let fail = |what: &str, e: std::io::Error| Error::new("The checkpoint could not be written", format!("{what}: {e}"));
            std::fs::create_dir_all(base).map_err(|e| fail(&base.display().to_string(), e))?;
            let _ = std::fs::write(base.join("folder.txt"), folder.to_string_lossy().as_bytes());
            let part = base.join(format!("{id}.partial"));
            let into = part.join("files");
            let result = (|| {
                std::fs::create_dir_all(&into).map_err(|e| fail(&into.display().to_string(), e))?;
                for (rel, (src, _)) in &files {
                    let dst = into.join(name_from_text(rel));
                    if let Some(p) = dst.parent() {
                        std::fs::create_dir_all(p).map_err(|e| fail(rel, e))?;
                    }
                    reflink_copy::reflink_or_copy(src, &dst).map_err(|e| fail(rel, e))?;
                }
                std::fs::write(part.join(META_NAME), meta_text(time_ms, note, auto, files.len() as u64, size)).map_err(|e| fail(META_NAME, e))?;
                let done = base.join(&id);
                std::fs::rename(&part, &done).map_err(|e| fail(&done.display().to_string(), e))?;
                Ok(done)
            })();
            match result {
                Ok(done) => Ok(Checkpoint { id, time_ms, note: one_line(note), auto, files: files.len() as u64, size, handle: done.to_string_lossy().into_owned() }),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&part);
                    Err(e)
                }
            }
        }
    }
}

/// Removes a checkpoint (git: its ref; the objects go with git's own clean-up later).
pub fn delete(store: &Store, cp: &Checkpoint) -> Result<(), Error> {
    match store {
        Store::Git { root, git_dir, key, empty_tree } => {
            let name = format!("{REF_ROOT}{key}/{}", cp.id);
            run(with_args(store_git(root, git_dir, empty_tree, false), &["update-ref", "-d", &name, &cp.handle]), &[0], Duration::from_secs(30))
                .map(|_| ())
                .map_err(|e| Error::new("The checkpoint could not be removed", e.details))
        }
        Store::Copy { base, .. } => {
            let dir = base.join(&cp.id);
            if cp.id.is_empty() || cp.id.contains(['/', '\\', '.']) || !dir.join(META_NAME).is_file() {
                return Err(Error::new("The checkpoint could not be removed", "not a checkpoint folder"));
            }
            std::fs::remove_dir_all(&dir).map_err(|e| Error::new("The checkpoint could not be removed", e.to_string()))
        }
    }
}

/// Keeps the newest `keep` of the user's own checkpoints and, apart from them, the newest `keep` automatic ones (never fewer than
/// one of each), so restores never push out what the user took. The ids in `protect` always stay. Returns how many went.
pub fn prune(store: &Store, keep: usize, protect: &[&str]) -> usize {
    let Ok(all) = list(store) else { return 0 };
    let mut gone = 0;
    for auto in [false, true] {
        gone += all
            .iter()
            .filter(|c| c.auto == auto)
            .skip(keep.max(1))
            .filter(|c| !protect.contains(&c.id.as_str()))
            .filter(|c| delete(store, c).is_ok())
            .count();
    }
    gone
}

// ---------------------------------------------------------------------------------------------------------------- plan and diff

/// What restoring `cp` would change, compared with the folder now.
pub fn plan(store: &Store, cp: &Checkpoint) -> Result<Plan, Error> {
    match store {
        Store::Git { root, git_dir, empty_tree, .. } => {
            let now = snapshot_tree(root, git_dir, empty_tree)?;
            plan_git(root, git_dir, empty_tree, cp, now)
        }
        Store::Copy { folder, base } => {
            let cp_files = PathBuf::from(&cp.handle).join("files");
            let theirs = walk(&cp_files, None)?;
            let ours = walk(folder, copy_skip(folder, base).as_deref())?;
            let mut items = Vec::new();
            for (rel, (src, size)) in &theirs {
                match ours.get(rel) {
                    None => items.push(PlanItem { rel: rel.clone(), change: Change::Back, src: Src::File(src.clone()) }),
                    Some((mine, my_size)) => {
                        if my_size != size || !same_content(mine, src) {
                            items.push(PlanItem { rel: rel.clone(), change: Change::Changed, src: Src::File(src.clone()) });
                        }
                    }
                }
            }
            for rel in ours.keys().filter(|r| !theirs.contains_key(*r)) {
                items.push(PlanItem { rel: rel.clone(), change: Change::Added, src: Src::None });
            }
            items.sort_by(|a, b| a.rel.cmp(&b.rel));
            Ok(Plan { items, unreadable: Vec::new(), now_tree: None, cp_tree: None, cp_files: Some(cp_files) })
        }
    }
}

fn plan_git(root: &Path, git_dir: &Path, empty_tree: &str, cp: &Checkpoint, now: String) -> Result<Plan, Error> {
    let cp_tree = format!("{}^{{tree}}", cp.handle);
    let raw = run(with_args(store_git(root, git_dir, empty_tree, false), &["diff-tree", "-r", "-z", "--no-renames", "--raw", &now, &cp_tree]), &[0], LONG)?;
    let (mut items, unreadable) = parse_raw_diff(&raw);
    items.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(Plan { items, unreadable, now_tree: Some(now), cp_tree: Some(cp_tree), cp_files: None })
}

/// Parses `diff-tree -r -z --raw <now> <checkpoint>`: `:mode mode oid oid S\0path\0`. Submodules are left out; a path that is not
/// valid UTF-8 goes to the second list (by a lossy name, for the message), never to the items.
fn parse_raw_diff(raw: &[u8]) -> (Vec<PlanItem>, Vec<String>) {
    let mut parts = raw.split(|&b| b == 0);
    let mut out = Vec::new();
    let mut bad = Vec::new();
    while let Some(head) = parts.next() {
        let head = String::from_utf8_lossy(head);
        let Some(head) = head.strip_prefix(':') else { continue };
        let Some(path) = parts.next() else { break };
        let Ok(path) = std::str::from_utf8(path) else {
            bad.push(String::from_utf8_lossy(path).into_owned());
            continue;
        };
        let f: Vec<&str> = head.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let (now_mode, cp_mode) = (u32::from_str_radix(f[0], 8).unwrap_or(0), u32::from_str_radix(f[1], 8).unwrap_or(0));
        if now_mode == 0o160000 || cp_mode == 0o160000 {
            continue;
        }
        let change = match f[4].chars().next() {
            Some('A') => Change::Back,
            Some('D') => Change::Added,
            Some('M' | 'T') => Change::Changed,
            _ => continue,
        };
        let src = if change == Change::Added { Src::None } else { Src::Blob { oid: f[3].to_string(), mode: cp_mode } };
        out.push(PlanItem { rel: path.to_string(), change, src });
    }
    (out, bad)
}

fn all_removed(bytes: &[u8]) -> FileDiff {
    let mut d = gitread::all_added(bytes, gitread::MAX_LINES);
    for h in &mut d.hunks {
        h.header = h.header.replace("@@ -0,0 +1,", "@@ -1,").replace(" @@", " +0,0 @@");
        h.old_start = 1;
        h.new_start = 0;
        for l in &mut h.lines {
            *l = DiffLine { kind: LineKind::Removed, old_no: l.new_no, new_no: None, text: std::mem::take(&mut l.text) };
        }
    }
    d.removed = d.added;
    d.added = 0;
    d
}

/// The difference one file makes in a restore: the file now as the old side, the checkpoint's as the new side (the + lines are what
/// the restore writes). `context` unchanged lines round each change. `None` when it could not be worked out.
pub fn diff(store: &Store, plan: &Plan, rel: &str, context: u32) -> Option<FileDiff> {
    let u = format!("-U{context}");
    let parse = |raw: Vec<u8>| {
        let mut d = gitread::parse_diff(&String::from_utf8_lossy(&raw).replace('\r', ""), gitread::MAX_LINES);
        d.truncated |= raw.len() >= gitread::MAX_OUT;
        d
    };
    match store {
        Store::Git { root, git_dir, empty_tree, .. } => {
            let (now, cp) = (plan.now_tree.as_deref()?, plan.cp_tree.as_deref()?);
            let raw = run(with_args(store_git(root, git_dir, empty_tree, false), &["--literal-pathspecs", "diff", "--no-color", "--no-ext-diff", "--no-renames", &u, now, cp, "--", rel]), &[0], LONG).ok()?;
            Some(parse(raw))
        }
        Store::Copy { folder, .. } => {
            let mine = folder.join(name_from_text(rel));
            let theirs = plan.cp_files.as_ref()?.join(name_from_text(rel));
            let read = |p: &Path| {
                let mut b = Vec::new();
                std::fs::File::open(p).ok()?.take(1024 * 1024).read_to_end(&mut b).ok()?;
                Some(b)
            };
            match (mine.is_file(), theirs.is_file()) {
                (true, true) => {
                    let mut c = Command::new("git");
                    clean_env(&mut c);
                    c.args(["--no-pager", "diff", "--no-index", "--no-color", "--no-ext-diff", &u, "--"]).arg(&mine).arg(&theirs);
                    c.env("GIT_TERMINAL_PROMPT", "0");
                    no_window(&mut c);
                    Some(parse(run(c, &[0, 1], LONG).ok()?))
                }
                (false, true) => Some(gitread::all_added(&read(&theirs)?, gitread::MAX_LINES)),
                (true, false) => Some(all_removed(&read(&mine)?)),
                (false, false) => Some(FileDiff::default()),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------- restore

/// Two relative paths name the same file on this system (Windows and macOS ignore case).
fn same_path(a: &str, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) { a.to_lowercase() == b.to_lowercase() } else { a == b }
}

/// Is this a link of any kind (a symbolic link; on Windows also a junction or another reparse point)?
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

/// Every folder from `folder` down to the parent of `rel` that exists must be a real folder: a link there would carry the write
/// out of the folder.
fn no_link_on_the_way(folder: &Path, rel: &str, copy: bool) -> Result<(), String> {
    let mut cur = folder.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err("the path is not valid".into());
        }
        if copy {
            cur.push(name_from_text(part));
        } else {
            cur.push(part);
        }
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if is_link(&m) => return Err(format!("{} is a link now: nothing is written through it", cur.display())),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Writes `bytes` to `path` through a file beside it and a rename, so an interrupted write never leaves half a file.
fn put(path: &Path, fill: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), String> {
    if path.is_dir() {
        return Err("a folder of that name is in the way".into());
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(".leaf-restore");
    let tmp = path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = fill(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(path) {
        let mut p = m.permissions();
        p.set_mode(if exec { p.mode() | 0o111 } else { p.mode() & !0o111 });
        let _ = std::fs::set_permissions(path, p);
    }
}

#[cfg(not(unix))]
fn set_exec(_: &Path, _: bool) {}

#[cfg(unix)]
fn put_link(path: &Path, target: &[u8]) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::OsStr::from_bytes(target);
    if path.is_dir() && !path.is_symlink() {
        return Err("a folder of that name is in the way".into());
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(path);
    std::os::unix::fs::symlink(target, path).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn put_link(_: &Path, _: &[u8]) -> Result<(), String> {
    Err("a symbolic link is not put back on this system".into())
}

/// Puts the folder back the way `cp` saw it. An automatic checkpoint of the state now is taken first (it is in the answer: restoring
/// it with `remove: Some(&restored.back)` is "Undo restore"). Files added since the checkpoint stay, except those named in `remove`,
/// which go to `trash` (never deleted here).
pub fn restore(store: &Store, cp: &Checkpoint, remove: Option<&[String]>, trash: &dyn Fn(&[PathBuf]) -> Result<(), String>) -> Result<Restored, Error> {
    let what = if cp.note.is_empty() { "a checkpoint" } else { cp.note.as_str() };
    let auto = create(store, &format!("Before restoring {what}"), true, None)?;
    // the plan made again against the state just saved: what is written is exactly what the automatic checkpoint can take back
    let plan = match store {
        Store::Git { root, git_dir, empty_tree, .. } => plan_git(root, git_dir, empty_tree, cp, format!("{}^{{tree}}", auto.handle))?,
        Store::Copy { .. } => plan(store, cp)?,
    };
    let folder = store.folder().to_path_buf();
    // (a copy checkpoint's paths keep every byte of a name, see `name_text`)
    let copy = matches!(store, Store::Copy { .. });
    let real = |rel: &str| if copy { folder.join(name_from_text(rel)) } else { folder.join(rel) };
    let mut blobs = match store {
        Store::Git { root, git_dir, empty_tree, .. } => Some(Blobs::open(root, git_dir, empty_tree)?),
        Store::Copy { .. } => None,
    };
    let mut done = Restored { auto, written: Vec::new(), back: Vec::new(), trashed: Vec::new(), failed: Vec::new() };
    done.failed.extend(plan.unreadable.iter().map(|n| (n.clone(), "the name is not valid Unicode: left as it is".to_string())));
    for it in plan.items.iter().filter(|i| i.change != Change::Added) {
        let path = real(&it.rel);
        if let Err(e) = no_link_on_the_way(&folder, &it.rel, copy) {
            done.failed.push((it.rel.clone(), e));
            continue;
        }
        // "comes back" yet something is there: a file git ignores now, or a link the copy store does not list. The automatic
        // checkpoint does not hold it, so it is never overwritten: it goes to the Trash first, or the file is left as it is.
        // (a name that only changed its case is one file: its content now is in the automatic checkpoint under the other spelling)
        let renamed = plan.items.iter().any(|o| o.change == Change::Added && same_path(&o.rel, &it.rel));
        if it.change == Change::Back
            && !renamed
            && let Ok(m) = std::fs::symlink_metadata(&path)
        {
            if m.is_dir() {
                done.failed.push((it.rel.clone(), "a folder of that name is in the way".into()));
                continue;
            }
            match trash(std::slice::from_ref(&path)) {
                Ok(()) if std::fs::symlink_metadata(&path).is_err() => done.trashed.push(it.rel.clone()),
                _ => {
                    done.failed.push((it.rel.clone(), "it exists now but is not in the automatic checkpoint, and it could not be moved to the Trash".into()));
                    continue;
                }
            }
        }
        let r = match &it.src {
            Src::Blob { oid, mode } => match blobs.as_mut().map(|b| b.get(oid)) {
                Some(Ok(bytes)) if *mode == 0o120000 => put_link(&path, &bytes),
                Some(Ok(bytes)) => put(&path, |tmp| std::fs::write(tmp, &bytes)).map(|()| set_exec(&path, *mode == 0o100755)),
                Some(Err(e)) => Err(e),
                None => Err("no repository".into()),
            },
            Src::File(src) => put(&path, |tmp| reflink_copy::reflink_or_copy(src, tmp).map(|_| ())),
            Src::None => continue,
        };
        match r {
            Ok(()) => {
                done.written.push(it.rel.clone());
                if it.change == Change::Back {
                    done.back.push(it.rel.clone());
                }
            }
            Err(e) => done.failed.push((it.rel.clone(), e)),
        }
    }
    drop(blobs);
    if let Some(remove) = remove {
        // (never a file just written under another case of its name)
        let gone: Vec<String> = plan
            .items
            .iter()
            .filter(|i| i.change == Change::Added && remove.contains(&i.rel) && !done.written.iter().any(|w| same_path(w, &i.rel)))
            .map(|i| i.rel.clone())
            .collect();
        if !gone.is_empty() {
            let paths: Vec<PathBuf> = gone.iter().map(|r| real(r)).collect();
            match trash(&paths) {
                Ok(()) => {
                    for (rel, p) in gone.into_iter().zip(&paths) {
                        if p.exists() {
                            done.failed.push((rel, "it is still there after moving it to the Trash".into()));
                        } else {
                            done.trashed.push(rel);
                        }
                    }
                }
                Err(e) => done.failed.extend(gone.into_iter().map(|r| (r, e.clone()))),
            }
        }
    }
    Ok(done)
}

/// A copy checkpoint's own file for `rel` (to show it, e.g. a picture before and after), `None` in a repository or when it has none.
pub fn saved_file(plan: &Plan, rel: &str) -> Option<PathBuf> {
    let p = plan.cp_files.as_ref()?.join(name_from_text(rel));
    p.is_file().then_some(p)
}

/// Puts ONE file of `plan` back the way its checkpoint has it (the Review page's "Revert"): what is there now goes to `trash` first
/// (never overwritten), then the checkpoint's content is written beside and renamed in. A file added since only goes to `trash`.
pub fn restore_one(store: &Store, plan: &Plan, rel: &str, trash: &dyn Fn(&[PathBuf]) -> Result<(), String>) -> Result<(), String> {
    let it = plan.items.iter().find(|i| i.rel == rel).ok_or("the file is not in the comparison any more")?;
    let folder = store.folder().to_path_buf();
    let copy = matches!(store, Store::Copy { .. });
    let path = if copy { folder.join(name_from_text(rel)) } else { folder.join(rel) };
    no_link_on_the_way(&folder, rel, copy)?;
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        if m.is_dir() {
            return Err("a folder of that name is in the way".into());
        }
        trash(std::slice::from_ref(&path))?;
        if std::fs::symlink_metadata(&path).is_ok() {
            return Err("it is still there after moving it to the Trash".into());
        }
    }
    match (&it.src, store) {
        (Src::None, _) => Ok(()),
        (Src::File(src), _) => put(&path, |tmp| reflink_copy::reflink_or_copy(src, tmp).map(|_| ())),
        (Src::Blob { oid, mode }, Store::Git { root, git_dir, empty_tree, .. }) => {
            let bytes = Blobs::open(root, git_dir, empty_tree).map_err(|e| e.to_string())?.get(oid)?;
            if *mode == 0o120000 {
                put_link(&path, &bytes)
            } else {
                put(&path, |tmp| std::fs::write(tmp, &bytes)).map(|()| set_exec(&path, *mode == 0o100755))
            }
        }
        (Src::Blob { .. }, Store::Copy { .. }) => Err("no repository".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("leaf-cp-{name}-{}-{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn git_ok() -> bool {
        Command::new("git").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
    }

    fn g(dir: &Path, args: &[&str]) -> Vec<u8> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    }

    fn write(dir: &Path, rel: &str, bytes: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    /// Every file under `dir` but `.git`, with its bytes.
    fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        walk(dir, Some(&dir.join(".git"))).unwrap().into_iter().map(|(r, (p, _))| (r, std::fs::read(p).unwrap())).collect()
    }

    fn repo(name: &str) -> PathBuf {
        let d = temp(name);
        g(&d, &["init", "-q"]);
        // the setting Git for Windows ships with: the checkpoint must still keep the exact bytes
        g(&d, &["config", "core.autocrlf", "true"]);
        write(&d, ".gitignore", b"*.log\n");
        write(&d, "a.txt", b"one\ntwo\n");
        write(&d, "src/b.rs", b"fn main() {}\r\n");
        g(&d, &["add", "-A"]);
        g(&d, &["commit", "-q", "-m", "first"]);
        d
    }

    #[test]
    fn checkpoint_in_repo_leaves_status_and_index_untouched() {
        if !git_ok() {
            return;
        }
        let d = repo("status");
        // a staged change, an unstaged one on top, an untracked file and an ignored one
        write(&d, "a.txt", b"one\ntwo\nthree\n");
        g(&d, &["add", "a.txt"]);
        write(&d, "a.txt", b"one\ntwo\nthree\nfour\n");
        write(&d, "new/untracked.txt", b"fresh");
        write(&d, "debug.log", b"ignored");
        g(&d, &["stash", "list"]);
        let status = g(&d, &["status", "--porcelain"]);
        let index = std::fs::read(d.join(".git/index")).unwrap();
        let head = g(&d, &["rev-parse", "HEAD"]);
        let stash = g(&d, &["stash", "list"]);

        let store = store_for(&d, &temp("status-data")).unwrap();
        assert!(store.is_git());
        let cp = create(&store, "before the agent", false, None).unwrap();

        assert_eq!(g(&d, &["status", "--porcelain"]), status);
        assert_eq!(std::fs::read(d.join(".git/index")).unwrap(), index, "the real index is byte for byte the same");
        assert_eq!(g(&d, &["rev-parse", "HEAD"]), head);
        assert_eq!(g(&d, &["stash", "list"]), stash);
        let names = String::from_utf8(g(&d, &["ls-tree", "-r", "--name-only", &cp.handle])).unwrap();
        assert!(names.lines().any(|l| l == "new/untracked.txt"), "untracked files are in it: {names}");
        assert!(!names.lines().any(|l| l == "debug.log"), "ignored files are not: {names}");
        let listed = list(&store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].note.as_str(), listed[0].auto, listed[0].files), ("before the agent", false, 4));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_then_undo_restore_gives_back_the_exact_previous_content() {
        let mut stores = Vec::new();
        if git_ok() {
            let d = repo("undo-git");
            stores.push((d.clone(), store_for(&d, &temp("undo-git-data")).unwrap()));
        }
        let plain = temp("undo-copy");
        write(&plain, "a.txt", b"one\ntwo\n");
        write(&plain, "src/b.rs", b"fn main() {}\r\n");
        stores.push((plain.clone(), Store::Copy { folder: plain.clone(), base: temp("undo-copy-data").join("checkpoints").join("k") }));
        for (d, store) in stores {
            let bin = temp("undo-bin");
            let trash = |paths: &[PathBuf]| -> Result<(), String> {
                for p in paths {
                    std::fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string())?;
                }
                Ok(())
            };
            let cp = create(&store, "", false, None).unwrap();
            // later: a change with mixed line ends, a file gone, a file new, a new folder
            write(&d, "a.txt", b"ONE\r\ntwo\nthree");
            std::fs::remove_file(d.join("src/b.rs")).unwrap();
            write(&d, "later/c.bin", &[0, 1, 2, 255]);
            let before = contents(&d);

            let p = plan(&store, &cp).unwrap();
            assert_eq!((p.count(Change::Changed), p.count(Change::Back), p.count(Change::Added)), (1, 1, 1), "{store:?}");
            let done = restore(&store, &cp, None, &trash).unwrap();
            assert!(done.failed.is_empty(), "{:?}", done.failed);
            assert_eq!(std::fs::read(d.join("a.txt")).unwrap(), b"one\ntwo\n");
            assert_eq!(std::fs::read(d.join("src/b.rs")).unwrap(), b"fn main() {}\r\n");
            assert!(d.join("later/c.bin").exists());

            let undo = restore(&store, &done.auto, Some(&done.back), &trash).unwrap();
            assert!(undo.failed.is_empty(), "{:?}", undo.failed);
            assert_eq!(contents(&d), before, "undo gives back every byte ({store:?})");
            assert_eq!(undo.trashed, vec!["src/b.rs".to_string()]);
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_file_ignored_now_goes_to_the_trash_before_it_is_overwritten() {
        if !git_ok() {
            return;
        }
        let d = repo("ignored-now");
        write(&d, "build.out", b"from then");
        let store = store_for(&d, &temp("ignored-now-data")).unwrap();
        let cp = create(&store, "", false, None).unwrap();
        // later git ignores it, and it holds other bytes: the automatic checkpoint will not have them
        write(&d, ".gitignore", b"*.log\n*.out\n");
        write(&d, "build.out", b"only copy of this");
        // (leaf's own index keeps a file it once took even when it is ignored later; made again from nothing, as after damage, it
        // does not take it)
        std::fs::remove_file(d.join(".git").join(INDEX_NAME)).unwrap();
        let bin = temp("ignored-now-bin");
        let trash = |paths: &[PathBuf]| -> Result<(), String> {
            for p in paths {
                std::fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let done = restore(&store, &cp, None, &trash).unwrap();
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert_eq!(std::fs::read(bin.join("build.out")).unwrap(), b"only copy of this");
        assert_eq!(std::fs::read(d.join("build.out")).unwrap(), b"from then");
        assert!(done.trashed.contains(&"build.out".to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_restore_never_writes_through_a_folder_that_is_a_link_now() {
        let d = temp("link-on-way");
        write(&d, "sub/a.txt", b"inside");
        let store = Store::Copy { folder: d.clone(), base: temp("link-on-way-data").join("checkpoints").join("k") };
        let cp = create(&store, "", false, None).unwrap();
        // later `sub` is a link to a folder outside
        let outside = temp("link-on-way-outside");
        std::fs::remove_dir_all(d.join("sub")).unwrap();
        #[cfg(windows)]
        {
            let ok = Command::new("cmd").arg("/c").arg("mklink").arg("/J").arg(d.join("sub")).arg(&outside).stdout(Stdio::null()).status().unwrap();
            assert!(ok.success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, d.join("sub")).unwrap();
        let done = restore(&store, &cp, None, &|_: &[PathBuf]| Ok(())).unwrap();
        assert_eq!(done.failed.len(), 1, "{:?}", done.failed);
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none(), "nothing was written outside the folder");
        let _ = std::fs::remove_dir(d.join("sub"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn a_name_that_only_changed_case_is_not_sent_to_the_trash() {
        let d = temp("case");
        write(&d, "A.txt", b"then");
        let store = Store::Copy { folder: d.clone(), base: temp("case-data").join("checkpoints").join("k") };
        let cp = create(&store, "", false, None).unwrap();
        std::fs::rename(d.join("A.txt"), d.join("a.txt")).unwrap();
        write(&d, "a.txt", b"now");
        let trashed = std::cell::RefCell::new(Vec::<PathBuf>::new());
        let trash = |paths: &[PathBuf]| -> Result<(), String> {
            trashed.borrow_mut().extend(paths.iter().cloned());
            Ok(())
        };
        let added: Vec<String> = plan(&store, &cp).unwrap().items.iter().filter(|i| i.change == Change::Added).map(|i| i.rel.clone()).collect();
        let done = restore(&store, &cp, Some(&added), &trash).unwrap();
        assert!(trashed.borrow().is_empty(), "{:?}", trashed.borrow());
        assert_eq!(std::fs::read(d.join("A.txt")).unwrap(), b"then");
        let undo = restore(&store, &done.auto, Some(&done.back), &trash).unwrap();
        assert!(trashed.borrow().is_empty(), "{:?} {:?}", trashed.borrow(), undo);
        assert_eq!(std::fs::read(d.join("a.txt")).unwrap(), b"now");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pruning_keeps_the_users_checkpoints_apart_from_the_automatic_ones() {
        let d = temp("prune");
        write(&d, "a.txt", b"x");
        let store = Store::Copy { folder: d.clone(), base: temp("prune-data").join("checkpoints").join("k") };
        let mine: Vec<Checkpoint> = (0..3).map(|i| create(&store, &format!("mine {i}"), false, None).unwrap()).collect();
        let autos: Vec<Checkpoint> = (0..3).map(|_| create(&store, "", true, None).unwrap()).collect();
        // the oldest automatic one is what "Undo restore" points at
        assert_eq!(prune(&store, 2, &[autos[0].id.as_str()]), 1);
        let left: Vec<String> = list(&store).unwrap().into_iter().map(|c| c.id).collect();
        for want in [&mine[1].id, &mine[2].id, &autos[0].id, &autos[1].id, &autos[2].id] {
            assert!(left.contains(want), "{want} kept: {left:?}");
        }
        assert!(!left.contains(&mine[0].id));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A name that is not valid UTF-8 (or holds U+FFFD) is copied under its exact name and put back under it.
    #[cfg(unix)]
    #[test]
    fn copy_checkpoint_keeps_names_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let d = temp("bytes");
        let odd = d.join(std::ffi::OsStr::from_bytes(b"sub/caf\xe9 \xff.txt"));
        let fffd = d.join("sub/real\u{FFFD}x41;.txt");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        if std::fs::write(&odd, b"odd name").is_err() {
            return; // (a file system that refuses such names)
        }
        std::fs::write(&fffd, b"replacement char").unwrap();
        let store = Store::Copy { folder: d.clone(), base: temp("bytes-data").join("checkpoints").join("k") };
        let cp = create(&store, "", false, None).unwrap();
        assert_eq!(cp.files, 2);
        let kept = PathBuf::from(&cp.handle).join("files");
        assert_eq!(std::fs::read(kept.join(odd.strip_prefix(&d).unwrap())).unwrap(), b"odd name");
        std::fs::remove_file(&odd).unwrap();
        std::fs::write(&fffd, b"changed").unwrap();
        let done = restore(&store, &cp, None, &|_: &[PathBuf]| Ok(())).unwrap();
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert_eq!(std::fs::read(&odd).unwrap(), b"odd name");
        assert_eq!(std::fs::read(&fffd).unwrap(), b"replacement char");
        // nothing under a lossy spelling
        assert!(!d.join("sub/caf\u{FFFD} \u{FFFD}.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_without_the_tick_removes_nothing() {
        let d = temp("keep");
        write(&d, "a.txt", b"kept");
        let store = Store::Copy { folder: d.clone(), base: temp("keep-data").join("checkpoints").join("k") };
        let cp = create(&store, "", false, None).unwrap();
        write(&d, "added.txt", b"new since");
        write(&d, "deep/er/added.bin", b"\0\0");
        write(&d, "a.txt", b"changed");
        let called = std::cell::Cell::new(false);
        let trash = |_: &[PathBuf]| -> Result<(), String> {
            called.set(true);
            Ok(())
        };
        let done = restore(&store, &cp, None, &trash).unwrap();
        assert!(!called.get(), "nothing goes to the Trash without the tick");
        assert!(done.trashed.is_empty());
        assert!(d.join("added.txt").is_file() && d.join("deep/er/added.bin").is_file());
        assert_eq!(std::fs::read(d.join("a.txt")).unwrap(), b"kept");
        let _ = std::fs::remove_dir_all(&d);
    }
}
