//! Command palette (Ctrl+Shift+P / F1): fuzzy-filter every command and the folders visited before (`folder_history`, unless
//! `palette_folder_history` is off), or type a path to jump. Opened by Ctrl+P (`SearchFolderHistory`) it lists past folders only;
//! opened by Ctrl+Shift+A (`SearchTabs`) the window's open tabs and the ones closed recently (issue 79, as FilePilot's tab search).

use std::path::PathBuf;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::{self, Palette};

use crate::registry::CommandSpec;

type Cmd = &'static CommandSpec;

/// The palette's commands, from the registry (its order).
fn commands() -> Vec<Cmd> {
    crate::registry::palette().collect()
}
pub enum PaletteEvent {
    Dismiss,
    Run(Box<dyn Action>),
    Navigate(PathBuf),
    /// A tab of the tab search: an open one to switch to, or a closed one to open again (`TabEntry::id`).
    Tab { closed: bool, id: usize },
}

/// One tab the tab search lists.
#[derive(Clone, Debug, PartialEq)]
pub struct TabEntry {
    pub label: String,
    /// The tab's place, written out (its path), matched as well.
    pub detail: String,
    pub closed: bool,
    /// The open tab's index in the window, or the closed one's in the window's list of closed tabs.
    pub id: usize,
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

/// What the palette lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteMode {
    /// Commands, and past folders once something is typed (when the setting allows).
    Commands { with_folders: bool },
    /// Past folders only (Ctrl+P).
    Folders,
    /// The window's tabs, open and recently closed (Ctrl+Shift+A).
    Tabs,
    /// The git worktrees of the open repository (the rows are `TabEntry`s: the branch and the folder).
    Worktrees,
}

/// One row under the typed path's row.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// An index into `cmds`.
    Cmd(usize),
    Folder(PathBuf),
    /// An index into `tabs`.
    Tab(usize),
}

/// How many rows are listed under the typed path's row.
const VISIBLE: usize = 12;
/// At most this many past folders among the commands (the folder-only mode lists [`VISIBLE`]).
const FOLDERS_AMONG_COMMANDS: usize = 5;

pub struct CommandPalette {
    input: Entity<InputState>,
    mode: PaletteMode,
    cmds: Vec<Cmd>,
    folders: Vec<crate::folder_history::Visit>,
    tabs: Vec<TabEntry>,
    /// What matches the query, best first (plus an optional path row at the top).
    filtered: Vec<Row>,
    path: Option<PathBuf>,
    selected: usize,
    _sub: Subscription,
}

/// How well `label` matches the lowercase query `q`: a substring (earlier is better), else its letters in order (fewer gaps is
/// better); `None` when it does not match.
pub(crate) fn score(label: &str, q: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let l = label.to_lowercase();
    if let Some(i) = l.find(q) {
        return Some(100 - i as i32);
    }
    // subsequence match ("ykl" → "yeni klasör")
    let mut it = l.chars();
    let mut gaps = 0;
    for qc in q.chars() {
        let mut found = false;
        for c in it.by_ref() {
            if c == qc {
                found = true;
                break;
            }
            gaps += 1;
        }
        if !found {
            return None;
        }
    }
    Some(50 - gaps)
}

fn as_path(q: &str) -> Option<PathBuf> {
    let t = q.trim().trim_matches('"');
    let looks = t.len() >= 2 && (t.as_bytes()[1] == b':' || t.starts_with("\\\\") || t.starts_with('%') || (!cfg!(windows) && t.starts_with('/')));
    looks.then(|| PathBuf::from(t))
}

/// The rows matching `query`, best first. Commands: every one while nothing is typed; past folders join them once something is
/// typed (at most [`FOLDERS_AMONG_COMMANDS`], a command first when both match as well). Folder mode: past folders only.
/// The tabs matching `q` (lower case), best first; every tab, open ones first, while nothing is typed.
fn filter_tabs(tabs: &[TabEntry], q: &str) -> Vec<Row> {
    let mut by: Vec<(i32, usize)> = tabs
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let s = score(&t.label, q).map(|s| s + 10).into_iter().chain(score(&t.detail, q)).max()?;
            Some((s - if t.closed { 1 } else { 0 }, i))
        })
        .collect();
    by.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    by.into_iter().map(|(_, i)| Row::Tab(i)).collect()
}

fn filter(mode: PaletteMode, cmds: &[Cmd], folders: &[crate::folder_history::Visit], query: &str) -> Vec<Row> {
    let q = query.trim().to_lowercase();
    let cmds = if matches!(mode, PaletteMode::Folders | PaletteMode::Tabs) { &[][..] } else { cmds };
    let mut by_cmd: Vec<(i32, usize)> = cmds.iter().enumerate().filter_map(|(i, c)| score(crate::i18n::t(c.title), &q).map(|s| (s, i))).collect();
    by_cmd.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let by_folder: Vec<(i32, PathBuf)> = match mode {
        PaletteMode::Commands { with_folders: true } if !q.is_empty() => {
            crate::folder_history::rank(folders, &q).into_iter().take(FOLDERS_AMONG_COMMANDS).map(|(s, v)| (s, v.path.clone())).collect()
        }
        PaletteMode::Commands { .. } => Vec::new(),
        PaletteMode::Folders => crate::folder_history::rank(folders, &q).into_iter().map(|(s, v)| (s, v.path.clone())).collect(),
        PaletteMode::Tabs | PaletteMode::Worktrees => Vec::new(),
    };
    // both lists are best first: merge them by how well they match
    let mut out = Vec::with_capacity(by_cmd.len() + by_folder.len());
    let (mut c, mut f) = (by_cmd.into_iter().peekable(), by_folder.into_iter().peekable());
    loop {
        let take_cmd = match (c.peek(), f.peek()) {
            (Some(a), Some(b)) => a.0 >= b.0,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_cmd {
            out.extend(c.next().map(|(_, i)| Row::Cmd(i)));
        } else {
            out.extend(f.next().map(|(_, p)| Row::Folder(p)));
        }
    }
    out
}

impl CommandPalette {
    /// `folders`: the remembered folders, most recent first (`folder_history::snapshot`); unused when the mode lists none.
    pub fn new(mode: PaletteMode, folders: Vec<crate::folder_history::Visit>, tabs: Vec<TabEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hint = match mode {
            PaletteMode::Commands { .. } => crate::t!("Search commands or type a path…"),
            PaletteMode::Folders => crate::t!("Search past folders…"),
            PaletteMode::Tabs => crate::t!("Search tabs…"),
            PaletteMode::Worktrees => crate::t!("Search worktrees…"),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(hint));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => this.refilter(cx),
            InputEvent::PressEnter { .. } => this.run_selected(cx),
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        let cmds = if matches!(mode, PaletteMode::Folders | PaletteMode::Tabs | PaletteMode::Worktrees) { Vec::new() } else { commands().into_iter().filter(|c| c.available(cx)).collect() };
        let mut this = Self { input, mode, cmds, folders, tabs, filtered: Vec::new(), path: None, selected: 0, _sub: sub };
        this.refilter(cx);
        this
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value().to_string();
        // (the tab search takes no typed path)
        self.path = as_path(&raw).filter(|_| !matches!(self.mode, PaletteMode::Tabs | PaletteMode::Worktrees)).map(|_| PathBuf::from(raw.trim().trim_matches('"')));
        self.filtered = if matches!(self.mode, PaletteMode::Tabs | PaletteMode::Worktrees) { filter_tabs(&self.tabs, &raw.trim().to_lowercase()) } else { filter(self.mode, &self.cmds, &self.folders, &raw) };
        self.selected = 0;
        cx.notify();
    }

    /// The rows drawn (and walked by Up/Down): the path row and at most [`VISIBLE`] others.
    fn rows(&self) -> usize {
        self.filtered.len().min(VISIBLE) + usize::from(self.path.is_some())
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        let mut ix = self.selected;
        if let Some(p) = &self.path {
            if ix == 0 {
                cx.emit(PaletteEvent::Navigate(p.clone()));
                return;
            }
            ix -= 1;
        }
        match self.filtered.get(ix) {
            Some(Row::Cmd(ci)) => cx.emit(PaletteEvent::Run(self.cmds[*ci].make_action())),
            Some(Row::Folder(p)) => cx.emit(PaletteEvent::Navigate(p.clone())),
            Some(Row::Tab(ti)) => {
                let t = &self.tabs[*ti];
                if self.mode == PaletteMode::Worktrees {
                    cx.emit(PaletteEvent::Navigate(PathBuf::from(&t.detail)))
                } else {
                    cx.emit(PaletteEvent::Tab { closed: t.closed, id: t.id })
                }
            }
            None => {}
        }
    }

    fn row(&self, ix: usize, icon: L, label: String, keys: SharedString, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let sel = ix == self.selected;
        h_flex()
            .id(("pal", ix))
            .h(px(34.))
            .px_3()
            .gap_3()
            .items_center()
            .rounded(theme::RADIUS_SM)
            .text_sm()
            .when(sel, |d| d.bg(p.selection))
            .when(!sel, |d| d.hover(|s| s.bg(p.hover)))
            .child(Icon::new(icon).size(px(16.)).text_color(if sel { p.accent } else { p.text_secondary }))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
            // (a folder's full path here can be long: it gives way first)
            .when(!keys.is_empty(), |d| {
                d.child(
                    div()
                        .max_w(px(320.))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(p.text_tertiary)
                        .child(keys),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = ix;
                this.run_selected(cx);
            }))
            .into_any_element()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let mut list = v_flex().gap_0p5();
        let mut ix = 0;
        if let Some(path) = self.path.clone() {
            list = list.child(self.row(ix, L::FolderOpen, format!("{} {}", crate::t!("Go to:"), path.display()), "Enter".into(), &p, cx));
            ix += 1;
        }
        let visible: Vec<Row> = self.filtered.iter().take(VISIBLE).cloned().collect();
        for r in visible {
            let (icon, label, keys): (L, String, SharedString) = match r {
                Row::Cmd(ci) => {
                    let c = &self.cmds[ci];
                    (c.icon, crate::i18n::t(c.title).to_string(), c.shown_keys().into())
                }
                Row::Folder(path) => (
                    L::Folder,
                    rf_core::history::display_name(&path),
                    path.to_string_lossy().into_owned().into(),
                ),
                // (a closed tab says so before its place)
                Row::Tab(ti) => {
                    let t = &self.tabs[ti];
                    let detail = if t.closed { format!("{} · {}", crate::t!("Closed"), t.detail) } else { t.detail.clone() };
                    (if t.closed { L::RotateCcw } else if self.mode == PaletteMode::Worktrees { L::GitBranch } else { L::PanelTop }, t.label.clone(), detail.into())
                }
            };
            list = list.child(self.row(ix, icon, label, keys, &p, cx));
            ix += 1;
        }
        // scrim + card
        div()
            .id("palette-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(hsla(0., 0., 0., 0.25))
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)))
            .child(
                v_flex()
                    .id("palette")
                    .absolute()
                    .top(px(72.))
                    .left(relative(0.5))
                    .ml(px(-290.))
                    .w(px(580.))
                    .p_2()
                    .gap_2()
                    .rounded(theme::RADIUS)
                    .bg(p.card_bg)
                    .border_1()
                    .border_color(p.border)
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        let n = this.rows();
                        match ev.keystroke.key.as_str() {
                            "down" if n > 0 => {
                                this.selected = (this.selected + 1) % n;
                                cx.stop_propagation();
                                cx.notify();
                            }
                            "up" if n > 0 => {
                                this.selected = (this.selected + n - 1) % n;
                                cx.stop_propagation();
                                cx.notify();
                            }
                            "escape" => {
                                cx.stop_propagation();
                                cx.emit(PaletteEvent::Dismiss);
                            }
                            _ => {}
                        }
                    }))
                    .child(
                        div()
                            .h(px(38.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(theme::RADIUS_SM)
                            .bg(p.hover)
                            .child(
                                Input::new(&self.input)
                                    .appearance(false)
                                    .prefix(
                                        Icon::new(if self.mode == PaletteMode::Folders { L::Clock } else { L::Command })
                                            .size(px(14.))
                                            .text_color(p.text_secondary),
                                    ),
                            ),
                    )
                    .child(list)
                    .when(self.rows() == 0, |d| {
                        let none = match self.mode {
                            PaletteMode::Commands { .. } => crate::t!("No matching commands"),
                            PaletteMode::Folders if self.folders.is_empty() => crate::t!("No folders visited yet"),
                            PaletteMode::Folders => crate::t!("No matching folders"),
                            PaletteMode::Tabs => crate::t!("No matching tab"),
                            PaletteMode::Worktrees => crate::t!("No matching worktree"),
                        };
                        d.child(div().p_3().text_sm().text_color(p.text_secondary).child(none))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{PaletteMode, Row, as_path, commands, filter, score};

    #[test]
    fn the_tab_search_finds_a_tab_by_its_name_or_place_open_ones_first() {
        use super::{TabEntry, filter_tabs};
        let t = |label: &str, detail: &str, closed: bool, id: usize| TabEntry { label: label.into(), detail: detail.into(), closed, id };
        let tabs = vec![t("Downloads", "C:/Users/user/Downloads", false, 0), t("Projects", "D:/Work/Projects", false, 1), t("Downloads", "E:/Old/Downloads", true, 0)];
        assert_eq!(filter_tabs(&tabs, ""), vec![Row::Tab(0), Row::Tab(1), Row::Tab(2)], "all, open ones first");
        assert_eq!(filter_tabs(&tabs, "down"), vec![Row::Tab(0), Row::Tab(2)], "the open one before the closed one of the same name");
        assert_eq!(filter_tabs(&tabs, "work"), vec![Row::Tab(1)], "by its place too");
        assert!(filter_tabs(&tabs, "zzz").is_empty());
    }
    use crate::folder_history::Visit;
    use std::path::PathBuf;

    fn visits() -> Vec<Visit> {
        [r"D:\Projects\leaf", r"C:\Users\user\Downloads", r"C:\Windows"]
            .iter()
            .enumerate()
            .map(|(i, p)| Visit { path: PathBuf::from(p), last_ms: 10 - i as i64, count: 1 })
            .collect()
    }

    fn folders(rows: &[Row]) -> Vec<String> {
        rows.iter().filter_map(|r| if let Row::Folder(p) = r { Some(p.to_string_lossy().into_owned()) } else { None }).collect()
    }

    #[test]
    fn folders_join_the_commands_only_once_typed_and_only_when_allowed() {
        let (cmds, v) = (commands(), visits());
        let on = PaletteMode::Commands { with_folders: true };
        assert!(folders(&filter(on, &cmds, &v, "")).is_empty(), "nothing typed: the commands as before");
        assert_eq!(folders(&filter(on, &cmds, &v, "downl")), [r"C:\Users\user\Downloads"]);
        assert!(folders(&filter(PaletteMode::Commands { with_folders: false }, &cmds, &v, "downl")).is_empty());
    }

    #[test]
    fn folder_mode_lists_past_folders_only_most_recent_first() {
        let (cmds, v) = (commands(), visits());
        let all = filter(PaletteMode::Folders, &cmds, &v, "");
        assert_eq!(folders(&all), [r"D:\Projects\leaf", r"C:\Users\user\Downloads", r"C:\Windows"]);
        assert_eq!(all.len(), 3, "no commands");
        assert_eq!(folders(&filter(PaletteMode::Folders, &cmds, &v, "leaf")), [r"D:\Projects\leaf"]);
    }

    #[test]
    fn fuzzy() {
        assert!(score("New folder", "fold").unwrap() > score("New folder", "nfl").unwrap());
        assert!(score("New folder", "xyz").is_none());
        assert_eq!(as_path(r"C:\Windows"), Some(PathBuf::from(r"C:\Windows")));
        assert!(as_path("new").is_none());
    }
}
