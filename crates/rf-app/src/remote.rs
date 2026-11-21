//! Places that are not local folders (`Location::Remote`), listed through `leaf-vfs` on a worker thread. Local folders never
//! come here: they keep `Location::Path` and the direct listing.
//!
//! - `sftp://` (W1a, read-only in this lane): `leaf-remote` serves it. Connecting asks through dialogs here (an unknown host
//!   key, a key's passphrase, a password); a changed host key is refused with a warning in the pane and no way past it.
//! - Opening or looking at a file fetches a size-capped copy into a cache folder first (as archives do), then opens that.
//! - Copy, Copy to other pane, Copy to..., a drag onto a local pane and Paste in a local folder download through
//!   `leaf_vfs::copy` as a Status Center job (progress, pause, cancel, names that exist asked; a cut-off file is removed).
//! - Uploading, renaming and deleting on a server are not built yet ("coming next").
//! - `mem://demo/` (the in-memory demo tree; `mem://offline/` and `mem://locked/` show the error banner) in debug builds, or in
//!   any build with `LEAF_VFS_DEMO=1`. Schemes without a backend (`ftp://` is still the old WinINet path) stay paths as before.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use leaf_remote::{HostKeyAnswer, HostKeyQuestion, Prompter, Secret, SecretQuestion};
use leaf_vfs::{Fs, FsError, LocalFs};
use rf_core::Location;
use rf_core::vpath::VPath;

use crate::t;

/// Marks a listing error that is the server's (offline, login refused): the pane shows it quietly with a Retry button.
const RETRY_MARK: &str = "\u{1}retry\u{1}";

/// Why moving, pasting onto, renaming and deleting are off in a folder on a server (the menus show it).
pub const NOT_WIRED: &str = "Uploading, renaming and deleting on a server: coming next";

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

/// Text typed into the address bar (or handed over at start) that names a remote place.
pub fn parse_typed(text: &str) -> Option<Location> {
    let t = text.trim().trim_matches('"');
    if !VPath::looks_like(t) {
        return None;
    }
    let v = VPath::parse(t).ok()?;
    usable(v.scheme()).then_some(Location::Remote(v))
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
}

impl Ask {
    fn cancel(self) {
        match self {
            Ask::HostKey(_, r) => r(HostKeyAnswer::Cancel),
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
}

/// Serves `sftp://` (cheap: nothing connects and no thread starts until the first remote place is opened). Before the
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
            let mut msg = format!(
                "{}\n\n{} {}\n{} {}",
                t!("leaf has not seen this server's key before. Check that it is the key the server's owner gave you before you trust it."),
                t!("Key type:"),
                q.key_type,
                t!("Fingerprint:"),
                q.fingerprint
            );
            if !q.other_types.is_empty() {
                msg.push_str(&format!("\n\n{} {}", t!("known_hosts has a key of another type for this server:"), q.other_types.join(", ")));
            }
            crate::prompt::show(
                format!("{} {}", t!("Unknown server:"), q.host),
                Some(msg),
                None,
                vec![
                    Button::new(t!("Trust and remember"), Kind::Primary, answer(HostKeyAnswer::TrustAndRemember)),
                    Button::new(t!("Trust once"), Kind::Normal, answer(HostKeyAnswer::TrustOnce)),
                    Button::new(t!("Cancel"), Kind::Normal, answer(HostKeyAnswer::Cancel)),
                ],
                window,
                cx,
            );
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
                SecretQuestion::Password { account, retry } => {
                    let first = if retry { t!("Wrong password. Try again.") } else { t!("The server asks for a password.") };
                    crate::prompt::show_secret(format!("{} {account}", t!("Password for")), Some(format!("{first}\n\n{kept}")), t!("Password").to_string(), buttons, window, cx);
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

/// The folder fetched copies live in (per server and path, so the same file is fetched once).
fn cache_path(v: &VPath) -> PathBuf {
    let safe = |s: &str| s.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ' ') { c } else { '_' }).collect::<String>();
    let mut p = std::env::temp_dir().join("leaf-remote").join(safe(&format!("{}-{}", v.scheme(), v.authority())));
    for seg in v.segments() {
        p.push(safe(seg));
    }
    p
}

/// A copy of the server's file in the cache folder, fetched unless a copy of the same size and time is there. Read-only on
/// purpose: changes to it would not go back to the server (editing on a server comes later).
fn fetch(v: &VPath, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, FsError> {
    let fs = leaf_vfs::fs_for(v.scheme())?;
    let m = fs.stat(v)?;
    let local = cache_path(v);
    if let Ok(lm) = std::fs::metadata(&local) {
        let same_time = lm.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).is_some_and(|d| d.as_millis() as i64 / 1000 == m.modified_ms / 1000);
        if lm.len() == m.size && same_time {
            return Ok(local);
        }
        let mut perm = lm.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(&local, perm);
    }
    let dir = local.parent().ok_or(FsError::Denied)?;
    std::fs::create_dir_all(dir)?;
    let size = m.size;
    let mut on_progress = |p: &leaf_vfs::copy::Progress| progress(p.bytes_done, size);
    let mut conflict = |_: &VPath, _: &leaf_vfs::Meta| leaf_vfs::copy::Conflict::Replace;
    leaf_vfs::copy::Copy::new(&*fs, &LocalFs, cancelled, &mut on_progress, &mut conflict).run(v, &LocalFs::address(&local))?;
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
        let local = match fetch(&v, &|| cancel.load(Ordering::Relaxed), &mut progress) {
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
    std::thread::spawn(move || match fetch(&v, &|| false, &mut |_, _| {}) {
        Ok(local) => crate::look::ipc::send(crate::look::ipc::Msg::Toggle(local), true),
        Err(e) => log::warn!("remote: could not fetch {v} for leaf-look: {e}"),
    });
}

// ------------------------------------------------------------------------------------------------ downloads

/// What Copy put on the clipboard from a server: the addresses (also on the clipboard as text, so a later copy of anything
/// else replaces it as the clipboard's content).
static CLIP: Mutex<Option<(String, Vec<(VPath, bool)>)>> = Mutex::new(None);

fn clip_text(items: &[(VPath, bool)]) -> String {
    items.iter().map(|(v, _)| v.to_string()).collect::<Vec<_>>().join("\n")
}

/// Copy in a folder on a server: remembered for Paste in a local folder.
pub fn copy_to_clipboard(items: Vec<(VPath, bool)>, cx: &mut App) {
    if items.is_empty() {
        return;
    }
    let text = clip_text(&items);
    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    *CLIP.lock().unwrap_or_else(|e| e.into_inner()) = Some((text, items));
}

/// The server items the clipboard holds (Copy in a remote folder, and nothing copied since).
pub fn clipboard_items(cx: &App) -> Option<Vec<(VPath, bool)>> {
    let (text, items) = CLIP.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    // (the clipboard is read only after a Copy on a server: a normal paste does not wait on it here)
    let now = cx.read_from_clipboard().and_then(|c| c.text())?;
    (now == text).then_some(items)
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

    // names that are here already: asked once, for the items themselves
    let mut asked = Vec::new();
    let mut at = Vec::new();
    for (v, is_dir) in items {
        let target = dest.join(v.name());
        if let Ok(m) = std::fs::symlink_metadata(&target) {
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
        let mut target = dest.join(v.name());
        if answer == Some(Resolve::KeepBoth) {
            let name = v.name();
            let (stem, ext) = match name.rfind('.') {
                Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
                _ => (name, ""),
            };
            target = leaf_sys::fileops::unique_name(dest, stem, ext);
        }
        let inner = if answer == Some(Resolve::Replace) { Conflict::Replace } else { Conflict::KeepBoth };
        let mut on_conflict = |_: &VPath, _: &leaf_vfs::Meta| inner;
        let mut on_progress = |p: &Progress| {
            while tr.paused.load(Ordering::Relaxed) && !cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            tr.bytes.set(base_bytes + p.bytes_done, total_bytes.max(1));
            let done = (base_files + p.files_done).min(u32::MAX as u64) as u32;
            let cur = (p.file_done == p.file_size || p.file_done <= 256 * 1024).then(|| p.file.name().to_string());
            (tr.on_progress)(total_files.max(1) as u32, done, cur);
        };
        let r = Copy::new(&*fs, &LocalFs, &cancelled, &mut on_progress, &mut on_conflict).run(v, &LocalFs::address(&target));
        match r {
            Ok(rep) => {
                base_bytes += rep.bytes;
                base_files += rep.files;
                arrived.lock().unwrap_or_else(|e| e.into_inner()).push(target);
                if !rep.skipped.is_empty() {
                    log::info!("download: left out {} links", rep.skipped.len());
                }
            }
            Err(FsError::Cancelled) => return Ok(false),
            Err(e) => return Err(anyhow::anyhow!("{}: {}", v.name(), crate::i18n::td(&e.to_string()))),
        }
    }
    Ok(true)
}

/// Files and bytes under `v` (links are not followed, as the copy does not follow them).
fn count(fs: &dyn Fs, v: &VPath, is_dir: bool, cancelled: &dyn Fn() -> bool, files: &mut u64, bytes: &mut u64) -> Result<(), FsError> {
    if !is_dir {
        *files += 1;
        *bytes += fs.stat(v)?.size;
        return Ok(());
    }
    let mut children = Vec::new();
    fs.list(v, 1024, cancelled, &mut |b| children.extend(b))?;
    for e in children {
        if e.link {
            continue;
        }
        if e.is_dir() {
            count(fs, &v.join(&e.name), true, cancelled, files, bytes)?;
        } else {
            *files += 1;
            *bytes += e.size;
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
        Some(t!("The folder on this PC the items are copied into.").to_string()),
        Some((start, t!("A folder on this PC").to_string())),
        vec![
            Button::new(t!("Copy"), Kind::Primary, move |text, _, cx| {
                let dir = PathBuf::from(text.unwrap_or_default().trim().trim_matches('"'));
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
