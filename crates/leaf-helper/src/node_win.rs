//! Windows: items opened RELATIVE to an open folder's handle, never by a path again (`NtCreateFile` with `RootDirectory` and
//! `FILE_OPEN_REPARSE_POINT`), for the helper's own tree walk (`tree.rs`). A name is always one part read from the folder's own
//! listing (or checked by `check_name`), so a link or junction swapped in for an item is opened as the link itself, and a folder
//! once opened is the same folder until it is closed, whatever its name is made to point at meanwhile.
//!
//! An item is first opened only to look at it (attributes; the kind comes from that handle), and opened again for what it is
//! needed for (listing a folder, reading a file) by the same name in the same open folder, checked to be the same file (volume and
//! file id): an unreadable file does not stop a walk that only needs to know it is there.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::OnceLock;

use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS, FILE_FULL_DIR_INFO,
    FILE_RENAME_INFO, FILE_STREAM_INFO, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    FileStreamInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

use crate::win::{Held, Owned, facts};

const DELETE: u32 = 0x0001_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_ADD_FILE: u32 = 0x2;
const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
const FILE_TRAVERSE: u32 = 0x20;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const SHARE_READ: u32 = 0x1;
const SHARE_ALL: u32 = 0x7;

const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

/// A reparse tag with this bit stands for another place (a symbolic link, a junction).
const NAME_SURROGATE: u32 = 0x2000_0000;
const ATTR_REPARSE: u32 = 0x400;
const ATTR_DIRECTORY: u32 = 0x10;

#[repr(C)]
struct UniStr {
    len: u16,
    max: u16,
    buf: *const u16,
}

#[repr(C)]
struct ObjAttr {
    length: u32,
    root: *mut c_void,
    name: *const UniStr,
    attributes: u32,
    sd: *const c_void,
    qos: *const c_void,
}

#[repr(C)]
struct Iosb {
    status: usize,
    info: usize,
}

type NtCreateFileFn = unsafe extern "system" fn(*mut *mut c_void, u32, *const ObjAttr, *mut Iosb, *const i64, u32, u32, u32, u32, *const c_void, u32) -> i32;
type ToDosFn = unsafe extern "system" fn(i32) -> u32;

/// `NtCreateFile` and `RtlNtStatusToDosError` from ntdll (always loaded in every process).
fn ntdll() -> Option<(NtCreateFileFn, ToDosFn)> {
    static F: OnceLock<Option<(usize, usize)>> = OnceLock::new();
    let (a, b) = (*F.get_or_init(|| unsafe {
        let m = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        let a = GetProcAddress(m, s!("NtCreateFile"))?;
        let b = GetProcAddress(m, s!("RtlNtStatusToDosError"))?;
        Some((a as usize, b as usize))
    }))?;
    Some(unsafe { (std::mem::transmute::<usize, NtCreateFileFn>(a), std::mem::transmute::<usize, ToDosFn>(b)) })
}

/// A name the helper opens relative to a folder: one plain part.
fn one_part(name: &OsStr) -> std::io::Result<Vec<u16>> {
    let w: Vec<u16> = name.encode_wide().collect();
    let bad = w.is_empty() || w.len() > 255 || w == [b'.' as u16] || w == [b'.' as u16, b'.' as u16] || w.iter().any(|&c| c == b'\\' as u16 || c == b'/' as u16 || c == b':' as u16 || c == 0);
    if bad { Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a single plain name")) } else { Ok(w) }
}

fn os_err(e: windows::core::Error) -> std::io::Error {
    std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

/// The open itself. `options` decide whether a reparse point at the end is opened as itself (always, except a file read through
/// a filter that needs it: see [`Kind::File::filtered`]). An empty name opens `dir` itself again, through its handle.
fn nt_open(dir: &Owned, w: &[u16], access: u32, share: u32, disposition: u32, options: u32) -> std::io::Result<Owned> {
    let (create, to_dos) = ntdll().ok_or_else(|| std::io::Error::other("ntdll entry points missing"))?;
    let us = UniStr { len: (w.len() * 2) as u16, max: (w.len() * 2) as u16, buf: w.as_ptr() };
    let oa = ObjAttr { length: size_of::<ObjAttr>() as u32, root: dir.0.0, name: &us, attributes: OBJ_CASE_INSENSITIVE, sd: std::ptr::null(), qos: std::ptr::null() };
    let mut iosb = Iosb { status: 0, info: 0 };
    let mut h: *mut c_void = std::ptr::null_mut();
    let st = unsafe { create(&mut h, access | SYNCHRONIZE, &oa, &mut iosb, std::ptr::null(), 0, share, disposition, options | FILE_SYNCHRONOUS_IO_NONALERT, std::ptr::null(), 0) };
    if st < 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe { to_dos(st) } as i32));
    }
    Ok(Owned(HANDLE(h)))
}

/// What an item is opened for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    /// Only to see what it is (attributes, times): any item the walk meets, readable or not.
    Look,
    /// To see what it is and to remove it.
    LookDelete,
    /// A folder whose names are read.
    List,
    /// A folder whose names are read and which is removed afterwards.
    ListDelete,
    /// A folder something is copied into.
    Merge,
    /// A file whose bytes are read. `move_source`: it is removed afterwards, and while it is open nobody else may write to it
    /// (shared for reading only: a file another program is writing cannot be opened, and is not moved).
    Read { move_source: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `filtered`: a file whose data comes through a file-system filter (a reparse point that is no link: WOF / compressed
    /// system files, deduplication, an app execution alias); it is read WITHOUT opening the reparse point as itself.
    File { size: u64, filtered: bool },
    Dir,
    /// A symbolic link or junction (a name-surrogate reparse point): never entered, never read through, removed as itself.
    Link,
    /// A cloud placeholder (OneDrive and the like): the helper never reads it (that would download it as administrator).
    Cloud,
}

/// An open item of a tree walk.
pub struct Node(Owned);

/// The innermost folder of `held`, opened again through its own handle (no path) for listing, or (`write`) for creating in it.
pub fn held_node(held: &Held, write: bool) -> std::io::Result<Node> {
    let h = held.last().ok_or_else(|| std::io::Error::other("nothing held"))?;
    let mut access = FILE_READ_DATA | FILE_TRAVERSE | FILE_READ_ATTRIBUTES;
    if write {
        access |= FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_WRITE_ATTRIBUTES;
    }
    nt_open(h, &[], access, SHARE_ALL, FILE_OPEN, FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT).map(Node)
}

/// (volume, file id) of every folder `held` holds.
pub fn held_ids(held: &Held) -> Vec<(u64, u64)> {
    held.all().iter().filter_map(|h| facts(h).ok()).map(|f| (u64::from(f.volume), f.index)).collect()
}

fn access_for(want: Want, filtered: bool) -> (u32, u32, u32) {
    let rp = FILE_OPEN_REPARSE_POINT;
    match want {
        Want::Look => (FILE_READ_ATTRIBUTES, SHARE_ALL, rp),
        Want::LookDelete => (FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE, SHARE_ALL, rp),
        Want::List => (FILE_READ_DATA | FILE_TRAVERSE | FILE_READ_ATTRIBUTES, SHARE_ALL, rp | FILE_DIRECTORY_FILE),
        Want::ListDelete => (FILE_READ_DATA | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE, SHARE_ALL, rp | FILE_DIRECTORY_FILE),
        Want::Merge => (FILE_READ_DATA | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES, SHARE_ALL, rp | FILE_DIRECTORY_FILE),
        Want::Read { move_source } => {
            let (extra, share) = if move_source { (DELETE | FILE_WRITE_ATTRIBUTES, SHARE_READ) } else { (0, SHARE_ALL) };
            (FILE_READ_DATA | FILE_READ_ATTRIBUTES | extra, share, FILE_NON_DIRECTORY_FILE | if filtered { 0 } else { rp })
        }
    }
}

/// Opens `name` in `dir` (only [`Want::Look`], [`Want::LookDelete`] and [`Want::Merge`] here: the others go through [`reopen`]).
pub fn open_child(dir: &Node, name: &OsStr, want: Want) -> std::io::Result<Node> {
    let (access, share, options) = access_for(want, false);
    nt_open(&dir.0, &one_part(name)?, access, share, FILE_OPEN, options).map(Node)
}

/// `name` in `dir` opened again for `want`, and only if it is still the same file as `seen` (volume and file id).
pub fn reopen(dir: &Node, name: &OsStr, seen: &Node, want: Want) -> std::io::Result<Node> {
    let filtered = matches!(kind(seen)?, Kind::File { filtered: true, .. });
    let (access, share, options) = access_for(want, filtered);
    let n = nt_open(&dir.0, &one_part(name)?, access, share, FILE_OPEN, options).map(Node)?;
    if id(&n)? != id(seen)? {
        return Err(std::io::Error::other("the item changed while it was opened"));
    }
    Ok(n)
}

/// The item's kind, from its own handle.
pub fn kind(n: &Node) -> std::io::Result<Kind> {
    let f = facts(&n.0)?;
    let reparse = f.attrs & ATTR_REPARSE != 0;
    // IO_REPARSE_TAG_CLOUD and its variants 0x9000X01A
    let cloud = reparse && f.tag & 0xFFFF_0FFF == 0x9000_001A;
    Ok(if reparse && f.tag & NAME_SURROGATE != 0 {
        Kind::Link
    } else if cloud {
        Kind::Cloud
    } else if f.attrs & ATTR_DIRECTORY != 0 {
        Kind::Dir
    } else {
        Kind::File { size: f.size, filtered: reparse }
    })
}

/// (volume, file id) of an open item.
pub fn id(n: &Node) -> std::io::Result<(u64, u64)> {
    let f = facts(&n.0)?;
    Ok((u64::from(f.volume), f.index))
}

pub fn meta(n: &Node) -> std::io::Result<std::fs::Metadata> {
    let f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(n.0.0.0 as _) });
    f.metadata()
}

/// The size an open file has now (from its handle).
pub fn size_now(n: &Node) -> std::io::Result<u64> {
    Ok(facts(&n.0)?.size)
}

/// What a copy of the file would lose: more than one data stream, or the encrypted attribute.
pub fn extras(n: &Node) -> std::io::Result<bool> {
    let f = facts(&n.0)?;
    if f.attrs & FILE_ATTRIBUTE_ENCRYPTED.0 != 0 {
        return Ok(true);
    }
    let mut buf = vec![0u64; 4096];
    match unsafe { GetFileInformationByHandleEx(n.0.0, FileStreamInfo, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32) } {
        Ok(()) => {
            let first = unsafe { &*(buf.as_ptr() as *const FILE_STREAM_INFO) };
            Ok(first.NextEntryOffset != 0)
        }
        // (no streams to tell of: a file system without them)
        Err(_) => Ok(false),
    }
}

/// The names in an open folder (from its handle).
pub fn children(dir: &Node) -> std::io::Result<Vec<OsString>> {
    let mut out = Vec::new();
    let mut buf = vec![0u64; 8192];
    let mut first = true;
    loop {
        let class = if first { FileFullDirectoryRestartInfo } else { FileFullDirectoryInfo };
        first = false;
        let r = unsafe { GetFileInformationByHandleEx(dir.0.0, class, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32) };
        if let Err(e) = r {
            if e.code() == ERROR_NO_MORE_FILES.to_hresult() {
                return Ok(out);
            }
            return Err(os_err(e));
        }
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        loop {
            let e = unsafe { &*(base.add(off) as *const FILE_FULL_DIR_INFO) };
            let n = e.FileNameLength as usize / 2;
            let name = unsafe { std::slice::from_raw_parts(std::ptr::addr_of!(e.FileName) as *const u16, n) };
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                out.push(OsString::from_wide(name));
            }
            if e.NextEntryOffset == 0 {
                break;
            }
            off += e.NextEntryOffset as usize;
        }
        if out.len() > 5_000_000 {
            return Err(std::io::Error::other("too many items"));
        }
    }
}

fn basic(h: HANDLE) -> std::io::Result<FILE_BASIC_INFO> {
    let mut b = FILE_BASIC_INFO::default();
    unsafe { GetFileInformationByHandleEx(h, FileBasicInfo, (&mut b as *mut FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32) }.map_err(os_err)?;
    Ok(b)
}

fn set_basic(h: HANDLE, b: &FILE_BASIC_INFO) -> std::io::Result<()> {
    unsafe { SetFileInformationByHandle(h, FileBasicInfo, (b as *const FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32) }.map_err(os_err)
}

/// Marks the open item for deletion through its own handle (a link or junction is removed as itself; a folder must be empty).
fn dispose(h: HANDLE) -> std::io::Result<()> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0 | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0),
    };
    let e = match unsafe { SetFileInformationByHandle(h, FileDispositionInfoEx, (&ex as *const FILE_DISPOSITION_INFO_EX).cast(), size_of::<FILE_DISPOSITION_INFO_EX>() as u32) } {
        Ok(()) => return Ok(()),
        Err(e) => os_err(e),
    };
    // the older call only where the extended one is not known (FAT, older systems: invalid function / parameter, not supported);
    // a sharing or not-empty error is the answer
    if !matches!(e.raw_os_error(), Some(1) | Some(50) | Some(87)) {
        return Err(e);
    }
    let b = basic(h)?;
    let read_only = b.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0;
    if read_only {
        let mut c = b;
        c.FileAttributes &= !FILE_ATTRIBUTE_READONLY.0;
        if c.FileAttributes == 0 {
            c.FileAttributes = 0x80;
        }
        set_basic(h, &c)?;
    }
    let d = FILE_DISPOSITION_INFO { DeleteFile: true };
    let r = unsafe { SetFileInformationByHandle(h, FileDispositionInfo, (&d as *const FILE_DISPOSITION_INFO).cast(), size_of::<FILE_DISPOSITION_INFO>() as u32) }.map_err(os_err);
    if r.is_err() && read_only {
        // not deleted: the read-only flag goes back
        let _ = set_basic(h, &b);
    }
    r
}

/// Removes the item `n` (opened for deletion) through its handle. `dir` and `name` are for the Linux twin.
pub fn remove(_dir: &Node, _name: &OsStr, n: Node, _is_dir: bool) -> std::io::Result<()> {
    dispose(n.0.0)
}

/// A new folder `name` in `dir` (never an existing name, never through a link), opened for copying into.
pub fn make_dir(dir: &Node, name: &OsStr) -> std::io::Result<Node> {
    let (access, _, options) = access_for(Want::Merge, false);
    nt_open(&dir.0, &one_part(name)?, access, SHARE_ALL, FILE_CREATE, options).map(Node)
}

/// A new file `name` in `dir`, for writing (never an existing name, never through a link).
pub fn make_file(dir: &Node, name: &OsStr) -> std::io::Result<std::fs::File> {
    let h = nt_open(&dir.0, &one_part(name)?, GENERIC_WRITE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE, SHARE_READ, FILE_CREATE, FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT)?;
    let raw = h.0;
    std::mem::forget(h);
    Ok(unsafe { std::fs::File::from_raw_handle(raw.0 as _) })
}

/// Throws away a file [`make_file`] made (a cancelled or failed copy), through its handle.
pub fn discard(_dir: &Node, _name: &OsStr, f: std::fs::File) {
    let _ = dispose(HANDLE(f.as_raw_handle() as _));
}

/// The open file behind a node, borrowed for reading (the node keeps owning the handle).
pub fn reader(n: &Node) -> std::mem::ManuallyDrop<std::fs::File> {
    std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(n.0.0.0 as _) })
}

/// Makes no change on Windows (a rename is on the disk when it returns; the Linux twin syncs the folder).
pub fn sync_dir(_dir: &Node) {}

fn rename_handle(h: HANDLE, dir: &Node, new_name: &OsStr, replace: bool) -> std::io::Result<()> {
    let w = one_part(new_name)?;
    let head = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    // (kernel32 wants at least the whole struct plus the name)
    let bytes = size_of::<FILE_RENAME_INFO>() + w.len() * 2;
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    unsafe {
        let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = dir.0.0;
        (*info).FileNameLength = (w.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(w.as_ptr(), (buf.as_mut_ptr() as *mut u8).add(head) as *mut u16, w.len());
        // NtSetInformationFile(FileRenameInformation) itself: kernel32's SetFileInformationByHandle refuses a RootDirectory
        // ("parameter is incorrect"), and the rename must be relative to the held folder's handle, not a path
        let (set, to_dos) = nt_set().ok_or_else(|| std::io::Error::other("ntdll entry points missing"))?;
        let mut iosb = Iosb { status: 0, info: 0 };
        let st = set(h.0, &mut iosb, buf.as_ptr().cast(), bytes as u32, 10);
        if st < 0 {
            return Err(std::io::Error::from_raw_os_error(to_dos(st) as i32));
        }
        Ok(())
    }
}

type NtSetInformationFileFn = unsafe extern "system" fn(*mut c_void, *mut Iosb, *const c_void, u32, u32) -> i32;

fn nt_set() -> Option<(NtSetInformationFileFn, ToDosFn)> {
    static F: OnceLock<Option<usize>> = OnceLock::new();
    let a = (*F.get_or_init(|| unsafe {
        let m = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        Some(GetProcAddress(m, s!("NtSetInformationFile"))? as usize)
    }))?;
    let (_, to_dos) = ntdll()?;
    Some((unsafe { std::mem::transmute::<usize, NtSetInformationFileFn>(a) }, to_dos))
}

/// Gives the finished temp file `f` (made in `dir`) its final name in the same folder, through its handle; over an existing item
/// only when `replace` (the answer was "Replace"; a link there is replaced as itself, a folder never).
pub fn finish_file(dir: &Node, f: &std::fs::File, _tmp: &OsStr, name: &OsStr, replace: bool) -> std::io::Result<()> {
    rename_handle(HANDLE(f.as_raw_handle() as _), dir, name, replace)
}

/// Renames `name` in `dir` to `new_name` in the same folder, through the item's handle and the folder's (never over an existing
/// item, never following a link: a link is renamed as itself).
pub fn rename_in(dir: &Node, name: &OsStr, new_name: &OsStr) -> std::io::Result<()> {
    let item = nt_open(&dir.0, &one_part(name)?, DELETE | FILE_READ_ATTRIBUTES, SHARE_ALL, FILE_OPEN, FILE_OPEN_REPARSE_POINT)?;
    rename_handle(item.0, dir, new_name, false)
}

/// Times and attributes of an item (read-only, hidden, system, archive…), to put on its copy.
#[derive(Clone, Copy)]
pub struct Stamp(FILE_BASIC_INFO);

pub fn stamp(n: &Node) -> std::io::Result<Stamp> {
    basic(n.0.0).map(Stamp)
}

/// The attributes a copy takes over (not the directory, reparse, compression, encryption or sparse ones, which describe the old
/// file's storage).
const CARRIED: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x2000;

fn apply(h: HANDLE, s: &Stamp, dir: bool) -> std::io::Result<()> {
    let mut b = s.0;
    b.ChangeTime = 0;
    b.FileAttributes &= CARRIED;
    if b.FileAttributes == 0 {
        // (0 keeps what is there; a file without any of them is NORMAL)
        b.FileAttributes = if dir { 0 } else { 0x80 };
    }
    set_basic(h, &b)
}

/// Puts the times and attributes on a finished copy (after its last write and its rename: read-only comes last).
pub fn apply_file(f: &std::fs::File, s: &Stamp) -> std::io::Result<()> {
    apply(HANDLE(f.as_raw_handle() as _), s, false)
}

/// The same for a copied folder, once everything is in it.
pub fn apply_dir(n: &Node, s: &Stamp) -> std::io::Result<()> {
    apply(n.0.0, s, true)
}

