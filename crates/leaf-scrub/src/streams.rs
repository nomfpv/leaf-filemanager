//! Alternate data streams on NTFS (`Zone.Identifier` = "downloaded from the internet" and the URL it came from) and,
//! on Linux, extended attributes in the `user.` namespace (`user.xdg.origin.url` and friends).

use std::path::Path;

pub struct Stream {
    pub label: String,
    pub value: String,
}

/// What the file carries besides its content. Empty when the file system has no such thing.
pub fn list_streams(path: &Path) -> Vec<Stream> {
    imp::list(path)
}

/// Removes every stream / `user.` attribute; returns the names removed. Used after a replace-in-place.
pub fn remove_streams(path: &Path) -> Vec<String> {
    imp::remove(path)
}

#[cfg(windows)]
mod imp {
    use super::Stream;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Storage::FileSystem::{
        DeleteFileW, FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard, WIN32_FIND_STREAM_DATA,
    };
    use windows::core::PCWSTR;

    fn wide(p: &std::ffi::OsStr) -> Vec<u16> {
        p.encode_wide().chain(Some(0)).collect()
    }

    /// The names of the streams except the main one, as `:Name:$DATA`.
    fn names(path: &Path) -> Vec<String> {
        let w = wide(path.as_os_str());
        let mut out = Vec::new();
        unsafe {
            let mut data = WIN32_FIND_STREAM_DATA::default();
            let h = match FindFirstStreamW(PCWSTR(w.as_ptr()), FindStreamInfoStandard, &mut data as *mut _ as *mut _, Some(0)) {
                Ok(h) => h,
                Err(_) => return out,
            };
            loop {
                let n = String::from_utf16_lossy(&data.cStreamName);
                let n = n.trim_end_matches('\0').to_string();
                if n != "::$DATA" && !n.is_empty() {
                    out.push(n);
                }
                if FindNextStreamW(h, &mut data as *mut _ as *mut _).is_err() {
                    break;
                }
            }
            let _ = FindClose(h);
        }
        out
    }

    fn short(n: &str) -> String {
        n.trim_start_matches(':').trim_end_matches(":$DATA").to_string()
    }

    pub fn list(path: &Path) -> Vec<Stream> {
        names(path)
            .into_iter()
            .map(|n| {
                let mut full = path.as_os_str().to_os_string();
                full.push(&n);
                let mut value = String::new();
                if let Ok(mut f) = std::fs::File::open(&full) {
                    use std::io::Read;
                    let mut b = vec![0u8; 400];
                    let k = f.read(&mut b).unwrap_or(0);
                    b.truncate(k);
                    value = String::from_utf8_lossy(&b).replace(['\r', '\n'], " ");
                }
                Stream { label: format!("Alternate data stream ({})", short(&n)), value }
            })
            .collect()
    }

    pub fn remove(path: &Path) -> Vec<String> {
        let mut gone = Vec::new();
        for n in names(path) {
            let mut full = path.as_os_str().to_os_string();
            full.push(&n);
            let w = wide(&full);
            if unsafe { DeleteFileW(PCWSTR(w.as_ptr())) }.is_ok() {
                gone.push(short(&n));
            }
        }
        gone
    }
}

#[cfg(unix)]
mod imp {
    use super::Stream;
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn names(path: &Path) -> Vec<String> {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return vec![] };
        unsafe {
            let n = libc::llistxattr(c.as_ptr(), std::ptr::null_mut(), 0);
            if n <= 0 {
                return vec![];
            }
            let mut buf = vec![0u8; n as usize];
            let n = libc::llistxattr(c.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len());
            if n <= 0 {
                return vec![];
            }
            buf.truncate(n as usize);
            buf.split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .filter_map(|s| String::from_utf8(s.to_vec()).ok())
                .filter(|s| s.starts_with("user."))
                .collect()
        }
    }

    pub fn list(path: &Path) -> Vec<Stream> {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return vec![] };
        names(path)
            .into_iter()
            .map(|n| {
                let mut value = String::new();
                if let Ok(name) = CString::new(n.clone()) {
                    let mut b = vec![0u8; 400];
                    let k = unsafe { libc::lgetxattr(c.as_ptr(), name.as_ptr(), b.as_mut_ptr() as *mut libc::c_void, b.len()) };
                    if k > 0 {
                        b.truncate(k as usize);
                        value = String::from_utf8_lossy(&b).into_owned();
                    }
                }
                Stream { label: format!("Extended attribute ({n})"), value }
            })
            .collect()
    }

    pub fn remove(path: &Path) -> Vec<String> {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return vec![] };
        let mut gone = Vec::new();
        for n in names(path) {
            let name = CString::new(n.clone()).unwrap_or_default();
            let _: &CStr = name.as_c_str();
            if unsafe { libc::lremovexattr(c.as_ptr(), name.as_ptr()) } == 0 {
                gone.push(n);
            }
        }
        gone
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use super::Stream;
    use std::path::Path;
    pub fn list(_: &Path) -> Vec<Stream> {
        vec![]
    }
    pub fn remove(_: &Path) -> Vec<String> {
        vec![]
    }
}
