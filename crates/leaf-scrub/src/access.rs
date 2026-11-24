//! Replace mode: the cleaned file takes the original's place, so it gets the original's access rules too. A cleaned file is a
//! new file: without this it would come out with the folder's defaults (on Linux usually readable by everyone, on Windows without
//! the read-only or hidden mark and without access rights set on the file itself).
//!
//! Carried over: on Linux the permission bits (and the owner and group where the system allows it); on Windows the read-only,
//! hidden, system and not-indexed marks, encryption, and the access rights set on the file itself (inherited rights come from the
//! folder anyway). Not carried over, on purpose: alternate data streams and extended attributes, the point of the cleaning.

use std::io;
use std::path::Path;

/// What is read from the original before it is put away.
pub(crate) struct Access(imp::Access);

impl Access {
    pub(crate) fn read(src: &Path) -> io::Result<Access> {
        imp::read(src).map(Access)
    }

    /// What must hold before the original is put away (the file never sits in its place with wider rights). An error here
    /// stops the replacement while nothing has changed yet.
    pub(crate) fn before(&self, cleaned: &Path) -> io::Result<()> {
        imp::before(&self.0, cleaned)
    }

    /// What can only be set once the cleaned file is in its place (marks and rights that could stop leaf moving or deleting it).
    /// The text of each error, for the result sheet.
    pub(crate) fn after(&self, placed: &Path) -> Vec<String> {
        imp::after(&self.0, placed)
    }
}

#[cfg(unix)]
mod imp {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
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

    pub(crate) fn before(a: &Access, cleaned: &Path) -> io::Result<()> {
        let m = fs::metadata(cleaned)?;
        // the owner first: changing it clears the set-id bits, which the mode below puts back
        let uid = (m.uid() != a.uid).then_some(a.uid);
        let gid = (m.gid() != a.gid).then_some(a.gid);
        if uid.is_some() || gid.is_some() {
            // only allowed when leaf runs as that user or is a member of that group; else the mode alone is carried
            let _ = std::os::unix::fs::chown(cleaned, uid, gid);
        }
        fs::set_permissions(cleaned, fs::Permissions::from_mode(a.mode))
    }

    pub(crate) fn after(a: &Access, placed: &Path) -> Vec<String> {
        match fs::metadata(placed) {
            Ok(m) if m.uid() != a.uid || m.gid() != a.gid => {
                vec!["The cleaned file belongs to you: the original belonged to another user or group, and only an administrator can give it back.".into()]
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
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

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
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
        /// None when the file has no access rights of its own (all inherited from the folder) or the drive has none (FAT, some
        /// network drives): then there is nothing to carry.
        rights: Option<Rights>,
    }

    pub(crate) fn read(src: &Path) -> io::Result<Access> {
        let w = wide(src);
        let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
        if attrs == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        Ok(Access { attrs, rights: own_rights(&w) })
    }

    fn own_rights(w: &[u16]) -> Option<Rights> {
        unsafe {
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            let e = GetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, None, None, Some(&mut dacl), None, &mut sd);
            if e.0 != 0 || sd.0.is_null() {
                return None;
            }
            let mut r = Rights { sd, dacl, protected: false };
            if r.dacl.is_null() {
                return None;
            }
            let (mut control, mut rev) = (0u16, 0u32);
            GetSecurityDescriptorControl(r.sd, &mut control, &mut rev).ok()?;
            r.protected = control & SE_DACL_PROTECTED.0 != 0;
            let mut explicit = false;
            for i in 0..(*r.dacl).AceCount as u32 {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                if GetAce(r.dacl, i, &mut ace).is_ok() && !ace.is_null() && (*(ace as *const ACE_HEADER)).AceFlags as u32 & INHERITED_ACE.0 == 0 {
                    explicit = true;
                    break;
                }
            }
            (r.protected || explicit).then_some(r)
        }
    }

    pub(crate) fn before(a: &Access, cleaned: &Path) -> io::Result<()> {
        if a.attrs & ENCRYPTED != 0 {
            let w = wide(cleaned);
            let now = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
            if now == INVALID_FILE_ATTRIBUTES || now & ENCRYPTED == 0 {
                unsafe { EncryptFileW(PCWSTR(w.as_ptr())) }.map_err(io::Error::other)?;
            }
        }
        Ok(())
    }

    pub(crate) fn after(a: &Access, placed: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let w = wide(placed);
        unsafe {
            let now = GetFileAttributesW(PCWSTR(w.as_ptr()));
            // the archive mark stays as the new file has it: its content changed, a backup should take it
            let want = (a.attrs & (READONLY | HIDDEN | SYSTEM | NOT_INDEXED)) | if now == INVALID_FILE_ATTRIBUTES { ARCHIVE } else { now & ARCHIVE };
            let marks = a.attrs & (READONLY | HIDDEN | SYSTEM | NOT_INDEXED) != 0;
            if marks && SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(if want == 0 { NORMAL } else { want })).is_err() {
                out.push(format!("The cleaned file did not get the original's read-only or hidden mark ({}).", io::Error::last_os_error()));
            }
            if let Some(r) = &a.rights {
                let how = DACL_SECURITY_INFORMATION | if r.protected { PROTECTED_DACL_SECURITY_INFORMATION } else { UNPROTECTED_DACL_SECURITY_INFORMATION };
                let e = SetNamedSecurityInfoW(PCWSTR(w.as_ptr()), SE_FILE_OBJECT, how, None, None, Some(r.dacl), None);
                if e.0 != 0 {
                    out.push(format!("The cleaned file did not get the original's access rights ({}): check who can open it.", io::Error::from_raw_os_error(e.0 as i32)));
                }
            }
        }
        out
    }
}
