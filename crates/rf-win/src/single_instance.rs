//! Single-instance plumbing: the first process owns a named mutex and serves a named pipe;
//! later launches forward their arguments (paths) through the pipe and exit.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_BUSY, GENERIC_WRITE, GetLastError, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

use crate::wide::wide;

/// (kilit adı, boru adı) bir uygulama için: leaf ve leaf-look ayrı örnek gruplarıdır.
pub(crate) fn names_for(app: &str) -> (String, String) {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    // A separate settings folder (tests, portable copies) is a separate instance group:
    // its second launch must not land in the user's everyday window.
    let suffix = match std::env::var_os("LEAF_CONFIG_DIR") {
        Some(dir) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            dir.to_string_lossy().to_lowercase().hash(&mut h);
            format!("-{:08x}", h.finish() as u32)
        }
        None => String::new(),
    };
    (format!("Local\\{app}-{user}{suffix}"), format!(r"\\.\pipe\{app}-{user}{suffix}"))
}

pub enum Role {
    /// We are the first instance. Keep the guard alive for the process lifetime.
    Primary(InstanceGuard),
    /// Another instance is running and received our message.
    Forwarded,
}

pub struct InstanceGuard(pub(crate) HANDLE);
unsafe impl Send for InstanceGuard {}
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Decides the role. `message` is sent to the primary when one exists ("" = just focus a new window/tab).
pub fn acquire(message: &str) -> Role {
    acquire_app(&instance_name(), message)
}

/// "leaf", or "leaf-NAME" with `LEAF_INSTANCE=NAME`: a copy that is a single
/// instance of its own (the speed test's "already running" copy) and never forwards to, or
/// receives from, the user's.
pub fn instance_name() -> String {
    match std::env::var("LEAF_INSTANCE") {
        Ok(n) if !n.is_empty() => format!("{}-{n}", crate::APP_EXE_STEM),
        _ => crate::APP_EXE_STEM.to_string(),
    }
}

/// [`acquire`] for another application of the bundle (e.g. "leaf-look").
pub fn acquire_app(app: &str, message: &str) -> Role {
    let (mutex_name, pipe_name) = names_for(app);
    let w = wide(&mutex_name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(w.as_ptr())) };
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    match handle {
        Ok(h) if !exists => Role::Primary(InstanceGuard(h)),
        Ok(h) => {
            // Primary may still be starting its pipe: retry briefly.
            for _ in 0..40 {
                if send(&pipe_name, message) {
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                    return Role::Forwarded;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // Primary unresponsive (hung or exiting): run on our own.
            Role::Primary(InstanceGuard(h))
        }
        Err(_) => Role::Primary(InstanceGuard(HANDLE::default())),
    }
}

/// Sends `message` to the running instance of `app` (false if none answers). Never waits long: a pipe that does not exist is
/// answered at once, and one whose every instance is taken at this moment is waited for, at most [`BUSY_TRIES`] times
/// [`BUSY_WAIT_MS`].
pub fn send_to(app: &str, message: &str) -> bool {
    send(&names_for(app).1, message)
}

/// How long one wait for a free instance of a busy pipe may take, and how many such waits a send makes.
const BUSY_WAIT_MS: u32 = 200;
const BUSY_TRIES: u32 = 5;

fn send(pipe: &str, message: &str) -> bool {
    let w = wide(pipe);
    unsafe {
        // A BUSY PIPE IS NOT AN ABSENT ONE. The server accepts a client on the one instance that listens and makes the next
        // instance after that, so a message that follows another at once finds every instance taken (`ERROR_PIPE_BUSY`). That
        // used to be read as "nobody there" and the message was dropped: the main app asks the dock "ping" and then tells it
        // "stop", and the stop was the one that got lost (issue 18: the Dock switch went off and the dock stayed; measured with
        // the test below, 199 of 200 such pairs lost a message). The pipe's own wait is for exactly this.
        let mut tries = 0;
        let h = loop {
            match CreateFileW(PCWSTR(w.as_ptr()), GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) {
                Ok(h) => break h,
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() && tries < BUSY_TRIES => {
                    tries += 1;
                    // (returns as soon as an instance listens; an error when none came in the time, and then the next try says so)
                    let _ = WaitNamedPipeW(PCWSTR(w.as_ptr()), BUSY_WAIT_MS);
                }
                Err(_) => return false,
            }
        };
        let mut bytes = message.as_bytes().to_vec();
        bytes.push(b'\n');
        let ok = WriteFile(h, Some(&bytes), None, None).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}

/// Serves the pipe on a background thread; each received line is passed to `on_message`.
pub fn serve(on_message: impl Fn(String) + Send + Sync + 'static) {
    // the pipe of the group the mutex was taken in (`acquire` uses `instance_name`): with LEAF_INSTANCE set, a copy used to
    // take the mutex of its own group and serve the pipe of the user's everyday one
    serve_app(&instance_name(), on_message)
}

/// [`serve`] for another application of the bundle.
pub fn serve_app(app: &str, on_message: impl Fn(String) + Send + Sync + 'static) {
    let on_message = std::sync::Arc::new(on_message);
    let (_, pipe_name) = names_for(app);
    std::thread::Builder::new()
        .name("rf-instance-pipe".into())
        .spawn(move || {
            let w = wide(&pipe_name);
            let make = || unsafe { CreateNamedPipeW(PCWSTR(w.as_ptr()), PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 0, 64 * 1024, 0, None) };
            let mut listening = make();
            loop {
                let pipe = listening;
                if pipe.is_invalid() {
                    log::warn!("single instance pipe failed");
                    return;
                }
                unsafe {
                    let _ = ConnectNamedPipe(pipe, None);
                }
                // The NEXT instance is made before this client is served: while this one is read and closed there is always a pipe
                // of this name (busy for a moment, which a sender waits out; never gone, which a sender takes for "nobody there").
                listening = make();
                // Read each client on its own thread so a stalled client can't block the next one.
                let raw = pipe.0 as isize;
                let cb = on_message.clone();
                std::thread::spawn(move || unsafe {
                    let pipe = HANDLE(raw as *mut _);
                    let mut buf = vec![0u8; 16 * 1024];
                    let mut total = Vec::new();
                    loop {
                        let mut n = 0u32;
                        if ReadFile(pipe, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                            break;
                        }
                        total.extend_from_slice(&buf[..n as usize]);
                        if total.len() > 64 * 1024 {
                            break;
                        }
                    }
                    let _ = DisconnectNamedPipe(pipe);
                    let _ = CloseHandle(pipe);
                    for line in String::from_utf8_lossy(&total).lines() {
                        cb(line.to_string());
                    }
                });
            }
        })
        .ok();
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn apps_have_separate_names() {
        let (m1, p1) = names_for("leaf");
        let (m2, p2) = names_for("leaf-look");
        assert_ne!(m1, m2);
        assert_ne!(p1, p2);
        assert!(p2.contains("leaf-look"));
        assert!(m1.starts_with("Local\\"), "the mutex stays in the per-session namespace");
    }

    #[test]
    fn send_reaches_a_serving_app_and_fails_without_one() {
        let app = format!("rf-test-{}", std::process::id());
        assert!(!send_to(&app, "yok"));
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        serve_app(&app, move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
        let t = std::time::Instant::now();
        while !send_to(&app, "merhaba") {
            assert!(t.elapsed().as_secs() < 3, "pipe never came up");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap(), "merhaba");
    }

    /// Issue 18: the main app asks the dock "are you there?" and, when it is, tells it "stop" right after. The server takes one
    /// client and only then makes the pipe for the next, so the second message found every pipe busy and was dropped: the switch
    /// went off and the dock stayed. Every message of a quick run must arrive.
    #[test]
    fn messages_sent_one_right_after_the_other_all_arrive() {
        let app = format!("rf-test-burst-{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        serve_app(&app, move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
        let t = std::time::Instant::now();
        while !send_to(&app, "up") {
            assert!(t.elapsed().as_secs() < 3, "pipe never came up");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut refused = 0;
        for i in 0..200 {
            // (a ping and a stop, as `sync_process` sends them)
            if !send_to(&app, "ping") || !send_to(&app, &format!("stop {i}")) {
                refused += 1;
            }
        }
        assert_eq!(refused, 0, "a message that follows another at once must not be lost");
        let mut got = 0;
        while let Ok(m) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            got += usize::from(m.starts_with("stop "));
            if got == 200 {
                break;
            }
        }
        assert_eq!(got, 200);
    }
}
