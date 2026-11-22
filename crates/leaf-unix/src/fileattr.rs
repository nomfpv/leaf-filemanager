//! Linux backend of the leaf-sys module `fileattr` (Windows side: rf-win fileattr.rs, the standard library's
//! `std::os::windows::fs::MetadataExt`). Status: REAL
//!
//! The same method names as Windows' `MetadataExt`, in its units, so the app's `Entry` rows are built by the same code: attributes
//! are FILE_ATTRIBUTE_* bits (rf_core::attrs, mapped as `listing` maps them), times are FILETIME (100 ns since 1601).

use std::fs::Metadata;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rf_core::attrs;

/// 100 ns steps from 1601-01-01 (FILETIME's start) to 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Windows' `MetadataExt`, for the four methods the app reads.
pub trait MetadataExt {
    /// FILE_ATTRIBUTE_* bits: DIRECTORY, READONLY (no write bit for the owner), REPARSE_POINT (a link). HIDDEN depends on the name
    /// (a leading dot), which a `Metadata` does not know: see [`attrs_of_named`].
    fn file_attributes(&self) -> u32;
    fn file_size(&self) -> u64;
    /// The last change, in FILETIME units (0 when unknown).
    fn last_write_time(&self) -> u64;
    /// The birth time where the file system keeps one, in FILETIME units (0 when unknown).
    fn creation_time(&self) -> u64;
}

impl MetadataExt for Metadata {
    fn file_attributes(&self) -> u32 {
        attrs_of(self)
    }
    fn file_size(&self) -> u64 {
        self.len()
    }
    fn last_write_time(&self) -> u64 {
        filetime(self.modified())
    }
    fn creation_time(&self) -> u64 {
        filetime(self.created())
    }
}

/// The FILE_ATTRIBUTE_* bits of `m` (without HIDDEN, see [`MetadataExt::file_attributes`]).
pub fn attrs_of(m: &Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let ft = m.file_type();
    let mut a = if ft.is_dir() { attrs::DIRECTORY } else { 0 };
    if ft.is_symlink() {
        a |= attrs::REPARSE_POINT;
    }
    if m.permissions().mode() & 0o200 == 0 {
        a |= attrs::READONLY;
    }
    a
}

/// [`attrs_of`] plus HIDDEN for a name with a leading dot.
pub fn attrs_of_named(m: &Metadata, name: &str) -> u32 {
    attrs_of(m) | if name.starts_with('.') { attrs::HIDDEN } else { 0 }
}

fn filetime(t: std::io::Result<SystemTime>) -> u64 {
    match t.map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(d)) => FILETIME_UNIX_EPOCH + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100,
        // (before 1970)
        Ok(Err(e)) => FILETIME_UNIX_EPOCH.saturating_sub(e.duration().as_secs() * 10_000_000),
        Err(_) => 0,
    }
}

/// Marks the file at `p` hidden: nothing to do, a name with a leading dot is hidden already (the only way Linux has).
pub fn set_hidden(_p: &Path) {}

/// The bytes free for this user on the file system of `p` (statvfs: blocks available to unprivileged users); 0 when it cannot be
/// read.
pub fn free_space(p: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return 0 };
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a NUL-terminated path; statvfs writes the struct, which is read only when it returned 0.
    if unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return 0;
    }
    let s = unsafe { s.assume_init() };
    (s.f_bavail as u64).saturating_mul(s.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_and_times_of_a_file_a_folder_and_a_link() {
        let dir = std::env::temp_dir().join(format!("leaf-unix-fileattr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, b"12345").unwrap();
        let m = std::fs::symlink_metadata(&f).unwrap();
        assert_eq!(m.file_attributes(), 0);
        assert_eq!(m.file_size(), 5);
        // written a moment ago: FILETIME of now, give or take a minute
        let now = filetime(Ok(SystemTime::now()));
        assert!(m.last_write_time().abs_diff(now) < 600_000_000, "{} {now}", m.last_write_time());
        assert_eq!(attrs_of_named(&m, ".a.txt"), attrs::HIDDEN);
        let mut ro = m.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut ro, 0o444);
        std::fs::set_permissions(&f, ro).unwrap();
        assert_eq!(std::fs::symlink_metadata(&f).unwrap().file_attributes(), attrs::READONLY);
        assert_eq!(std::fs::metadata(&dir).unwrap().file_attributes() & attrs::DIRECTORY, attrs::DIRECTORY);
        let l = dir.join("link");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        assert_ne!(std::fs::symlink_metadata(&l).unwrap().file_attributes() & attrs::REPARSE_POINT, 0);
        assert_eq!(filetime(Ok(UNIX_EPOCH)), FILETIME_UNIX_EPOCH);
        assert!(free_space(&dir) > 0);
        assert_eq!(free_space(Path::new("/no/such/place")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Cloud files' "Always keep" / "Free up space" (rf-win fileattr.rs): there are no sync roots of that kind here.
pub fn set_cloud_pin(_p: &Path, _keep: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "not available on this system"))
}
