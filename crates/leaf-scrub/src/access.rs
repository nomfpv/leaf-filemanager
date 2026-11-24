//! A cleaned file is a new file: without this it would come out with the folder's defaults (on Linux usually readable by
//! everyone, on Windows without access rights set on the file itself, without the read-only or hidden mark, unencrypted).
//!
//! - Every file leaf writes starts private: on Linux only its owner can read it, on Windows it is encrypted from the first byte
//!   when the original is encrypted ([`create_temp`]).
//! - Before a cleaned file takes any name (copy and replace mode), it gets the original's permissions: on Linux the permission
//!   bits (and the owner and group where the system allows it), on Windows the access rights set on the file itself (inherited
//!   rights come from the folder anyway) ([`Access::rights`]).
//! - In replace mode, once it is in the original's place, it gets the original's marks: on Windows read-only, hidden, system and
//!   not indexed ([`Access::marks`]). They are set last because a read-only mark would stop leaf moving the file.
//!
//! Not carried over, on purpose: alternate data streams and extended attributes, the point of the cleaning.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(all(windows, test))]
pub(crate) use imp::wide;

/// What is read from the original while it is in its place.
pub(crate) struct Access(imp::Access);

impl Access {
    /// An error means the original's rights cannot be known; the run stops before anything is written.
    pub(crate) fn read(src: &Path) -> io::Result<Access> {
        imp::read(src).map(Access)
    }

    /// The original's permissions onto a file leaf made, before it takes any name. An error stops the run; the file is deleted.
    pub(crate) fn rights(&self, file: &Path) -> io::Result<()> {
        imp::rights(&self.0, file)
    }

    /// The original's marks onto the cleaned file in its place (replace mode). The text of each failure, for the result sheet.
    pub(crate) fn marks(&self, placed: &Path) -> Vec<String> {
        imp::marks(&self.0, placed)
    }
}

/// A new file for leaf to write, beside `like` (the original, or a file made from it): only its owner can read it on Linux, and
/// on Windows it is encrypted before the first byte goes in when `like` is encrypted.
pub(crate) fn create_temp(like: &Path, path: &Path) -> io::Result<fs::File> {
    imp::create_temp(like, path)
}

/// Undoes what [`Access::rights`] and [`Access::marks`] did to a file leaf made, so it can be deleted: the copied access rights
/// could deny leaf that.
pub(crate) fn loosen(path: &Path) {
    imp::loosen(path)
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    pub(crate) struct Access {
        mode: u32,
        uid: u32,
        gid: u32,
    }

    pub(crate) fn read(src: &Path) -> io::Result<Access> {
        let m = fs::metadata(src)?;
        Ok(Access { mode: m.mode() & 0o7777, uid: m.uid(), gid: m.gid() })
    }

    pub(crate) fn rights(a: &Access, file: &Path) -> io::Result<()> {
        // through the open file, not the name: what is changed is the file that was opened
        let f = fs::File::open(file)?;
        let m = f.metadata()?;
        let mut mode = a.mode;
        if m.uid() != a.uid || m.gid() != a.gid {
            let uid = (m.uid() != a.uid).then_some(a.uid);
            let gid = (m.gid() != a.gid).then_some(a.gid);
            // only allowed when leaf runs as that user or is in that group; the owner goes first, it clears the set-id bits
            if std::os::unix::fs::fchown(&f, uid, gid).is_err() {
                // a set-user / set-group id bit on a file that now belongs to someone else would run as them: dropped
                mode &= !0o6000;
            }
        }
        f.set_permissions(fs::Permissions::from_mode(mode))
    }

    pub(crate) fn marks(a: &Access, placed: &Path) -> Vec<String> {
        match fs::metadata(placed) {
            Ok(m) if m.uid() != a.uid || m.gid() != a.gid => {
                vec!["The cleaned file belongs to you: the original belonged to another user or group, and only an administrator can give it back.".into()]
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn create_temp(_like: &Path, path: &Path) -> io::Result<fs::File> {
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
    }

    pub(crate) fn loosen(_path: &Path) {
        // deleting depends on the folder, not on the file's own permissions
    }
}

#[cfg(windows)]
mod imp {
    use std::fs;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows::Win32::Security::{
        ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl, INHERITED_ACE, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows::Win32::Storage::FileSystem::{EncryptFileW, FILE_FLAGS_AND_ATTRIBUTES, GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW};
    use windows::core::PCWSTR;

    const READONLY: u32 = 0x1;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const ARCHIVE: u32 = 0x20;
    const NORMAL: u32 = 0x80;
    const NOT_INDEXED: u32 = 0x2000;
    const ENCRYPTED: u32 = 0x4000;

    /// The path for the Win32 calls, with the `\\?\` prefix (`\\?\UNC\` for a share) so a path longer than 260 characters works
    /// too; a path that already has a prefix is left as it is.
    pub(crate) fn wide(p: &Path) -> Vec<u16> {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let w: Vec<u16> = abs.as_os_str().encode_wide().collect();
        let starts = |s: &str| w.len() >= s.len() && w.iter().zip(s.encode_utf16()).all(|(a, b)| *a == b);
        let colon = w.get(1) == Some(&(b':' as u16)) && w.get(2) == Some(&(b'\\' as u16));
        let (head, rest): (Vec<u16>, &[u16]) = if starts(r"\\?\") || starts(r"\\.\") {
            (Vec::new(), &w)
        } else if starts(r"\\") {
            (r"\\?\UNC\".encode_utf16().collect(), &w[2..])
        } else if colon {
            (r"\\?\".encode_utf16().collect(), &w)
        } else {
            (Vec::new(), &w)
        };
        head.into_iter().chain(rest.iter().copied()).chain(Some(0)).collect()
    }

    fn attrs(w: &[u16]) -> Option<u32> {
        let a = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
        (a != INVALID_FILE_ATTRIBUTES).then_some(a)
    }

    /// The security descriptor of the original, owned (freed with LocalFree), with its access list.
    struct Rights {
        sd: PSECURITY_DESCRIPTOR,
        dacl: *mut ACL,
        protected: bool,
    }

    impl Drop for Rights {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }

    pub(crate) struct Access {
        attrs: u32,
        /// None when the file has no access rights of its own (all inherited from the folder) or the drive keeps none (FAT):
        /// then there is nothing to carry.
        rights: Option<Rights>,
    }

    pub(crate) fn read(src: &Path) -> io::Result<Access> {
        let w = wide(src);
        let attrs = attrs(&w).ok_or_else(io::Error::last_os_error)?;
        Ok(Access { attrs, rights: own_rights(&w)? })
    }

    fn own_rights(w: &[u16]) -> io::Result<Option<Rights>> {
        unsafe {
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            let e = GetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(&mut dacl), None, &mut sd);
            // a file system without access rights: nothing to carry. Any other failure: the rights cannot be known, and a
            // cleaned file that silently lost them could be open to everyone, so the caller stops.
            if e == ERROR_NOT_SUPPORTED || e == ERROR_INVALID_FUNCTION {
                return Ok(None);
            }
            if e.0 != 0 {
                return Err(io::Error::from_raw_os_error(e.0 as i32));
            }
            if sd.0.is_null() {
                return Ok(None);
            }
            let mut r = Rights { sd, dacl, protected: false };
            if r.dacl.is_null() {
                return Ok(None);
            }
            let (mut control, mut rev) = (0u16, 0u32);
            GetSecurityDescriptorControl(r.sd, &mut control, &mut rev).map_err(io::Error::other)?;
            r.protected = control & SE_DACL_PROTECTED.0 != 0;
            let mut explicit = false;
            for i in 0..(*r.dacl).AceCount as u32 {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                if GetAce(r.dacl, i, &mut ace).is_ok() && !ace.is_null() && (*(ace as *const ACE_HEADER)).AceFlags as u32 & INHERITED_ACE.0 == 0 {
                    explicit = true;
                    break;
                }
            }
            Ok((r.protected || explicit).then_some(r))
        }
    }

    pub(crate) fn rights(a: &Access, file: &Path) -> io::Result<()> {
        let Some(r) = &a.rights else { return Ok(()) };
        let w = wide(file);
        let how = DACL_SECURITY_INFORMATION | if r.protected { PROTECTED_DACL_SECURITY_INFORMATION } else { UNPROTECTED_DACL_SECURITY_INFORMATION };
        let e = unsafe { SetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, how, None, None, Some(r.dacl), None) };
        if e.0 != 0 {
            return Err(io::Error::from_raw_os_error(e.0 as i32));
        }
        Ok(())
    }

    pub(crate) fn marks(a: &Access, placed: &Path) -> Vec<String> {
        let w = wide(placed);
        let wanted = a.attrs & (READONLY | HIDDEN | SYSTEM | NOT_INDEXED);
        if wanted == 0 {
            return Vec::new();
        }
        // the archive mark stays as the new file has it: its content changed, a backup should take it
        let set = wanted | attrs(&w).map_or(ARCHIVE, |n| n & ARCHIVE);
        match unsafe { SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(set)) } {
            Ok(()) => Vec::new(),
            Err(e) => vec![format!("The cleaned file did not get the original's read-only or hidden mark ({}).", e.message())],
        }
    }

    pub(crate) fn create_temp(like: &Path, path: &Path) -> io::Result<fs::File> {
        let encrypted = attrs(&wide(like)).is_some_and(|a| a & ENCRYPTED != 0);
        let f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        if !encrypted {
            return Ok(f);
        }
        // empty, closed, encrypted, opened again: no byte of the content is ever written unencrypted
        drop(f);
        let w = wide(path);
        let done = attrs(&w).is_some_and(|a| a & ENCRYPTED != 0) || unsafe { EncryptFileW(PCWSTR(w.as_ptr())) }.is_ok();
        if !done {
            let e = io::Error::last_os_error();
            let _ = fs::remove_file(path);
            return Err(io::Error::other(format!("the original is encrypted and the cleaned file could not be ({e})")));
        }
        fs::OpenOptions::new().write(true).open(path)
    }

    pub(crate) fn loosen(path: &Path) {
        let w = wide(path);
        // an empty access list without protection: only what the folder passes down
        let empty = ACL { AclRevision: 2, Sbz1: 0, AclSize: std::mem::size_of::<ACL>() as u16, AceCount: 0, Sbz2: 0 };
        unsafe {
            let _ = SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(NORMAL));
            let _ = SetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION, None, None, Some(&empty), None);
        }
    }
}
