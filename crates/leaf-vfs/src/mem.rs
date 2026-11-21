//! `mem://`: a filesystem in memory. It is the reference for the semantics of `Fs` (the shared tests run against it and
//! against `LocalFs`) and the demo behind `mem://demo/` in the app. Every authority is its own tree. Two authorities answer
//! with errors on purpose, to show the app's error banner without a network: `mem://offline/` (Offline) and
//! `mem://locked/` (Auth).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, VPath, WriteMode, WriteOpts, WriteSink, now_ms};

#[derive(Debug, Clone)]
enum Node {
    Dir { mtime: i64 },
    File { data: Vec<u8>, mtime: i64 },
    Link { target: String, mtime: i64 },
}

type Key = (String, String);
type Tree = BTreeMap<Key, Node>;

#[derive(Default)]
pub struct MemFs {
    tree: Arc<Mutex<Tree>>,
}

fn key(p: &VPath) -> Key {
    (p.authority().to_string(), p.path().to_string())
}

fn check_server(p: &VPath) -> Result<(), FsError> {
    match p.authority() {
        "offline" => Err(FsError::Offline("mem://offline/ is never reachable (a test server)".into())),
        "locked" => Err(FsError::Auth("mem://locked/ refuses every login (a test server)".into())),
        _ => Ok(()),
    }
}

impl MemFs {
    pub fn new() -> MemFs {
        MemFs::default()
    }

    /// The tree `mem://demo/` shows: a few folders and files of every kind the list draws.
    pub fn with_demo() -> MemFs {
        let fs = MemFs::new();
        let root = VPath::parse("mem://demo/").expect("demo address");
        let day = 86_400_000i64;
        let t0 = 1_763_000_000_000i64; // November 2025
        let dirs = ["Documents", "Pictures", "Projects", "Projects/leaf", "Projects/leaf/src", "Empty folder"];
        for (i, d) in dirs.iter().enumerate() {
            fs.put_dir(&root.join(d), t0 - i as i64 * day);
        }
        let files: &[(&str, usize)] = &[
            ("readme.txt", 1_200),
            ("notes.md", 4_800),
            ("Documents/report.pdf", 220_000),
            ("Documents/budget.xlsx", 36_000),
            ("Pictures/beach.jpg", 2_400_000),
            ("Pictures/sunset.png", 3_100_000),
            ("Projects/leaf/Cargo.toml", 900),
            ("Projects/leaf/src/main.rs", 2_600),
            ("archive.zip", 12_000_000),
            ("movie.mkv", 700_000_000),
        ];
        for (i, &(f, size)) in files.iter().enumerate() {
            // real bytes only for small files; the big ones are patterns of their size
            let data = if size <= 8_192 { format!("{f}\n").into_bytes().into_iter().cycle().take(size).collect() } else { vec![0u8; size.min(64 * 1024)] };
            fs.put_file(&root.join(f), data, t0 - (i as i64 + 2) * day / 3);
        }
        fs.put_link(&root.join("shortcut to Pictures"), "/Pictures");
        fs
    }

    /// Test and demo helpers (no checks: they make the parents as well).
    pub fn put_dir(&self, p: &VPath, mtime: i64) {
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        let mut cur = p.root();
        for seg in p.segments() {
            cur = cur.join(seg);
            t.entry(key(&cur)).or_insert(Node::Dir { mtime });
        }
    }
    pub fn put_file(&self, p: &VPath, data: Vec<u8>, mtime: i64) {
        if let Some(par) = p.parent() {
            self.put_dir(&par, mtime);
        }
        self.tree.lock().unwrap_or_else(|e| e.into_inner()).insert(key(p), Node::File { data, mtime });
    }
    /// A symbolic link at `p` pointing to `target` (a path on the same server).
    pub fn put_link(&self, p: &VPath, target: &str) {
        if let Some(par) = p.parent() {
            self.put_dir(&par, now_ms());
        }
        self.tree.lock().unwrap_or_else(|e| e.into_inner()).insert(key(p), Node::Link { target: target.into(), mtime: now_ms() });
    }
}

fn get<'a>(t: &'a Tree, p: &VPath) -> Option<&'a Node> {
    if p.is_root() {
        static ROOT: Node = Node::Dir { mtime: 0 };
        return Some(t.get(&key(p)).unwrap_or(&ROOT));
    }
    t.get(&key(p))
}

fn parent_is_dir(t: &Tree, p: &VPath) -> Result<(), FsError> {
    match p.parent().and_then(|par| get(t, &par).cloned()) {
        Some(Node::Dir { .. }) => Ok(()),
        Some(_) => Err(FsError::Other("the parent is not a folder".into())),
        None => Err(FsError::NotFound),
    }
}

/// The keys strictly under `p` (all depths).
fn under(t: &Tree, p: &VPath) -> Vec<Key> {
    let (auth, path) = key(p);
    let prefix = if path == "/" { "/".to_string() } else { format!("{path}/") };
    t.range((auth.clone(), prefix.clone())..)
        .take_while(|((a, k), _)| *a == auth && k.starts_with(&prefix))
        .filter(|((_, k), _)| *k != path)
        .map(|(k, _)| k.clone())
        .collect()
}

fn entry_of(name: &str, n: &Node) -> Entry {
    match n {
        Node::Dir { mtime } => Entry::new(name, EntryKind::Dir, 0, *mtime, *mtime, rf_core::attrs::DIRECTORY),
        Node::File { data, mtime } => Entry::new(name, EntryKind::File, data.len() as u64, *mtime, *mtime, 0),
        Node::Link { mtime, .. } => {
            let mut e = Entry::new(name, EntryKind::File, 0, *mtime, *mtime, rf_core::attrs::REPARSE_POINT);
            e.link = true;
            e
        }
    }
}

impl Fs for MemFs {
    fn caps(&self) -> Caps {
        Caps { write: true, set_times: true, links: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        check_server(dir)?;
        let entries: Vec<Entry> = {
            let t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
            match get(&t, dir) {
                Some(Node::Dir { .. }) => {}
                Some(_) => return Err(FsError::Other("not a folder".into())),
                None => return Err(FsError::NotFound),
            }
            under(&t, dir)
                .into_iter()
                .filter_map(|k| {
                    let rest = k.1.strip_prefix(dir.path())?.trim_start_matches('/');
                    (!rest.contains('/')).then(|| entry_of(rest, &t[&k]))
                })
                .collect()
        };
        let n = entries.len();
        let mut it = entries.into_iter().peekable();
        while it.peek().is_some() {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            on_batch(it.by_ref().take(batch.max(1)).collect());
        }
        Ok(n)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        check_server(p)?;
        let t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        let n = get(&t, p).ok_or(FsError::NotFound)?;
        Ok(match n {
            Node::Dir { mtime } => Meta { kind: Kind::Dir, size: 0, modified_ms: *mtime, created_ms: *mtime, mode: None },
            Node::File { data, mtime } => Meta { kind: Kind::File, size: data.len() as u64, modified_ms: *mtime, created_ms: *mtime, mode: None },
            Node::Link { target, mtime } => Meta { kind: Kind::Link, size: target.len() as u64, modified_ms: *mtime, created_ms: *mtime, mode: None },
        })
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        check_server(p)?;
        let t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        match get(&t, p) {
            Some(Node::File { data, .. }) => {
                let start = (offset as usize).min(data.len());
                Ok(Box::new(std::io::Cursor::new(data[start..].to_vec())))
            }
            Some(_) => Err(FsError::Other("not a file".into())),
            None => Err(FsError::NotFound),
        }
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        check_server(p)?;
        if p.is_root() {
            return Err(FsError::Exists);
        }
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        parent_is_dir(&t, p)?;
        let k = key(p);
        let pos = match (t.get(&k), o.mode) {
            (Some(Node::File { .. }), WriteMode::CreateNew) => return Err(FsError::Exists),
            (Some(Node::Dir { .. } | Node::Link { .. }), _) => return Err(FsError::Exists),
            (Some(Node::File { data, .. }), WriteMode::At(off)) => {
                if off > data.len() as u64 {
                    return Err(FsError::Other("the offset is past the end of the file".into()));
                }
                off as usize
            }
            (None, WriteMode::At(_)) => return Err(FsError::NotFound),
            _ => 0,
        };
        let now = now_ms();
        match t.get_mut(&k) {
            Some(Node::File { data, mtime }) => {
                data.truncate(pos);
                *mtime = now;
            }
            _ => {
                t.insert(k.clone(), Node::File { data: Vec::new(), mtime: now });
            }
        }
        Ok(Box::new(MemSink { tree: self.tree.clone(), key: k }))
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        check_server(p)?;
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        if get(&t, p).is_some() {
            return Err(FsError::Exists);
        }
        parent_is_dir(&t, p)?;
        t.insert(key(p), Node::Dir { mtime: now_ms() });
        Ok(())
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        check_server(from)?;
        if from.authority() != to.authority() || from.scheme() != to.scheme() {
            return Err(FsError::Unsupported);
        }
        if from.is_root() {
            return Err(FsError::Denied);
        }
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        let node = get(&t, from).cloned().ok_or(FsError::NotFound)?;
        match get(&t, to) {
            Some(Node::File { .. }) if replace && !matches!(node, Node::Dir { .. }) => {}
            Some(_) => return Err(FsError::Exists),
            None => {}
        }
        parent_is_dir(&t, to)?;
        let moved = under(&t, from);
        t.remove(&key(from));
        t.insert(key(to), node);
        for k in moved {
            if let Some(n) = t.remove(&k) {
                let rest = &k.1[from.path().len()..];
                t.insert((k.0, format!("{}{rest}", to.path())), n);
            }
        }
        Ok(())
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        check_server(p)?;
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        let n = get(&t, p).ok_or(FsError::NotFound)?;
        if matches!(n, Node::Dir { .. }) {
            let inside = under(&t, p);
            if !inside.is_empty() && !recursive {
                return Err(FsError::NotEmpty);
            }
            for k in inside {
                t.remove(&k);
            }
        }
        t.remove(&key(p));
        Ok(())
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        check_server(p)?;
        if m.mode.is_some() {
            return Err(FsError::Unsupported);
        }
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        if p.is_root() && !t.contains_key(&key(p)) {
            t.insert(key(p), Node::Dir { mtime: 0 });
        }
        let n = t.get_mut(&key(p)).ok_or(FsError::NotFound)?;
        if let Some(ms) = m.modified_ms {
            match n {
                Node::Dir { mtime } | Node::File { mtime, .. } | Node::Link { mtime, .. } => *mtime = ms,
            }
        }
        Ok(())
    }
}

struct MemSink {
    tree: Arc<Mutex<Tree>>,
    key: Key,
}

impl Write for MemSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        match t.get_mut(&self.key) {
            Some(Node::File { data, mtime }) => {
                data.extend_from_slice(buf);
                *mtime = now_ms();
                Ok(buf.len())
            }
            _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "the file was removed while it was written")),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl WriteSink for MemSink {
    fn finish(self: Box<Self>) -> Result<(), FsError> {
        let t = self.tree.lock().unwrap_or_else(|e| e.into_inner());
        match t.get(&self.key) {
            Some(Node::File { .. }) => Ok(()),
            _ => Err(FsError::NotFound),
        }
    }
}
