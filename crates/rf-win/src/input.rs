//! Keyboard and pointer state the app reads outside GPUI's events, and the system's click and hover times (moved from rf-app
//! ctx_menu.rs, folder_view.rs, ops.rs, shell_view.rs, workspace.rs and look/window.rs; Linux twin: leaf-unix input.rs).

/// Shift is down, as this thread's input state has it (`GetKeyState`: the state of the message being handled).
pub fn shift_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
    unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 }
}

/// Ctrl is down, as this thread's input state has it (`GetKeyState`).
pub fn ctrl_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}

/// Shift is down at this moment (`GetAsyncKeyState`: the keyboard itself, for a drop that came from another program).
pub fn shift_down_now() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    unsafe { GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 }
}

/// Ctrl is down at this moment (`GetAsyncKeyState`).
pub fn ctrl_down_now() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL};
    unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 }
}

/// The pointer's position on the screen (device px; `.x`, `.y`); (0, 0) when it cannot be read.
pub fn cursor_pos() -> crate::geom::POINT {
    let mut pt = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
    }
    pt
}

/// The system's double-click time in ms (Control Panel > Mouse).
pub fn double_click_ms() -> u32 {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
}

/// How long the pointer rests on an item before "Select on hover" selects it: the system's hover time (Control Panel > Mouse;
/// 400 ms out of the box), as Explorer's "point to select" waits.
pub fn hover_time() -> std::time::Duration {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETMOUSEHOVERTIME, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut ms: u32 = 0;
    let ok = unsafe { SystemParametersInfoW(SPI_GETMOUSEHOVERTIME, 0, Some(&mut ms as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }.is_ok();
    std::time::Duration::from_millis(if ok && ms > 0 { ms.min(5000) as u64 } else { 400 })
}

/// The id of the calling thread (a drag started on the shell thread names the UI thread it came from).
pub fn current_thread_id() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}

/// Lets go of the mouse capture of this thread's window (before a drag out of it starts on another thread).
pub fn release_capture() {
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
    }
}

/// Linux twin's feed of the last-known modifier keys; Windows reads them itself, so this does nothing.
pub fn note_modifiers(_shift: bool, _ctrl: bool) {}

/// Linux twin's feed of the last-known pointer position; Windows reads it itself, so this does nothing.
pub fn note_pointer(_x: i32, _y: i32) {}
