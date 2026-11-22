//! Windows: the pipes, who may connect, and opening a path so that a link or junction swapped in cannot redirect the operation.
//!
//! Started by leaf with `ShellExecuteEx` verb `runas` (the normal UAC prompt), with the command line of [`crate::Args`]. Then:
//! 1. leaf's process is opened by its id and must be the `leaf.exe` beside this program (the same file, compared by volume and file
//!    id), running as the user whose SID was passed. The handle is kept: the id cannot be reused while it is held, and the helper
//!    ends when that process ends.
//! 2. Two pipes are made, `\\.\pipe\<channel>-up` (leaf to helper) and `-down` (helper to leaf): FIRST instance only (if any
//!    program made a pipe of that name before, the helper ends), one instance each, remote clients rejected, and a security
//!    descriptor that lets ONLY the user's SID open them (at medium integrity, which leaf runs at). Two pipes because a pipe
//!    opened for plain blocking I/O does one read or write at a time: a cancel could not be read while progress is written.
//! 3. Each pipe's client must be leaf's process (by `GetNamedPipeClientProcessId`, checked against step 1). The token on the
//!    command line is then checked as the first message; it alone is not enough, as command lines can be read by other programs.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leaf_common::admin::{Item, MAX_FILE, Refusal};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, GETFINALPATHNAMEBYHANDLE_FLAGS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileAttributeTagInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, VOLUME_NAME_DOS,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject};
use windows::core::{PCWSTR, PWSTR};

use crate::Fail;

/// Windows has a Recycle Bin the helper can put items in (the shell's, for the same user).
pub const HAS_RECYCLE: bool = true;

/// GENERIC_READ / GENERIC_WRITE / SYNCHRONIZE.
const GENERIC_READ: u32 = 0x8000_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const READ_CONTROL: u32 = 0x0002_0000;
/// A reparse tag with this bit stands for another place (a symbolic link, a junction); other tags (cloud files, dedup) do not.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// A handle closed when dropped.
pub struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// (a handle is an index into the process's table; using it from another thread is fine)
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// `\\?\C:\…` for a path `check_path` accepted (a drive path): no length limit, and Windows takes it literally.
fn long(p: &Path) -> Vec<u16> {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = if s.len() > 3 { s.trim_end_matches('\\').to_string() } else { s };
    wide(OsStr::new(&format!(r"\\?\{s}")))
}

/// Opens `p` WITHOUT following a link or junction at its end (`FILE_FLAG_OPEN_REPARSE_POINT`), folders too
/// (`FILE_FLAG_BACKUP_SEMANTICS`, which grants nothing extra here: the backup privilege is not enabled).
fn open_nofollow(p: &Path, access: u32, share: FILE_SHARE_MODE) -> std::io::Result<Owned> {
    let w = long(p);
    let h = unsafe { CreateFileW(PCWSTR(w.as_ptr()), access, share, None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, None) }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    Ok(Owned(h))
}

/// What the handle says about the item.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    attrs: u32,
    tag: u32,
    links: u32,
    size: u64,
    volume: u32,
    index: u64,
}

impl Facts {
    fn dir(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0
    }
    /// A symbolic link or a junction.
    fn surrogate(&self) -> bool {
        self.attrs & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 && self.tag & NAME_SURROGATE != 0
    }
}

fn facts(h: &Owned) -> std::io::Result<Facts> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(h.0, &mut info) }.map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        unsafe { GetFileInformationByHandleEx(h.0, FileAttributeTagInfo, (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(), size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    }
    Ok(Facts {
        attrs: info.dwFileAttributes,
        tag: tag.ReparseTag,
        links: info.nNumberOfLinks,
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// Where the open handle really is (`\\?\C:\…`, long names, the case on the disk).
fn final_path(h: &Owned) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { GetFinalPathNameByHandleW(h.0, &mut buf, GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0)) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return String::from_utf16(&buf[..n]).ok();
        }
        if n > 40_000 {
            return None;
        }
        buf.resize(n + 1, 0);
    }
}

/// The handle is where the path's text says: no link, junction, mount point or short 8.3 name on the way changed the place.
/// (Compared without regard to case, as NTFS names are; a mismatch refuses, never the other way round.)
fn same_place(h: &Owned, p: &Path) -> bool {
    let Some(f) = final_path(h) else { return false };
    let Some(f) = f.strip_prefix(r"\\?\") else { return false };
    let want = p.to_string_lossy().replace('/', "\\");
    f.trim_end_matches('\\').to_uppercase() == want.trim_end_matches('\\').to_uppercase()
}

/// Folders held open for an operation: while held, none of them can be renamed, deleted or replaced by a junction (they were
/// opened without `FILE_SHARE_DELETE`), so a path through them keeps leading where it was checked to lead.
pub struct Held(#[allow(dead_code)] Vec<Owned>);

/// Opens and holds one folder on the way: a real folder (no link or junction), where its name says.
fn hold_one(p: &Path) -> Result<Owned, Fail> {
    let h = open_nofollow(p, FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE, FILE_SHARE_READ | FILE_SHARE_WRITE)?;
    let f = facts(&h)?;
    if f.surrogate() || !same_place(&h, p) {
        return Err(Refusal::Link.into());
    }
    if !f.dir() {
        return Err(Refusal::WrongKind.into());
    }
    Ok(h)
}

/// Holds every folder from the drive's root down to `p`'s parent (not `p` itself, which the operation may rename or delete).
pub fn hold_parents(p: &Path) -> Result<Held, Fail> {
    let mut chain: Vec<&Path> = p.ancestors().skip(1).collect();
    chain.reverse();
    let mut held = Vec::with_capacity(chain.len());
    for a in chain {
        held.push(hold_one(a)?);
    }
    Ok(Held(held))
}

/// [`hold_parents`] and `p` itself, which must be a real folder (a listing, a copy's destination, a new folder's parent).
pub fn hold_folder(p: &Path) -> Result<Held, Fail> {
    let mut h = hold_parents(p)?;
    h.0.push(hold_one(p)?);
    Ok(h)
}

/// What [`check_item`] found.
pub struct ItemFacts {
    pub dir: bool,
    /// The item is itself a link or junction (its own path, not followed).
    pub link: bool,
}

/// Opens the named item itself without following it and checks it is where its name says. Call with its parents held.
pub fn check_item(p: &Path) -> Result<ItemFacts, Fail> {
    let h = open_nofollow(p, FILE_READ_ATTRIBUTES.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)?;
    let f = facts(&h)?;
    if !same_place(&h, p) {
        return Err(Refusal::Link.into());
    }
    Ok(ItemFacts { dir: f.dir(), link: f.surrogate() })
}

pub fn list(dir: &Path) -> Result<Vec<Item>, Fail> {
    let mut items = Vec::new();
    let mut too_many = false;
    let r = leaf_sys::list_dir(dir, 4096, &|| false, &mut |batch| {
        if items.len() + batch.len() > 500_000 {
            too_many = true;
            return;
        }
        items.extend(batch.iter().map(Item::from));
    });
    match r {
        _ if too_many => Err(Fail::Failed { denied: false, detail: "too many items".into() }),
        Ok(_) => Ok(items),
        Err(e) => Err(Fail::Failed { denied: matches!(e, leaf_sys::ListError::AccessDenied), detail: e.to_string() }),
    }
}

pub fn stat(path: &Path) -> Result<Option<Item>, Fail> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else { return Err(Refusal::Path(leaf_common::admin::PathRefusal::Root).into()) };
    // (FindFirstFile does not follow a link at the end of the path: a link is reported as the link)
    match leaf_sys::stat_entry(dir, name) {
        Ok(e) => Ok(e.as_ref().map(Item::from)),
        Err(e) => Err(Fail::Failed { denied: matches!(e, leaf_sys::ListError::AccessDenied), detail: e.to_string() }),
    }
}

/// Reads a whole file through ONE handle that was opened without following a link and checked to be where its name says: the
/// check and the read are the same open file, nothing to swap in between.
pub fn read_file(p: &Path) -> Result<Vec<u8>, Fail> {
    let _held = hold_parents(p)?;
    read_checked(p, |_, _| Ok(()))
}

fn read_checked(p: &Path, extra: impl FnOnce(&Owned, &Facts) -> Result<(), Fail>) -> Result<Vec<u8>, Fail> {
    let h = open_nofollow(p, GENERIC_READ | SYNCHRONIZE | READ_CONTROL, FILE_SHARE_READ)?;
    let f = facts(&h)?;
    if f.surrogate() || !same_place(&h, p) {
        return Err(Refusal::Link.into());
    }
    if f.dir() {
        return Err(Refusal::WrongKind.into());
    }
    if f.size > MAX_FILE {
        return Err(Refusal::TooBig.into());
    }
    extra(&h, &f)?;
    let raw = h.0;
    std::mem::forget(h);
    // (the File owns and closes the handle from here)
    let mut file = unsafe { std::fs::File::from_raw_handle(raw.0 as _) };
    let mut out = Vec::with_capacity(f.size as usize);
    Read::by_ref(&mut file).take(MAX_FILE + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_FILE {
        return Err(Refusal::TooBig.into());
    }
    Ok(out)
}

/// The user's SID the helper was started for (set once in [`main`]); a part file must be owned by it.
static USER_SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The editor's save: the bytes of `part` (a file leaf wrote, ending in `.leaf-part`, owned by the user leaf runs as, so the
/// helper never copies out a file the user could not read) in place of the existing plain file `target`.
///
/// The target's folder is held; the new bytes go to a new temp file in it (`CREATE_NEW`: never an existing name, never through a
/// link), which then REPLACES the target's name (`MoveFileEx`: a rename moves names, it never writes through a link, and a file
/// with other hard links is refused before). The saved file gets the folder's inherited permissions, not the old file's own (in
/// a protected folder those are the folder's anyway); its attributes are the new file's.
pub fn write_from_part(part: &Path, target: &Path) -> Result<(), Fail> {
    if part.extension().and_then(|e| e.to_str()) != Some("leaf-part") {
        return Err(Refusal::WrongKind.into());
    }
    let bytes = {
        let _held = hold_parents(part)?;
        // (the owner is read from the same handle the bytes are read from)
        read_checked(part, |h, f| match (f.links, owned_by_user(h)) {
            (1, true) => Ok(()),
            (1, false) => Err(Refusal::WrongKind.into()),
            _ => Err(Refusal::Link.into()),
        })?
    };
    let _held = hold_parents(target)?;
    {
        let h = open_nofollow(target, FILE_READ_ATTRIBUTES.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)?;
        let f = facts(&h)?;
        if f.surrogate() || !same_place(&h, target) || f.links != 1 {
            return Err(Refusal::Link.into());
        }
        if f.dir() {
            return Err(Refusal::WrongKind.into());
        }
    }
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else { return Err(Refusal::WrongKind.into()) };
    let name = name.to_string_lossy();
    let mut n = 0;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(".{name}.leaf-helper-{}-{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 50 => n += 1,
            Err(e) => return Err(e.into()),
        }
    };
    let r = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        // CHECK-THEN-ACT: the target's NAME between the check above and this rename. Whatever is at the name then is replaced (a link
        // swapped in would be replaced, not written through), and the folder is held.
        let (a, b) = (long(&tmp), long(target));
        unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(Into::into)
}

/// The open file's owner is the user the helper serves.
fn owned_by_user(h: &Owned) -> bool {
    use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSID};
    let Some(want) = USER_SID.get() else { return false };
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        if GetSecurityInfo(h.0, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut sd)).0 != 0 {
            return false;
        }
        let mut text = PWSTR::null();
        let ok = ConvertSidToStringSidW(owner, &mut text).is_ok() && text.to_string().is_ok_and(|s| s.eq_ignore_ascii_case(want));
        if !text.is_null() {
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        ok
    }
}

/// A permanent delete with leaf's parallel delete and no hand-over to the system's engine (which could show its own dialogs).
pub fn delete_for_good(paths: &[PathBuf], t: Arc<leaf_sys::fileops::Tracker>) -> anyhow::Result<bool> {
    leaf_sys::fastdelete::delete_without_shell(paths, t, leaf_sys::fastdelete::threads_for(paths))
}

/// "Access denied" in an engine's error.
pub fn is_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
            || c.downcast_ref::<windows::core::Error>().is_some_and(|w| w.code().0 as u32 == 0x8007_0005)
    })
}

/// Every operation's thread: the shell's engine (the Recycle Bin, leftovers of a copy) wants COM.
pub fn thread_start() {
    leaf_sys::shell::ensure_com();
}

// ---------------------------------------------------------------------------------------------------------------- the client

/// Why a pipe's client is not leaf.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientRefusal {
    /// Another process than the leaf that started the helper.
    WrongProcess,
    /// Not the `leaf.exe` beside the helper.
    NotLeaf,
    /// Another user.
    WrongUser,
    /// Could not be looked at.
    Unknown,
}

/// The file identity (volume, file id) of a program file.
fn file_id(p: &Path) -> Option<(u32, u64)> {
    let h = open_nofollow(p, FILE_READ_ATTRIBUTES.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).ok()?;
    let f = facts(&h).ok()?;
    if f.surrogate() || f.dir() {
        return None;
    }
    Some((f.volume, f.index))
}

/// The program file a process runs.
fn image_of(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    let mut n = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n) }.ok()?;
    Some(PathBuf::from(String::from_utf16(&buf[..n as usize]).ok()?))
}

/// `leaf.exe` beside the helper's own program.
pub fn leaf_beside_helper() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    Some(me.parent()?.join(leaf_common::exe_name(leaf_common::APP_EXE_STEM)))
}

/// Checks that the process `pid` is the expected leaf: the same process id leaf gave, running the program file `leaf_exe` (the
/// same file by volume and file id, not only by name), as the user `sid`.
pub fn check_client(pid: u32, leaf_pid: u32, leaf_exe: &Path, sid: &str) -> Result<(), ClientRefusal> {
    if pid != leaf_pid {
        return Err(ClientRefusal::WrongProcess);
    }
    let process = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(|_| ClientRefusal::Unknown)?);
    let image = image_of(process.0).ok_or(ClientRefusal::Unknown)?;
    match (file_id(&image), file_id(leaf_exe)) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Err(ClientRefusal::NotLeaf),
    }
    match leaf_sys::kit::process_sid(pid) {
        Some(s) if s.eq_ignore_ascii_case(sid) => Ok(()),
        Some(_) => Err(ClientRefusal::WrongUser),
        None => Err(ClientRefusal::Unknown),
    }
}

/// One pipe end, made first-instance, one instance, local only, for the user `sid` alone.
fn make_pipe(name: &str, sid: &str, inbound: bool) -> Option<Owned> {
    // only the user (at medium integrity: leaf is not elevated); `sid` passed `user_ok`, so it is digits and dashes only
    let sddl = wide(OsStr::new(&format!("D:P(A;;GA;;;{sid})S:(ML;;NW;;;ME)")));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut sd, None) }.ok()?;
    let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
    let w = wide(OsStr::new(&format!(r"\\.\pipe\{name}")));
    let mode = if inbound { PIPE_ACCESS_INBOUND } else { PIPE_ACCESS_OUTBOUND } | FILE_FLAG_FIRST_PIPE_INSTANCE;
    let h = unsafe { CreateNamedPipeW(PCWSTR(w.as_ptr()), mode, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, 64 * 1024, 64 * 1024, 0, Some(&sa)) };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    if h.is_invalid() { None } else { Some(Owned(h)) }
}

/// Waits for the pipe's client and checks it is leaf.
fn accept(pipe: &Owned, leaf_pid: u32, leaf_exe: &Path, sid: &str) -> bool {
    match unsafe { ConnectNamedPipe(pipe.0, None) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
        Err(_) => return false,
    }
    let mut pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(pipe.0, &mut pid) }.is_err() {
        return false;
    }
    check_client(pid, leaf_pid, leaf_exe, sid).is_ok()
}

/// The helper's life on Windows. Returns the exit code.
pub fn main(args: Vec<String>) -> i32 {
    let Some(a) = crate::parse_args(&args) else { return 2 };
    let Some(leaf_exe) = leaf_beside_helper() else { return 3 };
    let _ = USER_SID.set(a.user.clone());
    // leaf's process, held for the helper's life (its id cannot be reused meanwhile) and checked now
    let Ok(leaf) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, a.leaf_pid) }) else { return 4 };
    let leaf = Owned(leaf);
    if check_client(a.leaf_pid, a.leaf_pid, &leaf_exe, &a.user).is_err() {
        return 5;
    }
    let (Some(up), Some(down)) = (make_pipe(&format!("{}-up", a.channel), &a.user, true), make_pipe(&format!("{}-down", a.channel), &a.user, false)) else {
        return 6;
    };
    let activity = Arc::new(crate::serve::Activity::default());
    // the watchdog: leaf's process ended, idle too long, or leaf never came
    {
        let activity = activity.clone();
        let leaf = Arc::new(leaf);
        std::thread::spawn(move || {
            loop {
                if unsafe { WaitForSingleObject(leaf.0, 1000) } == WAIT_OBJECT_0 || activity.expired() {
                    std::process::exit(0);
                }
            }
        });
    }
    if !accept(&up, a.leaf_pid, &leaf_exe, &a.user) || !accept(&down, a.leaf_pid, &leaf_exe, &a.user) {
        return 7;
    }
    let reader: Box<dyn Read + Send> = Box::new(unsafe { std::fs::File::from_raw_handle(take(up).0 as _) });
    let writer: Box<dyn Write + Send> = Box::new(unsafe { std::fs::File::from_raw_handle(take(down).0 as _) });
    crate::serve::serve(reader, writer, &a.token, activity);
    0
}

/// The raw handle, no longer closed by `Owned`.
fn take(o: Owned) -> HANDLE {
    let h = o.0;
    std::mem::forget(o);
    h
}

/// (tests) A junction at `link` to the folder `target`: what a program of the user could swap in. `mklink /J` needs no privilege.
#[cfg(test)]
pub fn make_dir_link(link: &Path, target: &Path) -> std::io::Result<()> {
    let ok = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output()?.status.success();
    if ok { Ok(()) } else { Err(std::io::Error::other("mklink /J failed")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_client_that_is_not_leaf_is_refused() {
        let me = std::process::id();
        let sid = leaf_sys::kit::current_sid().expect("own SID");
        let own_exe = std::env::current_exe().unwrap();
        // this test program is a real process of the user, but not leaf.exe: refused
        let fake_leaf = std::env::temp_dir().join(format!("leaf-helper-test-client-{me}"));
        std::fs::create_dir_all(&fake_leaf).unwrap();
        let fake_leaf = fake_leaf.join("leaf.exe");
        std::fs::write(&fake_leaf, b"MZ").unwrap();
        assert_eq!(check_client(me, me, &fake_leaf, &sid), Err(ClientRefusal::NotLeaf));
        // the process leaf did not name: refused before anything is looked at
        assert_eq!(check_client(me, me + 4, &own_exe, &sid), Err(ClientRefusal::WrongProcess));
        // another user's SID: refused
        assert_eq!(check_client(me, me, &own_exe, "S-1-5-21-1-2-3-1001"), Err(ClientRefusal::WrongUser));
        // the control: with this very program named as "leaf", the same process passes
        assert_eq!(check_client(me, me, &own_exe, &sid), Ok(()));
        let _ = std::fs::remove_dir_all(fake_leaf.parent().unwrap());
    }
}
