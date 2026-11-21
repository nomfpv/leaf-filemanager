//! Duplicate finder page (Location::Duplicates): the search runs as a Status Center job on a small pool (`leaf_sys::dupes`), and this
//! file draws what it found: groups of identical files, the most wasted space first, each file with a tick. Ticked files are deleted
//! only through the normal delete to the Recycle Bin (one undo entry), after a question with the count and size, and never when a
//! tick would take a group's last copy (`rf_core::dupes::plan_delete`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_sys::dupes::{self, Progress};
use rf_core::dupes::{DupGroup, Keep, Plan, folders_of, plan_delete, ticks, totals};
use rf_core::format;

use crate::fluent_menu::{self, Item, Menu, MenuIcon};
use crate::t;
use crate::theme::{self, Palette};

/// Tab title: "Duplicates — Pictures".
pub fn title(roots: &[PathBuf]) -> String {
    match roots {
        [one] => format!("{} — {}", t!("Duplicates"), rf_core::history::display_name(one)),
        _ => t!("Duplicates").to_string(),
    }
}

pub enum DupEvent {
    /// The file under the cursor changed (the preview pane follows it).
    Selected(Option<PathBuf>, u64, i64),
    /// Open this file's folder with it selected.
    ShowInFolder(PathBuf),
}

impl EventEmitter<DupEvent> for DuplicatesView {}

enum Phase {
    Scanning,
    Done,
    Stopped,
}

#[derive(Clone, Copy)]
enum Row {
    Head(usize),
    File(usize, usize),
}

pub struct DuplicatesView {
    roots: Vec<PathBuf>,
    phase: Phase,
    progress: Arc<Progress>,
    stop: Arc<AtomicBool>,
    found: Arc<Mutex<Option<Vec<DupGroup>>>>,
    groups: Vec<DupGroup>,
    rows: Vec<Row>,
    ticked: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    hover: Option<usize>,
    started: std::time::Instant,
    _poll: Option<Task<()>>,
}

impl Drop for DuplicatesView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl DuplicatesView {
    pub fn new(roots: Vec<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            roots,
            phase: Phase::Scanning,
            progress: Arc::new(Progress::default()),
            stop: Arc::new(AtomicBool::new(false)),
            found: Arc::new(Mutex::new(None)),
            groups: Vec::new(),
            rows: Vec::new(),
            ticked: HashSet::new(),
            selected: None,
            hover: None,
            started: std::time::Instant::now(),
            _poll: None,
        };
        this.start(cx);
        this
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Starts the search as a Status Center job; its Cancel and this page's Stop are the same flag.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        self.stop = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(Progress::default());
        self.found = Arc::new(Mutex::new(None));
        self.phase = Phase::Scanning;
        self.groups.clear();
        self.rows.clear();
        self.ticked.clear();
        self.selected = None;
        self.started = std::time::Instant::now();
        let (stop, progress, found, roots) = (self.stop.clone(), self.progress.clone(), self.found.clone(), self.roots.clone());
        let name = title(&self.roots);
        crate::status_center::start(cx, format!("{} {}", t!("Looking for duplicates:"), name), move |tr| {
            let opt = dupes::Options { include_hidden: false, threads: 2 };
            let done = Arc::new(AtomicBool::new(false));
            // the job's own Cancel reaches the search, and its counters reach the job's line
            let (s2, d2, p2, tr2) = (stop.clone(), done.clone(), progress.clone(), tr.clone());
            std::thread::spawn(move || {
                while !d2.load(Ordering::Relaxed) {
                    if tr2.cancel.load(Ordering::Relaxed) {
                        s2.store(true, Ordering::Relaxed);
                    }
                    (tr2.on_progress)(0, 0, Some(progress_text(&p2)));
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
            let r = dupes::find(&roots, &opt, &progress, &stop);
            done.store(true, Ordering::Relaxed);
            match r {
                Some(g) => {
                    *found.lock().unwrap() = Some(g);
                    Ok(true)
                }
                None => Ok(false),
            }
        });
        self.poll(cx);
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let Ok(done) = this.update(cx, |v, cx| {
                    let got = v.found.lock().unwrap().take();
                    if let Some(g) = got {
                        v.groups = g;
                        v.phase = Phase::Done;
                        v.rebuild_rows();
                        cx.notify();
                        return true;
                    }
                    if v.stop.load(Ordering::Relaxed) {
                        v.phase = Phase::Stopped;
                        cx.notify();
                        return true;
                    }
                    cx.notify();
                    false
                }) else {
                    return;
                };
                if done {
                    return;
                }
            }
        }));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (gi, g) in self.groups.iter().enumerate() {
            self.rows.push(Row::Head(gi));
            for fi in 0..g.files.len() {
                self.rows.push(Row::File(gi, fi));
            }
        }
    }

    fn select(&mut self, path: PathBuf, size: u64, ms: i64, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        cx.emit(DupEvent::Selected(Some(path), size, ms));
        cx.notify();
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.ticked.remove(path) {
            self.ticked.insert(path.to_path_buf());
        }
        cx.notify();
    }

    /// Replaces the ticks of one group (`Some`) or of every group (`None`) with the ones `keep` gives.
    pub(crate) fn apply_keep(&mut self, group: Option<usize>, keep: Keep, cx: &mut Context<Self>) {
        let range = match group {
            Some(g) => g..g + 1,
            None => 0..self.groups.len(),
        };
        for gi in range {
            let g = &self.groups[gi];
            for f in &g.files {
                self.ticked.remove(&f.path);
            }
            for i in ticks(g, &keep) {
                self.ticked.insert(g.files[i].path.clone());
            }
        }
        cx.notify();
    }

    fn ticked_total(&self) -> (usize, u64) {
        let (mut n, mut bytes) = (0, 0);
        for g in &self.groups {
            for f in &g.files {
                if self.ticked.contains(&f.path) {
                    n += 1;
                    bytes += g.size;
                }
            }
        }
        (n, bytes)
    }

    /// The status line: "5 files in 2 groups · 3 ticked (21.1 KB)"; nothing while searching or when there are no groups.
    pub(crate) fn status_text(&self) -> Option<String> {
        if !matches!(self.phase, Phase::Done) || self.groups.is_empty() {
            return None;
        }
        let (files, ngroups, _) = totals(&self.groups);
        let (nt, tbytes) = self.ticked_total();
        Some(format!("{} {} {} {} · {} {} ({})", files, t!("files in"), ngroups, t!("groups"), nt, t!("ticked"), format::size(tbytes)))
    }

    fn keep_menu(&mut self, group: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let folders: Vec<PathBuf> = match group {
            Some(g) => folders_of(&self.groups[g]),
            None => self.roots.clone(),
        };
        let items = folders
            .into_iter()
            .take(24)
            .map(|d| {
                let me = me.clone();
                let label = format!("{} {}", t!("Keep the one in"), d.display());
                Item::action(label, MenuIcon::Lucide(L::Folder), move |_, cx| {
                    let _ = me.update(cx, |v, cx| v.apply_keep(group, Keep::InFolder(d.clone()), cx));
                })
            })
            .collect();
        fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    fn stat(p: &Path) -> Option<(u64, i64)> {
        let m = std::fs::symlink_metadata(p).ok()?;
        let ms = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
        Some((m.len(), ms))
    }

    /// "Delete ticked": checked, asked, then the normal delete to the Recycle Bin.
    pub(crate) fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match plan_delete(&self.groups, &self.ticked, Self::stat) {
            Plan::LastCopy(_) => {
                crate::prompt::notice(
                    t!("Every copy is ticked"),
                    t!("A group would lose its last copy. Untick one file in every group where all of them are ticked.").to_string(),
                    window,
                    cx,
                );
            }
            Plan::Go { delete, skipped, kept_missing } => {
                if delete.is_empty() {
                    let mut msg = if skipped.is_empty() && kept_missing.is_empty() { t!("Nothing is ticked.").to_string() } else { skipped_text(&skipped) };
                    if !kept_missing.is_empty() {
                        msg = format!("{}
{}", msg.trim(), kept_missing_text(&kept_missing));
                    }
                    crate::prompt::notice(t!("Nothing to delete"), msg, window, cx);
                    return;
                }
                let bytes: u64 = self.groups.iter().map(|g| g.size * g.files.iter().filter(|f| delete.contains(&f.path)).count() as u64).sum();
                let mut msg = format!("{} ({}) {}", crate::i18n::items(delete.len()), format::size(bytes), t!("will be moved to the Recycle Bin."));
                if !skipped.is_empty() {
                    msg.push_str("\n\n");
                    msg.push_str(&skipped_text(&skipped));
                }
                let me = cx.entity().downgrade();
                crate::prompt::show(
                    t!("Move to the Recycle Bin?"),
                    Some(msg),
                    None,
                    vec![
                        crate::prompt::Button::new(t!("Delete"), crate::prompt::Kind::Primary, move |_, _, cx| {
                            let _ = me.update(cx, |v, cx| v.do_delete(cx));
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            }
        }
    }

    /// The check once more, now (the question was open for a while), then the delete.
    fn do_delete(&mut self, cx: &mut Context<Self>) {
        let Plan::Go { delete, .. } = plan_delete(&self.groups, &self.ticked, Self::stat) else { return };
        if delete.is_empty() {
            return;
        }
        crate::status_center::delete(cx, delete.clone(), false);
        self.prune_when_gone(delete, cx);
    }

    /// Takes the files off the page as they leave the disk (the delete is a job of its own); groups down to one file go.
    fn prune_when_gone(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            for _ in 0..120 {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let Ok(left) = this.update(cx, |v, cx| {
                    let gone: HashSet<&PathBuf> = paths.iter().filter(|p| !p.exists()).collect();
                    v.groups.iter_mut().for_each(|g| g.files.retain(|f| !gone.contains(&f.path)));
                    v.groups.retain(|g| g.files.len() > 1);
                    v.ticked.retain(|p| !gone.contains(p));
                    if v.selected.as_ref().is_some_and(|s| gone.contains(s)) {
                        v.selected = None;
                        cx.emit(DupEvent::Selected(None, 0, 0));
                    }
                    v.rebuild_rows();
                    cx.notify();
                    paths.iter().any(|p| p.exists())
                }) else {
                    return;
                };
                if !left {
                    return;
                }
            }
        }));
    }
}

fn kept_missing_text(files: &[PathBuf]) -> String {
    let names: Vec<String> = files.iter().take(8).map(|p| rf_core::history::display_name(p)).collect();
    format!("{} {}: {}", crate::i18n::items(files.len()), t!("not deleted: the copy that would be kept has changed or is gone"), names.join(", "))
}

fn skipped_text(skipped: &[PathBuf]) -> String {
    let names: Vec<String> = skipped.iter().take(8).map(|p| rf_core::history::display_name(p)).collect();
    let more = skipped.len().saturating_sub(8);
    format!(
        "{} {}: {}{}",
        crate::i18n::items(skipped.len()),
        t!("changed since the search and stay"),
        names.join(", "),
        if more > 0 { format!(" +{more}") } else { String::new() }
    )
}

fn progress_text(p: &Progress) -> String {
    let stage = match p.stage.load(Ordering::Relaxed) {
        dupes::WALKING => t!("Reading folders"),
        dupes::SAME_SIZE => t!("Comparing sizes"),
        dupes::EDGES => t!("Comparing file starts and ends"),
        _ => t!("Comparing whole files"),
    };
    format!("{stage} · {} {} · {} {}", p.files.load(Ordering::Relaxed), t!("files"), format::size(p.bytes.load(Ordering::Relaxed)), t!("hashed"))
}

fn button(id: &'static str, label: impl Into<SharedString>, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .whitespace_nowrap()
        .border_1()
        .border_color(p.border)
        .when(on, |d| d.cursor_pointer().text_color(p.text).hover(|s| s.bg(p.hover)))
        .when(!on, |d| d.text_color(p.text_disabled))
        .child(label.into())
}

impl Render for DuplicatesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let scanning = matches!(self.phase, Phase::Scanning);
        let (files, ngroups, freed) = totals(&self.groups);
        let (nt, tbytes) = self.ticked_total();
        let summary = match self.phase {
            Phase::Scanning => progress_text(&self.progress),
            Phase::Stopped => t!("The search was stopped").to_string(),
            Phase::Done if ngroups == 0 => t!("No duplicates found").to_string(),
            Phase::Done => format!("{} {} {} {}, {} {}", files, t!("files in"), ngroups, t!("groups"), format::size(freed), t!("could be freed")),
        };
        let has = !self.groups.is_empty();
        let top = h_flex()
            .px_3()
            .pt_2()
            .gap_2()
            .items_center()
            .child(Icon::new(L::Copy).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary));
        let bar = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .when(scanning, |d| {
                d.child(button("dup-stop", t!("Stop"), true, &p).on_click(cx.listener(|v, _, _, cx| {
                    v.stop.store(true, Ordering::Relaxed);
                    cx.notify();
                })))
            })
            .when(!scanning, |d| d.child(button("dup-again", t!("Search again"), true, &p).on_click(cx.listener(|v, _, _, cx| v.start(cx)))))
            .when(has, |d| {
                d.child(div().text_xs().text_color(p.text_tertiary).child(t!("Select all but one in every group:")))
                    .child(button("dup-newest", t!("Keep newest"), true, &p).on_click(cx.listener(|v, _, _, cx| v.apply_keep(None, Keep::Newest, cx))))
                    .child(button("dup-oldest", t!("Keep oldest"), true, &p).on_click(cx.listener(|v, _, _, cx| v.apply_keep(None, Keep::Oldest, cx))))
                    .child(button("dup-infolder", t!("Keep the one in…"), true, &p).on_click(cx.listener(|v, _, w, cx| v.keep_menu(None, w, cx))))
                    .child(button("dup-clear", t!("Clear ticks"), nt > 0, &p).on_click(cx.listener(|v, _, _, cx| {
                        v.ticked.clear();
                        cx.notify();
                    })))
                    .child(
                        button("dup-delete", format!("{} {} ({})", t!("Delete ticked"), nt, format::size(tbytes)), nt > 0, &p)
                            .when(nt > 0, |b| b.bg(p.accent).text_color(p.accent_text))
                            .on_click(cx.listener(|v, _, w, cx| v.ask_delete(w, cx))),
                    )
            });

        let body = if self.rows.is_empty() {
            let msg = match self.phase {
                Phase::Scanning => t!("Searching…"),
                _ => t!("Nothing to show"),
            };
            div().flex_1().flex().items_center().justify_center().text_sm().text_color(p.text_tertiary).child(msg).into_any_element()
        } else {
            uniform_list(
                "dup-rows",
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
        v_flex().size_full().child(top).child(bar).child(body)
    }
}

impl DuplicatesView {
    /// The file's folder as the page shows it: from the searched folder's own name down ("Photos\2024"), else in full.
    fn shown_dir(&self, path: &Path) -> String {
        let dir = path.parent().unwrap_or(path);
        for r in &self.roots {
            if let (Ok(rel), Some(base)) = (dir.strip_prefix(r), r.file_name()) {
                return Path::new(base).join(rel).to_string_lossy().into_owned();
            }
        }
        dir.to_string_lossy().into_owned()
    }

    fn render_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        match self.rows[i] {
            Row::Head(gi) => {
                let g = &self.groups[gi];
                let label = format!("{} × {}  ·  {} {}", g.files.len(), format::size(g.size), format::size(g.wasted()), t!("wasted"));
                h_flex()
                    .id(("dup-head", i))
                    .w_full()
                    .h(theme::ROW_H)
                    .px_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme::RADIUS_SM)
                    .bg(p.pressed)
                    .child(Icon::new(L::Files).size(px(14.)).text_color(p.text_secondary))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(label))
                    .child(button("g-newest", t!("Keep newest"), true, p).on_click(cx.listener(move |v, _, _, cx| v.apply_keep(Some(gi), Keep::Newest, cx))))
                    .child(button("g-oldest", t!("Keep oldest"), true, p).on_click(cx.listener(move |v, _, _, cx| v.apply_keep(Some(gi), Keep::Oldest, cx))))
                    .child(button("g-folder", t!("Keep the one in…"), true, p).on_click(cx.listener(move |v, _, w, cx| v.keep_menu(Some(gi), w, cx))))
                    .into_any_element()
            }
            Row::File(gi, fi) => {
                let g = &self.groups[gi];
                let f = &g.files[fi];
                let on = self.ticked.contains(&f.path);
                let sel = self.selected.as_ref() == Some(&f.path);
                let hov = self.hover == Some(i);
                let (path, size, ms) = (f.path.clone(), g.size, f.modified_ms);
                let dir = self.shown_dir(&f.path);
                let name = rf_core::history::display_name(&f.path);
                let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
                h_flex()
                    .id(("dup-row", i))
                    .relative()
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
                    .when(sel, |d| d.bg(p.selection).child(theme::selection_pill(p)))
                    .when(!sel && hov, |d| d.bg(p.hover))
                    .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                        let want = h.then_some(i);
                        if *h && v.hover != want || !*h && v.hover == Some(i) {
                            v.hover = want;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |v, ev: &ClickEvent, _, cx| {
                        v.select(p1.clone(), size, ms, cx);
                        if ev.click_count() >= 2 {
                            crate::folder_view::open_in_background(p1.clone(), cx);
                        }
                    }))
                    .child(
                        div()
                            .id(("dup-tick", i))
                            .size(px(16.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if on { p.accent } else { p.text_tertiary })
                            .when(on, |d| d.bg(p.accent).child(Icon::new(L::Check).size(px(12.)).text_color(p.accent_text)))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                cx.stop_propagation();
                                v.toggle(&p2, cx);
                            })),
                    )
                    .child(div().flex_shrink_0().max_w(px(260.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text).child(name))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(dir))
                    .child(div().w(px(120.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(format::datetime(f.modified_ms)))
                    .child(
                        div()
                            .id(("dup-open", i))
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
                                cx.emit(DupEvent::ShowInFolder(p3.clone()));
                            })),
                    )
                    .into_any_element()
            }
        }
    }
}
