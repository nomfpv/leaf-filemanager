//! The change feed (Location::Changes): every changed, new, deleted and renamed file of the current git repository, grouped by
//! folder, with its state, size and modified time. Selecting a file shows its diff in the preview pane (the pane asks
//! `diff_view::hint_of`, which this page fills). It refreshes itself through the tree watcher (debounced) and has a Refresh button.
//! Read-only: `leaf_dev::gitread` runs `status` only, off the UI thread, and nothing here writes to the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::gitread::{self, Change, Kind};
use rf_core::format;

use crate::diff_view::Hint;
use crate::t;
use crate::theme::{self, Palette};

/// Tab title: "Changes - project".
pub fn title(dir: &Path) -> String {
    format!("{} \u{2014} {}", t!("Changes"), rf_core::history::display_name(dir))
}

pub enum ChangesEvent {
    /// The file now chosen (path, size, modified ms), or none.
    Selected(Option<(PathBuf, u64, i64)>),
    /// Open this file's folder with it selected.
    ShowInFolder(PathBuf),
}

impl EventEmitter<ChangesEvent> for ChangesView {}

struct Item {
    change: Change,
    path: PathBuf,
    size: u64,
    modified_ms: i64,
}

enum Row {
    /// A folder (relative to the repository root, "" for the top) and how many changes are in it.
    Group(String, usize),
    File(usize),
}

enum Phase {
    Loading,
    Ready,
    /// Not in a repository, or git is missing.
    NoRepo,
}

pub struct ChangesView {
    dir: PathBuf,
    root: Option<PathBuf>,
    phase: Phase,
    items: Vec<Item>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    hover: Option<usize>,
    /// Set by the watcher thread when files in the repository changed.
    dirty: Arc<AtomicBool>,
    _watch: Option<leaf_sys::tree_watch::TreeWatch>,
    _task: Option<Task<()>>,
    _poll: Option<Task<()>>,
}

impl Drop for ChangesView {
    fn drop(&mut self) {
        crate::diff_view::forget_hints();
    }
}

fn meta_of(p: &Path) -> (u64, i64) {
    match std::fs::symlink_metadata(p) {
        Ok(m) => (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)),
        Err(_) => (0, 0),
    }
}

impl ChangesView {
    pub fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            dir,
            root: None,
            phase: Phase::Loading,
            items: Vec::new(),
            rows: Vec::new(),
            selected: None,
            hover: None,
            dirty: Arc::new(AtomicBool::new(false)),
            _watch: None,
            _task: None,
            _poll: None,
        };
        this.refresh(cx);
        // a change in the watched tree (flagged from the watcher's thread) is picked up here, at most twice a second
        this._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                if this.update(cx, |v, cx| v.dirty.swap(false, Ordering::AcqRel).then(|| v.refresh(cx))).is_err() {
                    return;
                }
            }
        }));
        this
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The status line while this page is the tab's: "7 changes: 3 changed, 2 new, 1 deleted, 1 renamed".
    pub fn status_text(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let n = |k: Kind| self.items.iter().filter(|i| i.change.kind == k).count();
        let mut parts = Vec::new();
        for (k, label) in [(Kind::Changed, t!("changed")), (Kind::New, t!("new")), (Kind::Deleted, t!("deleted")), (Kind::Renamed, t!("renamed")), (Kind::Conflicted, t!("in conflict"))] {
            if n(k) > 0 {
                parts.push(format!("{} {}", n(k), label));
            }
        }
        format!("{} {}: {}", self.items.len(), if self.items.len() == 1 { t!("change") } else { t!("changes") }, parts.join(", "))
    }

    /// Asks git again, off the UI thread; the list is replaced when the answer comes.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        let work = cx.background_spawn(async move {
            let root = gitread::repo_root(&dir)?;
            let changes = gitread::status(&root)?;
            let items: Vec<Item> = changes
                .into_iter()
                .map(|c| {
                    let path = root.join(&c.path);
                    let (size, modified_ms) = meta_of(&path);
                    Item { change: c, path, size, modified_ms }
                })
                .collect();
            Some((root, items))
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Some((root, items)) => {
                        if v.root.as_ref() != Some(&root) {
                            v.watch(&root);
                        }
                        v.root = Some(root);
                        v.items = items;
                        v.phase = Phase::Ready;
                        v.arrange();
                    }
                    None => {
                        v.phase = Phase::NoRepo;
                        v.items.clear();
                        v.rows.clear();
                    }
                }
                let still = v.selected.as_ref().is_some_and(|s| v.items.iter().any(|i| &i.path == s));
                if !still && v.selected.take().is_some() {
                    cx.emit(ChangesEvent::Selected(None));
                } else if let Some(pv) = v.selected.clone() {
                    // (the selected file's size and time may be new: the pane looks again)
                    if let Some(i) = v.items.iter().find(|i| i.path == pv) {
                        cx.emit(ChangesEvent::Selected(Some((i.path.clone(), i.size, i.modified_ms))));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn watch(&mut self, root: &Path) {
        let dirty = self.dirty.clone();
        self._watch = leaf_sys::tree_watch::watch_tree(root, std::time::Duration::from_millis(900), move |c| {
            // git's own bookkeeping inside .git is not a change of the files
            let only_git = !c.rescan && !c.paths.is_empty() && c.paths.iter().all(|p| p.components().any(|x| x.as_os_str() == ".git"));
            if !only_git {
                dirty.store(true, Ordering::Release);
            }
        })
        .map_err(|e| log::warn!("changes: cannot watch {}: {e:#}", root.display()))
        .ok();
    }

    /// Sorts the items by folder then name, builds the rows with their folder headers, and hands the pane its hints.
    fn arrange(&mut self) {
        let dir_of = |p: &str| p.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
        self.items.sort_by(|a, b| {
            dir_of(&a.change.path).to_lowercase().cmp(&dir_of(&b.change.path).to_lowercase()).then_with(|| a.change.path.to_lowercase().cmp(&b.change.path.to_lowercase()))
        });
        let mut groups: BTreeMap<usize, (String, usize)> = BTreeMap::new();
        let mut rows = Vec::new();
        let mut last: Option<String> = None;
        for (i, it) in self.items.iter().enumerate() {
            let d = dir_of(&it.change.path);
            if last.as_ref() != Some(&d) {
                groups.insert(rows.len(), (d.clone(), 0));
                rows.push(Row::Group(d.clone(), 0));
                last = Some(d);
            }
            if let Some((_, (_, n))) = groups.iter_mut().next_back() {
                *n += 1;
            }
            rows.push(Row::File(i));
        }
        for (at, (_, n)) in groups {
            if let Row::Group(_, c) = &mut rows[at] {
                *c = n;
            }
        }
        self.rows = rows;
        if let Some(root) = &self.root {
            crate::diff_view::remember_hints(
                self.items.iter().map(|i| (i.path.clone(), Hint { root: root.clone(), rel: i.change.path.clone(), from: i.change.from.clone() })).collect(),
            );
        }
    }

    /// Selects the first file whose path holds `part` (the scripted test driver).
    pub fn pick(&mut self, part: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.items.iter().position(|i| i.change.path.contains(part)) {
            self.select(i, cx);
        }
    }

    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(it) = self.items.get(i) else { return };
        self.selected = Some(it.path.clone());
        cx.emit(ChangesEvent::Selected(Some((it.path.clone(), it.size, it.modified_ms))));
        cx.notify();
    }
}

fn kind_color(k: Kind, p: &Palette) -> Hsla {
    match k {
        Kind::New => p.success,
        Kind::Changed => p.warning,
        Kind::Deleted | Kind::Conflicted => p.danger,
        Kind::Renamed => p.accent,
    }
}

fn button(id: &'static str, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .whitespace_nowrap()
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .text_color(p.text)
        .hover(|s| s.bg(p.hover))
        .child(label.into())
}

impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let summary = match self.phase {
            Phase::Loading => t!("Looking at the repository…").to_string(),
            Phase::NoRepo => t!("This folder is not in a git repository, or git could not be run.").to_string(),
            Phase::Ready if self.items.is_empty() => t!("No changes: the files match the last commit.").to_string(),
            Phase::Ready => self.status_text(),
        };
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::GitCompare).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary))
            .child(button("changes-refresh", t!("Refresh"), &p).on_click(cx.listener(|v, _, _, cx| v.refresh(cx))));
        let body = if self.rows.is_empty() {
            div().flex_1().into_any_element()
        } else {
            uniform_list(
                "changes-rows",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                    let p = Palette::for_window(window);
                    range.map(|i| this.render_row(i, &p, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .p_1()
            .into_any_element()
        };
        v_flex().size_full().child(top).child(body)
    }
}

impl ChangesView {
    fn render_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        match &self.rows[i] {
            Row::Group(dir, n) => {
                let name = if dir.is_empty() { t!("(top of the repository)").to_string() } else { dir.replace('/', "\\") };
                h_flex()
                    .id(("changes-row", i))
                    .w_full()
                    .h(theme::ROW_H)
                    .px_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(p.text_secondary)
                    .child(Icon::new(L::Folder).size(px(13.)).text_color(p.text_secondary))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().text_color(p.text_tertiary).child(n.to_string()))
                    .into_any_element()
            }
            Row::File(idx) => {
                let it = &self.items[*idx];
                let on = self.selected.as_ref() == Some(&it.path);
                let hov = self.hover == Some(i);
                let c = &it.change;
                let name = c.path.rsplit('/').next().unwrap_or(&c.path).to_string();
                let color = kind_color(c.kind, p);
                let state = format!("{}{}", crate::i18n::t(c.kind.label()), if c.staged { format!(" ({})", t!("staged")) } else { String::new() });
                let was = c.from.as_ref().map(|f| format!("\u{2190} {}", f.replace('/', "\\")));
                let size = if c.kind == Kind::Deleted { String::new() } else { format::size(it.size) };
                let when = if it.modified_ms > 0 { format::datetime(it.modified_ms) } else { String::new() };
                let p2 = it.path.clone();
                let idx = *idx;
                h_flex()
                    .id(("changes-row", i))
                    .w_full()
                    .h(theme::ROW_H)
                    .pl_6()
                    .pr_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .cursor_pointer()
                    .when(hov && !on, |d| d.bg(p.hover))
                    .when(on, |d| d.bg(p.selection))
                    .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                        let want = h.then_some(i);
                        if *h && v.hover != want || !*h && v.hover == Some(i) {
                            v.hover = want;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |v, _, _, cx| v.select(idx, cx)))
                    .child(div().w(px(16.)).flex_shrink_0().font_weight(FontWeight::SEMIBOLD).text_color(color).child(c.kind.letter(c.untracked).to_string()))
                    .child(div().flex_shrink_0().w(px(170.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text).child(crate::rtl::display_text(&name)))
                    .child(div().flex_shrink_0().w(px(110.)).overflow_hidden().whitespace_nowrap().text_xs().text_color(color).child(state))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(was.unwrap_or_default()))
                    .child(div().w(px(80.)).flex_shrink_0().text_xs().text_right().text_color(p.text).child(size))
                    .child(div().w(px(120.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(when))
                    .child(
                        div()
                            .id(("changes-open", i))
                            .size(px(24.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme::RADIUS_SM)
                            .hover(|s| s.bg(p.pressed))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Open location")).build(window, cx))
                            .child(Icon::new(L::FolderSearch).size(px(14.)).text_color(p.text_secondary))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ChangesEvent::ShowInFolder(p2.clone()));
                            })),
                    )
                    .into_any_element()
            }
        }
    }
}
