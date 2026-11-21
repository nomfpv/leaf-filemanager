//! Linux backend of rf_win::fileops. Status: REAL (rename, new items, routing of copy/move to `fastcopy` and of delete to `recycle`
//! or `fastdelete`).
//!
//! Data safety: a rename never replaces an existing item (`renameat2(RENAME_NOREPLACE)`; there is no "exists, then rename" check
//! anywhere). There is no shell engine on Linux: transfers always go through our own engine, deletes through our own trash or our own
//! permanent delete.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use leaf_common::fileops::{ByteProgress, Tracker, Transfer};

/// A path as a C string (an inner NUL byte cannot be in a real Linux path: it is an error).
pub(crate) fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// The last OS error after a failed libc call.
pub(crate) fn last_err() -> io::Error {
    io::Error::last_os_error()
}

/// `renameat2(AT_FDCWD, from, AT_FDCWD, to, flags)` through the system call (glibc before 2.28 has no wrapper).
pub(crate) fn renameat2(from: &Path, to: &Path, flags: libc::c_uint) -> io::Result<()> {
    let (a, b) = (cpath(from)?, cpath(to)?);
    // SAFETY: both pointers are NUL-terminated strings that live until the call returns; the other arguments are plain integers.
    let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), flags) };
    if r == 0 { Ok(()) } else { Err(last_err()) }
}

/// Renames `path` to `new_name` in the same folder. `Ok(true)` when it was renamed; an error when an item of that name exists (never
/// replaced), when the name is not a plain name, or when the system refuses.
pub fn rename(path: &Path, new_name: &str) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !new_name.is_empty() && new_name != "." && new_name != ".." && !new_name.contains('/') && !new_name.contains('\0'),
        "Invalid file name"
    );
    let to = path.with_file_name(new_name);
    if to == path {
        return Ok(true);
    }
    // a case-only change on a file system that does not tell case apart (vfat, exfat): the "other" name is the same item, so a
    // no-replace rename refuses; go through a temporary name in the same folder instead
    if let (Ok(a), Ok(b)) = (std::fs::symlink_metadata(path), std::fs::symlink_metadata(&to)) {
        if a.dev() == b.dev() && a.ino() == b.ino() {
            let tmp = unique_name(path.parent().unwrap_or(Path::new(".")), &format!(".leaf-rename-{}", std::process::id()), "");
            rename_no_replace(path, &tmp)?;
            if let Err(e) = rename_no_replace(&tmp, &to) {
                // put it back under its old name; if even that fails, say where it is
                if rename_no_replace(&tmp, path).is_err() {
                    anyhow::bail!("{e} (the item is now at {})", tmp.display());
                }
                return Err(e.into());
            }
            return Ok(true);
        }
    }
    rename_no_replace(path, &to)?;
    Ok(true)
}

/// Plain rename that fails (`AlreadyExists`) instead of silently replacing an existing target. Where the file system does not know
/// `RENAME_NOREPLACE`: a file (or link) is linked under the new name (which fails when the name is taken) and then unlinked under the
/// old one; a folder is refused.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    match renameat2(from, to, libc::RENAME_NOREPLACE) {
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)) => {
            // (EINVAL is also the answer for moving a folder into itself: that is a folder, so it is refused below either way)
            let m = std::fs::symlink_metadata(from)?;
            if m.is_dir() {
                return Err(e);
            }
            // link() does not follow a symlink (linkat without AT_SYMLINK_FOLLOW): the link itself gets the second name
            std::fs::hard_link(from, to)?;
            if let Err(e) = std::fs::remove_file(from) {
                // both names now point at the same data: take the new one away again, nothing is lost either way
                let _ = std::fs::remove_file(to);
                return Err(e);
            }
            Ok(())
        }
        r => r,
    }
}

/// `name`, `name (2)`, … (the caller passes the name in the UI language): the created path.
pub fn new_folder(dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let p = unique_name(dir, name, "");
    std::fs::create_dir(&p)?;
    Ok(p)
}

/// The created path of a new empty text file (`create_new`: an item that appeared meanwhile is an error, never truncated).
pub fn new_text_file(dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let p = unique_name(dir, name, ".txt");
    std::fs::OpenOptions::new().write(true).create_new(true).open(&p)?;
    Ok(p)
}

/// A name under `dir` that nothing holds yet (not even a dangling link): `stem.ext`, `stem (2).ext`, …
pub fn unique_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let free = |p: &Path| std::fs::symlink_metadata(p).is_err();
    let first = dir.join(format!("{stem}{ext}"));
    if free(&first) {
        return first;
    }
    (2..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| free(p)).unwrap()
}

/// Whether `a` and `b` are on the same device (file system), by `st_dev`. `a` is taken as it lies (a link is moved as a link), `b`
/// is followed (it is the destination folder). `false` when either cannot be read.
pub fn same_device(a: &Path, b: &Path) -> bool {
    match (std::fs::symlink_metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

/// Copy or move with in-app progress: always our own engine ([`crate::fastcopy::transfer`]); there is no shell engine here.
pub fn transfer_tracked(kind: Transfer, sources: &[PathBuf], dest_dir: &Path, tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    crate::fastcopy::transfer(kind, sources, dest_dir, tracker)
}

/// Delete with in-app progress: to the trash ([`crate::recycle::trash_tracked`]; a mount without a usable trash answers
/// [`crate::recycle::NoTrash`] and nothing is changed), or permanently ([`crate::fastdelete::delete_permanent`]).
pub fn delete_tracked(paths: &[PathBuf], permanent: bool, tracker: Arc<Tracker>) -> anyhow::Result<bool> {
    if permanent {
        crate::fastdelete::delete_permanent(paths, tracker, crate::fastdelete::threads_for(paths))
    } else {
        crate::recycle::trash_tracked(paths, tracker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_never_replaces() {
        let root = std::env::temp_dir().join(format!("leaf-unix-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir")).unwrap();
        let (a, b) = (root.join("a.txt"), root.join("b.txt"));
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        // onto an existing file: refused, both untouched
        assert!(rename(&a, "b.txt").is_err());
        assert_eq!(rename_no_replace(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        // onto an existing folder, and a folder onto a file: refused
        assert!(rename(&a, "dir").is_err());
        assert!(rename(&root.join("dir"), "b.txt").is_err());
        // a case-only change onto another item that exists (ext4/tmpfs tell case apart): refused
        let big = root.join("A.txt");
        std::fs::write(&big, b"AAA").unwrap();
        assert!(rename(&a, "A.txt").is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"aaa");
        assert_eq!(std::fs::read(&b).unwrap(), b"bbb");
        assert_eq!(std::fs::read(&big).unwrap(), b"AAA");
        assert!(root.join("dir").is_dir());
        // names that are not a plain name
        for bad in ["", ".", "..", "x/y"] {
            assert!(rename(&a, bad).is_err(), "{bad:?}");
        }
        // a free name works
        assert!(rename(&a, "c.txt").unwrap());
        assert_eq!(std::fs::read(root.join("c.txt")).unwrap(), b"aaa");
        assert!(std::fs::symlink_metadata(&a).is_err());
        // new items never take a name that is held
        assert_eq!(new_text_file(&root, "b").unwrap(), root.join("b (2).txt"));
        assert_eq!(std::fs::read(&b).unwrap(), b"bbb");
        assert_eq!(new_folder(&root, "dir").unwrap(), root.join("dir (2)"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
