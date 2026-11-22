//! Linux: items opened RELATIVE to an open folder's descriptor (`openat` with `O_PATH | O_NOFOLLOW`: only to see what the item is),
//! then opened again through `/proc/self/fd` for exactly what is needed (a folder for listing, a regular file for reading), checked
//! to be the same file (device and inode). For the helper's own tree walk (`tree.rs`). Compile-checked only.
//!
//! A symbolic link is never opened through. A folder on another file system or another mount than its parent (a mount point,
//! bind mounts included: `statx` mount id) counts as a link: never entered, by delete, copy or move alike. Devices, sockets and
//! pipes are never opened; a delete removes them as themselves.

use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::unix::Held;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    Look,
    LookDelete,
    List,
    ListDelete,
    Merge,
    Read { move_source: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File { size: u64, filtered: bool },
    Dir,
    Link,
    Cloud,
}

pub struct Node {
    fd: OwnedFd,
    kind: Kind,
    dev: u64,
    ino: u64,
    mnt: Option<u64>,
}

fn err() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn one_part(name: &OsStr) -> std::io::Result<CString> {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > 255 || b == b"." || b == b".." || b.contains(&b'/') {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a single plain name"));
    }
    CString::new(b).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in a name"))
}

fn stat_fd(fd: RawFd) -> std::io::Result<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(err());
    }
    Ok(st)
}

/// The mount id of an open item (`None` where the kernel does not tell it; the device number still separates file systems).
fn mount_id(fd: RawFd) -> Option<u64> {
    let mut sx: libc::statx = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::statx(fd, c"".as_ptr(), libc::AT_EMPTY_PATH, libc::STATX_MNT_ID, &mut sx) };
    (r == 0 && sx.stx_mask & libc::STATX_MNT_ID != 0).then_some(sx.stx_mnt_id)
}

fn node_of(fd: OwnedFd, parent: Option<(u64, Option<u64>)>) -> std::io::Result<Node> {
    let st = stat_fd(fd.as_raw_fd())?;
    let mnt = mount_id(fd.as_raw_fd());
    let (dev, ino) = (st.st_dev as u64, st.st_ino as u64);
    let other_mount = parent.is_some_and(|(pdev, pmnt)| pdev != dev || (pmnt.is_some() && mnt.is_some() && pmnt != mnt));
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR if other_mount => Kind::Link,
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File { size: st.st_size as u64, filtered: false },
        // links, devices, sockets and pipes: never opened, never entered
        _ => Kind::Link,
    };
    Ok(Node { fd, kind, dev, ino, mnt })
}

pub fn held_node(held: &Held, _write: bool) -> std::io::Result<Node> {
    let fd = unsafe { libc::openat(held.last(), c".".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err());
    }
    node_of(unsafe { OwnedFd::from_raw_fd(fd) }, None)
}

/// (device, inode) of every folder `held` holds.
pub fn held_ids(held: &Held) -> Vec<(u64, u64)> {
    held.all().iter().filter_map(|f| stat_fd(f.as_raw_fd()).ok()).map(|s| (s.st_dev as u64, s.st_ino as u64)).collect()
}

/// `name` in `dir`, opened only as a path (nothing is read or entered).
pub fn open_child(dir: &Node, name: &OsStr, want: Want) -> std::io::Result<Node> {
    let c = one_part(name)?;
    let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err());
    }
    let n = node_of(unsafe { OwnedFd::from_raw_fd(fd) }, Some((dir.dev, dir.mnt)))?;
    if want == Want::Merge {
        if n.kind != Kind::Dir {
            return Err(std::io::Error::from_raw_os_error(if n.kind == Kind::Link { libc::ELOOP } else { libc::ENOTDIR }));
        }
        return reopen(dir, name, &n, Want::Merge);
    }
    Ok(n)
}

/// The item `seen` opened again for `want` (through `/proc/self/fd`, exactly that file).
pub fn reopen(_dir: &Node, _name: &OsStr, seen: &Node, want: Want) -> std::io::Result<Node> {
    let flags = match (want, seen.kind) {
        (Want::List | Want::ListDelete | Want::Merge, Kind::Dir) => libc::O_RDONLY | libc::O_DIRECTORY,
        (Want::Read { .. }, Kind::File { .. }) => libc::O_RDONLY | libc::O_NONBLOCK,
        (Want::Look | Want::LookDelete, _) => libc::O_PATH,
        _ => return Err(std::io::Error::from_raw_os_error(libc::EINVAL)),
    };
    let p = CString::new(format!("/proc/self/fd/{}", seen.fd.as_raw_fd())).map_err(std::io::Error::other)?;
    let fd = unsafe { libc::open(p.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let st = stat_fd(fd.as_raw_fd())?;
    if (st.st_dev as u64, st.st_ino as u64) != (seen.dev, seen.ino) {
        return Err(std::io::Error::other("the item changed while it was opened"));
    }
    Ok(Node { fd, kind: seen.kind, dev: seen.dev, ino: seen.ino, mnt: seen.mnt })
}

pub fn kind(n: &Node) -> std::io::Result<Kind> {
    Ok(n.kind)
}

pub fn id(n: &Node) -> std::io::Result<(u64, u64)> {
    Ok((n.dev, n.ino))
}

pub fn meta(n: &Node) -> std::io::Result<std::fs::Metadata> {
    let f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(n.fd.as_raw_fd()) });
    f.metadata()
}

pub fn size_now(n: &Node) -> std::io::Result<u64> {
    Ok(stat_fd(n.fd.as_raw_fd())?.st_size as u64)
}

/// (Linux: extended attributes are not looked at; nothing is reported as lost.)
pub fn extras(_n: &Node) -> std::io::Result<bool> {
    Ok(false)
}

pub fn children(dir: &Node) -> std::io::Result<Vec<OsString>> {
    let dup = unsafe { libc::fcntl(dir.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(err());
    }
    let d = unsafe { libc::fdopendir(dup) };
    if d.is_null() {
        unsafe { libc::close(dup) };
        return Err(err());
    }
    unsafe { libc::rewinddir(d) };
    let mut out = Vec::new();
    let r = loop {
        // (errno is set to 0 first: a null answer with errno set is an error, not the end)
        unsafe { *libc::__errno_location() = 0 };
        let e = unsafe { libc::readdir(d) };
        if e.is_null() {
            let code = unsafe { *libc::__errno_location() };
            break if code == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(code)) };
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*e).d_name.as_ptr()) }.to_bytes().to_vec();
        if name != b"." && name != b".." {
            out.push(OsString::from_vec(name));
        }
    };
    unsafe { libc::closedir(d) };
    r.map(|_| out)
}

/// Removes the entry `name` of the open folder `dir` (`unlinkat`: a link is removed as itself), but only when that name still
/// leads to `n` (device and inode looked at just before; otherwise kept and an error).
/// CHECK-THEN-ACT: Linux removes by name, not by the item's own descriptor; the name could still change between that look and the
/// `unlinkat`, then the other entry of that same folder is removed (only as itself: a link as a link, a folder only when empty).
pub fn remove(dir: &Node, name: &OsStr, n: Node, is_dir: bool) -> std::io::Result<()> {
    let c = one_part(name)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir.fd.as_raw_fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(err());
    }
    if (st.st_dev as u64, st.st_ino as u64) != (n.dev, n.ino) {
        return Err(std::io::Error::other("the item changed before it was removed; it was kept"));
    }
    drop(n);
    if unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c.as_ptr(), if is_dir { libc::AT_REMOVEDIR } else { 0 }) } != 0 {
        return Err(err());
    }
    Ok(())
}

/// A new folder (mode 0700 until its contents are in and the source's mode is put on it).
pub fn make_dir(dir: &Node, name: &OsStr) -> std::io::Result<Node> {
    let c = one_part(name)?;
    if unsafe { libc::mkdirat(dir.fd.as_raw_fd(), c.as_ptr(), 0o700) } != 0 {
        return Err(err());
    }
    open_child(dir, name, Want::Merge)
}

/// A new file (mode 0600 until it is complete).
pub fn make_file(dir: &Node, name: &OsStr) -> std::io::Result<std::fs::File> {
    let c = one_part(name)?;
    let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
    if fd < 0 {
        return Err(err());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub fn discard(dir: &Node, name: &OsStr, f: std::fs::File) {
    drop(f);
    if let Ok(c) = one_part(name) {
        unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c.as_ptr(), 0) };
    }
}

pub fn reader(n: &Node) -> std::mem::ManuallyDrop<std::fs::File> {
    std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(n.fd.as_raw_fd()) })
}

/// The folder's entries on the disk (before a move removes its source).
pub fn sync_dir(dir: &Node) {
    unsafe { libc::fsync(dir.fd.as_raw_fd()) };
}

/// The finished temp file `tmp` takes its final name `name` in `dir` (`renameat2`; `RENAME_NOREPLACE` unless the answer was
/// "Replace").
pub fn finish_file(dir: &Node, _f: &std::fs::File, tmp: &OsStr, name: &OsStr, replace: bool) -> std::io::Result<()> {
    let (a, b) = (one_part(tmp)?, one_part(name)?);
    let fd = dir.fd.as_raw_fd();
    let flags = if replace { 0 } else { libc::RENAME_NOREPLACE };
    if unsafe { libc::renameat2(fd, a.as_ptr(), fd, b.as_ptr(), flags) } != 0 {
        return Err(err());
    }
    Ok(())
}

pub fn rename_in(dir: &Node, name: &OsStr, new_name: &OsStr) -> std::io::Result<()> {
    let (a, b) = (one_part(name)?, one_part(new_name)?);
    let fd = dir.fd.as_raw_fd();
    if unsafe { libc::renameat2(fd, a.as_ptr(), fd, b.as_ptr(), libc::RENAME_NOREPLACE) } != 0 {
        return Err(err());
    }
    Ok(())
}

/// Owner, group, mode and times of the source.
#[derive(Clone, Copy)]
pub struct Stamp {
    uid: u32,
    gid: u32,
    mode: u32,
    times: [libc::timespec; 2],
}

pub fn stamp(n: &Node) -> std::io::Result<Stamp> {
    let st = stat_fd(n.fd.as_raw_fd())?;
    Ok(Stamp {
        uid: st.st_uid,
        gid: st.st_gid,
        mode: st.st_mode as u32,
        times: [libc::timespec { tv_sec: st.st_atime, tv_nsec: st.st_atime_nsec as _ }, libc::timespec { tv_sec: st.st_mtime, tv_nsec: st.st_mtime_nsec as _ }],
    })
}

/// The copy keeps the SOURCE's owner and group (a copy made as root would otherwise belong to root; the destination folder's
/// owner is not taken over, as a copy by the user would not either) and its mode WITHOUT setuid and setgid (a set-id program
/// copied as root must not become one by the copy; the sticky bit stays). Times last.
fn apply(fd: RawFd, s: &Stamp) -> std::io::Result<()> {
    unsafe {
        if libc::fchown(fd, s.uid, s.gid) != 0 || libc::fchmod(fd, (s.mode & 0o1777) as libc::mode_t) != 0 || libc::futimens(fd, s.times.as_ptr()) != 0 {
            return Err(err());
        }
    }
    Ok(())
}

pub fn apply_file(f: &std::fs::File, s: &Stamp) -> std::io::Result<()> {
    apply(f.as_raw_fd(), s)
}

pub fn apply_dir(n: &Node, s: &Stamp) -> std::io::Result<()> {
    apply(n.fd.as_raw_fd(), s)
}
