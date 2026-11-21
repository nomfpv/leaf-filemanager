//! Linux backend of rf_win::listing. Status: REAL
//!
//! `read_dir` plus each entry's own metadata (`lstat`: a link is listed as itself, its kind taken from its target). The attributes are
//! the Windows bits the app reads: DIRECTORY from the kind, HIDDEN for a leading dot, READONLY when the owner may not write,
//! REPARSE_POINT (with `link`) for a symbolic link.

use std::fs::{DirEntry, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rf_core::{Entry, EntryKind, attrs};

pub use leaf_common::listing::ListError;

/// Lists `dir` and streams entries to `on_batch` in chunks so the UI can show the first items immediately.
///
/// `cancelled` is polled between batches; return early when the user navigated away.
pub fn list_dir(
    dir: &Path,
    batch_size: usize,
    cancelled: &dyn Fn() -> bool,
    on_batch: &mut dyn FnMut(Vec<Entry>),
) -> Result<usize, ListError> {
    let read = std::fs::read_dir(dir).map_err(|e| map_err(&e))?;
    let mut total = 0usize;
    let mut batch = Vec::with_capacity(batch_size.min(4096));
    let mut result = Ok(());
    for item in read {
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                result = Err(map_err(&e));
                break;
            }
        };
        if let Some(e) = from_dir_entry(dir, &item) {
            batch.push(e);
            if batch.len() >= batch_size {
                total += batch.len();
                on_batch(std::mem::replace(&mut batch, Vec::with_capacity(batch_size.min(4096))));
                if cancelled() {
                    result = Err(ListError::Cancelled);
                    break;
                }
            }
        }
    }
    if !batch.is_empty() {
        total += batch.len();
        on_batch(batch);
    }
    result.map(|_| total)
}

/// The entry `name` in `dir` as [`list_dir`] would list it (the same source, the same fields): `Ok(None)` when there is no such entry
/// (deleted, renamed away), `Err` when it can't be told (no access, the folder itself gone…). Names are told apart by case here.
pub fn stat_entry(dir: &Path, name: &str) -> Result<Option<Entry>, ListError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(ListError::InvalidName);
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(m) => Ok(Some(to_entry(name, &path, &m))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match std::fs::metadata(dir) {
            Ok(d) if d.is_dir() => Ok(None),
            Ok(_) => Err(ListError::InvalidName),
            Err(e) => Err(map_err(&e)),
        },
        Err(e) => Err(map_err(&e)),
    }
}

/// One `read_dir` item. Its metadata comes from `lstat`; when that fails (a folder that may be read but not entered, an item gone
/// meanwhile) the type from the directory itself stands in, without size and times. Nothing at all: left out.
pub(crate) fn from_dir_entry(dir: &Path, item: &DirEntry) -> Option<Entry> {
    let name = item.file_name();
    let name = name.to_string_lossy();
    let path = dir.join(&*name);
    match item.metadata() {
        Ok(m) => Some(to_entry(&name, &path, &m)),
        Err(_) => {
            let t = item.file_type().ok()?;
            let kind = if t.is_dir() { EntryKind::Dir } else { EntryKind::File };
            let mut a = if t.is_dir() { attrs::DIRECTORY } else { 0 };
            if name.starts_with('.') {
                a |= attrs::HIDDEN;
            }
            if t.is_symlink() {
                a |= attrs::REPARSE_POINT;
            }
            let mut e = Entry::new(&name, kind, 0, 0, 0, a);
            e.link = t.is_symlink();
            Some(e)
        }
    }
}

/// `m` is the item's own metadata (not followed).
fn to_entry(name: &str, path: &Path, m: &Metadata) -> Entry {
    let link = m.file_type().is_symlink();
    // a link's kind is its target's (a broken link is a file)
    let is_dir = if link { std::fs::metadata(path).is_ok_and(|t| t.is_dir()) } else { m.is_dir() };
    let kind = if is_dir { EntryKind::Dir } else { EntryKind::File };
    let mut a = if is_dir { attrs::DIRECTORY } else { 0 };
    if name.starts_with('.') {
        a |= attrs::HIDDEN;
    }
    if link {
        a |= attrs::REPARSE_POINT;
    } else if m.mode() & 0o200 == 0 {
        a |= attrs::READONLY;
    }
    // (a link's own size is its target's name: shown as 0, as Windows lists its links)
    let size = if is_dir || link { 0 } else { m.len() };
    let modified = m.mtime() * 1000 + m.mtime_nsec() / 1_000_000;
    let created = m
        .created()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    let mut e = Entry::new(name, kind, size, modified, created, a);
    e.link = link;
    e.mode = (m.mode() & 0o7777) as u16;
    e.uid = m.uid();
    e
}

/// errno to the app's error (the design's table).
fn map_err(e: &io::Error) -> ListError {
    match e.raw_os_error() {
        Some(libc::ENOENT) => ListError::NotFound,
        Some(libc::EACCES | libc::EPERM) => ListError::AccessDenied,
        Some(libc::ENOTDIR | libc::ENAMETOOLONG | libc::EINVAL | libc::EILSEQ) => ListError::InvalidName,
        Some(libc::ENXIO | libc::ENODEV | libc::ENOMEDIUM) => ListError::NotReady,
        Some(libc::EHOSTDOWN | libc::EHOSTUNREACH | libc::ENETUNREACH | libc::ETIMEDOUT | libc::ESTALE) => ListError::NetworkPath,
        Some(n) => ListError::Os(n as u32),
        None => ListError::Os(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    /// A fresh folder under the temporary folder (TMPDIR), removed by the guard.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("leaf-unix-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn all(dir: &Path) -> Vec<Entry> {
        let mut v = Vec::new();
        list_dir(dir, 100, &|| false, &mut |b| v.extend(b)).unwrap();
        v
    }

    #[test]
    fn kinds_and_attributes_of_files_dirs_dotfiles_readonly_and_links() {
        let s = Scratch::new("kinds");
        let d = &s.0;
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("plain.txt"), b"hello").unwrap();
        std::fs::write(d.join(".hidden"), b"x").unwrap();
        std::fs::write(d.join("ro.txt"), b"abc").unwrap();
        std::fs::set_permissions(d.join("ro.txt"), std::fs::Permissions::from_mode(0o444)).unwrap();
        symlink(d.join("plain.txt"), d.join("to-file")).unwrap();
        symlink(d.join("sub"), d.join("to-dir")).unwrap();
        symlink(d.join("nowhere"), d.join("broken")).unwrap();

        let v = all(d);
        assert_eq!(v.len(), 7);
        let get = |n: &str| v.iter().find(|e| &*e.name == n).unwrap_or_else(|| panic!("{n} listed"));

        let sub = get("sub");
        assert!(sub.is_dir() && sub.attrs & attrs::DIRECTORY != 0 && !sub.link && sub.size == 0);
        let plain = get("plain.txt");
        assert!(!plain.is_dir() && plain.size == 5 && &*plain.ext == "txt" && plain.attrs == 0 && !plain.link);
        assert!(plain.modified_ms > 1_600_000_000_000, "a real modification time: {}", plain.modified_ms);
        let hidden = get(".hidden");
        assert!(hidden.is_hidden() && &*hidden.ext == "", "a dotfile is hidden and has no extension");
        let ro = get("ro.txt");
        assert!(ro.attrs & attrs::READONLY != 0 && !ro.is_hidden());
        let tf = get("to-file");
        assert!(tf.link && tf.is_symlink() && !tf.is_dir());
        let td = get("to-dir");
        assert!(td.link && td.is_dir() && td.attrs & attrs::DIRECTORY != 0, "a link to a folder is a folder");
        let br = get("broken");
        assert!(br.link && !br.is_dir(), "a broken link is listed as a file");

        // stat_entry tells the same
        let one = stat_entry(d, "to-dir").unwrap().unwrap();
        assert!(one.link && one.is_dir());
        assert_eq!(stat_entry(d, "ro.txt").unwrap().unwrap().attrs, ro.attrs);
        assert!(stat_entry(d, "broken").unwrap().unwrap().link);
        assert!(stat_entry(d, "gone.txt").unwrap().is_none());
        assert!(stat_entry(d, "PLAIN.TXT").unwrap().is_none(), "names are told apart by case");
        assert!(matches!(stat_entry(d, "a/b"), Err(ListError::InvalidName)));
        assert!(matches!(stat_entry(d, ""), Err(ListError::InvalidName)));
        assert!(matches!(stat_entry(&d.join("missing"), "x"), Err(ListError::NotFound)));
        assert!(matches!(stat_entry(&d.join("plain.txt"), "x"), Err(ListError::InvalidName)));
    }

    #[test]
    fn five_thousand_entries_come_in_batches_and_a_cancel_stops_after_a_batch() {
        let s = Scratch::new("many");
        for i in 0..5000 {
            std::fs::write(s.0.join(format!("f{i}.dat")), b"").unwrap();
        }
        let mut batches = Vec::new();
        let n = list_dir(&s.0, 512, &|| false, &mut |b| batches.push(b.len())).unwrap();
        assert_eq!(n, 5000);
        assert_eq!(batches.iter().sum::<usize>(), 5000);
        assert_eq!(batches.len(), 10, "{batches:?}");
        assert!(batches[..9].iter().all(|&b| b == 512));

        // cancelled after the second batch: two batches were handed over, then Cancelled
        let calls = std::cell::Cell::new(0);
        let mut got = 0usize;
        let r = list_dir(&s.0, 1000, &|| {
            calls.set(calls.get() + 1);
            calls.get() >= 2
        }, &mut |b| got += b.len());
        assert!(matches!(r, Err(ListError::Cancelled)), "{r:?}");
        assert_eq!(got, 2000);
    }

    #[test]
    fn errors_map_to_the_apps_errors() {
        let s = Scratch::new("errors");
        assert!(matches!(list_dir(&s.0.join("missing"), 10, &|| false, &mut |_| {}), Err(ListError::NotFound)));
        std::fs::write(s.0.join("file"), b"x").unwrap();
        assert!(matches!(list_dir(&s.0.join("file"), 10, &|| false, &mut |_| {}), Err(ListError::InvalidName)));
        // a folder that may not be read (root may read anything: then this part says nothing)
        let locked = s.0.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = list_dir(&locked, 10, &|| false, &mut |_| {});
        let root = std::fs::read_dir(&locked).is_ok();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !root {
            assert!(matches!(r, Err(ListError::AccessDenied)), "{r:?}");
        }
        assert!(matches!(map_err(&io::Error::from_raw_os_error(libc::ENOMEDIUM)), ListError::NotReady));
        assert!(matches!(map_err(&io::Error::from_raw_os_error(libc::ESTALE)), ListError::NetworkPath));
        assert!(matches!(map_err(&io::Error::from_raw_os_error(libc::EIO)), ListError::Os(5)));
        assert_eq!(ListError::Os(5).to_string(), "Listing error (code 5)");
    }

    #[test]
    fn an_empty_folder_lists_nothing() {
        let s = Scratch::new("empty");
        let mut called = false;
        assert_eq!(list_dir(&s.0, 10, &|| false, &mut |_| called = true).unwrap(), 0);
        assert!(!called);
    }
}
