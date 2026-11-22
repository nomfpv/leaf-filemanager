//! The Agents page (`Location::Agents`, PLAN F2): the coding agent sessions running now and the recent ones, grouped by project folder
//! (a git worktree under its repository), each with a state dot, its branch, when it last did something, and two buttons: open its
//! folder, open a terminal in it. The list comes from `agents` (kept fresh by file watching); nothing here reads a file.
//! Read-only: it never starts, stops or answers an agent. The session title (the first prompt line) is shown here and nowhere else.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_agents::{Session, State};

use crate::t;
use crate::theme::{self, Palette};

pub enum AgentsEvent {
    /// Open this folder in this tab.
    OpenFolder(PathBuf),
    /// Open the Review side of this folder.
    Review(PathBuf),
}

/// The file whose "Which session changed this?" the next Agents page opens on (set just before the page is opened).
static WANT_WHO: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn want_who(path: PathBuf) {
    if let Ok(mut g) = WANT_WHO.lock() {
        *g = Some(path);
    }
}

/// "Which session changed this?": the sessions whose edit or write calls named the file, being looked for.
struct Who {
    file: PathBuf,
    /// `None` while the sessions are being read.
    found: Option<Vec<Session>>,
    cancel: Arc<AtomicBool>,
}

impl EventEmitter<AgentsEvent> for AgentsView {}

pub struct AgentsView {
    hover: Option<usize>,
    /// The session page, while one is open.
    open: Option<Entity<crate::session_view::SessionView>>,
    _open_sub: Option<Subscription>,
    who: Option<Who>,
}

impl Drop for AgentsView {
    fn drop(&mut self) {
        if let Some(w) = &self.who {
            w.cancel.store(true, Ordering::Relaxed);
        }
    }
}

enum Row {
    Group { name: String, path: PathBuf },
    Session(Session),
}

impl AgentsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut v = Self { hover: None, open: None, _open_sub: None, who: None };
        let want = WANT_WHO.lock().ok().and_then(|mut g| g.take());
        if let Some(file) = want {
            v.ask_who(file, cx);
        }
        v
    }

    /// Opens the page of a session (from `jump`: a byte position in its file).
    fn open_session(&mut self, session: Session, jump: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        let e = cx.new(|cx| crate::session_view::SessionView::new(session, jump, window, cx));
        self._open_sub = Some(cx.subscribe_in(&e, window, |this, _, ev: &crate::session_view::SessionEvent, window, cx| {
            use crate::session_view::SessionEvent as E;
            match ev {
                E::Back => {
                    this.open = None;
                    this._open_sub = None;
                }
                E::OpenFolder(d) => cx.emit(AgentsEvent::OpenFolder(d.clone())),
                E::Review(d) => cx.emit(AgentsEvent::Review(d.clone())),
                E::OpenSession(s, off) => this.open_session(s.clone(), Some(*off), window, cx),
            }
            cx.notify();
        }));
        cx.observe(&e, |_, _, cx| cx.notify()).detach();
        self.open = Some(e);
        cx.notify();
    }

    /// Looks, off the UI thread, for the sessions of the folders above `file` whose tool calls wrote it, newest first.
    fn ask_who(&mut self, file: PathBuf, cx: &mut Context<Self>) {
        if let Some(w) = &self.who {
            w.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.who = Some(Who { file: file.clone(), found: None, cancel: cancel.clone() });
        let known = crate::agents::sessions(cx);
        let file2 = file.clone();
        let cancel2 = cancel.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let (file, cancel) = (file2, cancel2);
                    let key = leaf_agents::folder_key(&file);
                    let mut out: Vec<Session> = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    for a in leaf_agents::adapters() {
                        let mut files = Vec::new();
                        for dir in file.ancestors().skip(1).filter(|d| !d.as_os_str().is_empty()) {
                            files.extend(a.folder_sessions(dir).into_iter().map(|f| (dir.to_path_buf(), f)));
                        }
                        files.sort_by(|a, b| b.1.modified_ms.cmp(&a.1.modified_ms));
                        // the newest sixty are looked into
                        for (dir, f) in files.into_iter().take(60) {
                            if cancel.load(Ordering::Relaxed) {
                                return out;
                            }
                            if !seen.insert(f.id.clone()) || !leaf_agents::transcript::wrote(&f.path, &key, &cancel) {
                                continue;
                            }
                            out.push(known.iter().find(|s| s.id == f.id).cloned().unwrap_or(Session {
                                agent: a.id(),
                                id: f.id,
                                repo: leaf_agents::worktree_repo(&dir),
                                folder: dir,
                                branch: None,
                                state: State::Ended,
                                last_activity_ms: f.modified_ms,
                                title: String::new(),
                            }));
                        }
                    }
                    out.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
                    out
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if let Some(w) = v.who.as_mut().filter(|w| !w.cancel.load(Ordering::Relaxed)) {
                    w.found = Some(found);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// What the status line says on this page.
    pub fn status_text(&self, cx: &App) -> String {
        if let Some(o) = &self.open {
            return o.read(cx).status_text();
        }
        let s = crate::agents::sessions(cx);
        let open = s.iter().filter(|s| s.state.is_live()).count();
        format!("{} {}, {} {}", s.len(), if s.len() == 1 { t!("session") } else { t!("sessions") }, open, t!("open"))
    }
}

/// Group headers and sessions in one flat list: groups by their newest session, sessions newest first (the input is).
fn rows(sessions: &[Session]) -> Vec<Row> {
    let mut groups: Vec<(PathBuf, Vec<&Session>)> = Vec::new();
    for s in sessions {
        let g = s.group().to_path_buf();
        match groups.iter_mut().find(|(p, _)| *p == g) {
            Some((_, v)) => v.push(s),
            None => groups.push((g, vec![s])),
        }
    }
    let mut out = Vec::new();
    for (path, v) in groups {
        out.push(Row::Group { name: rf_core::history::display_name(&path), path });
        out.extend(v.into_iter().cloned().map(Row::Session));
    }
    out
}

/// The branch, or for a worktree its folder name and the branch.
fn lane_text(s: &Session) -> String {
    let branch = s.branch.clone().unwrap_or_default();
    if s.repo.is_some() {
        let folder = s.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if branch.is_empty() || branch == folder { folder } else { format!("{folder} \u{b7} {branch}") }
    } else {
        branch
    }
}

impl Render for AgentsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        if let Some(o) = &self.open {
            return div().size_full().child(o.clone()).into_any_element();
        }
        let enabled = cx.global::<crate::settings::Settings>().ai_layer;
        let all = match &self.who {
            Some(w) => Arc::new(w.found.clone().unwrap_or_default()),
            None => crate::agents::sessions(cx),
        };
        let list: Vec<Row> = if self.who.is_some() { all.iter().cloned().map(Row::Session).collect() } else { rows(&all) };
        let n = list.len();
        let who_name = |w: &Who| w.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let summary = if let Some(w) = &self.who {
            match &w.found {
                None => format!("{} {}\u{2026}", t!("Looking for the sessions that changed"), who_name(w)),
                Some(f) if f.is_empty() => format!("{} {}", t!("No session found that changed"), who_name(w)),
                Some(f) => format!("{} {} ({})", t!("Sessions that changed"), who_name(w), f.len()),
            }
        } else if !enabled {
            t!("The agent layer is off. Turn it on in Settings under Developer.").to_string()
        } else if all.is_empty() {
            t!("No agent sessions in the last three days.").to_string()
        } else {
            self.status_text(cx)
        };
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::Brain).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(if self.who.is_some() { summary } else { format!("{}: {summary}", t!("Agents")) }))
            .when(self.who.is_some(), |d| {
                d.child(icon_button(("agents-who-close", 0), L::X, t!("Show all sessions"), &p).on_click(cx.listener(|v, _, _, cx| {
                    if let Some(w) = v.who.take() {
                        w.cancel.store(true, Ordering::Relaxed);
                    }
                    cx.notify();
                })))
            });
        let body = uniform_list(
            "agents-rows",
            n,
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let p = Palette::for_window(window);
                let list: Vec<Row> = match &this.who {
                    Some(w) => w.found.clone().unwrap_or_default().into_iter().map(Row::Session).collect(),
                    None => rows(&crate::agents::sessions(cx)),
                };
                range.filter_map(|r| list.get(r).map(|row| this.render_row(r, row, &p, cx))).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .p_1();
        // (D13, F5) the branch map of the lanes and the usage numbers, above the sessions
        let (map, usage) = if self.who.is_some() || !enabled { (None, None) } else { (crate::branch_map::view(cx, &p), crate::agent_usage::page(cx, &p)) };
        v_flex().size_full().child(top).children(map).children(usage).child(body).into_any_element()
    }
}

pub(crate) fn icon_button(id: (&'static str, usize), icon: L, tip: &'static str, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .hover(|s| s.bg(p.pressed))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        .child(Icon::new(icon).size(px(14.)).text_color(p.text_secondary))
}

impl AgentsView {
    fn render_row(&self, i: usize, row: &Row, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        match row {
            Row::Group { name, path } => h_flex()
                .id(("agents-group", i))
                .w_full()
                .h(theme::ROW_H)
                .px_2()
                .gap_2()
                .items_center()
                .flex_shrink_0()
                .when(i > 0, |d| d.mt_2())
                .child(Icon::new(L::Folder).size(px(14.)).text_color(p.accent))
                .child(div().flex_shrink_0().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(name.clone()))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(path.to_string_lossy().into_owned()))
                .into_any_element(),
            Row::Session(s) => {
                let hov = self.hover == Some(i);
                let (folder_a, folder_b): (PathBuf, PathBuf) = (s.folder.clone(), s.folder.clone());
                let title = if s.title.is_empty() { t!("(no title)").to_string() } else { s.title.clone() };
                let dim = s.state == State::Ended;
                let (open_s, resume_s) = (s.clone(), s.clone());
                let resumable = dim && crate::agents::can_resume(s);
                // (D6) a session in a lane: the lane's colour before its folder and branch
                let lane = crate::lanes::color_of_dir(cx, &s.folder, p);
                h_flex()
                    .id(("agents-row", i))
                    .w_full()
                    .h(theme::ROW_H)
                    .pl(px(28.))
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, _, window, cx| v.open_session(open_s.clone(), None, window, cx)))
                    .when(hov, |d| d.bg(p.hover))
                    .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                        let want = h.then_some(i);
                        if *h && v.hover != want || !*h && v.hover == Some(i) {
                            v.hover = want;
                            cx.notify();
                        }
                    }))
                    .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(crate::agents::state_color(s.state, p)))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(if dim { p.text_secondary } else { p.text }).child(title))
                    .children(lane.map(|c| div().size(px(6.)).flex_shrink_0().rounded_full().bg(c)))
                    .child(div().w(px(180.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_secondary).child(lane_text(s)))
                    .child(div().w(px(110.)).flex_shrink_0().text_xs().text_color(crate::agents::state_color(s.state, p)).child(crate::agents::state_text(s.state)))
                    .child(div().w(px(90.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(crate::agents::ago(s.last_activity_ms)))
                    .when(resumable, |d| d.child(icon_button(("agents-resume", i), L::Play, t!("Resume in terminal"), p).on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        crate::agents::resume(&resume_s, cx);
                    })))
                    .child(icon_button(("agents-open", i), L::FolderSearch, t!("Open folder"), p).on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(AgentsEvent::OpenFolder(folder_a.clone()));
                    })))
                    .child(icon_button(("agents-term", i), L::SquareTerminal, t!("Open terminal here"), p).on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        crate::terminal::open(folder_b.clone(), cx);
                    })))
                    .into_any_element()
            }
        }
    }
}
