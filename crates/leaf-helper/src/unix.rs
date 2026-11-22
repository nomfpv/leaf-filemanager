//! Linux: the socket, who may connect, and opening a path so that a symbolic link swapped in cannot redirect the operation.
//!
//! Started by leaf through `pkexec` (polkit asks for the administrator's password), with the command line of [`crate::Args`]
//! (`--user` is leaf's uid). Then:
//! 1. leaf's process is looked at by its id (`/proc/<pid>/exe` must be the same file, by device and inode, as the `leaf` beside
//!    this program, and run as that uid), and a pidfd is opened on it: the helper ends when that process ends.
//! 2. A Unix socket is made at `/run/user/<uid>/<channel>.sock` (the user's own runtime folder, which only the user can enter),
//!    created with mode 0600 and then given to the user, and ONE connection is taken.
//! 3. The connection's peer credentials (`SO_PEERCRED`) must be leaf's process id and uid. The token is then checked as the first
//!    message.
//!
//! Paths are walked one part at a time with `openat(O_PATH | O_NOFOLLOW)` from `/`, so a symbolic link anywhere on the way is
//! refused. Linux has no way to keep a folder from being renamed while it is open, so what [`Held`] holds here is the folders'
//! file descriptors, not their names: the operations that can work on the descriptor (listing, stat, read, the editor's write)
//! do, the ones that go through leaf's engines take the path again (CHECK-THEN-ACT: see `exec.rs`).
//!
//! Compile-checked only from the Windows PC this was written on; not run.

use std::ffi::{CString, OsStr};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use leaf_common::admin::{Item, MAX_FILE, Refusal};

use crate::Fail;

/// The Trash of a process running as root is root's, not the user's: the helper does not put items there (leaf offers a
/// permanent delete instead).
pub const HAS_RECYCLE: bool = false;

fn last_err() -> std::io::Error {
    std::io::Error::last_os_error()
}

fn cstr(s: &OsStr) -> Result<CString, Fail> {
    CString::new(s.as_bytes()).map_err(|_| Refusal::Path(leaf_common::admin::PathRefusal::BadPart).into())
}

fn open_at(dir: RawFd, name: &OsStr, flags: libc::c_int) -> Result<OwnedFd, Fail> {
    let c = cstr(name)?;
    let fd = unsafe { libc::openat(dir, c.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 {
        let e = last_err();
        // a link where a folder or the item was
        if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)) {
            return Err(Refusal::Link.into());
        }
        return Err(e.into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fstat(fd: RawFd) -> Result<libc::stat, Fail> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(last_err().into());
    }
    Ok(st)
}

fn lstat_at(dir: RawFd, name: &OsStr) -> Result<libc::stat, Fail> {
    let c = cstr(name)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(last_err().into());
    }
    Ok(st)
}

fn is(st: &libc::stat, kind: libc::mode_t) -> bool {
    st.st_mode & libc::S_IFMT == kind
}

/// The folders from `/` to a path's parent, each opened through the one before it without following a link.
pub struct Held(Vec<OwnedFd>);

impl Held {
    /// The innermost folder held.
    fn last(&self) -> RawFd {
        self.0.last().map_or(libc::AT_FDCWD, |f| f.as_raw_fd())
    }
}

fn walk(p: &Path, include_self: bool) -> Result<Held, Fail> {
    let parts: Vec<&OsStr> = p.components().filter_map(|c| if let Component::Normal(n) = c { Some(n) } else { None }).collect();
    let take = if include_self { parts.len() } else { parts.len().saturating_sub(1) };
    let root = open_at(libc::AT_FDCWD, OsStr::new("/"), libc::O_PATH | libc::O_DIRECTORY)?;
    let mut held = vec![root];
    for part in &parts[..take] {
        let fd = open_at(held.last().map(|f| f.as_raw_fd()).unwrap_or(libc::AT_FDCWD), part, libc::O_PATH | libc::O_DIRECTORY)?;
        if !is(&fstat(fd.as_raw_fd())?, libc::S_IFDIR) {
            return Err(Refusal::Link.into());
        }
        held.push(fd);
    }
    Ok(Held(held))
}

pub fn hold_parents(p: &Path) -> Result<Held, Fail> {
    walk(p, false)
}

pub fn hold_folder(p: &Path) -> Result<Held, Fail> {
    walk(p, true)
}

pub struct ItemFacts {
    pub dir: bool,
    pub link: bool,
}

/// The named item, looked at through its held parent without following it.
pub fn check_item(p: &Path) -> Result<ItemFacts, Fail> {
    let held = hold_parents(p)?;
    let name = p.file_name().ok_or(Refusal::WrongKind)?;
    let st = lstat_at(held.last(), name)?;
    Ok(ItemFacts { dir: is(&st, libc::S_IFDIR), link: is(&st, libc::S_IFLNK) })
}

/// The path that reaches an open descriptor itself (the kernel's link for it), so a listing goes into exactly that folder.
fn fd_path(fd: RawFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

pub fn list(dir: &Path) -> Result<Vec<Item>, Fail> {
    let held = hold_folder(dir)?;
    let at = fd_path(held.last());
    let mut items = Vec::new();
    let r = leaf_sys::list_dir(&at, 4096, &|| false, &mut |batch| items.extend(batch.iter().map(Item::from)));
    match r {
        Ok(_) => Ok(items),
        Err(e) => Err(Fail::Failed { denied: matches!(e, leaf_sys::ListError::AccessDenied), detail: e.to_string() }),
    }
}

pub fn stat(path: &Path) -> Result<Option<Item>, Fail> {
    let held = hold_parents(path)?;
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Err(Refusal::WrongKind.into()) };
    match leaf_sys::stat_entry(&fd_path(held.last()), name) {
        Ok(e) => Ok(e.as_ref().map(Item::from)),
        Err(e) => Err(Fail::Failed { denied: matches!(e, leaf_sys::ListError::AccessDenied), detail: e.to_string() }),
    }
}

fn read_at(dir: RawFd, name: &OsStr, extra: impl FnOnce(&libc::stat) -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
    let fd = open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let st = fstat(fd.as_raw_fd())?;
    if is(&st, libc::S_IFLNK) {
        return Err(Refusal::Link.into());
    }
    if !is(&st, libc::S_IFREG) {
        return Err(Refusal::WrongKind.into());
    }
    if st.st_size as u64 > MAX_FILE {
        return Err(Refusal::TooBig.into());
    }
    extra(&st)?;
    let mut f = std::fs::File::from(fd);
    let mut out = Vec::with_capacity(st.st_size as usize);
    Read::by_ref(&mut f).take(MAX_FILE + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_FILE {
        return Err(Refusal::TooBig.into());
    }
    Ok(out)
}

/// Reads through one descriptor opened (through the held parent) without following a link: check and read are the same file.
pub fn read_file(p: &Path) -> Result<Vec<u8>, Fail> {
    let held = hold_parents(p)?;
    read_at(held.last(), p.file_name().ok_or(Refusal::WrongKind)?, |_| Ok(()))
}

static USER_UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// The editor's save (see the Windows twin): the part file must be a plain file of the user with one link; the target a plain
/// file with one link. The new bytes go to a new file in the target's folder (`O_EXCL`, through the folder's descriptor), get the
/// old file's owner and mode, and are renamed over the target's name (`renameat`, folder descriptor to itself): no step follows
/// a link, and the folder is the one that was checked even if it is renamed meanwhile.
pub fn write_from_part(part: &Path, target: &Path) -> Result<(), Fail> {
    if part.extension().and_then(|e| e.to_str()) != Some("leaf-part") {
        return Err(Refusal::WrongKind.into());
    }
    let uid = *USER_UID.get().ok_or(Refusal::WrongKind)?;
    let bytes = {
        let held = hold_parents(part)?;
        read_at(held.last(), part.file_name().ok_or(Refusal::WrongKind)?, |st| {
            if st.st_nlink != 1 {
                Err(Refusal::Link.into())
            } else if st.st_uid != uid {
                Err(Refusal::WrongKind.into())
            } else {
                Ok(())
            }
        })?
    };
    let held = hold_parents(target)?;
    let dir = held.last();
    let name = target.file_name().ok_or(Refusal::WrongKind)?;
    let old = lstat_at(dir, name)?;
    if is(&old, libc::S_IFLNK) || old.st_nlink != 1 {
        return Err(Refusal::Link.into());
    }
    if !is(&old, libc::S_IFREG) {
        return Err(Refusal::WrongKind.into());
    }
    let mut n = 0;
    let (tmp, fd) = loop {
        let mut t = std::ffi::OsString::from(".");
        t.push(name);
        t.push(format!(".leaf-helper-{}-{n}.tmp", std::process::id()));
        let c = cstr(&t)?;
        let fd = unsafe { libc::openat(dir, c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
        if fd >= 0 {
            break (c, unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let e = last_err();
        if e.kind() == std::io::ErrorKind::AlreadyExists && n < 50 {
            n += 1;
            continue;
        }
        return Err(e.into());
    };
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::from(fd);
        f.write_all(&bytes)?;
        let raw = f.as_raw_fd();
        unsafe {
            if libc::fchown(raw, old.st_uid, old.st_gid) != 0 || libc::fchmod(raw, old.st_mode & 0o7777) != 0 {
                return Err(last_err());
            }
        }
        f.sync_all()?;
        drop(f);
        let c = CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
        // CHECK-THEN-ACT: the target's NAME between the lstat above and this rename (replaced, never written through)
        if unsafe { libc::renameat(dir, tmp.as_ptr(), dir, c.as_ptr()) } != 0 {
            return Err(last_err());
        }
        Ok(())
    })();
    if r.is_err() {
        unsafe {
            libc::unlinkat(dir, tmp.as_ptr(), 0);
        }
    }
    r.map_err(Into::into)
}

pub fn delete_for_good(paths: &[PathBuf], t: Arc<leaf_sys::fileops::Tracker>) -> anyhow::Result<bool> {
    leaf_sys::fastdelete::delete_permanent(paths, t, leaf_sys::fastdelete::threads_for(paths))
}

pub fn is_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied))
}

pub fn thread_start() {}

// ---------------------------------------------------------------------------------------------------------------- the client

#[derive(Debug, PartialEq, Eq)]
pub enum ClientRefusal {
    WrongProcess,
    NotLeaf,
    WrongUser,
    Unknown,
}

pub fn leaf_beside_helper() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    Some(me.parent()?.join(leaf_common::exe_name(leaf_common::APP_EXE_STEM)))
}

/// The process `pid` is the leaf that started the helper: that id, running the `leaf` file beside the helper (by device and
/// inode of `/proc/<pid>/exe`), as `uid`.
pub fn check_client(pid: u32, leaf_pid: u32, leaf_exe: &Path, uid: u32) -> Result<(), ClientRefusal> {
    if pid != leaf_pid {
        return Err(ClientRefusal::WrongProcess);
    }
    let running = std::fs::metadata(format!("/proc/{pid}/exe")).map_err(|_| ClientRefusal::Unknown)?;
    let beside = std::fs::metadata(leaf_exe).map_err(|_| ClientRefusal::NotLeaf)?;
    if (running.dev(), running.ino()) != (beside.dev(), beside.ino()) {
        return Err(ClientRefusal::NotLeaf);
    }
    let owner = std::fs::metadata(format!("/proc/{pid}")).map_err(|_| ClientRefusal::Unknown)?.uid();
    if owner != uid {
        return Err(ClientRefusal::WrongUser);
    }
    Ok(())
}

fn peer(fd: RawFd) -> Option<(u32, u32)> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
    (r == 0).then_some((cred.pid as u32, cred.uid))
}

/// The socket's path for a channel (leaf computes the same).
pub fn socket_path(uid: u32, channel: &str) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/{channel}.sock"))
}

pub fn main(args: Vec<String>) -> i32 {
    let Some(a) = crate::parse_args(&args) else { return 2 };
    let Ok(uid) = a.user.parse::<u32>() else { return 2 };
    // pkexec says who asked; it must be the user leaf named
    if let Ok(asked) = std::env::var("PKEXEC_UID") {
        if asked.parse::<u32>().ok() != Some(uid) {
            return 2;
        }
    }
    let _ = USER_UID.set(uid);
    let Some(leaf_exe) = leaf_beside_helper() else { return 3 };
    if check_client(a.leaf_pid, a.leaf_pid, &leaf_exe, uid).is_err() {
        return 5;
    }
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, a.leaf_pid as libc::c_long, 0 as libc::c_long) } as RawFd;
    if pidfd < 0 {
        return 4;
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    // the socket, made 0600 (the umask) in the user's runtime folder and then given to the user (not following a link)
    let path = socket_path(uid, &a.channel);
    let Some(dir) = path.parent() else { return 6 };
    let Ok(dirfd) = walk(&path, false) else { return 6 };
    let Ok(dir_st) = fstat(dirfd.last()) else { return 6 };
    if dir_st.st_uid != uid {
        return 6;
    }
    let old = unsafe { libc::umask(0o177) };
    let listener = std::os::unix::net::UnixListener::bind(dir.join(path.file_name().unwrap_or_default()));
    unsafe { libc::umask(old) };
    let Ok(listener) = listener else { return 6 };
    let name = cstr(path.file_name().unwrap_or_default());
    let given = name.is_ok_and(|c| unsafe { libc::fchownat(dirfd.last(), c.as_ptr(), uid, u32::MAX, libc::AT_SYMLINK_NOFOLLOW) } == 0);
    // CHECK-THEN-ACT: the socket's name between bind and fchownat; the folder is the user's own runtime folder on its own tmpfs, so
    // no file of another file system can be hard-linked in its place, and a link put there is changed, not followed.
    if !given {
        let _ = std::fs::remove_file(&path);
        return 6;
    }
    let activity = Arc::new(crate::serve::Activity::default());
    {
        let activity = activity.clone();
        std::thread::spawn(move || {
            loop {
                let mut p = libc::pollfd { fd: pidfd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                let r = unsafe { libc::poll(&mut p, 1, 1000) };
                if (r > 0 && p.revents != 0) || activity.expired() {
                    std::process::exit(0);
                }
            }
        });
    }
    let conn = listener.accept();
    let _ = std::fs::remove_file(&path);
    let Ok((stream, _)) = conn else { return 7 };
    match peer(stream.as_raw_fd()) {
        Some((pid, puid)) if pid == a.leaf_pid && puid == uid && check_client(pid, a.leaf_pid, &leaf_exe, uid).is_ok() => {}
        _ => return 7,
    }
    let Ok(writer) = stream.try_clone() else { return 7 };
    crate::serve::serve(Box::new(stream), Box::new(writer), &a.token, activity);
    0
}

/// (tests) A symbolic link at `link` to the folder `target`.
#[cfg(test)]
pub fn make_dir_link(link: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
