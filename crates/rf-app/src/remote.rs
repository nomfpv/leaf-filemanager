//! Places that are not local folders (`Location::Remote`), listed through `leaf-vfs` on a worker thread. Local folders never
//! come here: they keep `Location::Path` and the direct listing.
//!
//! - `sftp://` (W1a), `ftp://` and `ftps://` (W1c): `leaf-remote` serves them. Connecting asks through dialogs here (an unknown
//!   host key, a key's passphrase, a password, an FTPS certificate the system does not trust); a changed host key is refused
//!   with a warning in the pane and no way past it. A password for plain `ftp://` is asked with a line saying it crosses the
//!   network unencrypted.
//! - Opening or looking at a file fetches a size-capped copy into a cache folder first (as archives do), then opens that.
//! - Copy, Copy to other pane, Copy to..., a drag onto a local pane and Paste in a local folder download through
//!   `leaf_vfs::copy` as a Status Center job (progress, pause, cancel, names that exist asked; a cut-off file is removed).
//! - Changes on a server (W1a part 2): paste, a drop, Copy to other pane and "Copy to..." upload (or, within one server, move);
//!   new folder, rename, delete (asked first, always permanent: a server has no Trash) as Status Center jobs. A big upload
//!   offers "Verify on server" (the server's SHA-256 against this PC's).
//! - "Edit" on a file on a server (H10): a writable copy in the cache, opened in the local editor and watched; each save is
//!   uploaded (the old version kept as `name.leaf-bak`), refused when the server's file changed since the fetch, with "Save as
//!   copy on server" offered instead.
//! - `mem://demo/` (the in-memory demo tree; `mem://offline/` and `mem://locked/` show the error banner) in debug builds, or in
//!   any build with `LEAF_VFS_DEMO=1`. Schemes without a backend stay paths as before.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use leaf_remote::{CertQuestion, HostKeyAnswer, HostKeyQuestion, Prompter, Secret, SecretQuestion};
use leaf_vfs::{Fs, FsError, LocalFs};
use rf_core::Location;
use rf_core::vpath::VPath;

use crate::t;

/// Marks a listing error that is the server's (offline, login refused): the pane shows it quietly with a Retry button.
const RETRY_MARK: &str = "\u{1}retry\u{1}";

/// Why moving, pasting onto, renaming and deleting are off in a place that cannot be written to (the menus show it).
pub const NOT_WIRED: &str = "This place is read-only in leaf";

/// Files bigger than this are not opened straight from a server (copy them to this PC first).
const OPEN_MAX: u64 = 512 << 20;
/// Files bigger than this are not fetched for leaf-look (the same limit as for members of an archive).
const LOOK_MAX: u64 = 64 << 20;

/// The `mem://` demo is on: debug builds, or `LEAF_VFS_DEMO` set to anything but `0`.
pub fn demo_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("LEAF_VFS_DEMO").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether addresses of this scheme open as a remote place.
pub fn usable(scheme: &str) -> bool {
    match scheme {
        // local paths stay local (the fast path)
        "file" => false,
        "mem" => demo_enabled(),
        s => leaf_vfs::has_backend(s),
    }
}

/// A path that is an address (`ftp://host/x` kept as a `Location::Path` by an older leaf, in its history or a saved tab):
/// never a folder of this PC. Reads the name only.
pub fn is_address_path(p: &Path) -> bool {
    p.to_str().is_some_and(|s| VPath::looks_like(&s.replace(std::path::MAIN_SEPARATOR, "/")))
}

/// The remote place an address kept as a path stands for (`ftp://host/pub` from an older leaf's history), when leaf serves
/// it now. A password in it (`ftp://user:pw@host`, which an older leaf took) is dropped, so the place is written back to the
/// history without it. Reads the name only.
pub fn upgrade_address_path(loc: Location) -> Location {
    match &loc {
        Location::Path(p) if is_address_path(p) => match parse_typed(&rf_core::history::redact_credentials(&p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))) {
            Some(remote) => remote,
            None => loc,
        },
        _ => loc,
    }
}

/// Text typed into the address bar (or handed over at start) that names a remote place.
pub fn parse_typed(text: &str) -> Option<Location> {
    let t = text.trim().trim_matches('"');
    if !VPath::looks_like(t) {
        return None;
    }
    let v = VPath::parse(t).ok()?;
    usable(v.scheme()).then_some(Location::Remote(v))
}

/// The place of a Network entry: a mounted `smb://` or `nfs://` share keeps its address, any other target is a folder.
pub fn network_place(target: PathBuf) -> Location {
    target.to_str().and_then(parse_typed).unwrap_or(Location::Path(target))
}

/// "Disconnect" of a mounted network place (`gio mount -u`), off the UI thread; a refusal or a missing `gio` is told in a dialog.
pub fn disconnect(v: &VPath, cx: &mut App) {
    let uri = format!("{}://{}{}", v.scheme(), v.authority(), v.path());
    let work = cx.background_executor().spawn(async move { leaf_sys::network::disconnect(&uri) });
    cx.spawn(async move |cx| {
        let Err(e) = work.await else { return };
        let (title, text) = if e.kind() == std::io::ErrorKind::Unsupported {
            (t!("Not available"), t!("Network places need gio (gvfs), which is not installed on this system.").to_string())
        } else {
            (t!("Could not disconnect"), format!("{}\n\n{e}", t!("The place is still in use or the system refused.")))
        };
        let _ = cx.update(|cx| {
            if let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) {
                let _ = w.update(cx, |_, window, cx| crate::prompt::notice(title, text, window, cx));
            }
        });
    })
    .detach();
}

/// Why typed text that is an address of a place leaf serves is not opened: a password in it (`ftp://user:pw@host`), or
/// another fault of the address. `None` for text that is no such address (or an address of a scheme leaf does not serve).
pub fn refused_address(text: &str) -> Option<String> {
    let t = text.trim().trim_matches('"');
    if !VPath::looks_like(t) {
        return None;
    }
    let scheme = t.split_once("://").map_or("", |(s, _)| s).to_ascii_lowercase();
    // (other schemes, `https://` and the like, go on as before: an address command may hand them on)
    if !usable(&scheme) {
        return None;
    }
    match VPath::parse(t) {
        Ok(_) => None,
        Err(rf_core::vpath::VPathError::Password) => Some(t!("Passwords are not written in addresses: leaf asks for the password when it connects, and keeps it in memory only. Type the address without it (ftp://user@host).").to_string()),
        Err(e) => Some(crate::i18n::td(&e.to_string())),
    }
}

/// Lists `dir` (on a worker thread). The error is the text the pane shows.
pub fn list(dir: &VPath, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<rf_core::Entry>)) -> Result<usize, String> {
    let fs = leaf_vfs::fs_for(dir.scheme()).map_err(message)?;
    fs.list(dir, 512, cancelled, on_batch).map_err(message)
}

fn message(e: FsError) -> String {
    use leaf_vfs::FsError as E;
    match e {
        E::Offline(_) | E::Auth(_) => format!("{RETRY_MARK}{e}"),
        E::NotFound => "Folder not found".into(),
        E::Denied => "Access denied".into(),
        // the user said no to a question of the connection (or navigated away, which nobody sees)
        E::Cancelled => format!("{RETRY_MARK}{}", t!("Connecting was cancelled")),
        E::Unsupported => "Connecting is not built yet".into(),
        e => e.to_string(),
    }
}

/// The text of a server error that gets the Retry banner, or `None` for any other error.
pub fn retry_message(err: &str) -> Option<&str> {
    err.strip_prefix(RETRY_MARK)
}

// ------------------------------------------------------------------------------------------------ connecting: the dialogs

/// A question of a connection, on its way to the UI thread.
enum Ask {
    HostKey(HostKeyQuestion, Box<dyn FnOnce(HostKeyAnswer) + Send>),
    Secret(SecretQuestion, Box<dyn FnOnce(Option<Secret>) + Send>),
    Cert(CertQuestion, Box<dyn FnOnce(HostKeyAnswer) + Send>),
}

impl Ask {
    fn cancel(self) {
        match self {
            Ask::HostKey(_, r) | Ask::Cert(_, r) => r(HostKeyAnswer::Cancel),
            Ask::Secret(_, r) => r(None),
        }
    }
}

static ASKS: Mutex<Option<mpsc::UnboundedReceiver<Ask>>> = Mutex::new(None);

struct UiPrompter(mpsc::UnboundedSender<Ask>);

impl Prompter for UiPrompter {
    fn host_key(&self, q: HostKeyQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        if let Err(e) = self.0.unbounded_send(Ask::HostKey(q, reply)) {
            e.into_inner().cancel();
        }
    }
    fn secret(&self, q: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>) {
        if let Err(e) = self.0.unbounded_send(Ask::Secret(q, reply)) {
            e.into_inner().cancel();
        }
    }
    fn certificate(&self, q: CertQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        if let Err(e) = self.0.unbounded_send(Ask::Cert(q, reply)) {
            e.into_inner().cancel();
        }
    }
}

/// Serves `sftp://`, `ftp://` and `ftps://` (cheap: nothing connects and no thread starts until the first remote place is opened). Before the
/// command line is read, so `leaf sftp://...` opens the server.
pub fn register() {
    let (tx, rx) = mpsc::unbounded();
    *ASKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
    leaf_remote::register(leaf_remote::Config::for_user(crate::settings::app_dir()), Arc::new(UiPrompter(tx)));
}

/// Shows the connections' questions as dialogs, one at a time, in the main window in front.
pub fn init_ui(cx: &mut App) {
    let Some(mut rx) = ASKS.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    cx.spawn(async move |cx| {
        while let Some(a) = rx.next().await {
            let mut pending = Some(a);
            loop {
                cx.update(|cx| try_show(&mut pending, cx));
                if pending.is_none() {
                    break;
                }
                // another dialog is open there: this one waits its turn
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            }
        }
    })
    .detach();
}

fn try_show(pending: &mut Option<Ask>, cx: &mut App) {
    let mains = crate::workspace::main_windows(cx);
    let w = cx.active_window().filter(|a| mains.contains(a)).or_else(|| mains.first().copied());
    let Some(w) = w else {
        // no window to ask in: no
        if let Some(a) = pending.take() {
            a.cancel();
        }
        return;
    };
    let busy = w.update(cx, |_, window, cx| crate::prompt::current(window, cx).is_some()).unwrap_or(false);
    if busy {
        return;
    }
    let mut a = pending.take();
    let _ = w.update(cx, |_, window, cx| {
        if let Some(a) = a.take() {
            window.activate_window();
            show(a, window, cx);
        }
    });
    if let Some(a) = a {
        a.cancel();
    }
}

/// The reply of a dialog, taken by whichever button is pressed first. A dialog put away without a button (Escape) drops it,
/// which the connection reads as "Cancel".
type Once<T> = std::rc::Rc<std::cell::Cell<Option<Box<dyn FnOnce(T) + Send>>>>;

fn show(a: Ask, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    match a {
        Ask::HostKey(q, reply) => {
            let reply: Once<HostKeyAnswer> = std::rc::Rc::new(std::cell::Cell::new(Some(reply)));
            let answer = |a: HostKeyAnswer| {
                let reply = reply.clone();
                move |_: Option<String>, _: &mut Window, _: &mut App| {
                    if let Some(r) = reply.take() {
                        r(a);
                    }
                    Ok(())
                }
            };
            let msg = format!(
                "{}\n\n{} {}\n{} {}",
                t!("leaf has not seen this server's key before. Check that it is the key the server's owner gave you before you trust it."),
                t!("Key type:"),
                q.key_type,
                t!("Fingerprint:"),
                q.fingerprint
            );
            // (a key of another type than the known one never comes here: it is refused like a changed key)
            crate::prompt::show(
                format!("{} {}", t!("Unknown server:"), q.host),
                Some(msg),
                None,
                // Cancel is the default button: Enter (and the keyboard's first stop) never trusts a key
                vec![
                    Button::new(t!("Trust and remember"), Kind::Normal, answer(HostKeyAnswer::TrustAndRemember)),
                    Button::new(t!("Trust once"), Kind::Normal, answer(HostKeyAnswer::TrustOnce)),
                    Button::new(t!("Cancel"), Kind::Primary, answer(HostKeyAnswer::Cancel)),
                ],
                window,
                cx,
            );
        }
        Ask::Cert(q, reply) => {
            let reply: Once<HostKeyAnswer> = std::rc::Rc::new(std::cell::Cell::new(Some(reply)));
            let answer = |a: HostKeyAnswer| {
                let reply = reply.clone();
                move |_: Option<String>, _: &mut Window, _: &mut App| {
                    if let Some(r) = reply.take() {
                        r(a);
                    }
                    Ok(())
                }
            };
            if q.trusted_before.is_empty() {
                let first = t!("This PC does not trust the server's certificate. Trust it only if you know it is the server's own (a home server or NAS often has a self-signed one).");
                let msg = format!("{first}\n\n{} {}\n{} {}", t!("Reason:"), crate::i18n::td(&q.reason), t!("Fingerprint:"), q.fingerprint);
                crate::prompt::show(
                    format!("{} {}", t!("Certificate not trusted:"), q.host),
                    Some(msg),
                    None,
                    // Cancel is the default button: Enter never trusts a certificate
                    vec![
                        Button::new(t!("Trust this certificate"), Kind::Normal, answer(HostKeyAnswer::TrustAndRemember)),
                        Button::new(t!("Trust once"), Kind::Normal, answer(HostKeyAnswer::TrustOnce)),
                        Button::new(t!("Cancel"), Kind::Primary, answer(HostKeyAnswer::Cancel)),
                    ],
                    window,
                    cx,
                );
            } else {
                // a certificate other than the one trusted before: a warning of its own, no "Trust once"
                let first = t!("WARNING: this server shows another certificate than the one you trusted, and this PC does not trust the new one either. Someone may be intercepting the connection. Replace the trusted certificate only if you know the server got a new one (ask its owner).");
                let msg = format!(
                    "{first}\n\n{} {}\n{} {}\n{} {}",
                    t!("Trusted before:"),
                    q.trusted_before.join(", "),
                    t!("Shown now:"),
                    q.fingerprint,
                    t!("Reason:"),
                    crate::i18n::td(&q.reason)
                );
                crate::prompt::show(
                    format!("{} {}", t!("Certificate changed:"), q.host),
                    Some(msg),
                    None,
                    // Cancel is the default button: Enter never replaces a trusted certificate
                    vec![
                        Button::new(t!("Replace trusted certificate"), Kind::Normal, answer(HostKeyAnswer::TrustAndRemember)),
                        Button::new(t!("Cancel"), Kind::Primary, answer(HostKeyAnswer::Cancel)),
                    ],
                    window,
                    cx,
                );
            }
        }
        Ask::Secret(q, reply) => {
            let reply: Once<Option<Secret>> = std::rc::Rc::new(std::cell::Cell::new(Some(reply)));
            let ok = {
                let reply = reply.clone();
                move |text: Option<String>, _: &mut Window, _: &mut App| {
                    if let Some(r) = reply.take() {
                        r(Some(Secret::new(text.unwrap_or_default())));
                    }
                    Ok(())
                }
            };
            let cancel = move |_: Option<String>, _: &mut Window, _: &mut App| {
                if let Some(r) = reply.take() {
                    r(None);
                }
                Ok(())
            };
            let buttons = vec![Button::new(t!("OK"), Kind::Primary, ok), Button::new(t!("Cancel"), Kind::Normal, cancel)];
            let kept = t!("Kept in memory until leaf closes; never saved.");
            match q {
                SecretQuestion::Passphrase { key_file, retry } => {
                    let first = if retry { t!("Wrong passphrase. Try again.") } else { t!("This key file is locked with a passphrase.") };
                    crate::prompt::show_secret(t!("Key passphrase"), Some(format!("{first}\n{key_file}\n\n{kept}")), t!("Passphrase").to_string(), buttons, window, cx);
                }
                SecretQuestion::Password { account, retry, unencrypted } => {
                    let first = if retry { t!("Wrong password. Try again.") } else { t!("The server asks for a password.") };
                    let plain = if unencrypted { format!("\n{}", t!("Not encrypted: plain FTP sends the password and the files as they are. Use ftps:// where the server has it.")) } else { String::new() };
                    crate::prompt::show_secret(format!("{} {account}", t!("Password for")), Some(format!("{first}{plain}\n\n{kept}")), t!("Password").to_string(), buttons, window, cx);
                }
                SecretQuestion::Server { account, prompt, echo } => {
                    let title = format!("{account} {}", t!("asks"));
                    if echo {
                        crate::prompt::show(title, Some(prompt), Some((String::new(), String::new())), buttons, window, cx);
                    } else {
                        crate::prompt::show_secret(title, Some(format!("{prompt}\n\n{kept}")), String::new(), buttons, window, cx);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ copies of files

/// The cache file of a server's file: `<temp>/leaf-remote/<hash of the whole address>/<the name, made safe>`. The hash keeps
/// two addresses apart however alike their safe names are; the server's names never become folders here, so nothing can
/// lead out of the cache. On Unix the cache folder is the user's own (mode 0700).
fn cache_path(v: &VPath) -> PathBuf {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(v.to_string().as_bytes());
    let hash: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let root = cache_root();
    root.join(hash).join(leaf_vfs::names::safe_local_name(v.name()))
}

fn cache_root() -> PathBuf {
    #[cfg(unix)]
    {
        let user = std::env::var("USER").unwrap_or_default();
        std::env::temp_dir().join(format!("leaf-remote-{}", leaf_vfs::names::safe_local_name(&user)))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("leaf-remote")
    }
}

/// Makes the cache folder of `local` (on Unix the top one only for this user: another user cannot plant files there).
fn make_cache_dir(local: &Path) -> Result<(), FsError> {
    let root = cache_root();
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // a link is refused; only the folder's owner can change its mode, so a folder another user made fails here
        if !std::fs::symlink_metadata(&root)?.is_dir() {
            return Err(FsError::Denied);
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    if let Some(dir) = local.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// A copy of the server's file in the cache folder, fetched unless a copy of the same size and time is there. Only a regular
/// file is fetched (`fetch_file` refuses a folder or a link at fetch time), at most `max` bytes as they arrive, marked as
/// downloaded. Read-only on purpose: changes to it would not go back to the server (editing on a server comes later).
fn fetch(v: &VPath, max: u64, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, FsError> {
    let fs = leaf_vfs::fs_for(v.scheme())?;
    let m = fs.stat(v)?;
    if m.kind != leaf_vfs::Kind::File {
        return Err(FsError::Other("This is not a regular file on the server".into()));
    }
    let local = cache_path(v);
    if let Ok(lm) = std::fs::symlink_metadata(&local)
        && lm.is_file()
    {
        let same_time = lm.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).is_some_and(|d| d.as_millis() as i64 / 1000 == m.modified_ms / 1000);
        if lm.len() == m.size && same_time {
            return Ok(local);
        }
    }
    make_cache_dir(&local)?;
    let from = format!("{}://{}/", v.scheme(), leaf_remote::known_hosts::host_name(v.host(), v.port().unwrap_or(if v.scheme().starts_with("ftp") { 21 } else { 22 })));
    leaf_vfs::copy::fetch_file(&*fs, v, &local, max, Some(&from), cancelled, progress)?;
    if let Ok(lm) = std::fs::metadata(&local) {
        let mut perm = lm.permissions();
        perm.set_readonly(true);
        let _ = std::fs::set_permissions(&local, perm);
    }
    Ok(local)
}

fn anyhow_of(e: FsError) -> anyhow::Error {
    anyhow::anyhow!(crate::i18n::td(&e.to_string()))
}

/// Opens a file on a server: a copy is fetched (Status Center, with cancel), then opened as a local file would be.
pub fn open_file(cx: &mut App, v: VPath, size: u64) {
    let name = v.name().to_string();
    if size > OPEN_MAX {
        crate::status_center::start(cx, format!("{} {name}", t!("Opening from the server:")), move |_| {
            Err(anyhow::anyhow!(t!("This file is too big to open straight from the server (over 512 MB): copy it to this PC first.")))
        });
        return;
    }
    crate::status_center::start(cx, format!("{} {name}", t!("Opening from the server:")), move |tr| {
        let cancel = tr.cancel.clone();
        let mut progress = |d: u64, total: u64| {
            tr.bytes.set(d, total);
            (tr.on_progress)(total.max(1) as u32, d.min(u32::MAX as u64) as u32, None);
        };
        let local = match fetch(&v, OPEN_MAX, &|| cancel.load(Ordering::Relaxed), &mut progress) {
            Ok(l) => l,
            Err(FsError::Cancelled) => return Ok(false),
            Err(e) => return Err(anyhow_of(e)),
        };
        if let Err(e) = leaf_sys::openrules::open(&local) {
            log::warn!("open {}: {e}", local.display());
        }
        Ok(true)
    });
}

/// leaf-look for a file on a server: a copy (up to 64 MB) is fetched first, in the background.
pub fn look(v: VPath, size: u64) {
    if size > LOOK_MAX {
        log::info!("remote: {} is too big to fetch for leaf-look", v);
        return;
    }
    // (no cancel here: the fetch stops at LOOK_MAX bytes received, whatever size the server claimed)
    std::thread::spawn(move || match fetch(&v, LOOK_MAX, &|| false, &mut |_, _| {}) {
        Ok(local) => crate::look::ipc::send(crate::look::ipc::Msg::Toggle(local), true),
        Err(e) => log::warn!("remote: could not fetch {v} for leaf-look: {e}"),
    });
}

// ------------------------------------------------------------------------------------------------ following a file (H7)

/// A file of a server being followed: its newest bytes are read again every [`TAIL_EVERY`] from the last offset (the same
/// `open_read` with an offset a resumed download uses; nothing runs on the server) into a copy in the cache folder, which
/// leaf-look shows. Dropping it (the tab closed, Alt+F again) ends the reading.
pub struct Tail(Arc<std::sync::atomic::AtomicBool>);

impl Drop for Tail {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

const TAIL_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
/// How much of the end of a file is taken when following starts, and the most one look adds.
const TAIL_START: u64 = 1 << 20;
const TAIL_STEP: u64 = 8 << 20;

/// Starts following `v` (`size` as the listing showed it) and shows its tail in leaf-look.
pub fn follow(v: VPath, size: u64) -> Tail {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("rf-remote-tail".into())
        .spawn(move || {
            let mut shown = false;
            let mut offset = None::<u64>;
            let mut local = tail_path(&v);
            while !flag.load(Ordering::Relaxed) {
                match tail_step(&v, size, &mut local, &mut offset) {
                    Ok(()) if !shown => {
                        shown = true;
                        crate::look::ipc::send(crate::look::ipc::Msg::Toggle(local.clone()), true);
                    }
                    Ok(()) => {}
                    Err(e) => log::warn!("remote: following {v}: {e}"),
                }
                let mut waited = std::time::Duration::ZERO;
                while waited < TAIL_EVERY && !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    waited += std::time::Duration::from_millis(100);
                }
            }
        })
        .ok();
    Tail(stop)
}

/// The tail copy of a server's file (its own folder: the whole-file copy of `fetch` is not touched).
fn tail_path(v: &VPath) -> PathBuf {
    let p = cache_path(v);
    let dir = p.parent().map(|d| d.with_file_name(format!("{}-tail", d.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())));
    dir.map_or(p.clone(), |d| d.join(p.file_name().unwrap_or_default()))
}

/// One look: the first one writes the end of the file, later ones add what grew past `offset`; a file that got smaller is
/// started over.
fn tail_step(v: &VPath, _size: u64, local: &mut PathBuf, offset: &mut Option<u64>) -> Result<(), FsError> {
    use std::io::{Read as _, Write as _};
    let fs = leaf_vfs::fs_for(v.scheme())?;
    let now = fs.stat(v)?;
    if now.kind != leaf_vfs::Kind::File {
        return Err(FsError::Other("This is not a regular file on the server".into()));
    }
    let (from, fresh) = match *offset {
        Some(o) if now.size >= o => (o, false),
        _ => (now.size.saturating_sub(TAIL_START), true),
    };
    if !fresh && now.size == from {
        return Ok(());
    }
    make_cache_dir(local)?;
    let mut out = if fresh {
        std::fs::File::create(&*local)?
    } else {
        std::fs::OpenOptions::new().append(true).open(&*local)?
    };
    let mut src = fs.open_read(v, from)?.take(now.size.saturating_sub(from).min(TAIL_STEP));
    let n = std::io::copy(&mut src, &mut out)?;
    out.flush()?;
    *offset = Some(from + n);
    Ok(())
}

// ------------------------------------------------------------------------------------------------ downloads

/// What Copy put on the clipboard from a server: the addresses (also on the clipboard as text, so a later copy of anything
/// else replaces it as the clipboard's content).
/// The flag: the items were cut (Paste in a folder on the same server moves them).
static CLIP: Mutex<Option<(String, Vec<(VPath, bool)>, bool)>> = Mutex::new(None);

fn clip_text(items: &[(VPath, bool)]) -> String {
    items.iter().map(|(v, _)| v.to_string()).collect::<Vec<_>>().join("\n")
}

/// Copy (or Cut) in a folder on a server: remembered for Paste in a local folder or in a folder on a server.
pub fn copy_to_clipboard(items: Vec<(VPath, bool)>, cut: bool, cx: &mut App) {
    if items.is_empty() {
        return;
    }
    let text = clip_text(&items);
    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    *CLIP.lock().unwrap_or_else(|e| e.into_inner()) = Some((text, items, cut));
}

/// The server items the clipboard holds (Copy in a remote folder, and nothing copied since).
pub fn clipboard_items(cx: &App) -> Option<Vec<(VPath, bool)>> {
    clipboard_items_cut(cx).map(|(items, _)| items)
}

/// The server items the clipboard holds, and whether they were cut.
pub fn clipboard_items_cut(cx: &App) -> Option<(Vec<(VPath, bool)>, bool)> {
    let (text, items, cut) = CLIP.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    // (the clipboard is read only after a Copy on a server: a normal paste does not wait on it here)
    let now = cx.read_from_clipboard().and_then(|c| c.text())?;
    (now == text).then_some((items, cut))
}

/// Forgets what was cut (it was moved: a second Paste must not try again).
pub fn clear_clipboard() {
    *CLIP.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// What a drag of items on a server carries (dropped on a local pane: downloaded there).
#[derive(Clone)]
pub struct RemoteDrag(pub Vec<(VPath, bool)>);

impl Render for RemoteDrag {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::Palette::for_window(window);
        let label = match self.0.as_slice() {
            [(v, _)] => v.name().to_string(),
            items => crate::i18n::items(items.len()),
        };
        div().px_3().py_1p5().rounded(crate::theme::RADIUS).bg(p.card_bg).border_1().border_color(p.accent).text_sm().text_color(p.text).child(label)
    }
}

/// Copies the server's items into the local folder `dest` (a Status Center job): files and whole folders through
/// `leaf_vfs::copy`, names that exist asked once for the items themselves (a folder merged into takes the same answer for
/// what is in it); `after` gets what arrived.
pub fn download(cx: &mut App, items: Vec<(VPath, bool)>, dest: PathBuf, after: Option<Box<dyn FnOnce(Vec<PathBuf>, &mut App)>>) {
    if items.is_empty() {
        return;
    }
    let what = match items.as_slice() {
        [(v, _)] => v.name().to_string(),
        _ => crate::i18n::items(items.len()),
    };
    let title = format!("{} {what} → {}", t!("Downloading"), rf_core::history::display_name(&dest));
    let arrived: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let got = arrived.clone();
    let done: Option<crate::status_center::OnDone> = after.map(|after| -> crate::status_center::OnDone {
        Box::new(move |_, cx| {
            let v = std::mem::take(&mut *got.lock().unwrap_or_else(|e| e.into_inner()));
            after(v, cx)
        })
    });
    crate::status_center::start_with(cx, title, move |tr| download_work(&tr, &items, &dest, &arrived), done);
}

fn download_work(tr: &leaf_sys::fileops::Tracker, items: &[(VPath, bool)], dest: &Path, arrived: &Mutex<Vec<PathBuf>>) -> anyhow::Result<bool> {
    use leaf_sys::fastcopy::{Conflict as Asked, Resolve};
    use leaf_vfs::copy::{Conflict, Copy, Progress};
    let cancelled = || tr.cancel.load(Ordering::Relaxed);
    let fs = leaf_vfs::fs_for(items[0].0.scheme()).map_err(anyhow_of)?;
    // the server's names of the items themselves are checked before any path is made from them: `D:evil.exe` or `C:` joined
    // to `dest` would replace it (Windows' join), `..` would leave it; a refused item is left out and counted
    let mut refused = 0usize;
    let mut checked = Vec::new();
    let mut targets = Vec::new();
    for (v, is_dir) in items {
        match top_target(dest, v.name()) {
            Ok(t) => {
                checked.push((v.clone(), *is_dir));
                targets.push(t);
            }
            Err(why) => {
                log::warn!("download: an item's name is refused ({why})");
                refused += 1;
            }
        }
    }
    let items = checked.as_slice();
    // the sizes first (folders walked), for the progress bar
    (tr.on_progress)(0, 0, Some(t!("Counting what is on the server").to_string()));
    let (mut total_files, mut total_bytes) = (0u64, 0u64);
    for (v, is_dir) in items {
        count(&*fs, v, *is_dir, &cancelled, &mut total_files, &mut total_bytes).map_err(anyhow_of)?;
        if cancelled() {
            return Ok(false);
        }
    }
    tr.bytes.set(0, total_bytes.max(1));
    // the disk must hold what the server says it sends, and the copy stops when what really arrives would not fit
    let free = leaf_sys::fileattr::free_space(dest);
    let room = if free == 0 { u64::MAX } else { free.saturating_sub(leaf_vfs::copy::KEEP_FREE) };
    let no_room = || anyhow::anyhow!(t!("There is not enough free space on this PC for this download"));
    if total_bytes > room {
        return Err(no_room());
    }
    let full = std::sync::atomic::AtomicBool::new(false);
    let stop = || cancelled() || full.load(Ordering::Relaxed);

    // names that are here already: asked once, for the items themselves
    let mut asked = Vec::new();
    let mut at = Vec::new();
    for ((v, is_dir), target) in items.iter().zip(&targets) {
        if let Ok(m) = std::fs::symlink_metadata(target) {
            let src = fs.stat(v).map_err(anyhow_of)?;
            at.push(asked.len());
            asked.push(Asked {
                src: PathBuf::from(v.to_string()),
                dst: target.clone(),
                src_dir: *is_dir,
                dst_dir: m.is_dir(),
                src_size: src.size,
                dst_size: m.len(),
                src_time: (src.modified_ms > 0).then(|| std::time::UNIX_EPOCH + std::time::Duration::from_millis(src.modified_ms as u64)),
                dst_time: m.modified().ok(),
            });
        } else {
            at.push(usize::MAX);
        }
    }
    let answers: Vec<Resolve> = if asked.is_empty() {
        Vec::new()
    } else {
        match tr.ask.as_ref() {
            Some(ask) => match ask(&asked) {
                Some(a) => a,
                None => return Ok(false),
            },
            None => vec![Resolve::KeepBoth; asked.len()],
        }
    };

    let (mut base_bytes, mut base_files) = (0u64, 0u64);
    for (i, (v, is_dir)) in items.iter().enumerate() {
        let answer = answers.get(at[i]).copied();
        if answer == Some(Resolve::Skip) {
            continue;
        }
        let mut target = targets[i].clone();
        if answer == Some(Resolve::KeepBoth) {
            let name = v.name();
            let (stem, ext) = match name.rfind('.') {
                Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
                _ => (name, ""),
            };
            target = leaf_sys::fileops::unique_name(dest, stem, ext);
            if target.parent() != Some(dest) {
                refused += 1;
                continue;
            }
        }
        let inner = if answer == Some(Resolve::Replace) { Conflict::Replace } else { Conflict::KeepBoth };
        let mut on_conflict = |_: &VPath, _: &leaf_vfs::Meta| inner;
        let mut on_progress = |p: &Progress| {
            while tr.paused.load(Ordering::Relaxed) && !cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let got = base_bytes.saturating_add(p.bytes_done);
            if got > room {
                full.store(true, Ordering::Relaxed);
            }
            tr.bytes.set(got, total_bytes.max(1));
            let done = (base_files + p.files_done).min(u32::MAX as u64) as u32;
            let cur = (p.file_done == p.file_size || p.file_done <= 256 * 1024).then(|| p.file.name().to_string());
            (tr.on_progress)(total_files.max(1) as u32, done, cur);
        };
        // every file that arrives carries the download mark (SmartScreen, Office's Protected View)
        let from = format!("{}://{}/", v.scheme(), leaf_remote::known_hosts::host_name(v.host(), v.port().unwrap_or(if v.scheme().starts_with("ftp") { 21 } else { 22 })));
        let mut mark = |part: &VPath| {
            if let Err(e) = leaf_sys::fileinfo::mark_downloaded(&LocalFs::native(part), &from) {
                log::debug!("download: no download mark: {e}");
            }
        };
        let mut c = Copy::new(&*fs, &LocalFs, &stop, &mut on_progress, &mut on_conflict);
        c.on_written = Some(&mut mark);
        let r = c.run(v, &LocalFs::address(&target));
        match r {
            Err(FsError::Cancelled) if full.load(Ordering::Relaxed) => return Err(no_room()),
            Ok(rep) => {
                base_bytes = base_bytes.saturating_add(rep.bytes);
                base_files += rep.files;
                arrived.lock().unwrap_or_else(|e| e.into_inner()).push(target);
                if !rep.skipped.is_empty() {
                    log::info!("download: left out {} links", rep.skipped.len());
                }
                refused += rep.refused.len();
            }
            Err(FsError::Cancelled) => return Ok(false),
            Err(e) => return Err(anyhow::anyhow!("{}: {}", v.name(), crate::i18n::td(&e.to_string()))),
        }
    }
    if refused > 0 {
        return Err(anyhow::anyhow!("{} {refused}", t!("Everything else arrived. Left out, because their names cannot be used on this PC:")));
    }
    Ok(true)
}

/// Where a downloaded item named `name` goes in `dest`: refused unless the name is a usable local name and the folder the
/// result lies in is `dest` itself (also compared after resolving both on the disk).
fn top_target(dest: &Path, name: &str) -> Result<PathBuf, &'static str> {
    let t = leaf_vfs::names::local_child(dest, name)?;
    let parent = t.parent().ok_or("no folder")?;
    match (std::fs::canonicalize(dest), std::fs::canonicalize(parent)) {
        (Ok(a), Ok(b)) if a == b => Ok(t),
        _ => Err("the name leads out of the folder"),
    }
}

/// Files and bytes under `v` (links are not followed, as the copy does not follow them).
/// Walks with its own stack (no recursion) and stops with an error below `copy::MAX_DEPTH` levels, as the copy does; names the
/// copy would refuse are not followed; the sums saturate (the sizes are the server's word).
fn count(fs: &dyn Fs, v: &VPath, is_dir: bool, cancelled: &dyn Fn() -> bool, files: &mut u64, bytes: &mut u64) -> Result<(), FsError> {
    if !is_dir {
        *files = files.saturating_add(1);
        *bytes = bytes.saturating_add(fs.stat(v)?.size);
        return Ok(());
    }
    let mut todo = vec![(v.clone(), 0usize)];
    while let Some((dir, depth)) = todo.pop() {
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        if depth >= leaf_vfs::copy::MAX_DEPTH {
            return Err(leaf_vfs::copy::too_deep());
        }
        let mut children = Vec::new();
        fs.list(&dir, 1024, cancelled, &mut |b| children.extend(b))?;
        for e in children {
            if e.link {
                continue;
            }
            let Ok(child) = leaf_vfs::names::vchild(&dir, &e.name) else { continue };
            if e.is_dir() {
                todo.push((child, depth + 1));
            } else {
                *files = files.saturating_add(1);
                *bytes = bytes.saturating_add(e.size);
            }
        }
    }
    Ok(())
}

/// "Copy to...": asks for the local folder (the other pane's or Downloads to begin with), then downloads there.
pub fn copy_to_dialog(items: Vec<(VPath, bool)>, initial: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let start = initial.or_else(dirs::download_dir).map(|p| p.display().to_string()).unwrap_or_default();
    crate::prompt::show(
        t!("Copy to"),
        Some(t!("The folder the items are copied into: on this PC, or on a server as sftp://user@host/path.").to_string()),
        Some((start, t!("A folder on this PC").to_string())),
        vec![
            Button::new(t!("Copy"), Kind::Primary, move |text, _, cx| {
                let text = text.unwrap_or_default();
                // a folder on a server (typed as its address): copied there
                if let Some(Location::Remote(v)) = parse_typed(&text) {
                    if !writable(&v) {
                        return Err(t!("leaf cannot write to that place").to_string());
                    }
                    transfer(cx, items.clone(), v, false, None);
                    return Ok(());
                }
                let dir = PathBuf::from(text.trim().trim_matches('"'));
                if !dir.is_absolute() || !dir.is_dir() {
                    return Err(t!("That is not a folder on this PC").to_string());
                }
                download(cx, items.clone(), dir, None);
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

// ------------------------------------------------------------------------------------------------ changes on a server

/// Uploads of one file at least this big offer "Verify on server" when they are done.
const VERIFY_MIN: u64 = 64 << 20;

/// Whether the place of `v` can be written to (uploads, new folders, renames, deletes). `sftp://`, `ftp://` and `ftps://`
/// only: the writes of other backends (rclone) have not been reviewed, so leaf does not offer them.
pub fn writable(v: &VPath) -> bool {
    matches!(v.scheme(), "sftp" | "ftp" | "ftps") && leaf_vfs::fs_for(v.scheme()).is_ok_and(|f| f.caps().write)
}

/// A server's address as people read it in a title (`host:/path`, no user, never a secret: addresses carry none).
fn place_name(v: &VPath) -> String {
    format!("{}:{}", v.host(), v.path())
}

/// What the conflict dialog shows for an address (a local path as a path).
fn shown_path(v: &VPath) -> PathBuf {
    if v.scheme() == "file" { LocalFs::native(v) } else { PathBuf::from(v.to_string()) }
}

/// Refreshes `view` when the work is done (whatever came of it: a part may have arrived).
pub fn refresh_when_done(view: WeakEntity<crate::folder_view::FolderView>) -> crate::status_center::OnDone {
    Box::new(move |_, cx| {
        let _ = view.update(cx, |v, cx| v.refresh(cx));
    })
}

/// Local files and folders into the folder `dest` on a server (paste, a drop, Copy to other pane): a Status Center job.
pub fn upload(cx: &mut App, paths: Vec<PathBuf>, dest: VPath, after: Option<crate::status_center::OnDone>) {
    let items: Vec<(VPath, bool)> = paths.iter().map(|p| (LocalFs::address(p), std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))).collect();
    transfer(cx, items, dest, false, after);
}

/// Copies (or, on the same server, moves) `items` into the folder `dest` through `leaf_vfs::copy` as a Status Center job:
/// sizes counted first, pause and cancel, names that are there asked once in the conflict dialog. A move within one server is a
/// rename there (nothing is copied). An upload of one big file offers "Verify on server" when it is done.
pub fn transfer(cx: &mut App, items: Vec<(VPath, bool)>, dest: VPath, mv: bool, after: Option<crate::status_center::OnDone>) {
    if items.is_empty() {
        return;
    }
    let what = match items.as_slice() {
        [(v, _)] => v.name().to_string(),
        _ => crate::i18n::items(items.len()),
    };
    let verb = if mv {
        t!("Moving")
    } else if items[0].0.scheme() == "file" {
        t!("Uploading")
    } else {
        t!("Copying")
    };
    let title = format!("{verb} {what} → {}", place_name(&dest));
    let offer: Arc<Mutex<Option<(PathBuf, VPath)>>> = Arc::default();
    let check = offer.clone();
    let done: crate::status_center::OnDone = Box::new(move |ok, cx| {
        if let Some(after) = after {
            after(ok, cx);
        }
        let verify = check.lock().unwrap_or_else(|e| e.into_inner()).take();
        if ok && let Some((local, remote)) = verify {
            offer_verify(local, remote, cx);
        }
    });
    crate::status_center::start_with(cx, title, move |tr| transfer_work(&tr, &items, &dest, mv, &offer), Some(done));
}

fn transfer_work(tr: &leaf_sys::fileops::Tracker, items: &[(VPath, bool)], dest: &VPath, mv: bool, offer: &Mutex<Option<(PathBuf, VPath)>>) -> anyhow::Result<bool> {
    use leaf_sys::fastcopy::{Conflict as Asked, Resolve};
    use leaf_vfs::copy::{Conflict, Copy, Progress};
    let cancelled = || tr.cancel.load(Ordering::Relaxed);
    let from_fs = leaf_vfs::fs_for(items[0].0.scheme()).map_err(anyhow_of)?;
    let to_fs = leaf_vfs::fs_for(dest.scheme()).map_err(anyhow_of)?;
    let same_server = |v: &VPath| v.scheme() == dest.scheme() && v.authority() == dest.authority();
    // a move is offered within one server only (from anywhere else it would delete what was copied)
    if mv && !items.iter().all(|(v, _)| same_server(v)) {
        return Err(anyhow::anyhow!(t!("Moving works within one server; copy the items instead")));
    }
    // the items' own names, checked before they become part of the target (the copy checks the names inside folders)
    let mut targets = Vec::new();
    for (v, is_dir) in items {
        let t = leaf_vfs::names::vchild(dest, v.name()).map_err(|why| anyhow::anyhow!("{}: {why}", v.name()))?;
        if same_server(v) && dest.starts_with(v) && *is_dir {
            return Err(anyhow::anyhow!("{}: {}", v.name(), t!("a folder cannot go into itself")));
        }
        targets.push(t);
    }
    let (mut total_files, mut total_bytes) = (0u64, 0u64);
    if !mv {
        (tr.on_progress)(0, 0, Some(t!("Counting").to_string()));
        for (v, is_dir) in items {
            count(&*from_fs, v, *is_dir, &cancelled, &mut total_files, &mut total_bytes).map_err(anyhow_of)?;
            if cancelled() {
                return Ok(false);
            }
        }
        tr.bytes.set(0, total_bytes.max(1));
    }

    // names that are there already: asked once, for the items themselves (moving an item onto itself is no conflict)
    let mut asked = Vec::new();
    let mut at = Vec::new();
    for ((v, is_dir), target) in items.iter().zip(&targets) {
        match to_fs.stat(target) {
            Ok(m) if !(mv && target == v) => {
                let src = from_fs.stat(v).map_err(anyhow_of)?;
                at.push(asked.len());
                let time = |ms: i64| (ms > 0).then(|| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64));
                asked.push(Asked {
                    src: shown_path(v),
                    dst: shown_path(target),
                    src_dir: *is_dir,
                    dst_dir: m.is_dir(),
                    src_size: src.size,
                    dst_size: m.size,
                    src_time: time(src.modified_ms),
                    dst_time: time(m.modified_ms),
                });
            }
            Ok(_) | Err(FsError::NotFound) => at.push(usize::MAX),
            Err(e) => return Err(anyhow_of(e)),
        }
    }
    let answers: Vec<Resolve> = if asked.is_empty() {
        Vec::new()
    } else {
        match tr.ask.as_ref() {
            Some(ask) => match ask(&asked) {
                Some(a) => a,
                None => return Ok(false),
            },
            None => vec![Resolve::KeepBoth; asked.len()],
        }
    };

    let (mut base_bytes, mut base_files) = (0u64, 0u64);
    let mut left_out = 0usize;
    for (i, (v, is_dir)) in items.iter().enumerate() {
        if cancelled() {
            return Ok(false);
        }
        let answer = answers.get(at[i]).copied();
        if answer == Some(Resolve::Skip) || (mv && targets[i] == *v) {
            continue;
        }
        let target = if answer == Some(Resolve::KeepBoth) { free_name(&*to_fs, &targets[i], *is_dir).map_err(anyhow_of)? } else { targets[i].clone() };
        if mv {
            (tr.on_progress)(items.len() as u32, i as u32, Some(v.name().to_string()));
            match to_fs.rename(v, &target, answer == Some(Resolve::Replace)) {
                Ok(()) => {}
                Err(FsError::Exists) => return Err(anyhow::anyhow!("{}: {}", v.name(), t!("a folder of that name is there; it is not replaced"))),
                Err(e) => return Err(anyhow::anyhow!("{}: {}", v.name(), crate::i18n::td(&e.to_string()))),
            }
            continue;
        }
        let inner = if answer == Some(Resolve::Replace) { Conflict::Replace } else { Conflict::KeepBoth };
        let mut on_conflict = |_: &VPath, _: &leaf_vfs::Meta| inner;
        let mut on_progress = |p: &Progress| {
            while tr.paused.load(Ordering::Relaxed) && !cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            tr.bytes.set(base_bytes.saturating_add(p.bytes_done), total_bytes.max(1));
            let done = (base_files + p.files_done).min(u32::MAX as u64) as u32;
            let cur = (p.file_done == p.file_size || p.file_done <= 256 * 1024).then(|| p.file.name().to_string());
            (tr.on_progress)(total_files.max(1) as u32, done, cur);
        };
        let r = Copy::new(&*from_fs, &*to_fs, &cancelled, &mut on_progress, &mut on_conflict).run(v, &target);
        match r {
            Ok(rep) => {
                base_bytes = base_bytes.saturating_add(rep.bytes);
                base_files += rep.files;
                left_out += rep.refused.len() + rep.skipped.iter().filter(|(_, why)| *why == "link").count();
                if items.len() == 1 && !is_dir && v.scheme() == "file" && dest.scheme() == "sftp" && rep.bytes >= VERIFY_MIN {
                    *offer.lock().unwrap_or_else(|e| e.into_inner()) = Some((LocalFs::native(v), target.clone()));
                }
            }
            Err(FsError::Cancelled) => return Ok(false),
            Err(e) => return Err(anyhow::anyhow!("{}: {}", v.name(), crate::i18n::td(&e.to_string()))),
        }
    }
    if left_out > 0 {
        return Err(anyhow::anyhow!("{} {left_out}", t!("Everything else arrived. Left out (links, or names that cannot be used there):")));
    }
    Ok(true)
}

/// `name (2).ext`, `name (3).ext`... the first name not taken in the folder of `to` (a folder's name keeps its dots).
fn free_name(fs: &dyn Fs, to: &VPath, is_dir: bool) -> Result<VPath, FsError> {
    let parent = to.parent().ok_or(FsError::Denied)?;
    let name = to.name();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..10_000 {
        let cand = parent.join(&format!("{stem} ({n}){ext}"));
        match fs.stat(&cand) {
            Err(FsError::NotFound) => return Ok(cand),
            Err(e) => return Err(e),
            Ok(_) => {}
        }
    }
    Err(FsError::Exists)
}

/// After a big upload: the user may have the server compute the file's SHA-256 (`remote_sum`) and compare it with this PC's.
fn offer_verify(local: PathBuf, remote: VPath, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let Some(w) = crate::workspace::main_windows(cx).first().copied() else { return };
    let _ = w.update(cx, |_, window, cx| {
        let msg = format!("{}\n\n{}", remote.name(), t!("The upload is done. The server can compute the file's SHA-256 and leaf compares it with the file on this PC; nothing is sent again."));
        crate::prompt::show(
            t!("Verify on server?"),
            Some(msg),
            None,
            vec![
                Button::new(t!("Verify on server"), Kind::Primary, move |_, _, cx| {
                    verify(local.clone(), remote.clone(), cx);
                    Ok(())
                }),
                Button::new(t!("Not now"), Kind::Normal, |_, _, _| Ok(())),
            ],
            window,
            cx,
        );
    });
}

fn verify(local: PathBuf, remote: VPath, cx: &mut App) {
    let title = format!("{} {}", t!("Verifying on the server:"), remote.name());
    crate::status_center::start(cx, title, move |tr| {
        use sha2::Digest as _;
        use std::io::Read as _;
        let cancelled = || tr.cancel.load(Ordering::Relaxed);
        (tr.on_progress)(0, 0, Some(t!("The server computes its sum").to_string()));
        let theirs = leaf_remote::remote_sum(&remote, leaf_remote::checksum::Algo::Sha256, &cancelled);
        let mut f = std::fs::File::open(&local)?;
        let total = f.metadata()?.len();
        let mut h = sha2::Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            if cancelled() {
                return Ok(false);
            }
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            done += n as u64;
            tr.bytes.set(done, total.max(1));
        }
        let ours: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let theirs = match theirs {
            Ok(t) => t,
            Err(FsError::Cancelled) => return Ok(false),
            Err(e) => return Err(anyhow_of(e)),
        };
        if theirs.eq_ignore_ascii_case(&ours) {
            log::info!("remote: {} verified on the server", remote.name());
            Ok(true)
        } else {
            Err(anyhow::anyhow!(t!("The copy on the server is NOT the same as the file on this PC (SHA-256 differs). Upload it again.")))
        }
    });
}

/// A new folder (or an empty file) in `dir` on a server: its name asked, then made; a name that is taken is said plainly,
/// nothing is replaced.
pub fn new_item(dir: VPath, folder: bool, view: WeakEntity<crate::folder_view::FolderView>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let (title, start) = if folder { (t!("New folder"), t!("New folder").to_string()) } else { (t!("New text document"), format!("{}.txt", t!("New text document"))) };
    crate::prompt::show(
        title,
        Some(format!("{} {}", t!("In"), place_name(&dir))),
        Some((start, t!("Name").to_string())),
        vec![
            Button::new(t!("Create"), Kind::Primary, move |text, _, cx| {
                let name = text.unwrap_or_default().trim().to_string();
                let to = leaf_vfs::names::vchild(&dir, &name).map_err(|why| format!("{} ({why})", t!("This name cannot be used")))?;
                let title = format!("{} {name}", t!("Creating"));
                let (view, done_name) = (view.clone(), name.clone());
                let done: crate::status_center::OnDone = Box::new(move |_, cx| {
                    let _ = view.update(cx, |v, cx| {
                        v.refresh(cx);
                        v.select_when_listed(vec![done_name], cx);
                    });
                });
                crate::status_center::start_with(
                    cx,
                    title,
                    move |_| {
                        let fs = leaf_vfs::fs_for(to.scheme()).map_err(anyhow_of)?;
                        let made = if folder { fs.mkdir(&to) } else { fs.open_write(&to, leaf_vfs::WriteOpts::CREATE_NEW).and_then(|w| w.finish()) };
                        match made {
                            Ok(()) => Ok(true),
                            Err(FsError::Exists) => Err(anyhow::anyhow!(t!("An item with this name already exists. Try another name."))),
                            Err(e) => Err(anyhow_of(e)),
                        }
                    },
                    Some(done),
                );
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

/// Renames `from` to `name` in the same folder on a server (never over something that is there).
pub fn rename(cx: &mut App, from: VPath, name: String, view: WeakEntity<crate::folder_view::FolderView>) {
    let Ok(to) = leaf_vfs::names::vchild(&from.parent().unwrap_or_else(|| from.root()), &name) else {
        crate::status_center::start(cx, format!("{} {}", t!("Rename"), from.name()), |_| Err(anyhow::anyhow!(t!("This name cannot be used"))));
        return;
    };
    let title = format!("{} {} → {name}", t!("Renaming"), from.name());
    crate::status_center::start_with(
        cx,
        title,
        move |_| {
            let fs = leaf_vfs::fs_for(from.scheme()).map_err(anyhow_of)?;
            match fs.rename(&from, &to, false) {
                Ok(()) => Ok(true),
                Err(FsError::Exists) => Err(anyhow::anyhow!(t!("An item with this name already exists. Try another name."))),
                Err(e) => Err(anyhow_of(e)),
            }
        },
        Some(refresh_when_done(view)),
    );
}

/// Deletes items on a server, after leaf's usual question (Settings → "Confirm before deleting", as a permanent delete): a
/// server has no Trash, so the dialog says the delete cannot be undone. Folders go with everything in them; a link is
/// unlinked, what it points to is left alone.
pub fn delete(items: Vec<(VPath, bool)>, view: WeakEntity<crate::folder_view::FolderView>, window: &mut Window, cx: &mut App) {
    use crate::settings::DeletePref;
    if items.is_empty() {
        return;
    }
    let ask = !matches!(cx.global::<crate::settings::Settings>().confirm_delete, DeletePref::Never);
    if !ask {
        delete_now(items, view, cx);
        return;
    }
    let what = match items.as_slice() {
        [(v, _)] => format!("\"{}\"", v.name()),
        _ => crate::i18n::items(items.len()),
    };
    let msg = format!(
        "{what} {} {}\n\n{}",
        t!("will be deleted on"),
        items[0].0.host(),
        t!("A server has no Trash: this cannot be undone. Folders are deleted with everything in them.")
    );
    crate::prompt::show(
        t!("Delete permanently?"),
        Some(msg),
        None,
        vec![
            crate::prompt::Button::new(t!("Delete"), crate::prompt::Kind::Danger, move |_, _, cx| {
                delete_now(items.clone(), view.clone(), cx);
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

fn delete_now(items: Vec<(VPath, bool)>, view: WeakEntity<crate::folder_view::FolderView>, cx: &mut App) {
    let what = match items.as_slice() {
        [(v, _)] => v.name().to_string(),
        _ => crate::i18n::items(items.len()),
    };
    let title = format!("{} {what} ({})", t!("Deleting"), items[0].0.host());
    crate::status_center::start_with(
        cx,
        title,
        move |tr| {
            let fs = leaf_vfs::fs_for(items[0].0.scheme()).map_err(anyhow_of)?;
            for (i, (v, _)) in items.iter().enumerate() {
                if tr.cancel.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                (tr.on_progress)(items.len() as u32, i as u32, Some(v.name().to_string()));
                match fs.remove(v, true) {
                    Ok(()) | Err(FsError::NotFound) => {}
                    Err(e) => return Err(anyhow::anyhow!("{}: {}", v.name(), crate::i18n::td(&e.to_string()))),
                }
            }
            Ok(true)
        },
        Some(refresh_when_done(view)),
    );
}

// ------------------------------------------------------------------------------------------------ editing a file on a server

/// A file on a server open for editing: its cache copy is watched, every save goes back (H10).
struct Editing {
    remote: VPath,
    /// What the server had when it was fetched or last saved (shared with the save that runs).
    fetched: Arc<Mutex<leaf_remote::edit::Fetched>>,
    /// The local copy's (size, time) last sent or fetched: the watcher sends nothing for a change that is not one.
    stamp: Arc<Mutex<Option<(u64, std::time::SystemTime)>>>,
    /// One save at a time per file.
    saving: Arc<Mutex<()>>,
    _watch: leaf_sys::watch::DirWatch,
}

static EDITS: Mutex<Vec<(PathBuf, Editing)>> = Mutex::new(Vec::new());
static EDIT_TX: Mutex<Option<mpsc::UnboundedSender<PathBuf>>> = Mutex::new(None);

/// The editing copy of a server's file: `<cache>/edit/<hash>/<name>`, writable (the open-only copies are read-only).
fn edit_path(v: &VPath) -> PathBuf {
    let open = cache_path(v);
    let hash = open.parent().and_then(|p| p.file_name()).map(|n| n.to_os_string()).unwrap_or_default();
    cache_root().join("edit").join(hash).join(leaf_vfs::names::safe_local_name(v.name()))
}

fn local_stamp(p: &Path) -> Option<(u64, std::time::SystemTime)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Starts the loop that turns the watchers' news into saves (on the UI thread), once.
fn edit_channel(cx: &mut App) -> mpsc::UnboundedSender<PathBuf> {
    let mut g = EDIT_TX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = g.as_ref() {
        return tx.clone();
    }
    let (tx, mut rx) = mpsc::unbounded::<PathBuf>();
    cx.spawn(async move |cx| {
        while let Some(local) = rx.next().await {
            cx.update(|cx| save_edit(local, false, cx));
        }
    })
    .detach();
    *g = Some(tx.clone());
    tx
}

/// "Edit" on a file on a server: fetched into the cache (as a writable copy), opened in the program files of its type open
/// with, and watched: each save there is uploaded, unless the server's file changed meanwhile.
pub fn edit_file(cx: &mut App, v: VPath, size: u64) {
    let name = v.name().to_string();
    if size > OPEN_MAX {
        crate::status_center::start(cx, format!("{} {name}", t!("Editing from the server:")), |_| {
            Err(anyhow::anyhow!(t!("This file is too big to edit straight from the server (over 512 MB).")))
        });
        return;
    }
    let local = edit_path(&v);
    // already open for editing: the same copy again (what was saved is on the server already)
    let open_already = EDITS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(l, e)| *l == local && e.remote == v);
    if open_already {
        if let Err(e) = leaf_sys::openrules::open(&local) {
            log::warn!("edit {}: {e}", local.display());
        }
        return;
    }
    let tx = edit_channel(cx);
    let aside: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let told = aside.clone();
    let done: crate::status_center::OnDone = Box::new(move |_, cx| {
        let Some(p) = told.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if let Some(w) = crate::workspace::main_windows(cx).first().copied() {
            let msg = format!("{}

{}", t!("An editing copy from before was never sent to the server. It is kept here:"), p.display());
            let _ = w.update(cx, |_, window, cx| crate::prompt::notice(t!("Unsent edit kept"), msg, window, cx));
        }
    });
    crate::status_center::start_with(cx, format!("{} {name}", t!("Editing from the server:")), move |tr| {
        let cancel = tr.cancel.clone();
        let mut progress = |d: u64, total: u64| {
            tr.bytes.set(d, total);
            (tr.on_progress)(total.max(1) as u32, d.min(u32::MAX as u64) as u32, None);
        };
        let fs = leaf_vfs::fs_for(v.scheme()).map_err(anyhow_of)?;
        make_cache_dir(&local).map_err(anyhow_of)?;
        // a copy left from an earlier run: the server's version as it is, or an unsent edit that is moved aside first (never
        // overwritten)
        let server = fs.stat(&v).map_err(anyhow_of)?;
        let leftover = leaf_remote::edit::set_aside_unsent(&local, &server)?;
        let meta = if leftover == leaf_remote::edit::Leftover::Same {
            server
        } else {
            if let leaf_remote::edit::Leftover::SetAside(p) = leftover {
                log::info!("remote: an unsent editing copy was moved aside");
                *aside.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            }
            match leaf_vfs::copy::fetch_file(&*fs, &v, &local, OPEN_MAX, None, &|| cancel.load(Ordering::Relaxed), &mut progress) {
                Ok(m) => m,
                Err(FsError::Cancelled) => return Ok(false),
                Err(e) => return Err(anyhow_of(e)),
            }
        };
        let dir = local.parent().map(Path::to_path_buf).ok_or_else(|| anyhow::anyhow!("no folder"))?;
        let stamp = Arc::new(Mutex::new(local_stamp(&local)));
        let watch = {
            let (stamp, local, tx) = (stamp.clone(), local.clone(), tx.clone());
            let file = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            leaf_sys::watch::watch_dir_changes(&dir, std::time::Duration::from_millis(500), move |c| {
                if !c.rescan && !c.names.iter().any(|n| *n == file) {
                    return;
                }
                // (editors save in steps; only a copy that is there and differs from the last one counts)
                let now = local_stamp(&local);
                if now.is_some() && now != *stamp.lock().unwrap_or_else(|e| e.into_inner()) {
                    let _ = tx.unbounded_send(local.clone());
                }
            })?
        };
        let e = Editing { remote: v.clone(), fetched: Arc::new(Mutex::new(leaf_remote::edit::Fetched::of(&meta))), stamp, saving: Arc::default(), _watch: watch };
        {
            let mut edits = EDITS.lock().unwrap_or_else(|e| e.into_inner());
            edits.retain(|(l, _)| *l != local);
            edits.push((local.clone(), e));
        }
        if let Err(e) = leaf_sys::openrules::open(&local) {
            log::warn!("edit {}: {e}", local.display());
        }
        Ok(true)
    }, Some(done));
}

/// The editing copy `local` was saved: uploaded over the server's file (the old version kept as `name.leaf-bak`), or, when
/// the server's file changed since it was fetched, refused with the choice of saving it as a copy on the server. Sent only
/// once the copy has stopped changing (an editor saves in steps); an empty copy over a non-empty file is asked about first.
fn save_edit(local: PathBuf, allow_empty: bool, cx: &mut App) {
    #[derive(Clone, Copy, PartialEq)]
    enum Then {
        Nothing,
        OfferCopy,
        AskEmpty,
    }
    let found = EDITS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(l, _)| *l == local).map(|(_, e)| (e.remote.clone(), e.fetched.clone(), e.stamp.clone(), e.saving.clone()));
    let Some((remote, fetched, stamp, saving)) = found else { return };
    let then: Arc<Mutex<Then>> = Arc::new(Mutex::new(Then::Nothing));
    let next = then.clone();
    let (r2, l2) = (remote.clone(), local.clone());
    let done: crate::status_center::OnDone = Box::new(move |_, cx| match *next.lock().unwrap_or_else(|e| e.into_inner()) {
        Then::OfferCopy => offer_copy(r2, l2, cx),
        Then::AskEmpty => ask_empty(r2, l2, cx),
        Then::Nothing => {}
    });
    let again = EDIT_TX.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let title = format!("{} {}", t!("Saving to the server:"), remote.name());
    crate::status_center::start_with(
        cx,
        title,
        move |tr| {
            let _one = saving.lock().unwrap_or_else(|e| e.into_inner());
            let cancelled = || tr.cancel.load(Ordering::Relaxed);
            // the copy must hold still for 300 ms (at most ~10 s of waiting; a later change comes as its own save)
            let mut now = local_stamp(&local);
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if cancelled() {
                    return Ok(false);
                }
                let later = local_stamp(&local);
                if later == now {
                    break;
                }
                now = later;
            }
            if now.is_none() || now == *stamp.lock().unwrap_or_else(|e| e.into_inner()) {
                // (gone for now, or a save that came in while the one before ran: already sent)
                return Ok(true);
            }
            let before = *fetched.lock().unwrap_or_else(|e| e.into_inner());
            match leaf_remote::edit::save_back(&remote, &local, &before, allow_empty, &cancelled) {
                Ok(after) => {
                    *fetched.lock().unwrap_or_else(|e| e.into_inner()) = after;
                    *stamp.lock().unwrap_or_else(|e| e.into_inner()) = now;
                    // changed while it was being sent: the newer version goes too
                    if local_stamp(&local) != now
                        && let Some(tx) = again.as_ref()
                    {
                        let _ = tx.unbounded_send(local.clone());
                    }
                    Ok(true)
                }
                Err(leaf_remote::edit::SaveError::Changed(_)) => {
                    *then.lock().unwrap_or_else(|e| e.into_inner()) = Then::OfferCopy;
                    Err(anyhow::anyhow!(t!("The file on the server changed since you opened it. Your version was not uploaded; nothing there was overwritten.")))
                }
                Err(leaf_remote::edit::SaveError::WouldEmpty) => {
                    *then.lock().unwrap_or_else(|e| e.into_inner()) = Then::AskEmpty;
                    Err(anyhow::anyhow!(t!("The edited copy is empty; it was not uploaded over the file on the server.")))
                }
                Err(leaf_remote::edit::SaveError::Fs(FsError::Cancelled)) => Ok(false),
                Err(leaf_remote::edit::SaveError::Fs(e)) => Err(anyhow_of(e)),
            }
        },
        Some(done),
    );
}

/// The edited copy is empty and the server's file is not: uploaded only when the user says so.
fn ask_empty(remote: VPath, local: PathBuf, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let Some(w) = crate::workspace::main_windows(cx).first().copied() else { return };
    let _ = w.update(cx, |_, window, cx| {
        let msg = format!("{}\n\n{}", remote.name(), t!("The edited copy is now empty. Upload it, emptying the file on the server? (The server's version is kept as the .leaf-bak backup.)"));
        crate::prompt::show(
            t!("Upload an empty file?"),
            Some(msg),
            None,
            vec![
                Button::new(t!("Upload empty file"), Kind::Danger, move |_, _, cx| {
                    save_edit(local.clone(), true, cx);
                    Ok(())
                }),
                Button::cancel(),
            ],
            window,
            cx,
        );
    });
}

/// The server's file changed under an edit: say so, and offer to put this version beside it under a new name.
fn offer_copy(remote: VPath, local: PathBuf, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let Some(w) = crate::workspace::main_windows(cx).first().copied() else { return };
    let _ = w.update(cx, |_, window, cx| {
        let msg = format!(
            "{}\n\n{}",
            remote.name(),
            t!("Someone or something changed this file on the server after you opened it. leaf did not upload your version, so their change is kept. You can save yours next to it under a new name.")
        );
        crate::prompt::show(
            t!("The file changed on the server"),
            Some(msg),
            None,
            vec![
                Button::new(t!("Save as copy on server"), Kind::Primary, move |_, _, cx| {
                    let (remote, local) = (remote.clone(), local.clone());
                    crate::status_center::start(cx, format!("{} {}", t!("Saving a copy on the server:"), remote.name()), move |tr| {
                        match leaf_remote::edit::save_as_copy(&remote, &local, &|| tr.cancel.load(Ordering::Relaxed)) {
                            Ok(to) => {
                                log::info!("remote: the edit was saved as {}", to.name());
                                Ok(true)
                            }
                            Err(FsError::Cancelled) => Ok(false),
                            Err(e) => Err(anyhow_of(e)),
                        }
                    });
                    Ok(())
                }),
                Button::cancel(),
            ],
            window,
            cx,
        );
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use rf_core::Location;

    use super::upgrade_address_path;

    #[test]
    fn old_ftp_path_with_a_password_upgrades_without_it() {
        leaf_remote::register(leaf_remote::Config::for_user(None), Arc::new(leaf_remote::NoPrompter));
        let old = Location::Path(PathBuf::from("ftp://alice:s3cret@example.com/pub/x"));
        let Location::Remote(v) = upgrade_address_path(old) else { panic!("not upgraded") };
        assert_eq!(v.to_string(), "ftp://alice@example.com/pub/x");
        assert!(!format!("{v:?}").contains("s3cret"));
        // a path of this PC stays as it is
        let local = Location::Path(PathBuf::from(r"C:\Users"));
        assert_eq!(upgrade_address_path(local.clone()), local);
    }
}
