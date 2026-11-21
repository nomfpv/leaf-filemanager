//! Build-junk page (Location::BuildJunk): the folder a build or an install makes again (`node_modules`, `target`, `.venv`...) found
//! under one folder, each with its project, size and last change, the biggest first. The finding and the guards are `leaf_dev::junk`;
//! this file draws the list, measures the sizes in the background and, after a question with the count and size, moves the ticked
//! folders to the Recycle Bin through the normal delete job (one undo entry).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::junk::{self, Junk, Reject};
use leaf_sys::foldersize::{SizeBatch, SizeState};
use rf_core::format;

use crate::t;
use crate::theme::{self, Palette};

/// Tab title: "Build junk — project".
pub fn title(root: &Path) -> String {
    format!("{} — {}", t!("Build junk"), rf_core::history::display_name(root))
}

pub enum JunkEvent {
    /// Open this folder's parent with it selected.
    ShowInFolder(PathBuf),
}

impl EventEmitter<JunkEvent> for JunkView {}

enum Phase {
    Scanning,
    Done,
    Stopped,
}

struct Item {
    junk: Junk,
    modified_ms: i64,
    slot: Option<(Arc<SizeBatch>, usize)>,
    /// Bytes: the final size once measured, else what the walk has counted so far.
    bytes: u64,
    measured: bool,
}

pub struct JunkView {
    root: PathBuf,
    phase: Phase,
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    found: Arc<Mutex<Vec<Junk>>>,
    items: Vec<Item>,
    ticked: HashSet<PathBuf>,
    hover: Option<usize>,
    /// The guards are being checked for a clean (off the UI thread).
    checking: bool,
    _poll: Option<Task<()>>,
}

impl Drop for JunkView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for it in &self.items {
            if let Some((b, _)) = &it.slot {
                b.cancel();
            }
        }
    }
}

fn modified_ms(p: &Path) -> i64 {
    std::fs::symlink_metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

impl JunkView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            phase: Phase::Scanning,
            stop: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            found: Arc::new(Mutex::new(Vec::new())),
            items: Vec::new(),
            ticked: HashSet::new(),
            hover: None,
            checking: false,
            _poll: None,
        };
        this.start(cx);
        this
    }

    /// The status line while this page is the tab's: "7 folders · 2 ticked (1.3 GB)".
    pub fn status_text(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let (nt, bytes, all) = self.ticked_total();
        let sz = if all { format::size(bytes) } else { format!("{}+", format::size(bytes)) };
        format!("{} {} · {} {} ({})", self.items.len(), t!("folders"), nt, t!("ticked"), sz)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Starts the walk as a Status Center job; its Cancel and this page's Stop are the same flag.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        for it in &self.items {
            if let Some((b, _)) = &it.slot {
                b.cancel();
            }
        }
        self.stop = Arc::new(AtomicBool::new(false));
        self.finished = Arc::new(AtomicBool::new(false));
        self.found = Arc::new(Mutex::new(Vec::new()));
        self.phase = Phase::Scanning;
        self.items.clear();
        self.ticked.clear();
        let (stop, finished, found, root) = (self.stop.clone(), self.finished.clone(), self.found.clone(), self.root.clone());
        crate::status_center::start(cx, format!("{} {}", t!("Looking for build junk:"), rf_core::history::display_name(&root)), move |tr| {
            let done = Arc::new(AtomicBool::new(false));
            let (s2, d2, tr2) = (stop.clone(), done.clone(), tr.clone());
            std::thread::spawn(move || {
                while !d2.load(Ordering::Relaxed) {
                    if tr2.cancel.load(Ordering::Relaxed) {
                        s2.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
            junk::scan(&root, &stop, &junk::git_tracks, &mut |j| found.lock().unwrap().push(j));
            done.store(true, Ordering::Relaxed);
            let ok = !stop.load(Ordering::Relaxed);
            finished.store(true, Ordering::Release);
            Ok(ok)
        });
        self.poll(cx);
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let Ok(done) = this.update(cx, |v, cx| v.tick(cx)) else { return };
                if done {
                    return;
                }
            }
        }));
    }

    /// Takes what the walk found, starts the size measuring for it and reads the sizes in; true when nothing more will change.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self.finished.load(Ordering::Acquire);
        let new: Vec<Junk> = std::mem::take(&mut *self.found.lock().unwrap());
        if !new.is_empty() {
            let paths: Vec<PathBuf> = new.iter().map(|j| j.path.clone()).collect();
            let batch = SizeBatch::start(paths);
            for (i, j) in new.into_iter().enumerate() {
                let modified_ms = modified_ms(&j.path);
                self.items.push(Item { junk: j, modified_ms, slot: Some((batch.clone(), i)), bytes: 0, measured: false });
            }
        }
        let mut pending = false;
        for it in &mut self.items {
            if it.measured {
                continue;
            }
            match it.slot.as_ref().map(|(b, i)| b.state(*i)) {
                Some(SizeState::Done(fs)) => {
                    it.bytes = fs.bytes;
                    it.measured = true;
                }
                Some(SizeState::Partial(b)) => {
                    it.bytes = b;
                    pending = true;
                }
                Some(SizeState::Provisional(fs)) => {
                    it.bytes = fs.bytes;
                    pending = true;
                }
                None => {}
            }
        }
        if finished {
            self.phase = if self.stop.load(Ordering::Relaxed) { Phase::Stopped } else { Phase::Done };
        }
        let all_done = finished && !pending && self.found.lock().unwrap().is_empty();
        if all_done {
            // the biggest first, once the sizes are final (the rows would jump under the pointer before)
            self.items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.junk.path.cmp(&b.junk.path)));
        }
        cx.notify();
        all_done
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.ticked.remove(path) {
            self.ticked.insert(path.to_path_buf());
        }
        cx.notify();
    }

    /// Ticks every folder not touched for 30 days (the ticks of the others stay as they are).
    fn tick_old(&mut self, cx: &mut Context<Self>) {
        let cutoff = chrono::Local::now().timestamp_millis() - 30 * 24 * 3600 * 1000;
        for it in &self.items {
            if it.modified_ms < cutoff {
                self.ticked.insert(it.junk.path.clone());
            }
        }
        cx.notify();
    }

    fn ticked_total(&self) -> (usize, u64, bool) {
        let mut n = 0;
        let mut bytes = 0;
        let mut all = true;
        for it in self.items.iter().filter(|i| self.ticked.contains(&i.junk.path)) {
            n += 1;
            bytes += it.bytes;
            all &= it.measured;
        }
        (n, bytes, all)
    }

    /// The ticked folders, checked against every guard (`leaf_dev::junk::check`) off the UI thread; `then` gets the answer. The page
    /// says "Checking…" meanwhile.
    fn vet(&mut self, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Vec<PathBuf>, Vec<(PathBuf, Reject)>, &mut Context<Self>) + 'static) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        let ticked: Vec<PathBuf> = self.items.iter().filter(|i| self.ticked.contains(&i.junk.path)).map(|i| i.junk.path.clone()).collect();
        let root = self.root.clone();
        let work = cx.background_spawn(async move {
            let (mut ok, mut bad) = (Vec::new(), Vec::new());
            for p in ticked {
                match junk::check(&p, &root) {
                    Ok(_) => ok.push(p),
                    Err(r) => bad.push((p, r)),
                }
            }
            (ok, bad)
        });
        cx.spawn(async move |this, cx| {
            let (ok, bad) = work.await;
            let _ = this.update(cx, |v, cx| {
                v.checking = false;
                cx.notify();
                then(v, ok, bad, cx);
            });
        })
        .detach();
    }

    fn ask_clean(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        self.vet(cx, move |v, ok, bad, cx| {
            let me = cx.entity().downgrade();
            let msg_items = (v.items.iter().filter(|i| ok.contains(&i.junk.path)).map(|i| (i.bytes, i.measured)).collect::<Vec<_>>(), ok.len());
            let _ = handle.update(cx, |_, window, cx| {
                if ok.is_empty() {
                    let msg = if bad.is_empty() { t!("Nothing is ticked.").to_string() } else { left_text(&bad) };
                    crate::prompt::notice(t!("Nothing to clean"), msg, window, cx);
                    return;
                }
                let bytes: u64 = msg_items.0.iter().map(|m| m.0).sum();
                let size = if msg_items.0.iter().all(|m| m.1) { format::size(bytes) } else { format!("{}+", format::size(bytes)) };
                let mut msg = format!("{} ({}) {}", crate::i18n::items(msg_items.1), size, t!("will be moved to the Recycle Bin."));
                if !bad.is_empty() {
                    msg.push_str("\n\n");
                    msg.push_str(&left_text(&bad));
                }
                crate::prompt::show(
                    t!("Move to the Recycle Bin?"),
                    Some(msg),
                    None,
                    vec![
                        crate::prompt::Button::new(t!("Clean"), crate::prompt::Kind::Primary, move |_, _, cx| {
                            let _ = me.update(cx, |v, cx| v.do_clean(cx));
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            });
        });
    }

    /// The guards once more, now (the question was open for a while), then the normal delete.
    fn do_clean(&mut self, cx: &mut Context<Self>) {
        self.vet(cx, |v, ok, _, cx| {
            if ok.is_empty() {
                return;
            }
            crate::status_center::delete(cx, ok.clone(), false);
            v.prune_when_gone(ok, cx);
        });
    }

    /// Takes the folders off the page as they leave the disk (the delete is a job of its own).
    fn prune_when_gone(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            for _ in 0..240 {
                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                let Ok(left) = this.update(cx, |v, cx| {
                    let gone: HashSet<&PathBuf> = paths.iter().filter(|p| !p.exists()).collect();
                    v.items.retain(|i| !gone.contains(&i.junk.path));
                    v.ticked.retain(|p| !gone.contains(p));
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

    /// The folder's project as the page shows it: from the searched folder's own name down ("repo\app"), else in full.
    fn shown_project(&self, project: &Path) -> String {
        match (project.strip_prefix(&self.root), self.root.file_name()) {
            (Ok(rel), Some(base)) => Path::new(base).join(rel).to_string_lossy().into_owned(),
            _ => project.to_string_lossy().into_owned(),
        }
    }
}

fn left_text(bad: &[(PathBuf, Reject)]) -> String {
    let names: Vec<String> = bad.iter().take(8).map(|(p, _)| rf_core::history::display_name(p)).collect();
    let git = bad.iter().any(|(_, r)| matches!(r, Reject::Tracked | Reject::Unsure));
    let why = if git { t!("not cleaned: changed since the search, or git tracks files in them or could not be asked") } else { t!("not cleaned: changed since the search") };
    format!("{} {}: {}", crate::i18n::items(bad.len()), why, names.join(", "))
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

impl Render for JunkView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let scanning = matches!(self.phase, Phase::Scanning);
        let total: u64 = self.items.iter().map(|i| i.bytes).sum();
        let measuring = self.items.iter().any(|i| !i.measured);
        let (nt, tbytes, tall) = self.ticked_total();
        let summary = match self.phase {
            Phase::Scanning => format!("{} · {} {}", t!("Looking for build junk…"), self.items.len(), t!("found")),
            Phase::Stopped => t!("The search was stopped").to_string(),
            Phase::Done if self.items.is_empty() => t!("No build junk found").to_string(),
            Phase::Done => String::new(),
        };
        let summary = if !self.items.is_empty() {
            let sz = if measuring { format!("{}+", format::size(total)) } else { format::size(total) };
            format!("{} {} · {} {}{}", self.items.len(), t!("folders"), sz, t!("could be freed"), if summary.is_empty() { String::new() } else { format!("  ({summary})") })
        } else {
            summary
        };
        let has = !self.items.is_empty();
        let top = h_flex()
            .px_3()
            .pt_2()
            .gap_2()
            .items_center()
            .child(Icon::new(L::Eraser).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary));
        let clean_label = if self.checking { t!("Checking…").to_string() } else { format!("{} {} ({}{})", t!("Clean ticked"), nt, format::size(tbytes), if tall { "" } else { "+" }) };
        let bar = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .when(scanning, |d| {
                d.child(button("junk-stop", t!("Stop"), true, &p).on_click(cx.listener(|v, _, _, cx| {
                    v.stop.store(true, Ordering::Relaxed);
                    cx.notify();
                })))
            })
            .when(!scanning, |d| d.child(button("junk-again", t!("Search again"), true, &p).on_click(cx.listener(|v, _, _, cx| v.start(cx)))))
            .when(has, |d| {
                d.child(button("junk-old", t!("Tick all older than 30 days"), true, &p).on_click(cx.listener(|v, _, _, cx| v.tick_old(cx))))
                    .child(button("junk-clear", t!("Clear ticks"), nt > 0, &p).on_click(cx.listener(|v, _, _, cx| {
                        v.ticked.clear();
                        cx.notify();
                    })))
                    .child(
                        button("junk-clean", clean_label, nt > 0, &p)
                            .when(nt > 0, |b| b.bg(p.accent).text_color(p.accent_text))
                            .on_click(cx.listener(|v, _, w, cx| v.ask_clean(w, cx))),
                    )
            });
        let body = if self.items.is_empty() {
            let msg = if scanning { t!("Searching…") } else { t!("Nothing to show") };
            div().flex_1().flex().items_center().justify_center().text_sm().text_color(p.text_tertiary).child(msg).into_any_element()
        } else {
            uniform_list(
                "junk-rows",
                self.items.len(),
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

impl JunkView {
    fn render_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let it = &self.items[i];
        let on = self.ticked.contains(&it.junk.path);
        let hov = self.hover == Some(i);
        let name = rf_core::history::display_name(&it.junk.path);
        let kind = crate::i18n::t(it.junk.kind.label());
        let project = self.shown_project(&it.junk.project);
        let size = if it.measured { format::size(it.bytes) } else { format!("{}…", format::size(it.bytes)) };
        let (p1, p3) = (it.junk.path.clone(), it.junk.path.clone());
        h_flex()
            .id(("junk-row", i))
            .w_full()
            .h(theme::ROW_H)
            .px_2()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .rounded(theme::RADIUS_SM)
            .text_sm()
            .cursor_pointer()
            .when(hov, |d| d.bg(p.hover))
            .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                let want = h.then_some(i);
                if *h && v.hover != want || !*h && v.hover == Some(i) {
                    v.hover = want;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |v, _, _, cx| v.toggle(&p1, cx)))
            .child(
                div()
                    .size(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if on { p.accent } else { p.text_tertiary })
                    .when(on, |d| d.bg(p.accent).child(Icon::new(L::Check).size(px(12.)).text_color(p.accent_text))),
            )
            .child(div().flex_shrink_0().w(px(150.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text).child(name))
            .child(div().flex_shrink_0().w(px(150.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_secondary).child(kind))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(project))
            .child(div().w(px(90.)).flex_shrink_0().text_xs().text_right().text_color(p.text).child(size))
            .child(div().w(px(120.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(format::datetime(it.modified_ms)))
            .child(
                div()
                    .id(("junk-open", i))
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
                        cx.emit(JunkEvent::ShowInFolder(p3.clone()));
                    })),
            )
            .into_any_element()
    }
}
