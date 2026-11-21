//! The agent layer (PLAN F2, D2, D11): what coding agents on this PC are doing, from `leaf-agents`. This file keeps the list fresh and
//! answers the questions the rest of the app asks (is an agent working in this folder? how many are running?). The Agents page is
//! `agents_view`.
//!
//! Everything is behind `Settings::ai_layer`: off, nothing is read, no thread or timer runs and every question here answers "no".
//! Refreshed when the agents' folders change (watched, at most every 2 s) and at a slow tick (10 s while a session is open, 30 s
//! otherwise): the tick finds programs that start or stop without touching a file. Prompt text is only held for the Agents page.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui_kit::*;
use leaf_agents::{Session, State, folder_key};

use crate::settings::Settings;
use crate::t;

/// How long ago a session may have been touched to be listed.
const RECENT: Duration = Duration::from_secs(3 * 24 * 3600);
const MAX_SESSIONS: usize = 200;
const TICK_OPEN: Duration = Duration::from_secs(10);
const TICK_QUIET: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct AgentsState {
    sessions: Arc<Vec<Session>>,
    /// Folder key -> the state worth showing (Running over Waiting), for sessions that are running or waiting.
    active: HashMap<String, State>,
    /// Bumped to stop the loop that runs (a switch off, a new start).
    generation: u64,
    running: bool,
    _watch: Option<leaf_agents::Watch>,
}

impl Global for AgentsState {}

pub fn init(cx: &mut App) {
    cx.set_global(AgentsState::default());
    apply(cx);
}

fn on(cx: &App) -> bool {
    cx.try_global::<Settings>().is_some_and(|s| s.ai_layer)
}

/// Starts or stops the layer to match the setting. Cheap when nothing changed.
pub fn apply(cx: &mut App) {
    if cx.try_global::<AgentsState>().is_none() {
        return;
    }
    let want = on(cx) && std::env::var_os("LEAF_MULTI_INSTANCE").is_none();
    let running = cx.global::<AgentsState>().running;
    if want == running {
        return;
    }
    if !want {
        cx.update_global::<AgentsState, _>(|s, _| {
            s.generation += 1;
            *s = AgentsState { generation: s.generation, ..AgentsState::default() };
        });
        cx.refresh_windows();
        return;
    }
    let generation = cx.update_global::<AgentsState, _>(|s, _| {
        s.generation += 1;
        s.running = true;
        s.generation
    });
    let adapters: Arc<Vec<Box<dyn leaf_agents::Adapter>>> = Arc::new(leaf_agents::adapters());
    let roots: Vec<_> = adapters.iter().flat_map(|a| a.watch_roots()).collect();
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
    let watch = leaf_agents::Watch::start(&roots, move || {
        let _ = tx.unbounded_send(());
    });
    cx.update_global::<AgentsState, _>(|s, _| s._watch = watch);
    cx.spawn(async move |cx| {
        loop {
            let a = adapters.clone();
            let got = cx.background_executor().spawn(async move { leaf_agents::snapshot(&a, RECENT, MAX_SESSIONS) }).await;
            let alive = cx.update(|cx| {
                if cx.try_global::<AgentsState>().is_none_or(|s| s.generation != generation) {
                    return None;
                }
                let any_open = got.iter().any(|s| s.state.is_live());
                if *cx.global::<AgentsState>().sessions != got {
                    cx.update_global::<AgentsState, _>(|s, _| {
                        s.active = active_map(&got);
                        s.sessions = Arc::new(got);
                    });
                    cx.refresh_windows();
                }
                Some(any_open)
            });
            let Some(any_open) = alive else { return };
            // the next look: a change in the agents' folders, or the slow tick
            let wait = cx.background_executor().timer(if any_open { TICK_OPEN } else { TICK_QUIET });
            futures::future::select(Box::pin(rx.next()), Box::pin(wait)).await;
        }
    })
    .detach();
}

fn active_map(sessions: &[Session]) -> HashMap<String, State> {
    let mut m: HashMap<String, State> = HashMap::new();
    for s in sessions.iter().filter(|s| s.state.is_active()) {
        let e = m.entry(folder_key(&s.folder)).or_insert(s.state);
        *e = (*e).min(s.state);
    }
    m
}

/// All sessions now (newest first); empty while the layer is off.
pub fn sessions(cx: &App) -> Arc<Vec<Session>> {
    cx.try_global::<AgentsState>().map(|s| s.sessions.clone()).unwrap_or_default()
}

/// Some session is running or waiting.
pub fn any_active(cx: &App) -> bool {
    cx.try_global::<AgentsState>().is_some_and(|s| !s.active.is_empty())
}

/// A running or waiting session works exactly in this folder (the dot on its row).
pub fn folder_state(cx: &App, dir: &Path) -> Option<State> {
    let s = cx.try_global::<AgentsState>().filter(|s| !s.active.is_empty())?;
    s.active.get(&folder_key(dir)).copied()
}

/// A running or waiting session works in this folder or in one inside it (the dot on a tab that shows the folder or one of its subfolders).
pub fn tab_state(cx: &App, dir: &Path) -> Option<State> {
    let s = cx.try_global::<AgentsState>().filter(|s| !s.active.is_empty())?;
    let key = folder_key(dir);
    s.active.iter().filter(|(k, _)| key == **k || key.strip_prefix(k.as_str()).is_some_and(|rest| rest.starts_with('/'))).map(|(_, st)| *st).min()
}

/// The status-bar line ("2 agents running"), `None` when no session is running or waiting.
pub fn status_line(cx: &App) -> Option<String> {
    let s = cx.try_global::<AgentsState>().filter(|s| !s.active.is_empty())?;
    let running = s.sessions.iter().filter(|x| x.state == State::Running).count();
    let waiting = s.sessions.iter().filter(|x| x.state == State::Waiting).count();
    let agents = |n: usize| if n == 1 { t!("agent") } else { t!("agents") };
    Some(match (running, waiting) {
        (0, 0) => return None,
        (r, 0) => format!("{r} {} {}", agents(r), t!("running")),
        (0, w) => format!("{w} {} {}", agents(w), t!("waiting for you")),
        (r, w) => format!("{r} {} {}, {w} {}", agents(r), t!("running"), t!("waiting for you")),
    })
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(ms: i64) -> String {
    let secs = ((std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64) - ms) / 1000).max(0);
    match secs {
        0..=19 => t!("just now").to_string(),
        20..=3599 => format!("{} {}", (secs / 60).max(1), t!("min ago")),
        3600..=86399 => format!("{} {}", secs / 3600, t!("h ago")),
        _ => format!("{} {}", secs / 86400, t!("d ago")),
    }
}

/// The colour of a state's dot, from the theme: working is the success colour, waiting for the user the warning colour, the rest quiet.
pub fn state_color(state: State, p: &crate::theme::Palette) -> Hsla {
    match state {
        State::Running => p.success,
        State::Waiting => p.warning,
        State::Idle => p.text_tertiary,
        State::Ended => p.text_disabled,
    }
}

pub fn state_text(state: State) -> &'static str {
    match state {
        State::Running => t!("Running"),
        State::Waiting => t!("Waiting for you"),
        State::Idle => t!("Idle"),
        State::Ended => t!("Ended"),
    }
}
