//! Linux backend of rf_win::recycle. Status: REAL (freedesktop Trash spec 1.0: home trash, `$topdir/.Trash/$uid`,
//! `$topdir/.Trash-$uid`; delete, list, restore, delete for good, empty).
//!
//! Data safety:
//! - The `.trashinfo` file is created first with `O_EXCL` (this reserves the name), then the item is renamed into `files/` with
//!   `RENAME_NOREPLACE`. When the rename fails, the info file is removed again and the item stays where it was.
//! - Only a trash on the item's own device is used. When there is none (a USB stick without a writable top folder, a network share),
//!   [`trash_tracked`] changes nothing and answers [`NoTrash`]: the app must ask (owner's decision D1: "Delete permanently" or "Move
//!   to home trash"). Nothing is ever copied into the home trash, nor deleted, without that answer.
//! - `$topdir/.Trash` is only used when it is a real folder (not a link) with the sticky bit; `$uid` folders must be real folders that
//!   belong to us.
//! - Restore never replaces: an item that holds the original name stops that restore with an error; the item stays in the trash.
//! - Deleting for good (empty, delete by key) only touches items inside a known trash's `files/` folder.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub use leaf_common::recycle::RecycleItem;

use crate::fileops::Tracker;

/// The answer of a move to the trash when some items lie on a device without a usable trash. Nothing was changed; the app asks the
/// user (decision D1): delete them permanently, or move them into the home trash ([`to_home_trash`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoTrash {
    pub paths: Vec<PathBuf>,
}

impl std::fmt::Display for NoTrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("This drive has no trash that can be used")
    }
}

impl std::error::Error for NoTrash {}

/// One trash folder: `dir` holds `files/` and `info/`; `top` is the top folder of its device for a `$topdir` trash (the `Path=` of
/// its info files is relative to it), `None` for the home trash (absolute `Path=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Trash {
    pub dir: PathBuf,
    pub top: Option<PathBuf>,
}

impl Trash {
    fn files(&self) -> PathBuf {
        self.dir.join("files")
    }
    fn info(&self) -> PathBuf {
        self.dir.join("info")
    }
    fn info_of(&self, name: &std::ffi::OsStr) -> PathBuf {
        let mut n = name.to_os_string();
        n.push(".trashinfo");
        self.info().join(n)
    }
}

fn uid() -> u32 {
    // SAFETY: getuid cannot fail and takes no arguments.
    unsafe { libc::getuid() }
}

/// `$XDG_DATA_HOME/Trash` (`~/.local/share/Trash`).
fn home_trash_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("Trash"))
}

/// Makes `files/` and `info/` (mode 0700) and checks that the trash is a real folder on device `dev`.
fn ready(t: &Trash, dev: u64) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true).mode(0o700);
    b.create(t.files())?;
    b.create(t.info())?;
    for d in [&t.dir, &t.files(), &t.info()] {
        let m = std::fs::symlink_metadata(d)?;
        if !m.is_dir() || m.dev() != dev {
            return Err(io::Error::other(format!("{}: not a usable trash folder", d.display())));
        }
    }
    Ok(())
}

/// The top folder of the device `p` (a folder that exists) is on: the highest ancestor still on the same device.
fn mount_top(p: &Path) -> Option<PathBuf> {
    let p = std::fs::canonicalize(p).ok()?;
    let dev = std::fs::metadata(&p).ok()?.dev();
    let mut top = p.clone();
    for a in p.ancestors().skip(1) {
        match std::fs::metadata(a) {
            Ok(m) if m.dev() == dev => top = a.to_path_buf(),
            _ => break,
        }
    }
    Some(top)
}

/// The trash of a `$topdir`: `.Trash/$uid` when `.Trash` is a real sticky folder, else `.Trash-$uid` (made when missing). Each must
/// be a real folder that is ours.
fn top_trash(top: &Path, dev: u64, create: bool) -> Option<Trash> {
    let ours = |d: &Path| matches!(std::fs::symlink_metadata(d), Ok(m) if m.is_dir() && m.uid() == uid() && m.dev() == dev);
    let shared = top.join(".Trash");
    if let Ok(m) = std::fs::symlink_metadata(&shared) {
        if m.is_dir() && !m.file_type().is_symlink() && m.mode() & libc::S_ISVTX as u32 != 0 && m.dev() == dev {
            let d = shared.join(uid().to_string());
            if create && std::fs::symlink_metadata(&d).is_err() {
                let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
            }
            if ours(&d) {
                let t = Trash { dir: d, top: Some(top.to_path_buf()) };
                if !create || ready(&t, dev).is_ok() {
                    return Some(t);
                }
            }
        }
    }
    let d = top.join(format!(".Trash-{}", uid()));
    if create && std::fs::symlink_metadata(&d).is_err() {
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
    }
    if ours(&d) {
        let t = Trash { dir: d, top: Some(top.to_path_buf()) };
        if !create || ready(&t, dev).is_ok() {
            return Some(t);
        }
    }
    None
}

/// The trash an item at `path` goes to (made ready): the home trash when it is on the same device, else its device's own trash.
/// `None`: no trash on that device can be used.
fn trash_for(path: &Path) -> Option<Trash> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let item = std::fs::symlink_metadata(path).ok()?;
    let dev = std::fs::metadata(parent).ok()?.dev();
    if item.dev() != dev {
        // a mount point itself: it cannot be moved anywhere
        return None;
    }
    if let Some(home) = home_trash_dir() {
        // (the trash may not exist yet: its nearest existing ancestor tells the device)
        let home_dev = home.ancestors().find_map(|a| std::fs::metadata(a).ok()).map(|m| m.dev());
        if home_dev == Some(dev) {
            let t = Trash { dir: home, top: None };
            if ready(&t, dev).is_ok() {
                return Some(t);
            }
        }
    }
    top_trash(&mount_top(parent)?, dev, true)
}

/// Every trash folder that exists (none are made): the home trash, then each mount's.
pub(crate) fn all_trashes() -> Vec<Trash> {
    let mut out = Vec::new();
    if let Some(home) = home_trash_dir().filter(|h| h.is_dir()) {
        out.push(Trash { dir: home, top: None });
    }
    for d in crate::drives::list_drives() {
        let Ok(m) = std::fs::metadata(&d.root) else { continue };
        let top = d.root.clone();
        for t in [top.join(".Trash").join(uid().to_string()), top.join(format!(".Trash-{}", uid()))] {
            if matches!(std::fs::symlink_metadata(&t), Ok(x) if x.is_dir() && x.uid() == uid() && x.dev() == m.dev())
                && !out.iter().any(|o: &Trash| o.dir == t)
            {
                out.push(Trash { dir: t, top: Some(top.clone()) });
            }
        }
    }
    out
}

/// URI-escapes a path for `Path=` (RFC 2396: everything but unreserved characters and `/`).
fn escape(p: &Path) -> String {
    let mut s = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b) {
            s.push(b as char);
        } else {
            let _ = write!(s, "%{b:02X}");
        }
    }
    s
}

fn unescape(s: &str) -> PathBuf {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

/// `(original path, deletion time in Unix ms)` from a `.trashinfo` text; relative paths are joined to `top`.
fn parse_info(text: &str, top: Option<&Path>) -> Option<(PathBuf, i64)> {
    let mut in_group = false;
    let (mut path, mut ms) = (None, 0);
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('[') {
            in_group = line == "[Trash Info]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some(v) = line.strip_prefix("Path=") {
            let p = unescape(v);
            path = Some(if p.is_absolute() { p } else { top?.join(p) });
        } else if let Some(v) = line.strip_prefix("DeletionDate=") {
            use chrono::TimeZone;
            if let Ok(t) = chrono::NaiveDateTime::parse_from_str(v.trim(), "%Y-%m-%dT%H:%M:%S") {
                ms = chrono::Local.from_local_datetime(&t).earliest().map(|d| d.timestamp_millis()).unwrap_or(0);
            }
        }
    }
    Some((path?, ms))
}

/// The absolute path of `path` with its folder resolved (the item itself is not followed: a link is trashed as a link).
fn absolute(path: &Path) -> io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(std::fs::canonicalize(parent)?.join(name))
}

/// Candidate names in the trash: `name`, `name.2`, `name.3`, … (before the extension of a file: `photo.2.jpg`).
fn candidate(name: &std::ffi::OsStr, i: u32) -> std::ffi::OsString {
    if i == 1 {
        return name.to_os_string();
    }
    let p = Path::new(name);
    match (p.file_stem(), p.extension()) {
        (Some(stem), Some(ext)) => {
            let mut s = stem.to_os_string();
            s.push(format!(".{i}."));
            s.push(ext);
            s
        }
        _ => {
            let mut s = name.to_os_string();
            s.push(format!(".{i}"));
            s
        }
    }
}

/// Moves one item into `t`: info file first (`O_EXCL`), then the no-replace rename. The item's place in `files/`.
pub(crate) fn put(t: &Trash, path: &Path) -> io::Result<PathBuf> {
    let orig = absolute(path)?;
    let name = orig.file_name().ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?.to_os_string();
    let shown = match &t.top {
        Some(top) => orig.strip_prefix(top).map(Path::to_path_buf).unwrap_or_else(|_| orig.clone()),
        None => orig.clone(),
    };
    let text = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", escape(&shown), chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"));
    for i in 1..10_000 {
        let cand = candidate(&name, i);
        let info = t.info_of(&cand);
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = f.write_all(text.as_bytes()).and_then(|_| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(&info);
            return Err(e);
        }
        drop(f);
        let dst = t.files().join(&cand);
        match crate::fileops::renameat2(path, &dst, libc::RENAME_NOREPLACE) {
            Ok(()) => return Ok(dst),
            Err(e) => {
                let _ = std::fs::remove_file(&info);
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
        }
    }
    Err(io::Error::other("no free name in the trash"))
}

/// Moves `paths` to the trash with in-app progress. Each item goes to the trash of its own device. When any of them has no usable
/// trash, nothing at all is moved and the answer is [`NoTrash`] (the app asks). `Tracker.results` gets (source, `Trash/files/<name>`)
/// for every item moved. `Ok(false)` after a cancel.
pub fn trash_tracked(paths: &[PathBuf], tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    let mut plan = Vec::new();
    let mut none = Vec::new();
    for p in paths {
        match trash_for(p) {
            Some(t) => plan.push((p, t)),
            None => none.push(p.clone()),
        }
    }
    if !none.is_empty() {
        return Err(NoTrash { paths: none }.into());
    }
    let total = plan.len() as u32;
    let mut failed = Vec::new();
    for (i, (p, t)) in plan.iter().enumerate() {
        while tracker.paused.load(Ordering::Relaxed) && !tracker.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if tracker.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        (tracker.on_progress)(total, i as u32, p.file_name().map(|n| n.to_string_lossy().into_owned()));
        match put(t, p) {
            Ok(dst) => tracker.results.lock().unwrap().push(((*p).clone(), dst)),
            Err(e) => failed.push(format!("{}: {e}", p.display())),
        }
    }
    (tracker.on_progress)(total, total, None);
    anyhow::ensure!(failed.is_empty(), "{} item(s) could not be moved to the trash: {}", failed.len(), failed.join("; "));
    Ok(true)
}

/// Moves `paths` (items on a device without a usable trash, see [`NoTrash`]) into the home trash: the user's second choice of D1.
/// Not available yet: the cross-device copy comes with the copy engine.
pub fn to_home_trash(_paths: &[PathBuf], _tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    leaf_common::stub_err!("recycle::to_home_trash")
}

/// The items of one trash.
fn list_one(t: &Trash) -> Vec<RecycleItem> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(t.info()) else { return out };
    for e in rd.flatten() {
        let file = e.file_name();
        let Some(name) = file.as_bytes().strip_suffix(b".trashinfo") else { continue };
        let name = std::ffi::OsStr::from_bytes(name);
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let Some((orig, ms)) = parse_info(&text, t.top.as_deref()) else { continue };
        let at = t.files().join(name);
        // (an info file whose item is gone is left alone: it is not shown)
        let Ok(m) = std::fs::symlink_metadata(&at) else { continue };
        out.push(RecycleItem {
            name: orig.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            original_location: orig.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            deleted_ms: ms,
            size: if m.is_dir() { 0 } else { m.len() },
            parsing_path: at.to_string_lossy().into_owned(),
            is_dir: m.is_dir(),
        });
    }
    out
}

/// Lists everything in every trash.
pub fn list() -> anyhow::Result<Vec<RecycleItem>> {
    Ok(all_trashes().iter().flat_map(list_one).collect())
}

/// Whether two keys (`parsing_path`) name the same trashed item: exactly equal here (Linux tells `a.txt` and `A.txt` apart).
pub fn same_key(a: &str, b: &str) -> bool {
    a == b
}

/// The trash a key (`Trash/files/<name>`) belongs to, when it is directly inside a known trash's `files/` folder.
fn owner_of(key: &Path, trashes: &[Trash]) -> Option<Trash> {
    let parent = key.parent()?;
    key.file_name()?;
    trashes.iter().find(|t| t.files() == parent).cloned()
}

/// Puts one trashed item back where its info file says it came from. Never replaces.
fn restore_one(t: &Trash, at: &Path) -> anyhow::Result<()> {
    let name = at.file_name().ok_or_else(|| anyhow::anyhow!("not a trashed item"))?;
    let info = t.info_of(name);
    let (orig, _) = parse_info(&std::fs::read_to_string(&info)?, t.top.as_deref()).ok_or_else(|| anyhow::anyhow!("unreadable trash record"))?;
    anyhow::ensure!(std::fs::symlink_metadata(&orig).is_err(), "{}: an item with this name already exists", orig.display());
    if let Some(parent) = orig.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::fileops::renameat2(at, &orig, libc::RENAME_NOREPLACE).map_err(|e| anyhow::anyhow!("{}: {e}", orig.display()))?;
    // (the item is back; a record that cannot be removed only leaves a hidden orphan)
    if let Err(e) = std::fs::remove_file(&info) {
        log::warn!("trash: {} not removed ({e})", info.display());
    }
    Ok(())
}

/// Restores `items` to their original locations. Each that cannot be restored (its name is taken, its record is gone) stays in the
/// trash and is named in the error; the others are restored.
pub fn restore(items: &[RecycleItem]) -> anyhow::Result<()> {
    let trashes = all_trashes();
    let mut failed = Vec::new();
    for it in items {
        let at = PathBuf::from(&it.parsing_path);
        let r = match owner_of(&at, &trashes) {
            Some(t) => restore_one(&t, &at),
            None => Err(anyhow::anyhow!("not in a trash")),
        };
        if let Err(e) = r {
            failed.push(format!("{}: {e}", it.name));
        }
    }
    anyhow::ensure!(failed.is_empty(), "{} item(s) could not be restored: {}", failed.len(), failed.join("; "));
    Ok(())
}

/// Deletes one trashed item for good, with its record.
fn delete_one(t: &Trash, at: &Path) -> anyhow::Result<()> {
    crate::fastdelete::remove_tree(at)?;
    if let Some(name) = at.file_name() {
        let _ = std::fs::remove_file(t.info_of(name));
    }
    Ok(())
}

/// Deletes exactly the trashed items whose keys are given, for good. A key that is not directly inside a known trash's `files/` folder
/// is refused (nothing outside a trash is ever touched).
pub fn delete_by_keys(keys: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(!keys.is_empty(), "no items to delete");
    let trashes = all_trashes();
    let mut failed = Vec::new();
    for k in keys {
        let at = PathBuf::from(k);
        let r = match owner_of(&at, &trashes) {
            Some(t) => delete_one(&t, &at),
            None => Err(anyhow::anyhow!("not in a trash")),
        };
        if let Err(e) = r {
            failed.push(format!("{k}: {e}"));
        }
    }
    anyhow::ensure!(failed.is_empty(), "{} item(s) could not be deleted: {}", failed.len(), failed.join("; "));
    Ok(())
}

/// The real trash behind [`rf_core::recycle_bin::BinBackend`].
// TYPE-COPY(rf-win recycle.rs:284)
pub struct ShellBin;

/// Empties every trash. `confirm == true` asks for the system's own confirmation dialog, which does not exist here: that is refused
/// (nothing is deleted); the app asks itself and passes `false`.
pub fn empty(confirm: bool) -> anyhow::Result<()> {
    if confirm {
        return leaf_common::stub_err!("recycle::empty (system confirmation)");
    }
    let mut failed = 0;
    for t in all_trashes() {
        if let Ok(rd) = std::fs::read_dir(t.files()) {
            for e in rd.flatten() {
                if delete_one(&t, &e.path()).is_err() {
                    failed += 1;
                }
            }
        }
        // records whose item is gone
        if let Ok(rd) = std::fs::read_dir(t.info()) {
            for e in rd.flatten() {
                let n = e.file_name();
                if let Some(stem) = n.as_bytes().strip_suffix(b".trashinfo") {
                    if std::fs::symlink_metadata(t.files().join(std::ffi::OsStr::from_bytes(stem))).is_err() {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} item(s) could not be deleted from the trash");
    Ok(())
}

impl rf_core::recycle_bin::BinBackend for ShellBin {
    fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(list()?.into_iter().map(|i| i.parsing_path).collect())
    }

    fn delete_keys(&self, keys: &[String]) -> anyhow::Result<()> {
        delete_by_keys(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_restore_round_trip_never_replacing() {
        // a trash of our own in a throwaway folder (the user's real trash is never touched by the test)
        let root = std::env::temp_dir().join(format!("leaf-unix-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work dir");
        std::fs::create_dir_all(work.join("folder/inner")).unwrap();
        let dev = std::fs::metadata(&root).unwrap().dev();
        let t = Trash { dir: root.join("Trash"), top: None };
        ready(&t, dev).unwrap();
        let file = work.join("a b%.txt");
        std::fs::write(&file, b"one").unwrap();
        std::fs::write(work.join("folder/inner/x"), b"x").unwrap();

        let in_trash = put(&t, &file).unwrap();
        assert_eq!(in_trash, t.files().join("a b%.txt"));
        assert!(std::fs::symlink_metadata(&file).is_err());
        let info = std::fs::read_to_string(t.info_of(in_trash.file_name().unwrap())).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
        assert!(info.contains("work%20dir/a%20b%25.txt"), "{info}");
        // a second item of the same name gets another name; nothing in the trash is replaced
        std::fs::write(&file, b"two").unwrap();
        let second = put(&t, &file).unwrap();
        assert_eq!(second, t.files().join("a b%.2.txt"));
        assert_eq!(std::fs::read(&in_trash).unwrap(), b"one");
        let dir_in_trash = put(&t, &work.join("folder")).unwrap();

        let items = list_one(&t);
        assert_eq!(items.len(), 3);
        let first = items.iter().find(|i| i.parsing_path == in_trash.to_string_lossy()).unwrap();
        assert_eq!(first.name, "a b%.txt");
        assert_eq!(Path::new(&first.original_location), std::fs::canonicalize(&work).unwrap());
        assert!(first.deleted_ms > 0);
        assert_eq!(first.size, 3);

        // restore: back where it was; a taken name is refused and the item stays in the trash
        restore_one(&t, &in_trash).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        assert!(restore_one(&t, &second).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"one", "not replaced");
        assert_eq!(std::fs::read(&second).unwrap(), b"two", "still in the trash");
        restore_one(&t, &dir_in_trash).unwrap();
        assert_eq!(std::fs::read(work.join("folder/inner/x")).unwrap(), b"x");
        assert_eq!(list_one(&t).len(), 1);
        // deleting for good stays inside the trash
        assert!(owner_of(&file, std::slice::from_ref(&t)).is_none());
        delete_one(&t, &second).unwrap();
        assert!(list_one(&t).is_empty());
        assert_eq!(std::fs::read_dir(t.info()).unwrap().count(), 0);
        assert_eq!(parse_info("[Trash Info]\nPath=sub/f%C3%BC.txt\nDeletionDate=2026-11-21T07:00:00\n", Some(Path::new("/media/usb"))).unwrap().0, PathBuf::from("/media/usb/sub/fü.txt"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
