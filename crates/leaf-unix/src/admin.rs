//! Administrator mode, leaf's side on Linux (the twin of rf-win's `admin.rs`): `leaf-helper` started through `pkexec` (polkit
//! asks for the password), the same typed messages over a Unix socket in the user's runtime folder.
//!
//! The helper checks who connects (`SO_PEERCRED`, the `leaf` binary beside it); this side checks that the socket's other end is
//! the process `pkexec` became (pkexec runs the helper in its own process) and runs as root. Compile-checked only: not run.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use leaf_common::admin::*;

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("Administrator permission was not given")]
    Declined,
    #[error("The administrator helper is not installed with leaf")]
    Missing,
    #[error("The administrator helper could not be started")]
    Failed(String),
}

pub struct Session {
    client: Client,
    child: Mutex<Child>,
    pid: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.client.bye();
        let _ = self.child.lock().map(|mut c| c.try_wait());
    }
}

impl Session {
    pub fn alive(&self) -> bool {
        !self.client.broken() && self.child.lock().map(|mut c| matches!(c.try_wait(), Ok(None))).unwrap_or(false)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn call(&self, req: &Request, calls: &Calls, data: &mut Vec<u8>) -> Result<Reply, HelperError> {
        self.client.call(req, calls, data)
    }

    /// "End now": goodbye; the helper ends on it (a root process cannot be signalled from here, and does not need to be).
    pub fn end(&self) {
        self.client.bye();
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && self.alive() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn random32() -> Option<[u8; 32]> {
    let mut b = [0u8; 32];
    let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    (n == 32).then_some(b)
}

pub fn helper_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let p = me.parent()?.join(leaf_common::exe_name(HELPER_EXE_STEM));
    p.is_file().then_some(p)
}

/// Starts the helper through `pkexec` (the call waits while polkit asks; never on the UI thread), connects and greets it.
pub fn start() -> Result<Session, StartError> {
    let exe = helper_exe().ok_or(StartError::Missing)?;
    let token = random32().map(|b| token_text(&b)).ok_or_else(|| StartError::Failed("no random bytes".into()))?;
    let channel = random32().map(|b| format!("leaf-helper-{}-{}", std::process::id(), &token_text(&b)[..16])).ok_or_else(|| StartError::Failed("no random bytes".into()))?;
    let uid = unsafe { libc::getuid() };
    let socket = PathBuf::from(format!("/run/user/{uid}/{channel}.sock"));
    let mut child = Command::new("pkexec")
        .arg(&exe)
        .args(["--channel", &channel, "--token", &token, "--user", &uid.to_string(), "--pid", &std::process::id().to_string()])
        .spawn()
        .map_err(|e| StartError::Failed(e.to_string()))?;
    let pid = child.id();
    // (the password dialog may take a while: up to two minutes)
    let until = Instant::now() + Duration::from_secs(120);
    let stream = loop {
        if let Ok(Some(status)) = child.try_wait() {
            // 126: the dialog was dismissed, 127: not authorised
            return Err(if matches!(status.code(), Some(126) | Some(127)) { StartError::Declined } else { StartError::Failed(format!("{status}")) });
        }
        if let Ok(s) = UnixStream::connect(&socket) {
            break s;
        }
        if Instant::now() > until {
            return Err(StartError::Failed("the helper's socket did not open".into()));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) };
    if r != 0 || cred.pid as u32 != pid || cred.uid != 0 {
        return Err(StartError::Failed("another process answered".into()));
    }
    let writer = stream.try_clone().map_err(|e| StartError::Failed(e.to_string()))?;
    match Client::greet(Box::new(stream), Box::new(writer), &token) {
        Ok((client, said)) if said == pid => Ok(Session { client, child: Mutex::new(child), pid }),
        Ok(_) => Err(StartError::Failed("another process answered".into())),
        Err(e) => Err(StartError::Failed(e.to_string())),
    }
}

pub fn is_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(is_denied_io) || c.downcast_ref::<crate::ListError>().is_some_and(|l| matches!(l, crate::ListError::AccessDenied))
    })
}

pub fn is_denied_io(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}
