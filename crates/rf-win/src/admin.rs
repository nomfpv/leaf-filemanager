//! Administrator mode, leaf's side (PLAN W3): starting `leaf-helper.exe` with the normal UAC prompt and talking to it.
//!
//! Nothing here runs elevated, and nothing starts the helper but [`start`], which the app calls only after the user clicked
//! "Retry as administrator". The helper checks who connects (see `leaf-helper`'s `win.rs`); this side checks that the pipes it
//! connects to are served by the very process it started (`GetNamedPipeServerProcessId` against the process ShellExecuteEx gave),
//! so a program that made pipes of the same name first gets nothing from leaf. The pipes are opened at identification level only:
//! the helper can see who leaf is, not act as leaf.

use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;

pub use leaf_common::admin::*;

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_NONE, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::Win32::System::Threading::{GetProcessId, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::PCWSTR;

use crate::wide::wide;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Why the helper could not be started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The user said no at the UAC prompt.
    #[error("Administrator permission was not given")]
    Declined,
    /// `leaf-helper.exe` is not beside `leaf.exe`.
    #[error("The administrator helper is not installed with leaf")]
    Missing,
    #[error("The administrator helper could not be started")]
    Failed(String),
}

/// A running helper and the conversation with it.
pub struct Session {
    client: Client,
    process: HANDLE,
    pid: u32,
}

// (a process handle may be used from any thread)
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        self.client.bye();
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}

impl Session {
    /// The helper still runs and the pipes still work (it ends by itself after five idle minutes).
    pub fn alive(&self) -> bool {
        !self.client.broken() && unsafe { WaitForSingleObject(self.process, 0) } != WAIT_OBJECT_0
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// One request (see [`Client::call`]).
    pub fn call(&self, req: &Request, calls: &Calls, data: &mut Vec<u8>) -> Result<Reply, HelperError> {
        self.client.call(req, calls, data)
    }

    /// "End now": goodbye, and the helper's process is ended if it is still there a moment later.
    pub fn end(&self) {
        self.client.bye();
        unsafe {
            if WaitForSingleObject(self.process, 2000) != WAIT_OBJECT_0 {
                let _ = TerminateProcess(self.process, 0);
            }
        }
    }
}

fn random32() -> Option<[u8; 32]> {
    let mut b = [0u8; 32];
    unsafe { BCryptGenRandom(None, &mut b, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.is_ok().then_some(b)
}

/// The helper beside leaf's own program.
pub fn helper_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let p = me.parent()?.join(leaf_common::exe_name(HELPER_EXE_STEM));
    p.is_file().then_some(p)
}

/// Starts the helper: this is where Windows shows the UAC prompt (the call waits for the user's answer, so never on the UI
/// thread). Then connects to its two pipes, checks that its own process serves them, and greets it.
pub fn start() -> Result<Session, StartError> {
    let exe = helper_exe().ok_or(StartError::Missing)?;
    let token = random32().map(|b| token_text(&b)).ok_or_else(|| StartError::Failed("no random bytes".into()))?;
    let channel = random32().map(|b| format!("leaf-helper-{}-{}", std::process::id(), &token_text(&b)[..16])).ok_or_else(|| StartError::Failed("no random bytes".into()))?;
    let sid = crate::kit::current_sid().ok_or_else(|| StartError::Failed("no user SID".into()))?;
    let args = format!("--channel {channel} --token {token} --user {sid} --pid {}", std::process::id());
    crate::shell::ensure_com();
    let (file, params, verb) = (wide(&exe), wide(&args), wide("runas"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(e) = unsafe { ShellExecuteExW(&mut info) } {
        // ERROR_CANCELLED: "No" at the prompt
        return Err(if e.code().0 as u32 == 0x8007_04C7 { StartError::Declined } else { StartError::Failed(e.to_string()) });
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return Err(StartError::Failed("no process handle".into()));
    }
    let pid = unsafe { GetProcessId(process) };
    let fail = |why: &str| {
        unsafe {
            let _ = TerminateProcess(process, 0);
            let _ = CloseHandle(process);
        }
        StartError::Failed(why.to_string())
    };
    let Some(up) = connect(&format!("{channel}-up"), GENERIC_WRITE, process, pid) else { return Err(fail("the helper's pipe did not open")) };
    let Some(down) = connect(&format!("{channel}-down"), GENERIC_READ, process, pid) else {
        unsafe {
            let _ = CloseHandle(up);
        }
        return Err(fail("the helper's pipe did not open"));
    };
    let reader: Box<dyn std::io::Read + Send> = Box::new(unsafe { std::fs::File::from_raw_handle(down.0 as _) });
    let writer: Box<dyn std::io::Write + Send> = Box::new(unsafe { std::fs::File::from_raw_handle(up.0 as _) });
    match Client::greet(reader, writer, &token) {
        Ok((client, said)) if said == pid => Ok(Session { client, process, pid }),
        Ok(_) => Err(fail("another process answered")),
        Err(e) => Err(fail(&e.to_string())),
    }
}

/// Opens one of the helper's pipes once it is there (up to 20 s, while the helper runs), and only if `pid` serves it.
fn connect(name: &str, access: u32, process: HANDLE, pid: u32) -> Option<HANDLE> {
    let w = wide(format!(r"\\.\pipe\{name}"));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match unsafe { CreateFileW(PCWSTR(w.as_ptr()), access, FILE_SHARE_NONE, None, OPEN_EXISTING, SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, None) } {
            Ok(h) => {
                let mut server = 0u32;
                let ours = unsafe { GetNamedPipeServerProcessId(h, &mut server) }.is_ok() && server == pid;
                if ours {
                    return Some(h);
                }
                unsafe {
                    let _ = CloseHandle(h);
                }
                return None;
            }
            Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => unsafe {
                let _ = WaitNamedPipeW(PCWSTR(w.as_ptr()), 500);
            },
            Err(e) if e.code() == ERROR_FILE_NOT_FOUND.to_hresult() => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(_) => return None,
        }
        if std::time::Instant::now() > until || unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
            return None;
        }
    }
}

/// The system said "access denied" somewhere in this error (the shell's engine, a file call, leaf's own engines).
pub fn is_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(is_denied_io)
            || c.downcast_ref::<windows::core::Error>().is_some_and(|w| w.code().0 as u32 == 0x8007_0005)
            || c.downcast_ref::<crate::ListError>().is_some_and(|l| matches!(l, crate::ListError::AccessDenied))
    })
}

pub fn is_denied_io(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5)
}
