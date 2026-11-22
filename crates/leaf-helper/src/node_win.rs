//! Windows: items opened RELATIVE to an open folder's handle, never by a path again (`NtCreateFile` with `RootDirectory` and
//! `FILE_OPEN_REPARSE_POINT`), for the helper's own tree walk (`tree.rs`). A name is always one part read from the folder's own
//! listing (or checked by `check_name`), so a link or junction swapped in for an item is opened as the link itself, and a folder
//! once opened is the same folder until it is closed, whatever its name is made to point at meanwhile.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::OnceLock;

use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_DISPOSITION_INFO_EX_FLAGS,
    FILE_FULL_DIR_INFO, FILE_RENAME_INFO, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FileRenameInfo, GetFileInformationByHandleEx, SetFileInformationByHandle,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

use crate::win::{Held, Owned, facts};

const DELETE: u32 = 0x0001_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_ADD_FILE: u32 = 0x2;
const FILE_ADD_SUBDIRECTORY: u32 = 0x4;
const FILE_TRAVERSE: u32 = 0x20;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const SHARE_ALL: u32 = 0x7;

const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

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

fn nt_open(dir: &Owned, name: &OsStr, access: u32, disposition: u32, options: u32) -> std::io::Result<Owned> {
    nt_open_raw(dir, &one_part(name)?, access, disposition, options)
}

/// (an empty name opens `dir` itself again, through its handle)
fn nt_open_raw(dir: &Owned, w: &[u16], access: u32, disposition: u32, options: u32) -> std::io::Result<Owned> {
    let (create, to_dos) = ntdll().ok_or_else(|| std::io::Error::other("ntdll entry points missing"))?;
    let us = UniStr { len: (w.len() * 2) as u16, max: (w.len() * 2) as u16, buf: w.as_ptr() };
    let oa = ObjAttr { length: size_of::<ObjAttr>() as u32, root: dir.0.0, name: &us, attributes: OBJ_CASE_INSENSITIVE, sd: std::ptr::null(), qos: std::ptr::null() };
    let mut iosb = Iosb { status: 0, info: 0 };
    let mut h: *mut c_void = std::ptr::null_mut();
    let st = unsafe {
        create(&mut h, access | SYNCHRONIZE, &oa, &mut iosb, std::ptr::null(), 0, SHARE_ALL, disposition, options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT, std::ptr::null(), 0)
    };
    if st < 0 {
        return Err(std::io::Error::from_raw_os_error(unsafe { to_dos(st) } as i32));
    }
    Ok(Owned(HANDLE(h)))
}

/// What a tree walk needs an item for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Look at it, read it, list it.
    Read,
    /// Read it and remove it afterwards (a delete, a move's source).
    ReadDelete,
    /// A folder something is copied into.
    Merge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File(u64),
    Dir,
    /// A link, a junction, or any other reparse point: never entered, never read through.
    Link,
}

/// An open item of a tree walk.
pub struct Node(Owned);

/// The innermost folder of `held`, opened again from its handle (no path) for listing (`write` false) or for creating in it.
pub fn held_node(held: &Held, write: bool) -> std::io::Result<Node> {
    let h = held.last().ok_or_else(|| std::io::Error::other("nothing held"))?;
    let mut access = FILE_READ_DATA | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    if write {
        access |= FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY;
    }
    nt_open_raw(h, &[], access, FILE_OPEN, FILE_DIRECTORY_FILE).map(Node)
}

pub fn open_child(dir: &Node, name: &OsStr, want: Want) -> std::io::Result<Node> {
    let (access, options) = match want {
        Want::Read => (FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL, 0),
        Want::ReadDelete => (FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE, 0),
        Want::Merge => (FILE_READ_DATA | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES, FILE_DIRECTORY_FILE),
    };
    nt_open(&dir.0, name, access, FILE_OPEN, options).map(Node)
}

/// The item's kind, from its own handle.
pub fn kind(n: &Node) -> std::io::Result<Kind> {
    let f = facts(&n.0)?;
    Ok(if f.attrs & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        Kind::Link
    } else if f.dir() {
        Kind::Dir
    } else {
        Kind::File(f.size)
    })
}

pub fn meta(n: &Node) -> std::io::Result<std::fs::Metadata> {
    let f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(n.0.0.0 as _) });
    f.metadata()
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
            return Err(std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF));
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

/// Marks the open item for deletion through its own handle (a link or junction is removed as itself; a folder must be empty).
fn dispose(h: HANDLE) -> std::io::Result<()> {
    let ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0 | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0),
    };
    if unsafe { SetFileInformationByHandle(h, FileDispositionInfoEx, (&ex as *const FILE_DISPOSITION_INFO_EX).cast(), size_of::<FILE_DISPOSITION_INFO_EX>() as u32) }.is_ok() {
        return Ok(());
    }
    // (file systems without the extended form, FAT: clear a read-only flag through the handle first)
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe { GetFileInformationByHandleEx(h, FileBasicInfo, (&mut basic as *mut FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32) }.is_ok()
        && basic.FileAttributes & FILE_ATTRIBUTE_READONLY.0 != 0
    {
        basic.FileAttributes &= !FILE_ATTRIBUTE_READONLY.0;
        let _ = unsafe { SetFileInformationByHandle(h, FileBasicInfo, (&basic as *const FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32) };
    }
    let d = FILE_DISPOSITION_INFO { DeleteFile: true };
    unsafe { SetFileInformationByHandle(h, FileDispositionInfo, (&d as *const FILE_DISPOSITION_INFO).cast(), size_of::<FILE_DISPOSITION_INFO>() as u32) }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

/// Removes the item `n` (opened with [`Want::ReadDelete`]) through its handle. `dir` and `name` are for the Linux twin.
pub fn remove(_dir: &Node, _name: &OsStr, n: Node, _is_dir: bool) -> std::io::Result<()> {
    dispose(n.0.0)
}

/// A new folder `name` in `dir` (never an existing name, never through a link), opened for copying into.
pub fn make_dir(dir: &Node, name: &OsStr) -> std::io::Result<Node> {
    nt_open(&dir.0, name, FILE_READ_DATA | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | DELETE, FILE_CREATE, FILE_DIRECTORY_FILE).map(Node)
}

/// A new file `name` in `dir`, for writing (never an existing name, never through a link).
pub fn make_file(dir: &Node, name: &OsStr) -> std::io::Result<std::fs::File> {
    let h = nt_open(&dir.0, name, GENERIC_WRITE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE, FILE_CREATE, FILE_NON_DIRECTORY_FILE)?;
    let raw = h.0;
    std::mem::forget(h);
    Ok(unsafe { std::fs::File::from_raw_handle(raw.0 as _) })
}

/// Throws away a file [`make_file`] made (a cancelled or failed copy), through its handle.
pub fn discard(_dir: &Node, _name: &OsStr, f: std::fs::File) {
    let _ = dispose(HANDLE(f.as_raw_handle() as _));
}

/// The file behind a node, for reading.
pub fn into_file(n: Node) -> std::fs::File {
    let raw = n.0.0;
    std::mem::forget(n);
    unsafe { std::fs::File::from_raw_handle(raw.0 as _) }
}

/// Renames `name` in `dir` to `new_name` in the same folder, through the item's handle and the folder's (never over an existing
/// item, never following a link: a link is renamed as itself).
pub fn rename_in(dir: &Node, name: &OsStr, new_name: &OsStr) -> std::io::Result<()> {
    let item = nt_open(&dir.0, name, DELETE | FILE_READ_ATTRIBUTES, FILE_OPEN, 0)?;
    let w = one_part(new_name)?;
    let head = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let bytes = head + w.len() * 2 + 2;
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    unsafe {
        let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = dir.0.0;
        (*info).FileNameLength = (w.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(w.as_ptr(), (buf.as_mut_ptr() as *mut u8).add(head) as *mut u16, w.len());
        SetFileInformationByHandle(item.0, FileRenameInfo, buf.as_ptr().cast(), bytes as u32).map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
    }
}

/// The open file behind a node, borrowed for reading (the node keeps owning the handle).
pub fn reader(n: &Node) -> std::mem::ManuallyDrop<std::fs::File> {
    std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(n.0.0.0 as _) })
}

