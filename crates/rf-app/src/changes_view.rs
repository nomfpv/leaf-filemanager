//! The change feed (Location::Changes): every changed, new, deleted and renamed file of the current git repository, grouped by
//! folder, with its state, size and modified time. Selecting a file shows its diff in the preview pane (the pane asks
//! `diff_view::hint_of`, which this page fills). It refreshes itself through the tree watcher (debounced) and has a Refresh button.
//! Reads through `leaf_dev::gitread` off the UI thread; the one write is "Revert this file" (asked first, the current content goes
//! to the Trash, then git restores HEAD's version).
//!
//! Its other side, "Checkpoints", lists the folder's checkpoints (`leaf_dev::checkpoint`: hidden refs in a repository, copies in
//! leaf's data folder elsewhere) and restores one: first the list of what would change, come back or was added since (each file's
//! difference in the preview pane), then the restore, which takes an automatic checkpoint first so "Undo restore" is one click.
//! That side works outside a repository too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::checkpoint::{self, Checkpoint, Plan, Restored, Store};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Changes,
    Checkpoints,
}

/// The restore that was asked for: its checkpoint, what would change, and the tick for files added since.
struct Preview {
    cp: Checkpoint,
    plan: Option<Arc<Plan>>,
    error: Option<String>,
    remove_added: bool,
    selected: Option<String>,
}

/// The last restore (for "Undo restore") or its undo.
struct Last {
    done: Arc<Restored>,
    when_ms: i64,
    undone: bool,
}

#[derive(Default)]
struct Cps {
    store: Option<Arc<Store>>,
    list: Vec<Checkpoint>,
    loaded: bool,
    error: Option<String>,
    preview: Option<Preview>,
    /// What runs now ("Restoring…"): the buttons wait.
    busy: Option<SharedString>,
    last: Option<Last>,
    hover: Option<usize>,
    /// Listing and the preview's comparison.
    _task: Option<Task<()>>,
    /// The restore (its own slot: a Refresh must never drop the task that reports it).
    _restore: Option<Task<()>>,
}

pub struct ChangesView {
    dir: PathBuf,
    /// The tab this page is in (the "New checkpoint" button opens its question through it).
    tab: WeakEntity<crate::folder_view::FolderView>,
    mode: Mode,
    cps: Cps,
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
    pub fn new(dir: PathBuf, tab: WeakEntity<crate::folder_view::FolderView>, cx: &mut Context<Self>) -> Self {
        let mode = if crate::checkpoint_ui::take_want(&dir) { Mode::Checkpoints } else { Mode::Changes };
        let mut this = Self {
            tab,
            mode,
            cps: Cps::default(),
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
        if mode == Mode::Checkpoints {
            this.load_checkpoints(cx);
        }
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
        if self.mode == Mode::Checkpoints {
            return match self.cps.list.len() {
                0 => String::new(),
                1 => t!("1 checkpoint").to_string(),
                n => format!("{n} {}", t!("checkpoints")),
            };
        }
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
        if self.mode == Mode::Changes
            && let Some(root) = &self.root
        {
            crate::diff_view::remember_hints(
                self.items.iter().map(|i| (i.path.clone(), Hint { root: root.clone(), rel: i.change.path.clone(), from: i.change.from.clone(), custom: None })).collect(),
            );
        }
    }

    /// Selects the first file whose path holds `part` (the scripted test driver).
    pub fn pick(&mut self, part: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.items.iter().position(|i| i.change.path.contains(part)) {
            self.select(i, cx);
        }
    }

    /// "Revert this file": asks, then runs the revert job (the Changes feed refreshes itself when the tree changes).
    fn ask_revert(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(it), Some(root)) = (self.items.get(idx), self.root.clone()) else { return };
        let rel = it.change.path.clone();
        let name = rf_core::history::display_name(&it.path);
        crate::prompt::show(
            t!("Revert this file?"),
            Some(format!("\"{name}\" {}", t!("goes back to the last commit. Your current version is moved to the Trash first."))),
            None,
            vec![
                crate::prompt::Button::new(t!("Revert"), crate::prompt::Kind::Danger, move |_, _, cx| {
                    crate::status_center::revert_file(cx, root.clone(), rel.clone());
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
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

/// One half of the Changes / Checkpoints switch (the look of the diff pane's switches).
fn seg(id: &'static str, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .text_xs()
        .whitespace_nowrap()
        .cursor_pointer()
        .rounded(theme::RADIUS_SM)
        .when(on, |d| d.bg(p.selection).text_color(p.text))
        .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
        .child(label)
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
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
        let on_cps = self.mode == Mode::Checkpoints;
        let switch = h_flex()
            .gap_0p5()
            .flex_shrink_0()
            .child(seg("changes-mode-changes", t!("Changes"), !on_cps, &p).on_click(cx.listener(|v, _, _, cx| v.show_changes(cx))))
            .child(seg("changes-mode-checkpoints", t!("Checkpoints"), on_cps, &p).on_click(cx.listener(|v, _, _, cx| v.show_checkpoints(cx))));
        if on_cps {
            return self.render_checkpoints(switch, &p, cx);
        }
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
            .child(switch)
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
        v_flex().size_full().child(top).child(body).into_any_element()
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
                let can_revert = matches!(c.kind, Kind::Changed | Kind::Deleted) && !c.untracked;
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
                            .id(("changes-revert", i))
                            .size(px(24.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme::RADIUS_SM)
                            .when(can_revert, |d| {
                                d.hover(|s| s.bg(p.pressed))
                                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Revert this file")).build(window, cx))
                                    .child(Icon::new(L::Undo2).size(px(14.)).text_color(p.text_secondary))
                                    .on_click(cx.listener(move |v, _, window, cx| {
                                        cx.stop_propagation();
                                        v.ask_revert(idx, window, cx);
                                    }))
                            }),
                    )
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

// ---------------------------------------------------------------------------------------------------------------- checkpoints

fn when_text(ms: i64) -> String {
    if ms > 0 { format::datetime(ms) } else { String::new() }
}

impl ChangesView {
    /// The Checkpoints side, its list read again.
    pub fn show_checkpoints(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Checkpoints {
            self.mode = Mode::Checkpoints;
            crate::diff_view::forget_hints();
            if self.selected.take().is_some() {
                cx.emit(ChangesEvent::Selected(None));
            }
        }
        self.load_checkpoints(cx);
        cx.notify();
    }

    fn show_changes(&mut self, cx: &mut Context<Self>) {
        if self.mode == Mode::Changes {
            return;
        }
        self.mode = Mode::Changes;
        crate::diff_view::forget_hints();
        if let Some(pv) = self.cps.preview.as_mut()
            && pv.selected.take().is_some()
        {
            cx.emit(ChangesEvent::Selected(None));
        }
        self.arrange();
        cx.notify();
    }

    /// Finds the store (once) and lists its checkpoints, off the UI thread.
    fn load_checkpoints(&mut self, cx: &mut Context<Self>) {
        // (a restore lists again when it is done)
        if self.cps.busy.is_some() {
            return;
        }
        let (dir, store) = (self.dir.clone(), self.cps.store.clone());
        let work = cx.background_spawn(async move {
            let store = match store {
                Some(s) => s,
                None => Arc::new(crate::checkpoint_ui::store_of(&dir)?),
            };
            let list = checkpoint::list(&store)?;
            Ok::<_, checkpoint::Error>((store, list))
        });
        self.cps._task = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                v.cps.loaded = true;
                match got {
                    Ok((store, l)) => {
                        v.cps.store = Some(store);
                        v.cps.list = l;
                        v.cps.error = None;
                    }
                    Err(e) => v.cps.error = Some(crate::checkpoint_ui::error_text(&e)),
                }
                cx.notify();
            });
        }));
    }

    /// "Restore…": what the restore would do, worked out off the UI thread; each file's difference goes to the preview pane.
    fn open_preview(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.cps.busy.is_some() {
            return;
        }
        let (Some(store), Some(cp)) = (self.cps.store.clone(), self.cps.list.get(i).cloned()) else { return };
        self.cps.preview = Some(Preview { cp: cp.clone(), plan: None, error: None, remove_added: false, selected: None });
        let s2 = store.clone();
        let work = cx.background_spawn(async move { checkpoint::plan(&s2, &cp) });
        self.cps._task = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                let Some(pv) = v.cps.preview.as_mut() else { return };
                match got {
                    Ok(plan) => {
                        let plan = Arc::new(plan);
                        let folder = store.folder().to_path_buf();
                        crate::diff_view::remember_hints(
                            plan.items
                                .iter()
                                .map(|it| {
                                    let (store, plan, rel) = (store.clone(), plan.clone(), it.rel.clone());
                                    let custom: crate::diff_view::Custom = Arc::new(move |whole| {
                                        checkpoint::diff(&store, &plan, &rel, if whole { crate::diff_view::WHOLE } else { crate::diff_view::CONTEXT })
                                    });
                                    (folder.join(&it.rel), Hint { root: folder.clone(), rel: it.rel.clone(), from: None, custom: Some(custom) })
                                })
                                .collect(),
                        );
                        pv.plan = Some(plan);
                    }
                    Err(e) => pv.error = Some(crate::checkpoint_ui::error_text(&e)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn close_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(pv) = self.cps.preview.take()
            && pv.selected.is_some()
        {
            cx.emit(ChangesEvent::Selected(None));
        }
        crate::diff_view::forget_hints();
        cx.notify();
    }

    fn select_planned(&mut self, rel: String, cx: &mut Context<Self>) {
        let Some(store) = self.cps.store.clone() else { return };
        let path = store.folder().join(&rel);
        let (size, ms) = meta_of(&path);
        if let Some(pv) = self.cps.preview.as_mut() {
            pv.selected = Some(rel);
        }
        cx.emit(ChangesEvent::Selected(Some((path, size, ms))));
        cx.notify();
    }

    /// Restores `cp` (an automatic checkpoint of the state now first), then lists again and offers "Undo restore". `remove`: the
    /// files added since that may go to the Trash (None: none).
    fn run_restore(&mut self, cp: Checkpoint, remove: Option<Vec<String>>, undo: bool, cx: &mut Context<Self>) {
        let Some(store) = self.cps.store.clone() else { return };
        if self.cps.busy.is_some() {
            return;
        }
        self.cps.busy = Some(if undo { t!("Taking the restore back…") } else { t!("Restoring…") }.into());
        let keep = crate::checkpoint_ui::keep(cx);
        let when_ms = cp.time_ms;
        let work = cx.background_spawn(async move {
            let done = checkpoint::restore(&store, &cp, remove.as_deref(), &crate::checkpoint_ui::to_trash);
            if let Ok(d) = &done {
                crate::checkpoint_ui::prune(&store, keep, &[cp.id.as_str(), d.auto.id.as_str()]);
            }
            done
        });
        self.cps._restore = Some(cx.spawn(async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |v, cx| {
                v.cps.busy = None;
                match done {
                    Ok(d) => {
                        if v.cps.preview.as_ref().is_some_and(|p| p.selected.is_some()) {
                            cx.emit(ChangesEvent::Selected(None));
                        }
                        v.cps.preview = None;
                        crate::diff_view::forget_hints();
                        crate::checkpoint_ui::set_undo_point((!undo).then(|| d.auto.id.clone()));
                        v.cps.last = Some(Last { done: Arc::new(d), when_ms, undone: undo });
                        v.cps.error = None;
                    }
                    Err(e) => v.cps.error = Some(crate::checkpoint_ui::error_text(&e)),
                }
                v.load_checkpoints(cx);
                v.refresh(cx);
            });
        }));
        cx.notify();
    }

    fn restore_previewed(&mut self, cx: &mut Context<Self>) {
        let Some(pv) = self.cps.preview.as_ref() else { return };
        let Some(plan) = pv.plan.clone() else { return };
        let remove = pv.remove_added.then(|| plan.items.iter().filter(|i| i.change == checkpoint::Change::Added).map(|i| i.rel.clone()).collect());
        self.run_restore(pv.cp.clone(), remove, false, cx);
    }

    fn undo_restore(&mut self, cx: &mut Context<Self>) {
        let Some(last) = self.cps.last.as_ref().filter(|l| !l.undone) else { return };
        let (auto, back) = (last.done.auto.clone(), last.done.back.clone());
        self.run_restore(auto, Some(back), true, cx);
    }

    fn ask_delete(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        use crate::prompt::{Button, Kind};
        let (Some(store), Some(cp)) = (self.cps.store.clone(), self.cps.list.get(i).cloned()) else { return };
        let me = cx.entity().downgrade();
        let message = format!("{} {}{}", t!("The checkpoint of"), when_text(cp.time_ms), if cp.note.is_empty() { String::new() } else { format!(" (\u{201c}{}\u{201d})", cp.note) });
        let go = Button::new(t!("Delete"), Kind::Danger, move |_, _, cx| {
            let (store, cp, me) = (store.clone(), cp.clone(), me.clone());
            let work = cx.background_spawn(async move { checkpoint::delete(&store, &cp) });
            cx.spawn(async move |cx| {
                let r = work.await;
                let _ = me.update(cx, |v, cx| {
                    v.cps.error = r.err().map(|e| crate::checkpoint_ui::error_text(&e));
                    v.load_checkpoints(cx);
                });
            })
            .detach();
            Ok(())
        });
        crate::prompt::show(t!("Delete this checkpoint?"), Some(format!("{message}. {}", t!("Your files are not touched."))), None, vec![go, Button::cancel()], window, cx);
    }

    fn render_checkpoints(&mut self, switch: Div, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.cps.busy.clone();
        let kind = match self.cps.store.as_deref() {
            Some(Store::Git { .. }) => t!("Hidden snapshots in the git repository"),
            Some(Store::Copy { .. }) => t!("Copies in leaf's data folder"),
            None => "",
        };
        let summary = if !self.cps.loaded {
            t!("Looking for checkpoints…").to_string()
        } else if self.cps.list.is_empty() {
            t!("No checkpoints yet. A checkpoint saves how the folder is now, to put it back later.").to_string()
        } else {
            format!("{} \u{00b7} {kind}", self.status_text())
        };
        let (dir, tab) = (self.dir.clone(), self.tab.clone());
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::FolderClock).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary))
            .child(switch)
            .child(
                button("checkpoint-new", t!("New checkpoint"), p)
                    .when(busy.is_some(), |d| d.opacity(0.5))
                    .on_click(cx.listener(move |v, _, window, cx| {
                        if v.cps.busy.is_none() {
                            crate::checkpoint_ui::new_checkpoint(dir.clone(), tab.clone(), window, cx);
                        }
                    })),
            )
            .child(button("checkpoint-refresh", t!("Refresh"), p).on_click(cx.listener(|v, _, _, cx| v.load_checkpoints(cx))));
        let mut page = v_flex().size_full().child(top);
        if let Some(b) = busy {
            page = page.child(self.bar(L::LoaderCircle, b.to_string(), p.text_secondary, p));
        }
        if let Some(e) = self.cps.error.clone() {
            page = page.child(self.bar(L::CircleAlert, e, p.danger, p));
        }
        if let Some(last) = &self.cps.last {
            let d = &last.done;
            let mut msg = if last.undone {
                format!("{} {} {}", t!("The restore was taken back:"), d.written.len() + d.trashed.len(), t!("files as they were before it."))
            } else {
                format!("{} {} {} {}.", t!("Restored the checkpoint of"), when_text(last.when_ms), t!("\u{2014} files written:"), d.written.len())
            };
            if !d.trashed.is_empty() {
                msg.push_str(&format!(" {} {}.", t!("Moved to the Trash:"), d.trashed.len()));
            }
            if !d.failed.is_empty() {
                let first = &d.failed[0];
                msg.push_str(&format!(" {} {} ({}: {}).", t!("Not put back:"), d.failed.len(), first.0, first.1));
            }
            let undo = (!last.undone).then(|| button("checkpoint-undo", t!("Undo restore"), p).on_click(cx.listener(|v, _, _, cx| v.undo_restore(cx))));
            page = page.child(
                h_flex()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(p.divider)
                    .bg(p.hover)
                    .child(Icon::new(L::ArchiveRestore).size(px(14.)).text_color(if d.failed.is_empty() { p.success } else { p.warning }))
                    .child(div().flex_1().min_w_0().text_xs().text_color(p.text).child(msg))
                    .children(undo)
                    .child(button("checkpoint-last-close", t!("Close"), p).on_click(cx.listener(|v, _, _, cx| {
                        v.cps.last = None;
                        cx.notify();
                    }))),
            );
        }
        if self.cps.preview.is_some() {
            return page.child(self.render_preview(p, cx)).into_any_element();
        }
        let rows: Vec<AnyElement> = (0..self.cps.list.len()).map(|i| self.render_checkpoint(i, p, cx)).collect();
        page.child(v_flex().id("checkpoint-rows").flex_1().min_h_0().overflow_y_scroll().p_1().children(rows)).into_any_element()
    }

    fn bar(&self, icon: L, text: String, color: Hsla, p: &Palette) -> Div {
        h_flex()
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(icon).size(px(14.)).text_color(color))
            .child(div().flex_1().min_w_0().text_xs().text_color(p.text).whitespace_normal().child(text))
    }

    fn render_checkpoint(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let cp = &self.cps.list[i];
        let hov = self.cps.hover == Some(i);
        let note = if cp.note.is_empty() { t!("(no note)").to_string() } else { cp.note.clone() };
        let files = format!("{} {}", cp.files, if cp.files == 1 { t!("file") } else { t!("files") });
        h_flex()
            .id(("checkpoint-row", i))
            .w_full()
            .h(theme::ROW_H)
            .px_2()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .rounded(theme::RADIUS_SM)
            .text_sm()
            .when(hov, |d| d.bg(p.hover))
            .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                let want = h.then_some(i);
                if *h && v.cps.hover != want || !*h && v.cps.hover == Some(i) {
                    v.cps.hover = want;
                    cx.notify();
                }
            }))
            .child(Icon::new(L::FolderClock).size(px(14.)).flex_shrink_0().text_color(if cp.auto { p.text_tertiary } else { p.accent }))
            .child(div().w(px(140.)).flex_shrink_0().whitespace_nowrap().text_color(p.text).child(when_text(cp.time_ms)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(if cp.note.is_empty() { p.text_tertiary } else { p.text })
                    .child(crate::rtl::display_text(&note)),
            )
            .when(cp.auto, |d| {
                d.child(
                    div().flex_shrink_0().px_1p5().rounded(theme::RADIUS_SM).border_1().border_color(p.border).text_xs().text_color(p.text_secondary).child(t!("automatic")),
                )
            })
            .child(div().w(px(80.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(files))
            .child(div().w(px(80.)).flex_shrink_0().text_xs().text_right().text_color(p.text).child(format::size(cp.size)))
            .child(button(("checkpoint-restore", i), t!("Restore…"), p).on_click(cx.listener(move |v, _, _, cx| v.open_preview(i, cx))))
            .child(
                div()
                    .id(("checkpoint-delete", i))
                    .size(px(24.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme::RADIUS_SM)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.pressed))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Delete checkpoint")).build(window, cx))
                    .child(Icon::new(L::Trash).size(px(14.)).text_color(p.text_secondary))
                    .on_click(cx.listener(move |v, _, window, cx| v.ask_delete(i, window, cx))),
            )
            .into_any_element()
    }

    fn render_preview(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        use checkpoint::Change as C;
        let Some(pv) = self.cps.preview.as_ref() else { return div().into_any_element() };
        let title = format!("{} {}", t!("Restore the checkpoint of"), when_text(pv.cp.time_ms));
        let (line, ready) = match (&pv.plan, &pv.error) {
            (_, Some(e)) => (e.clone(), false),
            (None, None) => (t!("Comparing with the files now…").to_string(), false),
            (Some(plan), None) if plan.items.is_empty() => (t!("Nothing to restore: the files are as the checkpoint saw them.").to_string(), false),
            (Some(plan), None) => (
                format!(
                    "{} {} \u{00b7} {} {} \u{00b7} {} {}. {}",
                    plan.count(C::Changed),
                    t!("change"),
                    plan.count(C::Back),
                    t!("come back"),
                    plan.count(C::Added),
                    t!("added since"),
                    t!("Before restoring, leaf takes an automatic checkpoint of the files now. In the preview pane, + lines are what the restore writes.")
                ),
                true,
            ),
        };
        let tick = pv.remove_added;
        let added = pv.plan.as_ref().map_or(0, |pl| pl.count(C::Added));
        let head = v_flex()
            .px_3()
            .py_2()
            .gap_1p5()
            .border_b_1()
            .border_color(p.divider)
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title))
            .child(div().text_xs().text_color(p.text_secondary).whitespace_normal().child(line))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(added > 0, |d| {
                        d.child(
                            h_flex()
                                .id("checkpoint-remove-added")
                                .gap_1p5()
                                .items_center()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(p.text)
                                .child(Icon::new(if tick { L::SquareCheck } else { L::Square }).size(px(14.)).text_color(if tick { p.accent } else { p.text_secondary }))
                                .child(t!("Also remove files added since (they go to the Trash)"))
                                .on_click(cx.listener(|v, _, _, cx| {
                                    if let Some(pv) = v.cps.preview.as_mut() {
                                        pv.remove_added = !pv.remove_added;
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .when(ready, |d| {
                        d.child(
                            button("checkpoint-do-restore", t!("Restore"), p)
                                .bg(p.accent)
                                .text_color(p.accent_text)
                                .border_color(p.accent)
                                .on_click(cx.listener(|v, _, _, cx| v.restore_previewed(cx))),
                        )
                    })
                    .child(button("checkpoint-cancel", t!("Cancel"), p).on_click(cx.listener(|v, _, _, cx| v.close_preview(cx)))),
            );
        let items: Vec<(String, C)> = pv.plan.as_ref().map(|pl| pl.items.iter().map(|i| (i.rel.clone(), i.change)).collect()).unwrap_or_default();
        let selected = pv.selected.clone();
        let body = if items.is_empty() {
            div().flex_1().into_any_element()
        } else {
            let items = std::rc::Rc::new(items);
            let count = items.len();
            uniform_list(
                "checkpoint-plan",
                count,
                cx.processor(move |_, range: std::ops::Range<usize>, window, cx| {
                    let p = Palette::for_window(window);
                    range
                        .map(|i| {
                            let (rel, ch) = items[i].clone();
                            let on = selected.as_deref() == Some(rel.as_str());
                            let (letter, label, color) = match ch {
                                C::Changed => ("M", t!("Changes back"), p.warning),
                                C::Back => ("+", t!("Comes back"), p.success),
                                C::Added if tick => ("\u{2212}", t!("To the Trash"), p.danger),
                                C::Added => ("?", t!("Added since: stays"), p.text_tertiary),
                            };
                            let (dir, name) = rel.rsplit_once('/').map_or((String::new(), rel.clone()), |(d, n)| (d.replace('/', "\\"), n.to_string()));
                            h_flex()
                                .id(("checkpoint-plan-row", i))
                                .w_full()
                                .h(theme::ROW_H)
                                .px_2()
                                .gap_2()
                                .items_center()
                                .flex_shrink_0()
                                .rounded(theme::RADIUS_SM)
                                .text_sm()
                                .cursor_pointer()
                                .when(on, |d| d.bg(p.selection))
                                .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                                .on_click(cx.listener(move |v, _, _, cx| v.select_planned(rel.clone(), cx)))
                                .child(div().w(px(16.)).flex_shrink_0().font_weight(FontWeight::SEMIBOLD).text_color(color).child(letter))
                                .child(div().flex_shrink_0().w(px(200.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text).child(crate::rtl::display_text(&name)))
                                .child(div().flex_shrink_0().w(px(130.)).overflow_hidden().whitespace_nowrap().text_xs().text_color(color).child(label))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(dir))
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .p_1()
            .into_any_element()
        };
        v_flex().flex_1().min_h_0().child(head).child(body).into_any_element()
    }
}
