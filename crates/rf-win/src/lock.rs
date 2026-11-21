//! The settings file's save lock (moved from rf-app settings_merge.rs; its Linux twin is leaf-unix lock.rs, with the same API).

use std::path::Path;

/// A mutex shared by every process of the user's session that saves this file, so that two saves do not read-modify-write at the
/// same moment. Waits at most three seconds; if it cannot be had the save goes on without it (the window for a lost change is the few
/// milliseconds between a read and a rename, against a settings file that would never be saved at all).
pub struct FileLock(windows::Win32::Foundation::HANDLE);

impl FileLock {
    pub fn acquire(path: &Path) -> Option<FileLock> {
        use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        use windows::core::PCWSTR;
        let key = path.to_string_lossy().to_lowercase();
        let mut h: u32 = 0x811c_9dc5;
        for b in key.bytes() {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        let name = crate::wide::wide(format!("Local\\leaf-settings-{h:08x}"));
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.ok()?;
        let got = unsafe { WaitForSingleObject(handle, 3_000) };
        if got == WAIT_OBJECT_0 || got == WAIT_ABANDONED {
            Some(FileLock(handle))
        } else {
            log::warn!("settings: the save lock could not be had in 3 s; saving without it");
            let _ = unsafe { CloseHandle(handle) };
            None
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
