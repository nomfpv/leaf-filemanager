//! Who holds a file open: the Windows Restart Manager. It is only asked who uses the files; nothing is ended or restarted.
//!
//! Used when a delete, move, rename or copy fails because the file is in use, so the failure can say which program has it.
//! Call it off the UI thread (it can take a moment). Every failure of the API answers "nobody found", never an error.

use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmExplorer, RmGetList, RmMainWindow, RmOtherWindow, RmRegisterResources, RmService,
    RmStartSession,
};
use windows::core::{PCWSTR, PWSTR};

pub use leaf_common::locks::{Locker, LockerKind};

/// The programs that hold any of `paths` open (empty when none, or when the question could not be asked).
pub fn who_locks(paths: &[PathBuf]) -> Vec<Locker> {
    if paths.is_empty() {
        return Vec::new();
    }
    let wide: Vec<Vec<u16>> = paths.iter().map(|p| crate::wide::wide(p)).collect();
    let names: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();
    let mut session = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: plain Restart Manager calls on buffers that live through them; the session is ended on every path out.
    unsafe {
        if RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) != ERROR_SUCCESS {
            return Vec::new();
        }
        let out = query(session, &names);
        let _ = RmEndSession(session);
        out
    }
}

unsafe fn query(session: u32, names: &[PCWSTR]) -> Vec<Locker> {
    unsafe {
        if RmRegisterResources(session, Some(names), None, None) != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut have = 0u32;
        let mut reasons = 0u32;
        let mut needed = 0u32;
        // the list can grow between the two calls: ask again a few times with room to spare
        let mut buf: Vec<RM_PROCESS_INFO> = Vec::new();
        for _ in 0..4 {
            have = buf.len() as u32;
            let first = if buf.is_empty() { None } else { Some(buf.as_mut_ptr()) };
            let rc = RmGetList(session, &mut needed, &mut have, first, &mut reasons);
            if rc == ERROR_SUCCESS {
                buf.truncate(have as usize);
                break;
            }
            if rc != ERROR_MORE_DATA {
                return Vec::new();
            }
            buf = vec![RM_PROCESS_INFO::default(); needed as usize + 4];
        }
        if have as usize > buf.len() {
            return Vec::new();
        }
        let mut out: Vec<Locker> = Vec::new();
        for info in buf.iter().take(have as usize) {
            let end = info.strAppName.iter().position(|&c| c == 0).unwrap_or(info.strAppName.len());
            let mut name = String::from_utf16_lossy(&info.strAppName[..end]);
            if name.is_empty() {
                let e = info.strServiceShortName.iter().position(|&c| c == 0).unwrap_or(info.strServiceShortName.len());
                name = String::from_utf16_lossy(&info.strServiceShortName[..e]);
            }
            let kind = match info.ApplicationType {
                t if t == RmMainWindow || t == RmOtherWindow => LockerKind::Application,
                t if t == RmService => LockerKind::Service,
                t if t == RmExplorer => LockerKind::Explorer,
                _ => LockerKind::Other,
            };
            let pid = info.Process.dwProcessId;
            if !out.iter().any(|l| l.pid == pid) {
                out.push(Locker { name, pid, kind });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn names_this_process_for_a_file_it_holds_exclusively() {
        let path = std::env::temp_dir().join(format!("leaf-locks-test-{}.tmp", std::process::id()));
        let file = std::fs::OpenOptions::new().write(true).create(true).share_mode(0).open(&path).unwrap();
        let found = who_locks(std::slice::from_ref(&path));
        drop(file);
        let _ = std::fs::remove_file(&path);
        assert!(found.iter().any(|l| l.pid == std::process::id()), "{found:?}");
    }
}
