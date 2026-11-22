//! "Retry as administrator" (PLAN W3): an operation that Windows (or Linux) refused with "access denied" can be done again by
//! leaf's administrator helper, after one permission prompt.
//!
//! Nothing here is elevated. The helper is started only from a click on "Retry as administrator" (the prompt comes then); while
//! it lives (until "End now" in the Status Center, leaf's end, or five idle minutes) later retries use it without a new prompt,
//! but each one still needs its own click. What the helper may do is the fixed list of `leaf_sys::admin::Request`s.
//!
//! The work runs as normal Status Center jobs: progress, cancel, the conflict question and the undo list are the same as for an
//! operation leaf does itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use leaf_sys::admin::{Calls, Choice, Clash, HelperError, Reply, Request, Session, StartError};
use leaf_sys::fileops::{Tracker, Transfer};

use crate::t;

static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);

/// A helper is being started (the permission prompt is up); the lock above is never held while it is.
static STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Folders the user chose to list as administrator: (folder, the next listing may start the helper, the helper's process id
/// that listed it). A listing belongs to the helper the click started: another helper later does not take it over.
static LISTED: Mutex<Vec<(PathBuf, bool, Option<u32>)>> = Mutex::new(Vec::new());

/// The helper is running (the Status Center shows it with "End now").
pub fn active() -> bool {
    current().is_some()
}

fn current() -> Option<Arc<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|s| s.alive()).cloned()
}

/// Why this install cannot do things as administrator (a per-user install, the helper missing), in leaf's words; `None`: it can.
/// Looked at once.
pub fn blocked() -> Option<String> {
    static WHY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    WHY.get_or_init(|| {
        leaf_sys::admin::blocked().map(|e| match e {
            StartError::Unsafe => t!("leaf is installed for this user only, in a folder this user can change. A program started from there as administrator could be swapped by any other program, so leaf does not do that. Install leaf for all users to use this.").to_string(),
            _ => t!("The administrator helper is not installed with leaf.").to_string(),
        })
    })
    .clone()
}

/// The program can ask for administrator rights at all: the helper is shipped beside it, and this is leaf itself (the helper
/// talks only to `leaf`, not to the Preview program).
pub fn available() -> bool {
    let is_leaf = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().eq_ignore_ascii_case(leaf_sys::APP_EXE_STEM))).unwrap_or(false);
    is_leaf && leaf_sys::admin::helper_exe().is_some()
}

/// The system refused with "access denied" (and a retry as administrator can be offered).
pub fn denied(e: &anyhow::Error) -> bool {
    available() && leaf_sys::admin::is_denied(e)
}

pub fn denied_io(e: &std::io::Error) -> bool {
    available() && leaf_sys::admin::is_denied_io(e)
}

/// The running helper; `may_start`: start one (the permission prompt) when none runs. Blocking: never on the UI thread.
fn session(may_start: bool) -> anyhow::Result<Arc<Session>> {
    if let Some(s) = current() {
        return Ok(s);
    }
    if !may_start {
        anyhow::bail!("{}", t!("The administrator helper is no longer running"));
    }
    if STARTING.swap(true, Ordering::AcqRel) {
        anyhow::bail!("{}", t!("The administrator helper is being started. Try again once it runs."));
    }
    // (started without holding the lock: the UI thread asks `active()` while the prompt is up)
    let started = leaf_sys::admin::start();
    let r = match started {
        Ok(s) => {
            let s = Arc::new(s);
            *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
            Ok(s)
        }
        Err(e) => Err(match e {
            StartError::Declined => anyhow::anyhow!("{}", t!("Administrator permission was not given. Nothing was changed.")),
            StartError::Missing | StartError::Unsafe => anyhow::anyhow!("{}", blocked().unwrap_or_default()),
            StartError::Failed(why) => anyhow::anyhow!("{}\n{why}", t!("The administrator helper could not be started.")),
        }),
    };
    STARTING.store(false, Ordering::Release);
    r
}

/// "End now" in the Status Center.
pub fn end_now(cx: &mut App) {
    let s = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    LISTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    if let Some(s) = s {
        std::thread::spawn(move || s.end());
    }
    if let Some(ops) = cx.try_global::<crate::status_center::OpsGlobal>().map(|g| g.0.clone()) {
        ops.update(cx, |_, cx| cx.notify());
    }
}

/// leaf's words for what the helper said (its own or the system's text goes on a second line).
fn words(e: HelperError) -> anyhow::Error {
    match e {
        HelperError::Refused(why) => anyhow::anyhow!("{}\n{why:?}", t!("The administrator helper refused this: the path is not a plain local path, or leads somewhere else than it says.")),
        HelperError::Denied(d) => anyhow::anyhow!("{}\n{d}", t!("Access was denied even to the administrator.")),
        HelperError::Failed(d) => anyhow::anyhow!("{}\n{d}", t!("The operation failed as administrator.")),
        HelperError::Gone(_) | HelperError::Confused => anyhow::anyhow!("{}", t!("The administrator helper is no longer running")),
    }
}

/// One request through the helper with a Status Center tracker: its progress, cancel and conflict question.
fn call(req: Request, t: &Tracker, may_start: bool, data: &mut Vec<u8>) -> anyhow::Result<Reply> {
    let s = session(may_start)?;
    let progress = |total: u32, done: u32, cur: Option<String>, bd: u64, bt: u64| {
        t.bytes.set(bd, bt);
        (t.on_progress)(total, done, cur);
    };
    let ask = |clashes: &[Clash]| -> Option<Vec<Choice>> {
        let list: Vec<leaf_sys::fastcopy::Conflict> = clashes.iter().map(Into::into).collect();
        match &t.ask {
            Some(a) => a(&list).map(|v| v.into_iter().map(Choice::from).collect()),
            None => Some(vec![Choice::KeepBoth; list.len()]),
        }
    };
    let calls = Calls { progress: &progress, ask: &ask, cancel: &t.cancel };
    s.call(&req, &calls, data).map_err(words)
}

fn quiet() -> Tracker {
    Tracker::new(Box::new(|_, _, _| {}), Default::default())
}

/// What a finished operation said beyond "done": items already gone, move sources that stayed, copies without everything.
#[derive(Default)]
struct Left {
    missing: u32,
    kept: u32,
    partial: u32,
}

impl Left {
    /// leaf's words for it (`None`: nothing to say).
    fn text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.missing > 0 {
            parts.push(format!("{} {}", t!("no longer at their old place, skipped:"), self.missing));
        }
        if self.kept > 0 {
            parts.push(format!("{} {}", t!("not moved and kept where they are (in use by another program, changed during the copy, or with data a copy would lose):"), self.kept));
        }
        if self.partial > 0 {
            parts.push(format!("{} {}", t!("copied without their extra data streams or encryption:"), self.partial));
        }
        (!parts.is_empty()).then(|| format!("{} {}", t!("Done, but items"), parts.join("; ")))
    }
}

/// (ok, undo pairs, what was left)
fn done(r: Reply) -> anyhow::Result<(bool, Vec<(PathBuf, PathBuf)>, Left)> {
    match r {
        Reply::Done { ok, pairs, missing, kept, partial } => Ok((ok, pairs, Left { missing, kept, partial })),
        _ => Err(words(HelperError::Confused)),
    }
}

/// The dialog that offers it: `title`, what happened, and the button. Nothing happens elevated until it is clicked.
pub fn offer(title: String, what: String, retry: impl Fn(&mut App) + 'static, window: &mut Window, cx: &mut App) {
    if let Some(why) = blocked() {
        return crate::prompt::notice(title, format!("{what}\n\n{why}"), window, cx);
    }
    let more = if active() {
        t!("leaf's administrator helper is already running: this is done by it, without a new prompt.")
    } else if cfg!(windows) {
        t!("Windows will ask for permission. Then leaf's administrator helper does this operation; it ends after five minutes without work, or with \"End now\" in the Status Center.")
    } else {
        t!("The system will ask for the administrator's password. Then leaf's administrator helper does this operation; it ends after five minutes without work, or with \"End now\" in the Status Center.")
    };
    crate::prompt::show(
        title,
        Some(format!("{what}\n\n{more}")),
        None,
        vec![
            crate::prompt::Button::new(t!("Retry as administrator"), crate::prompt::Kind::Primary, move |_, _, cx| {
                retry(cx);
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

/// Shows the helper's start in the Status Center as soon as it is there.
fn notify_status(cx: &mut App) {
    if let Some(ops) = cx.try_global::<crate::status_center::OpsGlobal>().map(|g| g.0.clone()) {
        ops.update(cx, |_, cx| cx.notify());
    }
}

/// A copy or move again as administrator. `record`: it goes on the undo list (not for undo / redo replays).
pub fn retry_transfer(cx: &mut App, kind: Transfer, sources: Vec<PathBuf>, dest: PathBuf, record: bool) {
    let verb = match kind {
        Transfer::Copy => t!("Copying as administrator"),
        Transfer::Move => t!("Moving as administrator"),
    };
    let title = format!("{verb}: {} → {}", crate::i18n::items(sources.len()), rf_core::history::display_name(&dest));
    crate::status_center::start_with(
        cx,
        title,
        move |t| {
            let req = match kind {
                Transfer::Copy => Request::Copy { sources, dest },
                Transfer::Move => Request::Move { sources, dest },
            };
            let (ok, pairs, left) = done(call(req, &t, true, &mut Vec::new())?)?;
            if record && !pairs.is_empty() {
                crate::undo::push(crate::undo::UndoOp::Transferred { kind, pairs: pairs.clone() });
            }
            *t.results.lock().unwrap() = pairs;
            // (an earlier try that got part of the way: what is no longer at its old place is said, the rest was done)
            if let Some(text) = left.text() {
                anyhow::bail!("{text}");
            }
            Ok(ok)
        },
        Some(Box::new(|_, cx| notify_status(cx))),
    );
}

/// A delete again as administrator. The administrator does not use the Recycle Bin (the helper refuses it: see `leaf-helper`'s
/// exec.rs), so a delete that was meant for the bin asks first whether to delete for good; a permanent one was asked already.
pub fn retry_delete(cx: &mut App, paths: Vec<PathBuf>, permanent: bool, _record: bool) {
    if !permanent {
        let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
        let msg = format!(
            "{}\n\n{}",
            crate::i18n::items(paths.len()),
            t!("As administrator, items cannot be moved to the Recycle Bin. Delete them for good instead? This cannot be undone.")
        );
        let _ = w.update(cx, |_, window, cx| {
            crate::prompt::show(
                t!("Delete for good?"),
                Some(msg),
                None,
                vec![
                    crate::prompt::Button::new(t!("Delete for good as administrator"), crate::prompt::Kind::Danger, move |_, _, cx| {
                        retry_delete(cx, paths.clone(), true, false);
                        Ok(())
                    }),
                    crate::prompt::Button::cancel(),
                ],
                window,
                cx,
            )
        });
        return;
    }
    let title = format!("{} {}", t!("Permanently deleting as administrator:"), crate::i18n::items(paths.len()));
    crate::status_center::start_with(
        cx,
        title,
        move |t| {
            // (items already gone, from an earlier try that got part of the way, count as deleted)
            let (ok, _, _) = done(call(Request::DeleteForGood { paths, asked_user: true }, &t, true, &mut Vec::new())?)?;
            Ok(ok)
        },
        Some(Box::new(|_, cx| notify_status(cx))),
    );
}

/// A rename again as administrator (never over an existing name).
pub fn retry_rename(cx: &mut App, from: PathBuf, new_name: String) {
    let title = format!("{} {}", t!("Renaming as administrator:"), rf_core::history::display_name(&from));
    crate::status_center::start_with(
        cx,
        title,
        move |t| {
            let (ok, pairs, _) = done(call(Request::Rename { path: from.clone(), new_name }, &t, true, &mut Vec::new())?)?;
            if let Some((a, b)) = pairs.into_iter().next() {
                crate::undo::push(crate::undo::UndoOp::Renamed { from: a, to: b });
            }
            Ok(ok)
        },
        Some(Box::new(|_, cx| notify_status(cx))),
    );
}

/// A new folder again as administrator; `then` gets its path on the UI thread.
pub fn retry_new_folder(cx: &mut App, dir: PathBuf, name: String, then: impl FnOnce(PathBuf, &mut App) + 'static) {
    let title = format!("{} {}", t!("New folder as administrator:"), rf_core::history::display_name(&dir));
    let made = Arc::new(Mutex::new(None::<PathBuf>));
    let out = made.clone();
    crate::status_center::start_with(
        cx,
        title,
        move |t| {
            // the name the normal try would have used, or the first free "name (2)" (looked at from here: listing is allowed)
            let target = leaf_sys::fileops::unique_name(&dir, &name, "");
            let new_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
            match call(Request::CreateFolder { parent: dir, name: new_name }, &t, true, &mut Vec::new())? {
                Reply::Created { path } => {
                    crate::undo::push(crate::undo::UndoOp::Created { path: path.clone() });
                    *out.lock().unwrap() = Some(path);
                    Ok(true)
                }
                _ => Err(words(HelperError::Confused)),
            }
        },
        Some(Box::new(move |_, cx| {
            notify_status(cx);
            if let Some(p) = made.lock().unwrap().take() {
                then(p, cx);
            }
        })),
    );
}

/// The user chose to see `dir` as administrator: its listings go through the helper (the first one may start it).
pub fn list_here(dir: &Path) {
    let mut l = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    l.retain(|(p, _, _)| p != dir);
    l.push((dir.to_path_buf(), true, None));
}

/// `dir` is listed as administrator: `None` when it is not (or the helper is gone, which ends that), else the listing. Only the
/// first listing after the click may start the helper; refreshes later never bring a prompt up by themselves. Blocking.
pub fn list(dir: &Path) -> Option<Result<Vec<rf_core::Entry>, String>> {
    let may_start = {
        let mut l = LISTED.lock().unwrap_or_else(|e| e.into_inner());
        let ix = l.iter().position(|(p, _, _)| p == dir)?;
        let fresh = std::mem::replace(&mut l[ix].1, false);
        // a later listing only through the very helper that made the first (another one, started later for something else, does
        // not take the folder over)
        let same = l[ix].2.is_some() && l[ix].2 == current().map(|s| s.pid());
        if !fresh && !same {
            l.remove(ix);
            return None;
        }
        fresh
    };
    // (while an operation runs through the helper, this waits for its end: one call at a time)
    let r = call(Request::List { dir: dir.to_path_buf() }, &quiet(), may_start, &mut Vec::new());
    if r.is_ok() {
        let pid = current().map(|s| s.pid());
        if let Some(e) = LISTED.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|(p, _, _)| p == dir) {
            e.2 = e.2.or(pid);
        }
    }
    let r = r.and_then(|r| match r {
        Reply::Listing { items } => Ok(items.iter().map(|i| i.to_entry()).collect()),
        _ => Err(words(HelperError::Confused)),
    });
    Some(r.map_err(|e| format!("{e:#}")))
}

/// The health numbers of one disk (`Some(number)`) or of all disks the administrator's reading gives (reliability counters: temperature,
/// wear, power-on hours). READ ONLY. `may_start` is true only from the user's click on "Read health as administrator" (the permission
/// prompt comes then); the sampling of the drive tiles asks with `false`, so it uses a helper that already runs and never starts one.
/// Blocking.
pub fn drive_health(disk: Option<u32>, may_start: bool) -> anyhow::Result<Vec<leaf_sys::drives::DriveHealth>> {
    match call(Request::DriveHealth { disk }, &quiet(), may_start, &mut Vec::new())? {
        Reply::Health { disks } => Ok(disks),
        _ => Err(words(HelperError::Confused)),
    }
}

/// The editor's save as administrator: the bytes go to a part file in leaf's own temp folder, which the helper puts in place of
/// `path`. Blocking.
pub fn write_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = std::env::temp_dir().join("leaf-admin-parts");
    std::fs::create_dir_all(&dir)?;
    // the long form of the folder (a %TEMP% with an 8.3 short name in it would not be accepted by the helper's checks)
    let dir = std::fs::canonicalize(&dir).map(|c| {
        let s = c.to_string_lossy().into_owned();
        PathBuf::from(s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s))
    })?;
    let part = dir.join(format!("{}-{}.leaf-part", std::process::id(), crate::admin::seq()));
    std::fs::write(&part, bytes)?;
    let r = call(Request::WriteFromPart { part: part.clone(), target: path.to_path_buf() }, &quiet(), true, &mut Vec::new()).and_then(done);
    let _ = std::fs::remove_file(&part);
    r.map(|_| ())
}

fn seq() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// The Status Center's row while the helper runs: what it is, and "End now".
pub fn status_row(p: &crate::theme::Palette) -> Option<AnyElement> {
    use gpui_kit::assets::IconName as L;
    use gpui_kit::component::{Icon, h_flex};
    use gpui_kit::prelude::FluentBuilder as _;
    let starting = STARTING.load(Ordering::Acquire);
    if !starting && !active() {
        return None;
    }
    let label = if starting { t!("Starting the administrator helper…") } else { t!("Administrator helper active") };
    Some(
        h_flex()
            .mx_2()
            .mb_1()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .rounded(crate::theme::RADIUS_SM)
            .border_1()
            .border_color(p.border)
            .child(Icon::new(L::ShieldCheck).size(px(16.)).text_color(p.text_secondary))
            .child(div().flex_1().text_sm().text_color(p.text).child(label))
            .child(
                div()
                    .id("admin-end")
                    .px_2()
                    .py_1()
                    .rounded(crate::theme::RADIUS_SM)
                    .text_xs()
                    .border_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .active(|s| s.bg(p.pressed))
                    .on_click(|_, _, cx| end_now(cx))
                    .child(t!("End now")),
            )
            .when(starting, |d| d.opacity(0.7))
            .into_any_element(),
    )
}
