//! Linux: items opened RELATIVE to an open folder's descriptor (`openat` with `O_NOFOLLOW`, first as `O_PATH` to see what the item
//! is, then a plain open of exactly that file through `/proc/self/fd` for a regular file or a folder), for the helper's own tree
//! walk (`tree.rs`). A symbolic link is never opened through. Compile-checked only.

use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::unix::Held;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Want {
    Read,
    ReadDelete,
    Merge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File(u64),
    Dir,
    Link,
}

pub struct Node {
    fd: OwnedFd,
    kind: Kind,
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

fn kind_of(st: &libc::stat) -> Kind {
    match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File(st.st_size as u64),
        // links, and devices, sockets and pipes: never opened, never entered
        _ => Kind::Link,
    }
}

/// Opens the very file an `O_PATH` descriptor stands for, for reading (a folder: for listing).
fn reopen(path_fd: &OwnedFd, dir: bool) -> std::io::Result<OwnedFd> {
    let p = CString::new(format!("/proc/self/fd/{}", path_fd.as_raw_fd())).map_err(std::io::Error::other)?;
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | if dir { libc::O_DIRECTORY } else { libc::O_NONBLOCK };
    let fd = unsafe { libc::open(p.as_ptr(), flags) };
    if fd < 0 {
        return Err(err());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // (the same file: device and inode as the O_PATH descriptor)
    let (a, b) = (stat_fd(path_fd.as_raw_fd())?, stat_fd(fd.as_raw_fd())?);
    if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino) {
        return Err(std::io::Error::other("the item changed while it was opened"));
    }
    Ok(fd)
}

fn node_at(dir: RawFd, name: &OsStr) -> std::io::Result<Node> {
    let c = one_part(name)?;
    let fd = unsafe { libc::openat(dir, c.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err());
    }
    let path_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let kind = kind_of(&stat_fd(path_fd.as_raw_fd())?);
    let fd = match kind {
        Kind::Dir => reopen(&path_fd, true)?,
        Kind::File(_) => reopen(&path_fd, false)?,
        Kind::Link => path_fd,
    };
    Ok(Node { fd, kind })
}

pub fn held_node(held: &Held, _write: bool) -> std::io::Result<Node> {
    let fd = unsafe { libc::openat(held.last(), c".".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err());
    }
    Ok(Node { fd: unsafe { OwnedFd::from_raw_fd(fd) }, kind: Kind::Dir })
}

pub fn open_child(dir: &Node, name: &OsStr, want: Want) -> std::io::Result<Node> {
    let n = node_at(dir.fd.as_raw_fd(), name)?;
    if want == Want::Merge && n.kind != Kind::Dir {
        return Err(std::io::Error::other("not a folder"));
    }
    Ok(n)
}

pub fn kind(n: &Node) -> std::io::Result<Kind> {
    Ok(n.kind)
}

pub fn meta(n: &Node) -> std::io::Result<std::fs::Metadata> {
    let f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(n.fd.as_raw_fd()) });
    f.metadata()
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
    loop {
        let e = unsafe { libc::readdir(d) };
        if e.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*e).d_name.as_ptr()) }.to_bytes().to_vec();
        if name != b"." && name != b".." {
            out.push(OsString::from_vec(name));
        }
    }
    unsafe { libc::closedir(d) };
    Ok(out)
}

/// Removes the entry `name` of the held folder `dir` (`unlinkat`: a link is removed as itself).
/// CHECK-THEN-ACT: Linux removes by name in the folder's descriptor, not by the item's own descriptor; between the item's open and
/// this the name could be given to another entry of that same folder, which is then removed (only an empty folder, or as a link).
pub fn remove(dir: &Node, name: &OsStr, n: Node, is_dir: bool) -> std::io::Result<()> {
    drop(n);
    let c = one_part(name)?;
    if unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c.as_ptr(), if is_dir { libc::AT_REMOVEDIR } else { 0 }) } != 0 {
        return Err(err());
    }
    Ok(())
}

pub fn make_dir(dir: &Node, name: &OsStr) -> std::io::Result<Node> {
    let c = one_part(name)?;
    if unsafe { libc::mkdirat(dir.fd.as_raw_fd(), c.as_ptr(), 0o755) } != 0 {
        return Err(err());
    }
    open_child(dir, name, Want::Merge)
}

pub fn make_file(dir: &Node, name: &OsStr) -> std::io::Result<std::fs::File> {
    let c = one_part(name)?;
    let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o644) };
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

pub fn into_file(n: Node) -> std::fs::File {
    std::fs::File::from(n.fd)
}

pub fn rename_in(dir: &Node, name: &OsStr, new_name: &OsStr) -> std::io::Result<()> {
    let (a, b) = (one_part(name)?, one_part(new_name)?);
    let fd = dir.fd.as_raw_fd();
    if unsafe { libc::renameat2(fd, a.as_ptr(), fd, b.as_ptr(), libc::RENAME_NOREPLACE) } != 0 {
        return Err(err());
    }
    Ok(())
}

/// The open file behind a node, borrowed for reading (the node keeps owning the descriptor).
pub fn reader(n: &Node) -> std::mem::ManuallyDrop<std::fs::File> {
    std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(n.fd.as_raw_fd()) })
}
