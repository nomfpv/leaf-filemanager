//! Linux backend of rf_win::single_instance.
//!
//! The first process holds an advisory lock (`flock`) on `XDG_RUNTIME_DIR/leaf/<app>.lock`, the twin of rf-win's named mutex: the kernel
//! lets it go when the process ends, however it ends, so a crash leaves nothing to clean. It serves a Unix socket beside it
//! (`<app>.sock`, the twin of the pipe; the folder is 0700, the socket 0600: per user). Later launches connect, write their
//! message as one line and leave. A socket file whose owner is gone is stale: connecting to it fails, and the lock is free.
//!
//! The wire format is rf-win's: one line per message, text as the app encodes it. A line that starts with `\u{1}` is a header of this
//! backend (the xdg-activation token of the sender, see `activate`), not a message.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// What starting up found (rf-win single_instance.rs:34).
pub enum Role {
    /// We are the first instance. Keep the guard alive for the process lifetime.
    Primary(InstanceGuard),
    /// Another instance is running and received our message.
    Forwarded,
}

/// Held while this process is the first instance (rf-win: a named mutex). Here: the locked file.
pub struct InstanceGuard {
    _lock: Option<std::fs::File>,
}

impl InstanceGuard {
    pub(crate) fn locked(f: std::fs::File) -> Self {
        InstanceGuard { _lock: Some(f) }
    }

    /// A guard that holds nothing (the lock could not be made, or a launch that runs beside an unresponsive first one).
    pub fn inert() -> Self {
        InstanceGuard { _lock: None }
    }
}

/// "leaf", or "leaf-NAME" with `LEAF_INSTANCE=NAME` (rf-win single_instance.rs `instance_name`).
pub fn instance_name() -> String {
    match std::env::var("LEAF_INSTANCE") {
        Ok(n) if !n.is_empty() => format!("{}-{n}", crate::APP_EXE_STEM),
        _ => crate::APP_EXE_STEM.to_string(),
    }
}

/// (lock file, socket) of an application's instance group. A separate settings folder (`LEAF_CONFIG_DIR`: tests, portable copies) is
/// a separate group, as on Windows.
pub(crate) fn paths_for(app: &str) -> (PathBuf, PathBuf) {
    let suffix = match std::env::var_os("LEAF_CONFIG_DIR") {
        Some(dir) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            dir.to_string_lossy().hash(&mut h);
            format!("-{:08x}", h.finish() as u32)
        }
        None => String::new(),
    };
    let base = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join("leaf"),
        // (no runtime dir: a per-user folder in the temp dir, made 0700 as well)
        None => std::env::temp_dir().join(format!("leaf-{}", unsafe { libc::getuid() })),
    };
    (base.join(format!("{app}{suffix}.lock")), base.join(format!("{app}{suffix}.sock")))
}

/// Makes the folder of the group (0700) and returns it.
fn ensure_dir(file: &std::path::Path) -> bool {
    let Some(dir) = file.parent() else { return false };
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
}

/// Takes the group's lock: Some when this process is the first instance, None when another holds it.
pub(crate) fn take_lock(app: &str) -> Result<Option<std::fs::File>, ()> {
    let (lock, _) = paths_for(app);
    if !ensure_dir(&lock) {
        return Err(());
    }
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).read(true).mode_600().open(&lock).map_err(|_| ())?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(f))
    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(())
    }
}

trait Mode600 {
    fn mode_600(&mut self) -> &mut Self;
}
impl Mode600 for std::fs::OpenOptions {
    fn mode_600(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o600)
    }
}

/// Connects to the running instance of `app` (None when nobody answers: no socket, or a stale one).
pub(crate) fn connect(app: &str) -> Option<UnixStream> {
    UnixStream::connect(paths_for(app).1).ok()
}

/// Writes one message (with the activation token of this launch, when the compositor gave one) and says whether it was written.
pub(crate) fn write_message(s: &mut UnixStream, message: &str) -> bool {
    let mut out = Vec::new();
    if let Some(t) = std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty() && !t.contains('\n')) {
        out.extend_from_slice(format!("\u{1}activation={t}\n").as_bytes());
    }
    // (a message is one line: the app's encodings carry no newline)
    out.extend_from_slice(message.replace(['\n', '\r'], " ").as_bytes());
    out.push(b'\n');
    s.write_all(&out).is_ok()
}

/// Reads the headers (their token is handed to `activate`) and the first message line of a client. None on an empty client.
pub(crate) fn read_request(r: &mut impl BufRead) -> Option<String> {
    loop {
        let mut line = String::new();
        // (a client that sends no newline is cut off at 64 KiB)
        if r.by_ref().take(64 * 1024).read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        match line.strip_prefix('\u{1}') {
            Some(h) => {
                if let Some(t) = h.strip_prefix("activation=") {
                    crate::activate::offer_token(t);
                }
            }
            None => return Some(line.to_string()),
        }
    }
}

/// Binds the group's socket (0600). The caller holds the lock, so a socket file in the way is stale; without the lock (it could not be
/// made) a file that still answers is left alone.
fn bind(app: &str, have_lock: bool) -> Option<UnixListener> {
    let (_, sock) = paths_for(app);
    ensure_dir(&sock);
    if !have_lock && UnixStream::connect(&sock).is_ok() {
        return None;
    }
    let _ = std::fs::remove_file(&sock);
    let l = UnixListener::bind(&sock).ok()?;
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    Some(l)
}

/// Accepts clients on a background thread, each served on a thread of its own so a stalled client does not block the next.
pub(crate) fn serve_clients(app: &str, name: &str, on_client: Arc<dyn Fn(UnixStream) + Send + Sync>) {
    let Some(listener) = bind(app, true) else {
        log::warn!("single instance socket failed");
        return;
    };
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for c in listener.incoming() {
                let Ok(c) = c else { continue };
                let cb = on_client.clone();
                std::thread::spawn(move || cb(c));
            }
        })
        .ok();
}

/// Serves the socket of `app`; each received line is passed to `on_message`.
pub fn serve_app(app: &str, on_message: impl Fn(String) + Send + Sync + 'static) {
    serve_clients(
        app,
        "leaf-instance-socket",
        Arc::new(move |c| {
            let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
            let mut r = BufReader::new(c);
            while let Some(line) = read_request(&mut r) {
                on_message(line);
            }
        }),
    );
}

/// Decides the role. The first instance gets the guard; a later one hands `message` over and is [`Role::Forwarded`].
pub fn acquire_app(app: &str, message: &str) -> Role {
    match take_lock(app) {
        Ok(Some(f)) => return Role::Primary(InstanceGuard { _lock: Some(f) }),
        Err(()) => return Role::Primary(InstanceGuard::inert()),
        Ok(None) => {}
    }
    // The first one may still be starting its socket: retry briefly.
    for _ in 0..40 {
        if send_to(app, message) {
            return Role::Forwarded;
        }
        std::thread::sleep(Duration::from_millis(50));
        // (it may have gone meanwhile)
        if let Ok(Some(f)) = take_lock(app) {
            return Role::Primary(InstanceGuard { _lock: Some(f) });
        }
    }
    // Unresponsive (hung or exiting): run on our own.
    Role::Primary(InstanceGuard::inert())
}

/// Sends `message` to the running instance of `app` (false if none answers).
pub fn send_to(app: &str, message: &str) -> bool {
    match connect(app) {
        Some(mut s) => write_message(&mut s, message),
        None => false,
    }
}

#[cfg(test)]
pub(crate) fn test_runtime_dir() {
    // one temp runtime dir for every test of the crate (the real one is never touched)
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("leaf-si-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_launch_reaches_the_first_and_a_gone_first_leaves_the_place_free() {
        test_runtime_dir();
        let app = "leaf-test";
        assert!(!send_to(app, "nobody"));
        let guard = match acquire_app(app, "first") {
            Role::Primary(g) => g,
            Role::Forwarded => panic!("nobody runs"),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        serve_app(app, move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
        // a second launch on another thread hands its message over
        let t = std::thread::spawn(move || matches!(acquire_app("leaf-test", "/tmp/x"), Role::Forwarded));
        assert!(t.join().unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), "/tmp/x");
        let (_, sock) = paths_for(app);
        assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        // the first ends: its lock goes, the socket file stays (stale); the next launch is the first
        drop(guard);
        assert!(matches!(acquire_app(app, "again"), Role::Primary(_)));
    }
}
