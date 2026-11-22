//! Disk usage page (Location::DiskUsage): where the space under a folder or drive goes.
//! The walk is [`leaf_sys::diskusage`] (below-normal priority, closing the tab stops it); this
//! file draws the result: the current folder's children as a list, biggest first, and as a
//! squarified treemap (folders show their own children inside them, one level deep).

use std::path::PathBuf;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;
use rf_core::treemap::{Arc, Rect, RingNode, ring_bounds, skyline, squarify, sunburst, sunburst_hit};
use leaf_sys::diskusage::{DiskScan, DiskTree, Metric};

use crate::fluent_menu::{self, Item, Menu, MenuIcon};
use crate::icons;
use crate::t;
use crate::theme::{self, Palette};

/// What the sizes in the view measure: bytes (the scan's two) or, with the agent layer on, an estimate of the tokens in text files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Measure {
    Bytes(Metric),
    Tokens,
    /// How many agent sessions wrote the file (a folder: the sum below it); from the sessions' own files.
    Activity,
}

/// The scan with the token counts beside it (when they were made): `weight` and `sorted_children` take the measure.
struct Weighed<'a> {
    tree: &'a DiskTree,
    tokens: Option<&'a [u64]>,
    activity: Option<&'a [u64]>,
}

impl std::ops::Deref for Weighed<'_> {
    type Target = DiskTree;
    fn deref(&self) -> &DiskTree {
        self.tree
    }
}

impl<'a> Weighed<'a> {
    fn of(tree: &'a Option<DiskTree>, tokens: &'a Option<Vec<u64>>, activity: &'a Option<Vec<u64>>) -> Option<Weighed<'a>> {
        tree.as_ref().map(|tree| Weighed { tree, tokens: tokens.as_deref(), activity: activity.as_deref() })
    }

    fn weight(&self, i: u32, m: Measure) -> u64 {
        match m {
            Measure::Bytes(b) => self.tree.weight(i, b),
            Measure::Tokens => self.tokens.and_then(|t| t.get(i as usize)).copied().unwrap_or(0),
            Measure::Activity => self.activity.and_then(|t| t.get(i as usize)).copied().unwrap_or(0),
        }
    }

    fn sorted_children(&self, i: u32, m: Measure) -> Vec<u32> {
        match m {
            Measure::Bytes(b) => self.tree.sorted_children(i, b),
            Measure::Tokens | Measure::Activity => {
                let mut v: Vec<u32> = self.tree.children(i).collect();
                v.sort_unstable_by(|&a, &b| self.weight(b, m).cmp(&self.weight(a, m)).then_with(|| self.tree.name(a).cmp(self.tree.name(b))));
                v
            }
        }
    }
}

/// Estimated tokens below every entry, from the sizes the scan already holds: a text-like file counts its size by the byte rule
/// "Copy as context" uses (bytes / 4), a folder the sum of its files. No file is read.
fn token_counts(t: &DiskTree) -> Vec<u64> {
    let mut out = vec![0u64; t.len()];
    for id in 0..t.len() as u32 {
        if t.is_dir(id) || !text_like(t.name(id)) {
            continue;
        }
        let n = leaf_dev::context::estimate_tokens(t.size(id));
        let mut at = Some(id);
        while let Some(i) = at {
            out[i as usize] += n;
            at = t.parent(i);
        }
    }
    out
}

/// A name leaf reads as text (source, plain text, the extensions the Space viewer shows as text).
fn text_like(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else { return false };
    let ext = ext.to_ascii_lowercase();
    let cat = icons::category_for_ext(&ext);
    matches!(cat, icons::Category::Code | icons::Category::Text)
        || (cat == icons::Category::Other && crate::look::viewers::text::EXTENSIONS.contains(&ext.as_str()))
        || matches!(ext.as_str(), "csv" | "gitignore" | "env" | "editorconfig" | "gitattributes" | "srt")
}

/// A weight as shown: a size, or "~12.4k" tokens.
fn show(m: Measure, w: u64) -> String {
    match m {
        Measure::Bytes(_) => format::size(w),
        Measure::Tokens => {
            let (n, u) = show_parts(m, w);
            format!("~{n}{u}")
        }
        Measure::Activity => w.to_string(),
    }
}

/// The number and the unit of a weight, for the list's two columns.
fn show_parts(m: Measure, w: u64) -> (String, String) {
    match m {
        Measure::Bytes(_) => {
            let (n, u) = format::size_parts(w);
            (n.to_string(), u.to_string())
        }
        Measure::Tokens if w >= 1_000_000 => (format!("{:.1}", w as f64 / 1e6), "M tok".into()),
        Measure::Tokens if w >= 1000 => (format!("{:.1}", w as f64 / 1e3), "k tok".into()),
        Measure::Tokens => (w.to_string(), " tok".into()),
        Measure::Activity => (w.to_string(), if w == 1 { " session" } else { " sessions" }.into()),
    }
}

/// Rows in the list (the biggest ones; the treemap shows the rest by area).
const LIST_MAX: usize = 400;
/// Rectangles smaller than this (either side) are not drawn.
const MIN_RECT: f32 = 3.;
/// Height of a folder's name strip inside the treemap.
const HEAD_H: f32 = 17.;
/// A folder rectangle shows its children when it is at least this big.
const NEST_MIN: (f32, f32) = (70., 46.);

/// Tab and breadcrumb title: "Disk kullanımı — Windows".
pub fn title(root: &std::path::Path) -> String {
    format!("{} — {}", t!("Disk usage"), rf_core::history::display_name(root))
}

pub enum DiskEvent {
    /// Open this path's folder in a normal view with the item selected.
    ShowInFolder(PathBuf),
    /// Open this folder in a new tab, and leave this tab on the analysis (issue 35: "investigating a large subfolder ruins your
    /// current analysis progress because you lose the broader disk usage view and have to trigger a rescan all over again").
    OpenInNewTab(PathBuf),
}

impl EventEmitter<DiskEvent> for DiskUsageView {}

/// How the current level is drawn (same data, colours, hover text and menu in each).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Treemap,
    Sunburst,
    Skyline,
}

/// Rings around the centre of the sunburst, and how many children each ring keeps.
const RINGS: u8 = 3;
const RING_KIDS: usize = 40;

/// Centre, hole radius and outer radius of the sunburst inside a `w` x `h` area.
fn sun_geom(w: f32, h: f32) -> (f32, f32, f32, f32) {
    let outer = (w.min(h) / 2. - 4.).max(10.);
    (w / 2., h / 2., outer * 0.24, outer)
}

fn ring_nodes(t: &Weighed, m: Measure, ids: &[u32], depth: u8) -> Vec<RingNode> {
    ids.iter()
        .take(RING_KIDS)
        .map(|&id| {
            let children = if depth + 1 < RINGS && t.is_dir(id) { ring_nodes(t, m, &t.sorted_children(id, m), depth + 1) } else { Vec::new() };
            RingNode { id, weight: t.weight(id, m), children }
        })
        .collect()
}

/// A column of the list that can be sorted by (Share and Size give the same order inside one folder).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortCol {
    Name,
    Size,
    Share,
    Files,
}

/// The list's widths, shared by the heading row and the rows so they line up.
const SIZE_NUM_W: f32 = 44.;
const SIZE_UNIT_W: f32 = 22.;
const BAR_W: f32 = 110.;
const PCT_W: f32 = 32.;
const FILES_W: f32 = 48.;

/// One drawn rectangle of the treemap.
struct Cell {
    id: u32,
    rect: Rect,
    /// A folder's name strip (its children are separate cells inside it).
    head: bool,
    nested: bool,
}

pub struct DiskUsageView {
    root: PathBuf,
    scan: Option<DiskScan>,
    /// (entries, bytes) while walking.
    progress: (u64, u64),
    tree: Option<DiskTree>,
    cur: u32,
    metric: Measure,
    /// Estimated tokens below every entry (text-like files only), made when the Tokens measure is first chosen.
    tokens: Option<Vec<u64>>,
    /// Agent sessions that wrote every entry, made when the Agent activity measure is first chosen.
    activity: Option<Vec<u64>>,
    /// Children of `cur`, biggest first.
    kids: Vec<u32>,
    /// `kids` in the list's order (the chosen sort; the treemap keeps `kids`).
    rows: Vec<u32>,
    sort: SortCol,
    sort_desc: bool,
    selected: Option<u32>,
    hover: Option<u32>,
    /// Size of the treemap area, measured while drawing.
    area: Size<Pixels>,
    cells: Option<((u32, Measure, Mode, i32, i32), Vec<Cell>)>,
    mode: Mode,
    /// Top left of the drawing area in the window, measured while drawing (the sunburst needs it for the pointer).
    origin: Point<Pixels>,
    /// Sunburst segments of `cur`, built when first needed.
    arcs: Option<Vec<Arc>>,
    started: std::time::Instant,
    _poll: Option<Task<()>>,
}

impl DiskUsageView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            scan: Some(DiskScan::start(root.clone())),
            root,
            progress: (0, 0),
            tree: None,
            cur: 0,
            metric: Measure::Bytes(Metric::Logical),
            tokens: None,
            activity: None,
            kids: Vec::new(),
            rows: Vec::new(),
            sort: SortCol::Size,
            sort_desc: true,
            selected: None,
            hover: None,
            area: size(px(600.), px(400.)),
            cells: None,
            mode: Mode::Treemap,
            origin: point(px(0.), px(0.)),
            arcs: None,
            started: std::time::Instant::now(),
            _poll: None,
        };
        this.poll(cx);
        this
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Watches the walk: counters twice a second, the finished tree when it arrives.
    fn poll(&mut self, cx: &mut Context<Self>) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let Ok(done) = this.update(cx, |v, cx| {
                    let Some(scan) = &v.scan else { return true };
                    v.progress = scan.progress();
                    if let Some(t) = scan.take() {
                        v.tree = Some(t);
                        v.tokens = None;
                        v.activity = None;
                        v.scan = None;
                        v.cur = 0;
                        v.refresh_kids();
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

    fn refresh_kids(&mut self) {
        self.cells = None;
        self.arcs = None;
        self.kids = Weighed::of(&self.tree, &self.tokens, &self.activity).map(|t| t.sorted_children(self.cur, self.metric)).unwrap_or_default();
        self.sort_rows();
    }

    /// Puts `kids` in the list's order.
    fn sort_rows(&mut self) {
        let mut rows = self.kids.clone();
        if let Some(t) = Weighed::of(&self.tree, &self.tokens, &self.activity) {
            let m = self.metric;
            // ascending for the column, then flipped for descending; kids come biggest first, which is Size/Share descending
            match self.sort {
                SortCol::Size | SortCol::Share => rows.reverse(),
                SortCol::Name => rows.sort_by_cached_key(|&i| t.name(i).to_lowercase()),
                SortCol::Files => rows.sort_by_key(|&i| (if t.is_dir(i) { t.files(i) } else { 0 }, std::cmp::Reverse(t.weight(i, m)))),
            }
            if self.sort_desc {
                rows.reverse();
            }
        }
        self.rows = rows;
    }

    /// A heading was clicked: sort by it, or reverse when it is already the sort.
    fn set_sort(&mut self, c: SortCol, cx: &mut Context<Self>) {
        if self.sort == c {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = c;
            self.sort_desc = c != SortCol::Name;
        }
        self.sort_rows();
        cx.notify();
    }

    fn go(&mut self, id: u32, cx: &mut Context<Self>) {
        self.cur = id;
        self.selected = None;
        self.refresh_kids();
        cx.notify();
    }

    fn up(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.tree.as_ref().and_then(|t| t.parent(self.cur)) {
            let from = self.cur;
            self.go(p, cx);
            self.selected = Some(from);
        }
    }

    fn set_mode(&mut self, m: Mode, cx: &mut Context<Self>) {
        if self.mode != m {
            self.mode = m;
            cx.notify();
        }
    }

    fn set_hover(&mut self, want: Option<u32>, cx: &mut Context<Self>) {
        if self.hover != want {
            self.hover = want;
            cx.notify();
        }
    }

    fn ensure_arcs(&mut self) {
        if self.arcs.is_none()
            && let Some(t) = Weighed::of(&self.tree, &self.tokens, &self.activity)
        {
            self.arcs = Some(sunburst(&ring_nodes(&t, self.metric, &self.kids, 0)));
        }
    }

    /// The sunburst segment under a window position: Some(Some(id)) a segment, Some(None) the centre, None nothing.
    fn sun_hit(&mut self, pos: Point<Pixels>) -> Option<Option<u32>> {
        self.ensure_arcs();
        let (w, h) = (f32::from(self.area.width), f32::from(self.area.height));
        let (cx, cy, hole, outer) = sun_geom(w, h);
        let (dx, dy) = (f32::from(pos.x - self.origin.x) - cx, f32::from(pos.y - self.origin.y) - cy);
        let arcs = self.arcs.as_ref()?;
        match sunburst_hit(arcs, dx, dy, hole, outer, RINGS) {
            Some(i) => Some(Some(arcs[i].id)),
            None if dx.hypot(dy) < hole => Some(None),
            None => None,
        }
    }

    fn set_metric(&mut self, m: Measure, cx: &mut Context<Self>) {
        if self.metric != m {
            self.metric = m;
            if m == Measure::Tokens && self.tokens.is_none() {
                self.tokens = self.tree.as_ref().map(token_counts);
            }
            if m == Measure::Activity && self.activity.is_none() {
                self.read_activity(cx);
            }
            self.refresh_kids();
            cx.notify();
        }
    }

    /// Reads which files the agent sessions wrote (their own session files, off the UI thread) and counts them into the tree.
    fn read_activity(&mut self, cx: &mut Context<Self>) {
        let sessions = crate::agents::sessions(cx);
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let adapters = leaf_agents::adapters();
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    sessions
                        .iter()
                        .map(|s| adapters.iter().find(|a| a.id() == s.agent).and_then(|a| a.session_file(&s.id)).map(|f| leaf_agents::transcript::files_touched(&f, &cancel)).unwrap_or_default())
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                let Some(t) = &v.tree else { return };
                let mut out = vec![0u64; t.len()];
                for session in files {
                    for f in session {
                        let mut at = t.find(std::path::Path::new(&f));
                        if at.is_some_and(|i| t.is_dir(i)) {
                            at = None;
                        }
                        while let Some(i) = at {
                            out[i as usize] += 1;
                            at = t.parent(i);
                        }
                    }
                }
                v.activity = Some(out);
                v.refresh_kids();
                cx.notify();
            });
        })
        .detach();
    }

    /// Click on a row or rectangle: folders open, files get selected; a double click on a
    /// file opens it.
    fn activate(&mut self, id: u32, clicks: usize, cx: &mut Context<Self>) {
        let Some(t) = &self.tree else { return };
        if t.is_dir(id) {
            self.go(id, cx);
        } else if clicks >= 2 {
            crate::folder_view::open_in_background(t.path(id), cx);
        } else {
            self.selected = Some(id);
            cx.notify();
        }
    }

    fn show_in_folder(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(t) = &self.tree {
            cx.emit(DiskEvent::ShowInFolder(t.path(id)));
        }
    }

    /// A folder of the analysis in a tab of its own: from its menu, or a middle click on its row or rectangle. A file has no tab to
    /// open; its folder is what "show in folder" is for.
    fn open_in_new_tab(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(t) = &self.tree
            && t.is_dir(id)
        {
            cx.emit(DiskEvent::OpenInNewTab(t.path(id)));
        }
    }

    fn context_menu(&mut self, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = &self.tree else { return };
        let is_dir = t.is_dir(id);
        self.selected = Some(id);
        cx.notify();
        let me = cx.entity().downgrade();
        let mut items = Vec::new();
        {
            let me = me.clone();
            let label = if is_dir { t!("Go to this folder") } else { t!("Open") };
            items.push(Item::action(label, MenuIcon::Lucide(if is_dir { L::FolderOpen } else { L::SquareArrowOutUpRight }), move |_, cx| {
                let _ = me.update(cx, |v, cx| v.activate(id, 2, cx));
            }));
        }
        if is_dir {
            let me = me.clone();
            items.push(Item::action(t!("Open in new tab"), MenuIcon::Lucide(L::SquarePlus), move |_, cx| {
                let _ = me.update(cx, |v, cx| v.open_in_new_tab(id, cx));
            }));
        }
        items.push(Item::action(t!("Show in folder"), MenuIcon::Lucide(L::FolderSearch), move |_, cx| {
            let _ = me.update(cx, |v, cx| v.show_in_folder(id, cx));
        }));
        fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    // ---------- treemap layout

    /// Rectangles for the current folder inside `w` x `h`.
    fn layout(&mut self, w: f32, h: f32) -> &[Cell] {
        let key = (self.cur, self.metric, self.mode, w as i32, h as i32);
        if self.cells.as_ref().is_none_or(|(k, _)| *k != key) {
            let cells = Weighed::of(&self.tree, &self.tokens, &self.activity).map(|t| if self.mode == Mode::Skyline { build_bars(&t, self.metric, &self.kids, w, h) } else { build_cells(&t, self.metric, &self.kids, w, h) }).unwrap_or_default();
            self.cells = Some((key, cells));
        }
        self.cells.as_ref().map(|(_, c)| c.as_slice()).unwrap_or(&[])
    }
}

fn build_bars(t: &Weighed, m: Measure, kids: &[u32], w: f32, h: f32) -> Vec<Cell> {
    let weights: Vec<u64> = kids.iter().map(|&k| t.weight(k, m)).collect();
    // room under the bars for nothing: they stand on the bottom edge
    skyline(&weights, Rect::new(0., 0., w, h), 3., 14., 96.)
        .into_iter()
        .zip(kids)
        .filter(|(r, _)| r.h >= 1.)
        .map(|(rect, &id)| Cell { id, rect, head: false, nested: false })
        .collect()
}

fn build_cells(t: &Weighed, m: Measure, kids: &[u32], w: f32, h: f32) -> Vec<Cell> {
    let mut out = Vec::new();
    let weights: Vec<u64> = kids.iter().map(|&k| t.weight(k, m)).collect();
    let rects = squarify(&weights, Rect::new(0., 0., w, h));
    for (&id, r) in kids.iter().zip(rects) {
        if r.w < MIN_RECT || r.h < MIN_RECT {
            continue;
        }
        let r = r.inset(1.);
        out.push(Cell { id, rect: r, head: false, nested: false });
        if !t.is_dir(id) || r.w < NEST_MIN.0 || r.h < NEST_MIN.1 {
            continue;
        }
        // the folder's children inside it, below its name strip
        let inner = Rect::new(r.x + 3., r.y + HEAD_H, (r.w - 6.).max(0.), (r.h - HEAD_H - 3.).max(0.));
        let sub = t.sorted_children(id, m);
        let sw: Vec<u64> = sub.iter().map(|&k| t.weight(k, m)).collect();
        out.push(Cell { id, rect: Rect::new(r.x, r.y, r.w, HEAD_H), head: true, nested: false });
        for (&c, cr) in sub.iter().zip(squarify(&sw, inner)) {
            if cr.w >= MIN_RECT && cr.h >= MIN_RECT {
                out.push(Cell { id: c, rect: cr.inset(0.5), head: false, nested: true });
            }
        }
    }
    out
}

fn root_label(t: &DiskTree, cur: u32, root: &std::path::Path) -> String {
    if t.parent(cur).is_none() {
        let n = rf_core::history::display_name(root);
        if n.is_empty() { root.to_string_lossy().into_owned() } else { n }
    } else {
        t.name(cur).to_string()
    }
}

fn category_of(t: &DiskTree, id: u32) -> icons::Category {
    if t.is_dir(id) {
        return icons::Category::Folder;
    }
    let name = t.name(id);
    name.rsplit_once('.').map_or(icons::Category::Other, |(_, e)| icons::category_for_ext(&e.to_ascii_lowercase()))
}

fn count(n: u64) -> String {
    crate::bench_view::count(n)
}

impl Render for DiskUsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        // (the agent layer was switched off while Tokens was showing: back to sizes)
        if matches!(self.metric, Measure::Tokens | Measure::Activity) && !cx.global::<crate::settings::Settings>().ai_layer {
            self.metric = Measure::Bytes(Metric::Logical);
            self.refresh_kids();
        }
        let tokens_on = cx.global::<crate::settings::Settings>().ai_layer;
        let Some(tw) = Weighed::of(&self.tree, &self.tokens, &self.activity) else {
            return self.render_scanning(&p).into_any_element();
        };
        let t = &tw;
        let metric = self.metric;
        let cur = self.cur;
        let total = t.weight(cur, metric).max(1);

        // ---- header: up button, path, size, metric switch
        let mut crumbs = Vec::new();
        let mut c = cur;
        loop {
            crumbs.push(c);
            match t.parent(c) {
                Some(par) => c = par,
                None => break,
            }
        }
        crumbs.reverse();
        let root_name = {
            let n = rf_core::history::display_name(&self.root);
            if n.is_empty() { self.root.to_string_lossy().into_owned() } else { n }
        };
        let mut path_row = h_flex().gap_1().items_center().min_w_0().overflow_hidden();
        for (i, &c) in crumbs.iter().enumerate() {
            let label = if i == 0 { root_name.clone() } else { t.name(c).to_string() };
            let last = i + 1 == crumbs.len();
            if i > 0 {
                path_row = path_row.child(Icon::new(L::ChevronRight).size(px(12.)).text_color(p.text_tertiary));
            }
            path_row = path_row.child(
                div()
                    .id(("crumb", i))
                    .px_1p5()
                    .py_0p5()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .whitespace_nowrap()
                    .when(last, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(p.text))
                    .when(!last, |d| d.text_color(p.text_secondary).cursor_pointer().hover(|s| s.bg(p.hover)).on_click(cx.listener(move |v, _, _, cx| v.go(c, cx))))
                    .child(label),
            );
        }
        let can_up = t.parent(cur).is_some();
        let up_btn = div()
            .id("du-up")
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme::RADIUS_SM)
            .when(can_up, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)).on_click(cx.listener(|v, _, _, cx| v.up(cx))))
            .child(Icon::new(L::ArrowUp).size(px(16.)).text_color(if can_up { p.text } else { p.text_tertiary }));
        let seg = |id: &'static str, label: &'static str, m: Measure| {
            let on = metric == m;
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(theme::RADIUS_SM)
                .text_xs()
                .cursor_pointer()
                .when(on, |d| d.bg(p.selection).text_color(p.text))
                .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |v, _, _, cx| v.set_metric(m, cx)))
                .child(label)
        };
        let mode = self.mode;
        let vseg = |id: &'static str, label: &'static str, m: Mode| {
            let on = mode == m;
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded(theme::RADIUS_SM)
                .text_xs()
                .cursor_pointer()
                .when(on, |d| d.bg(p.selection).text_color(p.text))
                .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
                .on_click(cx.listener(move |v, _, _, cx| v.set_mode(m, cx)))
                .child(label)
        };
        let summary = format!(
            "{}: {} · {}",
            match metric {
                Measure::Bytes(Metric::OnDisk) => t!("Size on disk"),
                Measure::Bytes(Metric::Logical) => t!("Size"),
                Measure::Tokens => t!("Tokens (estimate)"),
                Measure::Activity => t!("Agent activity"),
            },
            show(metric, t.weight(cur, metric)),
            format!("{} {}", count(t.files(cur) as u64), t!("files"))
        );
        let header = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(up_btn)
            .child(div().flex_1().min_w_0().child(path_row))
            .child(div().text_xs().text_color(p.text_secondary).whitespace_nowrap().child(summary))
            .child(
                h_flex()
                    .gap_0p5()
                    .p_0p5()
                    .rounded(theme::RADIUS_SM)
                    .bg(p.pressed)
                    .child(vseg("v-tree", t!("Treemap"), Mode::Treemap))
                    .child(vseg("v-sun", t!("Sunburst"), Mode::Sunburst))
                    .child(vseg("v-sky", t!("Skyline"), Mode::Skyline)),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .p_0p5()
                    .rounded(theme::RADIUS_SM)
                    .bg(p.pressed)
                    .child(seg("m-logical", t!("Size"), Measure::Bytes(Metric::Logical)))
                    .child(seg("m-disk", t!("Size on disk"), Measure::Bytes(Metric::OnDisk)))
                    .children(tokens_on.then(|| seg("m-tokens", t!("Tokens (estimate)"), Measure::Tokens)))
                    .children(tokens_on.then(|| seg("m-activity", t!("Agent activity"), Measure::Activity))),
            );

        // ---- list
        let sort = self.sort;
        let sort_desc = self.sort_desc;
        let head_col = |id: &'static str, label: &'static str, col: SortCol, right: bool, cx: &mut Context<Self>| {
            let active = sort == col;
            let arrow = if sort_desc { L::ChevronDown } else { L::ChevronUp };
            h_flex()
                .id(id)
                .h_full()
                .gap_1()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .text_color(if active { p.text_secondary } else { p.text_tertiary })
                .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                .when(right, |d| d.justify_end())
                .overflow_hidden()
                .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(label))
                .when(active, |d| d.child(Icon::new(arrow).size(px(12.)).flex_shrink_0().text_color(p.text_tertiary)))
                .on_click(cx.listener(move |v, _, _, cx| v.set_sort(col, cx)))
        };
        // ("1.1 M tok" needs a wider unit column than "MB")
        let unit_w = if metric == Measure::Tokens { 44. } else { SIZE_UNIT_W };
        // same padding and gaps as a row, so the headings sit over their columns (the Files heading says what the column counts)
        let list_head = h_flex()
            .h(px(24.))
            .px(px(12.))
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .text_xs()
            .border_b_1()
            .border_color(p.divider)
            .child(div().w(px(16.)).flex_shrink_0())
            .child(head_col("lh-name", t!("Name"), SortCol::Name, false, cx).flex_1().min_w_0())
            .child(head_col("lh-size", t!("Size"), SortCol::Size, true, cx).w(px(SIZE_NUM_W + unit_w + 4.)).flex_shrink_0())
            .child(head_col("lh-share", t!("Share"), SortCol::Share, false, cx).w(px(BAR_W + PCT_W + 4.)).flex_shrink_0())
            .child(head_col("lh-files", t!("Files"), SortCol::Files, true, cx).w(px(FILES_W)).flex_shrink_0());
        let mut list = v_flex().id("du-list").flex_1().min_h_0().overflow_y_scroll().p_1();
        let tnum = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".to_string(), 1)]));
        for (i, &id) in self.rows.iter().take(LIST_MAX).enumerate() {
            let w = t.weight(id, metric);
            let frac = w as f32 / total as f32;
            let cat = category_of(t, id);
            let is_dir = t.is_dir(id);
            let sel = self.selected == Some(id);
            let hov = self.hover == Some(id);
            let (num, unit) = show_parts(metric, w);
            let bar = if is_dir { p.accent } else { theme::kind_color(cat, p.dark) };
            list = list.child(
                h_flex()
                    .id(("du-row", i))
                    .relative()
                    .h(theme::ROW_H)
                    .px_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(p.selection).child(theme::selection_pill(&p)))
                    .when(!sel && hov, |d| d.bg(p.hover))
                    .on_hover(cx.listener(move |v, on: &bool, _, cx| {
                        let want = on.then_some(id);
                        if *on && v.hover != want || !*on && v.hover == Some(id) {
                            v.hover = want;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |v, ev: &ClickEvent, _, cx| v.activate(id, ev.click_count(), cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |v, _, window, cx| v.context_menu(id, window, cx)))
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |v, _, _, cx| v.open_in_new_tab(id, cx)))
                    .child(Icon::new(icons::icon_for(cat)).size(px(16.)).text_color(theme::kind_color(cat, p.dark)))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(t.name(id).to_string()))
                    // number and unit in their own columns, so the decimal points and the units form straight lines
                    .child(
                        h_flex()
                            .w(px(SIZE_NUM_W + unit_w + 4.))
                            .flex_shrink_0()
                            .gap_1()
                            .text_color(p.text_secondary)
                            .child(div().w(px(SIZE_NUM_W)).text_right().font_features(tnum.clone()).child(num))
                            .child(div().w(px(unit_w)).whitespace_nowrap().child(unit)),
                    )
                    .child(
                        h_flex()
                            .w(px(BAR_W + PCT_W + 4.))
                            .flex_shrink_0()
                            .items_center()
                            .gap_1()
                            .child(div().h(px(6.)).w(px(BAR_W)).flex_shrink_0().rounded_full().bg(p.pressed).child(div().h_full().rounded_full().bg(bar).w(relative(frac.clamp(0.0, 1.0)))))
                            .child(div().w(px(PCT_W)).text_right().text_color(p.text_secondary).font_features(tnum.clone()).child(crate::i18n::pct(frac * 100.))),
                    )
                    .child(div().w(px(FILES_W)).flex_shrink_0().text_right().text_color(p.text_secondary).font_features(tnum.clone()).when(is_dir, |d| d.child(count(t.files(id) as u64)))),
            );
        }
        let hidden = self.rows.len().saturating_sub(LIST_MAX);
        if hidden > 0 {
            list = list.child(div().px_2().py_1().text_xs().text_color(p.text_tertiary).child(format!("+{} {}", count(hidden as u64), t!("more items"))));
        }
        if self.rows.is_empty() {
            list = list.child(div().p_4().text_sm().text_color(p.text_tertiary).child(t!("This folder is empty")));
        }

        // ---- treemap
        let (aw, ah) = (f32::from(self.area.width), f32::from(self.area.height));
        let hover = self.hover;
        let selected = self.selected;
        let dark = p.dark;
        let folder_bg = if dark { hsla(0., 0., 1., 0.07) } else { hsla(0., 0., 0., 0.06) };
        let folder_head = if dark { hsla(0., 0., 1., 0.12) } else { hsla(0., 0., 0., 0.10) };
        let sun = if mode == Mode::Sunburst {
            self.ensure_arcs();
            self.arcs.clone().unwrap_or_default()
        } else {
            Vec::new()
        };
        let flat = mode == Mode::Skyline;
        let cells: Vec<(u32, Rect, bool, bool)> = if mode == Mode::Sunburst { Vec::new() } else { self.layout(aw, ah).iter().map(|c| (c.id, c.rect, c.head, c.nested)).collect() };
        let tw = Weighed::of(&self.tree, &self.tokens, &self.activity).expect("tree");
        let t = &tw;
        let mut map = div().relative().size_full().overflow_hidden();
        for (i, &(id, r, head, nested)) in cells.iter().enumerate() {
            let is_dir = t.is_dir(id);
            let cat = category_of(t, id);
            let lit = hover == Some(id) || selected == Some(id);
            let base = if is_dir && flat {
                let mut c = theme::kind_color(cat, dark);
                c.a = if lit { 1.0 } else { 0.82 };
                c
            } else if is_dir {
                if head { folder_head } else { folder_bg }
            } else {
                let mut c = theme::kind_color(cat, dark);
                c.a = if lit { 1.0 } else { 0.82 };
                c
            };
            let mut cell = div()
                .id(("du-cell", i))
                .absolute()
                .left(px(r.x))
                .top(px(r.y))
                .w(px(r.w))
                .h(px(r.h))
                .rounded(px(if r.w > 14. && r.h > 14. { 4. } else { 1.5 }))
                .overflow_hidden()
                .bg(base)
                .when(is_dir && !head && !nested && !flat, |d| d.border_1().border_color(p.border))
                .when(lit && !head, |d| d.border_1().border_color(p.text_secondary))
                .cursor_pointer()
                .on_hover(cx.listener(move |v, on: &bool, _, cx| {
                    let want = on.then_some(id);
                    if *on && v.hover != want || !*on && v.hover == Some(id) {
                        v.hover = want;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |v, ev: &ClickEvent, _, cx| v.activate(id, ev.click_count(), cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(move |v, _, window, cx| v.context_menu(id, window, cx)))
                .on_mouse_down(MouseButton::Middle, cx.listener(move |v, _, _, cx| v.open_in_new_tab(id, cx)));
            // a folder's body sits behind its children: only its strip and the empty part take
            // clicks, which is the same folder either way
            let label_color = if is_dir && !flat { p.text } else { gpui::white() };
            if head {
                // the name gives way first: the size keeps its room when the strip is narrow
                cell = cell
                    .px_1p5()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(label_color)
                    .whitespace_nowrap()
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(t.name(id).to_string()))
                    .when(r.w > 56., |d| d.child(div().flex_shrink_0().child(show(metric, t.weight(id, metric)))));
            } else if !is_dir && r.w > 44. && r.h > 20. {
                let big = r.w > 80. && r.h > 36.;
                cell = cell.p_1().text_xs().text_color(label_color).child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(t.name(id).to_string())).when(big, |d| {
                    d.child(div().overflow_hidden().whitespace_nowrap().opacity(0.8).child(show(metric, t.weight(id, metric))))
                });
            } else if is_dir && !head && !cells.iter().any(|&(i2, _, h2, _)| h2 && i2 == id) && r.w > 44. && r.h > 20. {
                // a folder without room for its children (or one inside another folder): just its name
                cell = cell.p_1().text_xs().text_color(label_color).child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(t.name(id).to_string()));
            }
            map = map.child(cell);
        }
        let this = cx.entity();
        let probe = canvas(
            move |bounds, window, cx| {
                let changed = this.update(cx, |v, cx| {
                    v.origin = bounds.origin;
                    let ch = v.area != bounds.size;
                    if ch {
                        v.area = bounds.size;
                        cx.notify();
                    }
                    ch
                });
                if changed {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        if mode == Mode::Sunburst {
            let colors: Vec<(Arc, Hsla)> = sun
                .iter()
                .map(|a| {
                    let mut c = theme::kind_color(category_of(t, a.id), dark);
                    let lit = hover == Some(a.id) || selected == Some(a.id);
                    // folders stay quieter than the files, so the files show up against them
                    c.a = if lit { 1.0 } else if t.is_dir(a.id) { 0.5 - 0.08 * a.depth as f32 } else { 0.92 - 0.08 * a.depth as f32 };
                    (*a, c)
                })
                .collect();
            let (cx0, cy0, hole, outer) = sun_geom(aw, ah);
            let paint = canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    for (a, color) in &colors {
                        let (r0, r1) = ring_bounds(a.depth, hole, outer, RINGS);
                        let (r0, r1) = (r0 + 0.5, r1 - 0.5);
                        // half a pixel of gap on both sides of the segment
                        let (p0, p1) = (0.5 / r1, 0.5 / r1);
                        let (s, e) = (a.a0 + p0, (a.a1 - p1).max(a.a0 + p0));
                        let steps = (((e - s) / 0.05).ceil() as usize).max(1);
                        let at = |r: f32, ang: f32| point(bounds.origin.x + px(cx0 + r * ang.sin()), bounds.origin.y + px(cy0 - r * ang.cos()));
                        let mut pts = Vec::with_capacity(2 * steps + 2);
                        for i in 0..=steps {
                            pts.push(at(r1, s + (e - s) * i as f32 / steps as f32));
                        }
                        for i in (0..=steps).rev() {
                            pts.push(at(r0, s + (e - s) * i as f32 / steps as f32));
                        }
                        let mut b = PathBuilder::fill();
                        b.add_polygon(&pts, true);
                        if let Ok(path) = b.build() {
                            window.paint_path(path, *color);
                        }
                    }
                },
            )
            .absolute()
            .size_full();
            let focus = hover.or(selected).filter(|&f| f != cur);
            let centre_name = focus.map_or_else(|| root_label(t, cur, &self.root), |f| t.name(f).to_string());
            let centre_size = show(metric, t.weight(focus.unwrap_or(cur), metric));
            let can_up = t.parent(cur).is_some();
            let centre = v_flex()
                .absolute()
                .left(px(cx0 - hole))
                .top(px(cy0 - hole))
                .size(px(hole * 2.))
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(if can_up && focus.is_none() && hover.is_none() { p.hover } else { hsla(0., 0., 0., 0.) })
                .px_2()
                .child(div().max_w_full().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(centre_name))
                .child(div().text_xs().text_color(p.text_secondary).child(centre_size))
                .when(can_up, |d| d.child(Icon::new(L::ArrowUp).size(px(14.)).text_color(p.text_tertiary)));
            let events = div()
                .id("du-sun")
                .absolute()
                .size_full()
                .cursor_pointer()
                .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                    let h = v.sun_hit(ev.position).flatten();
                    v.set_hover(h, cx);
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, _, cx| match v.sun_hit(ev.position) {
                    Some(Some(id)) => v.activate(id, ev.click_count, cx),
                    Some(None) => v.up(cx),
                    None => {}
                }))
                .on_mouse_down(MouseButton::Right, cx.listener(|v, ev: &MouseDownEvent, window, cx| {
                    if let Some(Some(id)) = v.sun_hit(ev.position) {
                        v.context_menu(id, window, cx);
                    }
                }))
                .on_mouse_down(MouseButton::Middle, cx.listener(|v, ev: &MouseDownEvent, _, cx| {
                    if let Some(Some(id)) = v.sun_hit(ev.position) {
                        v.open_in_new_tab(id, cx);
                    }
                }));
            map = map.child(paint).child(centre).child(events);
        }

        // ---- footer: what is under the pointer
        let focus = hover.or(selected).unwrap_or(cur);
        let foot = {
            let path = t.path(focus);
            let w = t.weight(focus, metric);
            let share = |of: u64| if of == 0 { 0. } else { w as f32 / of as f32 * 100. };
            let mut s = format!("{}  ·  {}", path.to_string_lossy(), show(metric, w));
            if focus != cur {
                s.push_str(&format!("  ·  {} {}", crate::i18n::pct(share(t.weight(cur, metric))), t!("of this folder")));
            }
            if focus != 0 {
                s.push_str(&format!("  ·  {} {}", crate::i18n::pct(share(t.weight(0, metric))), t!("of the scan")));
            }
            if t.is_dir(focus) {
                s.push_str(&format!("  ·  {} {}", count(t.files(focus) as u64), t!("files")));
            }
            if t.skipped() > 0 && focus == 0 {
                s.push_str(&format!("  ·  {} {}", t.skipped(), t!("folders could not be read")));
            }
            s
        };

        v_flex()
            .size_full()
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(v_flex().w(px(540.)).flex_shrink_0().h_full().border_r_1().border_color(p.divider).child(list_head).child(list))
                    .child(div().relative().flex_1().min_w_0().h_full().p_2().child(div().relative().size_full().child(probe).child(map))),
            )
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .border_t_1()
                    .border_color(p.divider)
                    .text_xs()
                    .text_color(p.text_secondary)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(foot),
            )
            .into_any_element()
    }
}

impl DiskUsageView {
    fn render_scanning(&self, p: &Palette) -> impl IntoElement {
        let (entries, bytes) = self.progress;
        let secs = self.started.elapsed().as_secs();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Icon::new(L::ChartPie).size(px(40.)).text_color(p.accent))
            .child(div().text_color(p.text).child(t!("Working out disk usage…")))
            .child(div().text_sm().text_color(p.text_secondary).child(format!("{} {} · {} · {} s", count(entries), t!("items"), format::size(bytes), secs)))
            .child(div().text_xs().text_color(p.text_tertiary).child(t!("Closing the tab stops the scan")))
    }
}
