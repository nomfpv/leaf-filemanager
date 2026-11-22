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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::*;
use rf_core::agent_ops::{self as ops, Op, Plan, Root, Rules};
use serde_json::{Value, json};

use leaf_sys::fileops::{Tracker, Transfer};

use crate::t;

/// How long the consent dialog waits for an answer (then: refused), and a call that waits behind it.
const CONSENT_WAIT: Duration = Duration::from_secs(60);
/// How long a content search through Windows Search may take.
const CONTENT_WAIT: Duration = Duration::from_secs(10);
/// Sessions kept at once (each is a running leaf-mcp); more are refused until leaf restarts.
const MAX_SESSIONS: usize = 64;

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
            match s.get(&req.session) {
                Some(x) if x.given_root != req.root || x.agent != req.agent => Err("This session already belongs to another project folder or agent name; a session keeps the folder it started with.".to_string()),
                Some(x) => Ok(x.root.clone()),
                None if s.len() >= MAX_SESSIONS => Err("leaf has too many agent sessions open; restart leaf to start afresh.".to_string()),
                None => {
                    s.insert(req.session.clone(), Session { agent: req.agent.clone(), given_root: req.root.clone(), root: None, consent: Consent::Unasked });
                    Ok(None)
                }
            }
        })?;
        Ok((req, known))
    })();
    cx.spawn(async move |cx| {
        let r = match first {
            Ok((req, known)) => run(req, known, cx).await,
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
        ack.reply(&text);
        ack.done()
    })
    .await;
    if held {
        cx.update(|cx| crate::last_window::settle(cx));
    }
}

/// The places no agent works in, the organizer's list (Windows, Program Files, AppData, leaf's own, the user folder itself).
fn protected() -> rf_core::organize::ProtectedPaths {
    let mut p = crate::organizer::protected();
    if p.user_profile.is_none() {
        p.user_profile = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    }
    p
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
    let protected = protected();
    cx.background_spawn(async move {
        let root = match known {
            Some(r) => r,
            None => Arc::new(Root::open(&given_root, &protected)?),
        };
        let rules = load_rules(&root);
        let plan = ops::check(&root, &rules, &op)?;
        Ok((root, rules, plan))
    })
    .await
}

async fn run(req: ops::Request, known: Option<Arc<Root>>, cx: &mut AsyncApp) -> Result<Value, String> {
    let (root, rules, plan) = checked(req.root.clone(), known.clone(), req.op.clone(), cx).await?;
    // the folder is resolved once: the first answer is kept for the session
    if known.is_none() {
        SESSIONS.with(|s| {
            if let Some(x) = s.borrow_mut().get_mut(&req.session)
                && x.root.is_none()
            {
                x.root = Some(root.clone());
            }
        });
    }
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
        button(t!("Allow for this session"), crate::prompt::Kind::Normal, Choice::Session),
        button(t!("Allow once"), crate::prompt::Kind::Normal, Choice::Once),
        button(t!("Refuse"), crate::prompt::Kind::Primary, Choice::Refuse),
    ];
    drop(tx);
    let message = format!(
        "{}\n    {agent}\n\n{}\n    {root}\n\n{}\n    {verb}\n\n{}",
        t!("Agent (the name it gives itself):"),
        t!("Project folder (nothing outside it can be touched):"),
        t!("Wants to:"),
        t!("Every change shows in the Status Center and can be undone; nothing is overwritten and nothing is deleted for good. \"Allow for this session\" lasts until leaf closes. Closing this dialog refuses, and so does no answer within 60 seconds."),
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
    let (tx, rx) = std::sync::mpsc::channel::<Option<Vec<PathBuf>>>();
    let stop = Arc::new(AtomicBool::new(false));
    let (r, q, s) = (root.path().to_path_buf(), query.to_string(), stop.clone());
    std::thread::Builder::new()
        .name("rf-agent-search".into())
        .spawn(move || {
            if !leaf_sys::wsearch::is_indexed(&r) {
                let _ = tx.send(None);
                return;
            }
            let mut out = Vec::new();
            let stopped = || s.load(Ordering::Relaxed);
            let _ = leaf_sys::wsearch::search(&r, &q, max * 4 + 50, &stopped, &mut |hits| out.extend(hits.into_iter().map(|h| h.path)));
            let _ = tx.send(Some(out));
        })
        .map_err(|_| "the search could not be started".to_string())?;
    match rx.recv_timeout(CONTENT_WAIT) {
        Ok(Some(found)) => Ok(found),
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
    match plan {
        Plan::Transfer { moving, sources, dest, targets } => transfer(&tag, root, moving, sources, dest, targets, reply, cx),
        Plan::Rename { from, to } => {
            let title = format!("{tag} {} {} → {}", t!("Renaming"), short(&from), short(&to));
            let (f, d) = (from.clone(), to.clone());
            simple_job(title, root, vec![from.clone()], move |_| {
                leaf_sys::fileops::rename_no_replace(&f, &d)?;
                crate::undo::push(crate::undo::UndoOp::Renamed { from: f.clone(), to: d.clone() });
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
                crate::undo::push(crate::undo::UndoOp::Created { path: p.clone() });
                Ok(vec![p.clone()])
            }, |_, _| {}, reply, cx)
        }
        Plan::Trash { paths } => {
            let title = format!("{tag} {} {}", t!("Moving to Trash:"), crate::i18n::items(paths.len()));
            let (ps, gone) = (paths.clone(), paths.clone());
            simple_job(title, root, paths, move |t| {
                // Recycle Bin / Trash only: never the permanent delete (a drive with no Trash on Linux is an error here, not a question)
                let r = leaf_sys::fileops::delete_tracked(&ps, false, t.clone());
                let pairs = t.results.lock().unwrap().clone();
                if !pairs.is_empty() {
                    crate::undo::push(crate::undo::UndoOp::Recycled { pairs: pairs.clone() });
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

/// Copy or move with leaf's own engine. Never the shell's (it would ask about conflicts in its own dialog): any conflict found
/// when the engine plans the work (an item that appeared since the check) stops the job before anything is copied, and the engine
/// itself creates every new file and folder without replacing (a file that appears after the plan is a failure of that item).
#[allow(clippy::too_many_arguments)]
fn transfer(tag: &str, root: Arc<Root>, moving: bool, sources: Vec<PathBuf>, dest: PathBuf, targets: Vec<PathBuf>, reply: Reply, cx: &mut App) {
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
                crate::undo::push(crate::undo::UndoOp::Transferred { kind, pairs: pairs.clone() });
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
