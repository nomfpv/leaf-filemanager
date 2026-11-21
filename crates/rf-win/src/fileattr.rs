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
