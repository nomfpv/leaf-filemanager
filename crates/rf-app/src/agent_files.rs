//! File operations an agent asks for through `leaf-mcp` (plan F9, `docs/mcp.md`): search, list a folder, and, with the switch in
//! Settings > Developer on, copy, move, rename, create a folder and move to the Recycle Bin / Trash. The requests come over the
//! hand-over channel as typed JSON (`rf_core::agent_ops::Request`) and are checked HERE, in leaf, not only in leaf-mcp: the other
//! process is untrusted input.
//!
//! The order for a change: parse -> the switch -> the session (its folder and its agent name are fixed by its first request) ->
//! the checks (`agent_ops::check`, off the UI thread) -> the user's consent (once per session, or once per call) -> the checks
//! again (the dialog may have stood for a minute) -> a job in the Status Center, named with the agent, with undo recorded, the
//! same as one the user started -> the answer. Nothing about sessions or consent is written anywhere: a restart forgets it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui_kit::*;
use rf_core::agent_ops::{self as ops, Op, Plan, Root, RootLimits, Rules};
use serde_json::{Value, json};

use leaf_sys::fileops::{Tracker, Transfer};

use crate::t;

/// How long the consent dialog waits for an answer (then: refused), and a call that waits behind it.
const CONSENT_WAIT: Duration = Duration::from_secs(60);
/// How long a content search through Windows Search may take.
const CONTENT_WAIT: Duration = Duration::from_secs(10);
/// Sessions kept at once (each is a running leaf-mcp); more are refused until leaf restarts.
const MAX_SESSIONS: usize = 64;
/// A session not heard from for this long is forgotten (its consent with it).
const SESSION_IDLE: Duration = Duration::from_secs(4 * 3600);
/// Searches and listings one session may have running at once.
const MAX_READS: u8 = 2;
/// Content searches (Windows Search) running at once in all; one that hangs in the system's search keeps its place until it ends.
static CONTENT_RUNNING: AtomicUsize = AtomicUsize::new(0);

const OFF: &str = "File operations through leaf are turned off. The user can turn them on in leaf: Settings > Developer > \"Let agents move and delete files through leaf\".";
const REFUSED: &str = "The user did not allow this in leaf (refused, closed, or no answer within 60 seconds). Nothing was changed.";
const FAILED: &str = "The operation failed in leaf; the details are in leaf's Status Center. What had been done when it stopped is listed.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Session,
    Once,
    Refuse,
}

/// What a call waiting behind an open dialog is told.
#[derive(Clone, Copy)]
enum Waited {
    Go,
    No,
    /// "Allow once" was for the call that opened the dialog: ask again.
    Again,
}

enum Consent {
    Unasked,
    Asking(Vec<oneshot::Sender<Waited>>),
    Session,
}

struct Session {
    agent: String,
    given_root: String,
    root: Option<Arc<Root>>,
    consent: Consent,
    /// When its last request came.
    last: Instant,
    /// Searches and listings of it running now.
    reading: u8,
}

/// Forgets every session's "Allow for this session" (the switch went off; a dialog that is open still refuses then).
pub fn forget_consent() {
    SESSIONS.with(|s| {
        for x in s.borrow_mut().values_mut() {
            if matches!(x.consent, Consent::Session) {
                x.consent = Consent::Unasked;
            }
        }
    });
}

/// Room for one more session: the ones idle too long go, then (when still full) the idle ones without consent, oldest first.
/// A session whose dialog is open is never dropped.
fn make_room(s: &mut HashMap<String, Session>) -> bool {
    s.retain(|_, x| matches!(x.consent, Consent::Asking(_)) || x.reading > 0 || x.last.elapsed() < SESSION_IDLE);
    while s.len() >= MAX_SESSIONS {
        let oldest = s.iter().filter(|(_, x)| matches!(x.consent, Consent::Unasked) && x.reading == 0).min_by_key(|(_, x)| x.last).map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                s.remove(&k);
            }
            None => return false,
        }
    }
    true
}

fn end_read(session: &str) {
    SESSIONS.with(|s| {
        if let Some(x) = s.borrow_mut().get_mut(session) {
            x.reading = x.reading.saturating_sub(1);
        }
    });
}

thread_local! {
    // the UI thread's: every request is taken there
    static SESSIONS: RefCell<HashMap<String, Session>> = RefCell::new(HashMap::new());
}

/// Whether the switch is on (leaf-mcp asks before it lists its tools).
pub fn enabled(cx: &App) -> bool {
    cx.global::<crate::settings::Settings>().agent_file_ops
}

/// Takes one request (the JSON after `op:`) and answers it on `ack`.
pub fn handle(json: String, ack: leaf_sys::handover::Ack, cx: &mut App) {
    let on = enabled(cx);
    let first = (|| {
        let req = ops::parse_request(&json)?;
        if req.op.mutating() && !on {
            return Err(OFF.to_string());
        }
        let known = SESSIONS.with(|s| {
            let mut s = s.borrow_mut();
            if !s.contains_key(&req.session) {
                if !make_room(&mut s) {
                    return Err("leaf has too many agent sessions waiting for an answer; try again later.".to_string());
                }
                s.insert(req.session.clone(), Session { agent: req.agent.clone(), given_root: req.root.clone(), root: None, consent: Consent::Unasked, last: Instant::now(), reading: 0 });
            }
            let x = s.get_mut(&req.session).expect("just made");
            if x.given_root != req.root || x.agent != req.agent {
                return Err("This session already belongs to another project folder or agent name; a session keeps the folder it started with.".to_string());
            }
            x.last = Instant::now();
            if !req.op.mutating() {
                if x.reading >= MAX_READS {
                    return Err("This session has two searches or listings running already; wait for them.".to_string());
                }
                x.reading += 1;
            }
            Ok(x.root.clone())
        })?;
        Ok((req, known))
    })();
    cx.spawn(async move |cx| {
        let r = match first {
            Ok((req, known)) => {
                let reading = (!req.op.mutating()).then(|| req.session.clone());
                let r = run(req, known, cx).await;
                if let Some(session) = reading {
                    end_read(&session);
                }
                r
            }
            Err(e) => Err(e),
        };
        answer(ack, r, cx).await;
    })
    .detach();
}

/// Sends the answer as one data line and ends the request.
async fn answer(ack: leaf_sys::handover::Ack, r: Result<Value, String>, cx: &mut AsyncApp) {
    let v = match r {
        Ok(mut v) => {
            v["ok"] = json!(true);
            v
        }
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let text = v.to_string();
    // (off the UI thread: a long answer is written as fast as the client reads it)
    let held = cx.background_spawn(async move {
        // (the Linux handover's reply takes the acknowledgement by mutable reference)
        #[allow(unused_mut)]
        let mut ack = ack;
        ack.reply(&text);
        ack.done()
    })
    .await;
    if held {
        cx.update(|cx| crate::last_window::settle(cx));
    }
}

/// What a project folder may not be: the organizer's list (Windows, Program Files, AppData, leaf's own, the user folder itself),
/// the user's own folders themselves, the agent's settings folder and `.ssh`, and anything not on a local fixed drive.
fn limits() -> RootLimits {
    let mut p = crate::organizer::protected();
    if p.user_profile.is_none() {
        p.user_profile = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    }
    let mut not_itself: Vec<PathBuf> = [dirs::document_dir(), dirs::desktop_dir(), dirs::download_dir(), dirs::picture_dir(), dirs::audio_dir(), dirs::video_dir(), dirs::home_dir()]
        .into_iter()
        .flatten()
        .collect();
    for v in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(d) = std::env::var_os(v).filter(|d| !d.is_empty()) {
            not_itself.push(PathBuf::from(d));
        }
    }
    let mut not_within: Vec<PathBuf> = dirs::home_dir().map(|h| vec![h.join(".claude"), h.join(".ssh")]).unwrap_or_default();
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        not_within.push(d);
    }
    RootLimits { protected: p, not_itself, not_within, local_fixed: Box::new(local_fixed) }
}

/// On a drive the system calls fixed (the drive whose root is the longest one `p` is under).
fn local_fixed(p: &std::path::Path) -> bool {
    use rf_core::organize::safety::{is_inside, norm};
    let k = norm(p);
    leaf_sys::drives::list_drives()
        .into_iter()
        .filter(|d| is_inside(&k, &norm(&d.root)))
        .max_by_key(|d| norm(&d.root).len())
        .is_some_and(|d| d.kind == leaf_sys::drives::DriveKind::Fixed)
}

/// Whether the Recycle Bin of the root's drive takes `bytes` without Windows asking to delete for good: the drive has a bin that
/// keeps files, and a custom bin size (when set) is larger. On Linux the Trash of a drive without one fails the job instead.
fn bin_takes(root: &std::path::Path, bytes: u64) -> Result<(), String> {
    #[cfg(windows)]
    {
        use rf_core::recycle_bin::BinSettingsStore;
        let s = root.to_string_lossy();
        let label = s.get(..2).unwrap_or("").to_ascii_uppercase();
        let settings = leaf_sys::recycle_settings::SystemBinSettings.read().map_err(|_| "the Recycle Bin of this drive cannot be looked at, so nothing is moved there".to_string())?;
        let Some(v) = settings.volumes.iter().find(|v| v.label.eq_ignore_ascii_case(&label)) else {
            return Err("this drive has no Recycle Bin; nothing is moved there".into());
        };
        if v.remove_immediately {
            return Err("the Recycle Bin of this drive is set to remove files at once; leaf does not delete for good for an agent".into());
        }
        if let Some(mb) = v.max_mb
            && bytes > u64::from(mb) << 20
        {
            return Err("these items are larger than this drive's Recycle Bin; nothing is moved there".into());
        }
    }
    #[cfg(not(windows))]
    let _ = (root, bytes);
    Ok(())
}

/// The deny rules of the project's agent settings (the project's and the local file, of the root and of its Git root when that is
/// another folder, and the user's). A file that is there and cannot be read refuses every change.
fn load_rules(root: &Root) -> Rules {
    let mut rules = Rules::new();
    let home = dirs::home_dir().and_then(|h| std::fs::canonicalize(h).ok()).map(ops::simplify);
    let mut bases = vec![root.path().to_path_buf()];
    if let Some(git) = crate::agent_rules::project_root(root.path()).and_then(|g| std::fs::canonicalize(g).ok()).map(ops::simplify)
        && rf_core::organize::safety::norm(&git) != rf_core::organize::safety::norm(root.path())
    {
        bases.push(git);
    }
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for b in &bases {
        files.push((b.clone(), b.join(".claude").join("settings.json")));
        files.push((b.clone(), b.join(".claude").join("settings.local.json")));
    }
    let user_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    if let Some(d) = user_dir {
        files.push((root.path().to_path_buf(), d.join("settings.json")));
    }
    for (base, f) in files {
        // the file and its folder are never an agent's to change, whether they hold rules or not (or a missing file could be
        // put in place, and an existing one moved away, to change what is protected)
        rules.guard(&f);
        match std::fs::read_to_string(&f) {
            Ok(text) if text.trim().is_empty() => {}
            Ok(text) => match crate::agent_rules::rules_in(&text, crate::agent_rules::Kind::Deny) {
                Ok(list) => rules.add(&base, &list, home.as_deref()),
                Err(_) => rules.refuse_all("an agent settings file of this project is not readable JSON, so its rules cannot be honoured"),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => rules.refuse_all("an agent settings file of this project cannot be read, so its rules cannot be honoured"),
        }
    }
    rules
}

/// Opens the root (once per session) and checks `op`, off the UI thread.
async fn checked(given_root: String, known: Option<Arc<Root>>, op: Op, cx: &mut AsyncApp) -> Result<(Arc<Root>, Rules, Plan), String> {
    cx.background_spawn(async move {
        let root = match known {
            Some(r) => r,
            None => Arc::new(Root::open(&given_root, &limits())?),
        };
        let rules = load_rules(&root);
        let plan = ops::check(&root, &rules, &op)?;
        if let Plan::Trash { bytes, .. } = &plan {
            bin_takes(root.path(), *bytes)?;
        }
        Ok((root, rules, plan))
    })
    .await
}

async fn run(req: ops::Request, known: Option<Arc<Root>>, cx: &mut AsyncApp) -> Result<Value, String> {
    let (root, rules, plan) = checked(req.root.clone(), known.clone(), req.op.clone(), cx).await?;
    // the folder is resolved once: the first answer is kept for the session, and every later step uses the kept one (two first
    // requests at once each resolved it: the one that was kept first wins, the other is checked again against it)
    let kept = SESSIONS.with(|s| {
        let mut s = s.borrow_mut();
        let x = s.get_mut(&req.session)?;
        if x.root.is_none() {
            x.root = Some(root.clone());
        }
        x.root.clone()
    });
    let Some(kept) = kept else { return Err("This session was forgotten meanwhile; try again.".into()) };
    let (root, rules, plan) = if Arc::ptr_eq(&kept, &root) { (root, rules, plan) } else { checked(req.root.clone(), Some(kept), req.op.clone(), cx).await? };
    if !req.op.mutating() {
        let r = root.clone();
        return cx.background_spawn(async move { read_only(&r, &rules, plan) }).await;
    }
    drop(rules);
    consent(&req.session, &req.agent, &root.shown(root.path()), verb_text(&req.op), cx).await?;
    // switched off while the dialog stood: no
    if !cx.update(|cx| enabled(cx)) {
        return Err(OFF.into());
    }
    let (root, _, plan) = checked(req.root.clone(), Some(root), req.op.clone(), cx).await?;
    let (tx, rx) = oneshot::channel::<Result<Value, String>>();
    let agent = req.agent.clone();
    cx.update(|cx| start_job(&agent, root, plan, tx, cx));
    rx.await.unwrap_or_else(|_| Err("leaf could not start the operation (no Status Center to run it in).".into()))
}

fn verb_text(op: &Op) -> &'static str {
    match op {
        Op::Copy { .. } => t!("copy files"),
        Op::Move { .. } => t!("move files"),
        Op::Rename { .. } => t!("rename an item"),
        Op::CreateFolder { .. } => t!("create a folder"),
        Op::Trash { .. } => t!("move items to the Recycle Bin / Trash"),
        Op::Search { .. } | Op::ListFolder { .. } => t!("look at files"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// consent
// ---------------------------------------------------------------------------------------------------------------------------

async fn within<T>(rx: oneshot::Receiver<T>, wait: Duration, cx: &mut AsyncApp) -> Option<T> {
    let timer = Box::pin(cx.background_executor().timer(wait));
    match futures::future::select(rx, timer).await {
        futures::future::Either::Left((Ok(v), _)) => Some(v),
        _ => None,
    }
}

/// `Ok` once the user allowed this call (or this session earlier); a call while the dialog is open waits for its answer.
async fn consent(session: &str, agent: &str, root: &str, verb: &str, cx: &mut AsyncApp) -> Result<(), String> {
    enum Step {
        Go,
        Wait(oneshot::Receiver<Waited>),
        Ask,
    }
    loop {
        let step = SESSIONS.with(|s| {
            let mut s = s.borrow_mut();
            let Some(x) = s.get_mut(session) else { return Step::Ask };
            match &mut x.consent {
                Consent::Session => Step::Go,
                Consent::Asking(waiting) => {
                    let (tx, rx) = oneshot::channel();
                    waiting.push(tx);
                    Step::Wait(rx)
                }
                Consent::Unasked => {
                    x.consent = Consent::Asking(Vec::new());
                    Step::Ask
                }
            }
        });
        match step {
            Step::Go => return Ok(()),
            Step::Wait(rx) => match within(rx, CONSENT_WAIT, cx).await {
                Some(Waited::Go) => return Ok(()),
                Some(Waited::Again) => continue,
                _ => return Err(REFUSED.into()),
            },
            Step::Ask => {
                let shown = cx.update(|cx| ask_user(agent, root, verb, cx));
                let choice = match shown {
                    Ok((rx, window, view)) => {
                        let c = within(rx, CONSENT_WAIT, cx).await;
                        if c.is_none() {
                            // no answer in time: the dialog goes, if it is still this one
                            let _ = cx.update(|cx| {
                                window.update(cx, |_, window, cx| {
                                    if view.is_some() && crate::prompt::current(window, cx).map(|v| v.entity_id()) == view {
                                        crate::prompt::close(window, cx);
                                    }
                                })
                            });
                        }
                        c.unwrap_or(Choice::Refuse)
                    }
                    Err(e) => {
                        log::info!("agent files: consent not asked: {e}");
                        Choice::Refuse
                    }
                };
                let waiting = SESSIONS.with(|s| {
                    let mut s = s.borrow_mut();
                    let Some(x) = s.get_mut(session) else { return Vec::new() };
                    let was = std::mem::replace(&mut x.consent, if choice == Choice::Session { Consent::Session } else { Consent::Unasked });
                    match was {
                        Consent::Asking(w) => w,
                        _ => Vec::new(),
                    }
                });
                let told = match choice {
                    Choice::Session => Waited::Go,
                    Choice::Once => Waited::Again,
                    Choice::Refuse => Waited::No,
                };
                for w in waiting {
                    let _ = w.send(told);
                }
                return if choice == Choice::Refuse { Err(REFUSED.into()) } else { Ok(()) };
            }
        }
    }
}

/// Shows the consent dialog in leaf's window. Refuse is the default button; Escape or closing it refuses (the buttons go with
/// the dialog, and with them the only sender). Not shown over another dialog of leaf's (that one is the user's): refused then.
#[allow(clippy::type_complexity)]
fn ask_user(agent: &str, root: &str, verb: &str, cx: &mut App) -> Result<(oneshot::Receiver<Choice>, AnyWindowHandle, Option<EntityId>), String> {
    let window = cx.active_window().or_else(|| cx.windows().first().copied()).ok_or("leaf has no window open to ask in")?;
    let (tx, rx) = oneshot::channel::<Choice>();
    let tx = Rc::new(std::cell::Cell::new(Some(tx)));
    let button = |label: &'static str, kind: crate::prompt::Kind, c: Choice| {
        let tx = tx.clone();
        crate::prompt::Button::new(label, kind, move |_, _, _| {
            if let Some(tx) = tx.take() {
                let _ = tx.send(c);
            }
            Ok(())
        })
    };
    let buttons = vec![
        button(t!("Allow changes in this folder for this session: copy, move, rename, new folders, Recycle Bin"), crate::prompt::Kind::Normal, Choice::Session),
        button(t!("Allow once"), crate::prompt::Kind::Normal, Choice::Once),
        button(t!("Refuse"), crate::prompt::Kind::Primary, Choice::Refuse),
    ];
    drop(tx);
    let message = format!(
        "{}\n    {agent}\n\n{}\n    {root}\n\n{}\n    {verb}\n\n{}",
        t!("Agent (the name it gives itself):"),
        t!("Project folder (nothing outside it can be touched):"),
        t!("Wants to, now:"),
        t!("Every change shows in the Status Center and can be undone; nothing is overwritten and nothing is deleted for good. Allowing for this session covers every kind of change above in this folder until leaf closes or the switch goes off. Closing this dialog refuses, and so does no answer within 60 seconds."),
    );
    window
        .update(cx, |_, window, cx| {
            if crate::prompt::current(window, cx).is_some() {
                return Err("leaf is showing another question; try again in a moment".to_string());
            }
            crate::prompt::show(t!("An agent wants to change files"), Some(message), None, buttons, window, cx);
            window.activate_window();
            Ok(crate::prompt::current(window, cx).map(|v| v.entity_id()))
        })
        .map_err(|_| "leaf's window could not be reached".to_string())?
        .map(|view| (rx, window, view))
}

// ---------------------------------------------------------------------------------------------------------------------------
// the read-only tools
// ---------------------------------------------------------------------------------------------------------------------------

fn paths_json(root: &Root, paths: &[PathBuf]) -> Vec<String> {
    paths.iter().filter(|p| root.holds(p)).map(|p| p.display().to_string()).collect()
}

fn read_only(root: &Root, rules: &Rules, plan: Plan) -> Result<Value, String> {
    match plan {
        Plan::List { dir } => {
            let (entries, cut) = ops::list(rules, &dir)?;
            let list: Vec<Value> = entries.iter().map(|e| json!({ "name": e.name, "kind": e.kind, "size": e.size })).collect();
            let text = format!("{}: {} entries{}", root.shown(&dir), list.len(), if cut { " (cut at the limit)" } else { "" });
            Ok(json!({ "text": text, "folder": root.shown(&dir), "entries": list, "truncated": cut }))
        }
        Plan::Search { query, contents, max } => {
            let (found, how) = if contents { (content_search(root, &query, max)?, "Windows Search (names and contents)") } else { name_search(root, rules, &query, max) };
            let kept = ops::keep_found(root, rules, found, max);
            let paths = paths_json(root, &kept);
            Ok(json!({ "text": format!("{} found with {how}", paths.len()), "paths": paths, "count": paths.len() }))
        }
        _ => Err("not a read-only request".into()),
    }
}

/// leaf's name index where it covers the root, else a bounded walk.
fn name_search(root: &Root, rules: &Rules, query: &str, max: usize) -> (Vec<PathBuf>, &'static str) {
    // (more than asked for: some may be left out as protected)
    match leaf_sys::indexer::search(query, root.path(), false, max * 4 + 50) {
        Some(found) => (found, "leaf's name index"),
        None => (ops::walk_search(root, rules, query, max), "a walk of the folder (leaf's index does not cover it)"),
    }
}

/// Windows Search where it indexes the root (names and contents); it can hang where it is broken, so it is waited for only so long.
fn content_search(root: &Root, query: &str, max: usize) -> Result<Vec<PathBuf>, String> {
    if CONTENT_RUNNING.fetch_add(1, Ordering::SeqCst) >= 2 {
        CONTENT_RUNNING.fetch_sub(1, Ordering::SeqCst);
        return Err("two content searches are running already; try again in a moment or search by name".into());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<PathBuf>>>();
    let stop = Arc::new(AtomicBool::new(false));
    let (r, q, s) = (root.path().to_path_buf(), query.to_string(), stop.clone());
    let started = std::thread::Builder::new().name("rf-agent-search".into()).spawn(move || {
        // (its place is given back when the thread ends, also when the system's search held it long after the answer went)
        struct Leave;
        impl Drop for Leave {
            fn drop(&mut self) {
                CONTENT_RUNNING.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _leave = Leave;
        if s.load(Ordering::Relaxed) || !leaf_sys::wsearch::is_indexed(&r) {
            let _ = tx.send(None);
            return;
        }
        let mut out = Vec::new();
        let limit = max * 4 + 50;
        // stopped between every batch of rows: by the time limit, and as soon as enough were found
        let full = AtomicBool::new(false);
        let stopped = || s.load(Ordering::Relaxed) || full.load(Ordering::Relaxed);
        let _ = leaf_sys::wsearch::search(&r, &q, limit, &stopped, &mut |hits| {
            if !s.load(Ordering::Relaxed) {
                out.extend(hits.into_iter().map(|h| h.path));
            }
            if out.len() >= limit {
                full.store(true, Ordering::Relaxed);
            }
        });
        let _ = tx.send((!s.load(Ordering::Relaxed)).then_some(out));
    });
    if started.is_err() {
        CONTENT_RUNNING.fetch_sub(1, Ordering::SeqCst);
        return Err("the search could not be started".into());
    }
    match rx.recv_timeout(CONTENT_WAIT) {
        Ok(Some(found)) => Ok(found),
        Ok(None) if stop.load(Ordering::Relaxed) => Err("the content search was stopped".into()),
        Ok(None) => Err("content search needs a system search index that covers this folder (Windows Search); it does not here. Search by name instead.".into()),
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err("the content search took too long and was stopped".into())
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// the changes: jobs in the Status Center
// ---------------------------------------------------------------------------------------------------------------------------

fn short(p: &std::path::Path) -> String {
    rf_core::history::display_name(p)
}

type Reply = oneshot::Sender<Result<Value, String>>;

/// Starts the checked change as a Status Center job titled with the agent's name; `reply` gets the answer when it ends.
fn start_job(agent: &str, root: Arc<Root>, plan: Plan, reply: Reply, cx: &mut App) {
    let tag = t!("Agent {name}:").replace("{name}", agent);
    let who = agent.to_string();
    match plan {
        Plan::Transfer { moving, sources, dest, targets } => transfer(&tag, who, root, moving, sources, dest, targets, reply, cx),
        Plan::Rename { from, to } => {
            let title = format!("{tag} {} {} → {}", t!("Renaming"), short(&from), short(&to));
            let (f, d) = (from.clone(), to.clone());
            simple_job(title, root, vec![from.clone()], move |_| {
                leaf_sys::fileops::rename_no_replace(&f, &d)?;
                crate::undo::push_by_agent(&who, crate::undo::UndoOp::Renamed { from: f.clone(), to: d.clone() });
                Ok(vec![d.clone()])
            }, move |ok, cx| {
                if ok {
                    crate::tags::follow(cx, &[(from.clone(), to.clone())]);
                    crate::places::pins_follow(cx, &from, Some(&to));
                }
            }, reply, cx)
        }
        Plan::CreateFolder { path } => {
            let title = format!("{tag} {} {}", t!("Creating folder"), short(&path));
            let p = path.clone();
            simple_job(title, root, Vec::new(), move |_| {
                // (one level, and never over anything: an item that appeared since the check is an error here)
                std::fs::create_dir(&p)?;
                crate::undo::push_by_agent(&who, crate::undo::UndoOp::Created { path: p.clone() });
                Ok(vec![p.clone()])
            }, |_, _| {}, reply, cx)
        }
        Plan::Trash { paths, .. } => {
            let title = format!("{tag} {} {}", t!("Moving to Trash:"), crate::i18n::items(paths.len()));
            let (ps, gone) = (paths.clone(), paths.clone());
            simple_job(title, root, paths, move |t| {
                // Recycle Bin / Trash only: never the permanent delete (a drive with no Trash on Linux is an error here, not a question)
                let r = leaf_sys::fileops::delete_tracked(&ps, false, t.clone());
                let pairs = t.results.lock().unwrap().clone();
                if !pairs.is_empty() {
                    crate::undo::push_by_agent(&who, crate::undo::UndoOp::Recycled { pairs: pairs.clone() });
                }
                if !r? {
                    anyhow::bail!("{}", t!("Cancelled"));
                }
                // what went: the originals (the places inside the bin are not the agent's to know)
                Ok(ps.iter().filter(|p| std::fs::symlink_metadata(p).is_err()).cloned().collect())
            }, move |ok, cx| {
                if ok {
                    for p in &gone {
                        crate::places::pins_follow(cx, p, None);
                    }
                }
            }, reply, cx)
        }
        Plan::Search { .. } | Plan::List { .. } => {
            let _ = reply.send(Err("not a change".into()));
        }
    }
}

/// A job whose work gives the paths it made or removed (inside the root); its end answers `reply`.
#[allow(clippy::too_many_arguments)]
fn simple_job(
    title: String,
    root: Arc<Root>,
    lock_paths: Vec<PathBuf>,
    work: impl FnOnce(&Arc<Tracker>) -> anyhow::Result<Vec<PathBuf>> + Send + 'static,
    after: impl FnOnce(bool, &mut App) + 'static,
    reply: Reply,
    cx: &mut App,
) {
    let out = Arc::new(Mutex::new(None::<Result<Vec<PathBuf>, ()>>));
    let slot = out.clone();
    let id = crate::status_center::start_with_id(
        cx,
        title,
        move |t| {
            let r = work(&t);
            let ok = r.is_ok();
            *slot.lock().unwrap() = Some(r.as_ref().map(|v| v.clone()).map_err(|_| ()));
            r.map(|_| ok)
        },
        Some(Box::new(move |ok, cx| {
            after(ok, cx);
            let v = match out.lock().unwrap().take() {
                Some(Ok(paths)) => Ok(json!({ "text": format!("Done: {}", paths_json(&root, &paths).join(", ")), "paths": paths_json(&root, &paths) })),
                _ => Err(FAILED.to_string()),
            };
            let _ = reply.send(v);
        })),
    );
    if id != 0 && !lock_paths.is_empty() {
        crate::status_center::set_lock_paths(cx, id, lock_paths);
    }
}

/// Copy or move with leaf's own engine only (`Tracker::own_engine_only`): what it would hand to the system's engine (a folder
/// with a link inside, a file it could not copy, a network path, a copy into the items' own folder) is left and the job fails,
/// so no shell can follow a link or ask about replacing. Any conflict found when the engine plans the work (an item that appeared
/// since the check) stops the job before anything is copied, and the engine creates every new file and folder without replacing.
#[allow(clippy::too_many_arguments)]
fn transfer(tag: &str, who: String, root: Arc<Root>, moving: bool, sources: Vec<PathBuf>, dest: PathBuf, targets: Vec<PathBuf>, reply: Reply, cx: &mut App) {
    let kind = if moving { Transfer::Move } else { Transfer::Copy };
    let verb = if moving { t!("Moving") } else { t!("Copying") };
    let title = format!("{tag} {verb}: {} → {}", crate::i18n::items(sources.len()), short(&dest));
    let s = cx.global::<crate::settings::Settings>();
    let (verify, limit) = (s.copy_verify.engine(), u64::from(s.copy_speed_limit) << 20);
    let conflict = Arc::new(AtomicBool::new(false));
    let hit = conflict.clone();
    let conflict_seen = conflict.clone();
    let card_dest = dest.clone();
    let mark = Arc::new(Mutex::new(None::<PathBuf>));
    let marker = mark.clone();
    let moved = Arc::new(Mutex::new(Vec::<(PathBuf, PathBuf)>::new()));
    let moved_out = moved.clone();
    let failed = Arc::new(AtomicBool::new(false));
    let failed_out = failed.clone();
    let lock_paths = sources.clone();
    let open_dest = dest.clone();
    let words = format!("{tag} {verb} {}", crate::i18n::items(sources.len()));
    let id = crate::status_center::start_with_id(
        cx,
        title,
        move |t| {
            // a tracker of its own whose only difference is the answer to a conflict: stop (nothing is replaced, nothing asked)
            let o = t.clone();
            let mut inner = Tracker::new(Box::new(move |a, b, c| (o.on_progress)(a, b, c)), t.cancel.clone());
            inner.paused = t.paused.clone();
            inner.bytes = t.bytes.clone();
            inner.set_verify(verify);
            inner.limit.store(limit, Ordering::Relaxed);
            // never the system's engine: a link inside, a file ours could not copy, a network path is left, not handed to the shell
            inner.own_engine_only = true;
            inner.ask = Some(Box::new(move |_| {
                hit.store(true, Ordering::Relaxed);
                None
            }));
            let inner = Arc::new(inner);
            let r = leaf_sys::fastcopy::transfer(kind, &sources, &dest, inner.clone());
            let pairs = inner.results.lock().unwrap().clone();
            *t.results.lock().unwrap() = pairs.clone();
            if !pairs.is_empty() {
                *marker.lock().unwrap() = pairs.first().map(|(_, made)| made.clone());
                crate::undo::push_by_agent(&who, crate::undo::UndoOp::Transferred { kind, pairs: pairs.clone() });
            }
            if kind == Transfer::Move {
                *moved_out.lock().unwrap() = crate::tags::settled(pairs);
            }
            if r.is_err() {
                failed_out.store(true, Ordering::Relaxed);
            }
            match r {
                Ok(false) if conflict.load(Ordering::Relaxed) => Err(anyhow::anyhow!("{}", t!("An item with the same name appeared at the destination; nothing was replaced and the operation stopped"))),
                other => other,
            }
        },
        Some(Box::new(move |ok, cx| {
            crate::tags::follow(cx, &std::mem::take(&mut *moved.lock().unwrap()));
            let landed: Vec<PathBuf> = targets.iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).cloned().collect();
            let paths = paths_json(&root, &landed);
            let arrived = if paths.is_empty() { "nothing".to_string() } else { paths.join(", ") };
            let v = if ok {
                Ok(json!({ "text": format!("{} {} item(s) into {}", if moving { "Moved" } else { "Copied" }, paths.len(), root.shown(&open_dest)), "paths": paths }))
            } else if conflict_seen.load(Ordering::Relaxed) {
                Err(format!("An item with the same name appeared at the destination after the check; nothing was replaced and the operation stopped. Arrived: {arrived}"))
            } else if failed.load(Ordering::Relaxed) || paths.is_empty() {
                Err(format!("{FAILED} Arrived: {arrived}"))
            } else {
                Err(format!("Cancelled in leaf. Arrived: {arrived}"))
            };
            let _ = reply.send(v);
        })),
    );
    if id != 0 {
        crate::status_center::set_extras(cx, id, Some(words), Some(card_dest), Some(mark));
        crate::status_center::set_lock_paths(cx, id, lock_paths);
    }
}
