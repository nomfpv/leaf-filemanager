//! Other processes and the machine's CPU time (moved from rf-app lib.rs and preview_trace.rs; Linux twin: leaf-unix process.rs).

/// `Command::creation_flags` (CREATE_NO_WINDOW and the like); off Windows the same method does nothing.
pub use std::os::windows::process::CommandExt;

/// Waits until the process `pid` has ended, at most `timeout_ms`; returns at once when there is no such process.
pub fn wait_pid(pid: u32, timeout_ms: u32) {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: a handle to wait on, closed right after
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, timeout_ms);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

/// Whether the window in front belongs to this process.
pub fn front_is_ours() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain reads.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            // (nothing is in front for the moment between two windows: not another program's)
            return true;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == windows::Win32::System::Threading::GetCurrentProcessId()
    }
}

/// (idle, kernel + user) CPU time of the whole machine, in 100 ns units.
pub fn cpu_times() -> Option<(u64, u64)> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()? };
    let n = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    Some((n(idle), n(kernel) + n(user)))
}
