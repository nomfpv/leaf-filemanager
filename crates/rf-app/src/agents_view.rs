//! The Agents page (`Location::Agents`, PLAN F2): the coding agent sessions running now and the recent ones, grouped by project folder
//! (a git worktree under its repository), each with a state dot, its branch, when it last did something, and two buttons: open its
//! folder, open a terminal in it. The list comes from `agents` (kept fresh by file watching); nothing here reads a file.
//! Read-only: it never starts, stops or answers an agent. The session title (the first prompt line) is shown here and nowhere else.

use std::path::PathBuf;

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
}

impl EventEmitter<AgentsEvent> for AgentsView {}

pub struct AgentsView {
    hover: Option<usize>,
}

enum Row {
    Group { name: String, path: PathBuf },
    Session(Session),
}

impl AgentsView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self { hover: None }
    }

    /// What the status line says on this page.
    pub fn status_text(&self, cx: &App) -> String {
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
        let enabled = cx.global::<crate::settings::Settings>().ai_layer;
        let all = crate::agents::sessions(cx);
        let list = rows(&all);
        let n = list.len();
        let summary = if !enabled {
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
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(format!("{}: {summary}", t!("Agents"))));
        let body = uniform_list(
            "agents-rows",
            n,
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let p = Palette::for_window(window);
                let list = rows(&crate::agents::sessions(cx));
                range.filter_map(|r| list.get(r).map(|row| this.render_row(r, row, &p, cx))).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .p_1();
        v_flex().size_full().child(top).child(body)
    }
}

fn icon_button(id: (&'static str, usize), icon: L, tip: &'static str, p: &Palette) -> Stateful<Div> {
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
                    .child(div().w(px(180.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_secondary).child(lane_text(s)))
                    .child(div().w(px(110.)).flex_shrink_0().text_xs().text_color(crate::agents::state_color(s.state, p)).child(crate::agents::state_text(s.state)))
                    .child(div().w(px(90.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(crate::agents::ago(s.last_activity_ms)))
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
