//! A file's attributes and times as the app's `Entry` holds them, and the free space of a volume (moved from rf-app folder_view.rs,
//! properties_view.rs and bench.rs; Linux twin: leaf-unix fileattr.rs, the same names).

use std::path::Path;

/// `file_attributes()` (FILE_ATTRIBUTE_*), `file_size()`, `last_write_time()` and `creation_time()` (FILETIME units) of a
/// `Metadata`: on Windows the standard library's own trait.
pub use std::os::windows::fs::MetadataExt;

/// The FILE_ATTRIBUTE_* bits of `m` (rf_core::attrs).
#[inline]
pub fn attrs_of(m: &std::fs::Metadata) -> u32 {
    m.file_attributes()
}

/// Marks the file at `p` hidden (FILE_ATTRIBUTE_HIDDEN only; failures are ignored).
pub fn set_hidden(p: &Path) {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let _ = windows::Win32::Storage::FileSystem::SetFileAttributesW(
            windows::core::PCWSTR(w.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN,
        );
    }
}

/// The bytes free for this user on the volume of `p` (0 when it cannot be read).
pub fn free_space(p: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    unsafe {
        let _ = windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(windows::core::PCWSTR(w.as_ptr()), Some(&mut free), None, None);
    }
    free
}

/// Cloud files (OneDrive and other sync roots): "Always keep on this device" (`keep`) sets FILE_ATTRIBUTE_PINNED and clears
/// UNPINNED, "Free up space" does the reverse; the sync provider then downloads or dehydrates the file (a folder: what is in it). This
/// is the documented attribute route, the same Explorer takes; the file's data is not read here.
pub fn set_cloud_pin(p: &Path, keep: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, GetFileAttributesW, SetFileAttributesW};
    const PINNED: u32 = 0x0008_0000;
    const UNPINNED: u32 = 0x0010_0000;
    // READONLY | HIDDEN | SYSTEM | ARCHIVE | TEMPORARY | OFFLINE | NOT_CONTENT_INDEXED | NO_SCRUB_DATA | PINNED | UNPINNED: the bits SetFileAttributes takes
    const SETTABLE: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x1000 | 0x2000 | 0x2_0000 | PINNED | UNPINNED;
    let w: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
    let now = unsafe { GetFileAttributesW(windows::core::PCWSTR(w.as_ptr())) };
    if now == u32::MAX {
        return Err(std::io::Error::last_os_error());
    }
    let next = (if keep { (now | PINNED) & !UNPINNED } else { (now | UNPINNED) & !PINNED }) & SETTABLE;
    unsafe { SetFileAttributesW(windows::core::PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(next)) }.map_err(std::io::Error::from)
}
