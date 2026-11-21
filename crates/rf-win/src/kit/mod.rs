//! Small helpers the file manager kept from the cut dock: the spring of the UI motions, a screen rectangle, the pointer and mouse
//! buttons for watching pop-up menus, the monitor of a window, the signed-in user's SID and an icon as BGRA.

pub mod icons;
pub mod spring;

/// A rectangle in device px, `right` and `bottom` exclusive (the same shape as Win32's `RECT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// How long a pop-up menu of ours may be open without being the window in front before that counts ([`menu_orphaned`]): the time
/// Windows may take to hand it the foreground.
pub const MENU_FRONT_GRACE_MS: u64 = 700;

/// Whether a pop-up menu of ours has to be put away by its watch, because the usual way will never come.
///
/// A menu closes when its window stops being the active one. That only happens to a window that WAS active. A menu whose window
/// never got the foreground (another window took it at that very moment) is never deactivated: it stayed on the screen, over
/// everything, until the program was closed. So an open menu is also watched from outside: it goes when a mouse button goes down
/// outside its windows (`outside`), or when a window of ANOTHER program has been in front since it opened or for longer than the
/// grace. `front_is_ours`: the window in front belongs to this process (the menu, its own window or another of ours: the usual way
/// handles those).
pub fn menu_orphaned(outside: bool, front_is_ours: bool, open_ms: u64) -> bool {
    outside || (!front_is_ours && open_ms >= MENU_FRONT_GRACE_MS)
}

/// Whether any mouse button is down right now.
pub fn any_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    unsafe { [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].into_iter().any(|k| (GetAsyncKeyState(k.0 as i32) as u16 & 0x8000) != 0) }
}

/// The pointer, device px, screen coordinates.
pub fn cursor() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

/// Makes the calling thread per-monitor DPI aware (v2) for as long as it lives, so that rectangles come back in device px whatever
/// the process was started as.
struct Aware(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);

impl Aware {
    fn enter() -> Aware {
        use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
        Aware(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for Aware {
    fn drop(&mut self) {
        use windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext;
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

/// The monitor (its handle) a window is on. A minimised window is on the monitor it will be restored to (its rectangle while minimised
/// is far off the screen). `None` when the window is gone or no monitor can be told.
pub fn monitor_of_window(hwnd: isize) -> Option<isize> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MonitorFromRect, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, IsIconic, IsWindow, WINDOWPLACEMENT};
    let h = HWND(hwnd as *mut _);
    let _aware = Aware::enter();
    // SAFETY: plain queries about a window handle; a handle that is gone makes them fail, which is the `None` below.
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return None;
        }
        let mon = if IsIconic(h).as_bool() {
            let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            GetWindowPlacement(h, &mut wp).ok()?;
            MonitorFromRect(&wp.rcNormalPosition, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromWindow(h, MONITOR_DEFAULTTONULL)
        };
        (!mon.is_invalid()).then_some(mon.0 as isize)
    }
}

/// The text of the signed-in user's SID ("S-1-5-21-..."), from the process's token.
pub fn current_sid() -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Security::TOKEN_QUERY;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let sid = sid_of_token(token);
        let _ = CloseHandle(token);
        sid
    }
}

unsafe fn sid_of_token(token: windows::Win32::Foundation::HANDLE) -> Option<String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
    unsafe {
        let mut need = 0u32;
        // (the first call only says how big the answer is)
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut need);
        if need == 0 || need > 4096 {
            return None;
        }
        // (8-byte aligned: TOKEN_USER holds a pointer)
        let mut buf = vec![0u64; (need as usize).div_ceil(8)];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), need, &mut need).ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let s = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        s
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_menu_goes_on_an_outside_click_or_when_another_program_stays_in_front() {
        assert!(super::menu_orphaned(true, true, 0));
        assert!(!super::menu_orphaned(false, true, 10_000));
        assert!(!super::menu_orphaned(false, false, super::MENU_FRONT_GRACE_MS - 1));
        assert!(super::menu_orphaned(false, false, super::MENU_FRONT_GRACE_MS));
    }

    #[test]
    fn the_signed_in_user_has_a_sid() {
        assert!(super::current_sid().is_some_and(|s| s.starts_with("S-1-5-")));
    }
}
