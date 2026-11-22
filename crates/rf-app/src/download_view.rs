//! The Downloads page: one list of every download of the download manager (a web address, a video page, Usenet, the browser's):
//! the running and waiting ones from the Status Center, the finished and failed ones from the history of transfers. Nothing is
//! kept for the page itself. A details list as the file list is: the same row height, columns that sort by a click on their
//! header, a search box, filter chips, the selection with Ctrl and Shift, the keys of the file list (arrows, Ctrl+A, Enter,
//! Delete, Ctrl+V), a right-click menu. The actions are the Status Center card's own (pause, cancel, retry) plus open, show in
//! folder, copy the address and "Remove from the list" (the list entry only: the file is only trashed by the second choice,
//! "Remove and delete the file"). A link or an NZB file dropped or pasted on the page is added.

use crate::t;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;

use crate::actions::{ClearSelection, DeleteItems, ExtendNext, ExtendPrev, GoSettings, InvertSelection, OpenSelected, PasteItems, SelectAll, SelectFirst, SelectLast, SelectNext, SelectPrev};
use crate::fluent_menu::{self, Item, Menu, MenuIcon};
use crate::folder_view::FolderView;
use crate::recycle_view::button;
use crate::status_center::{LiveDl, OpPhase, OpsGlobal, QueueAct, ResultAction};
use crate::theme::{self, Palette};
use crate::xfer_history::{Outcome, Record, Via};

// ---------------------------------------------------------------------------------------------------------------------------------
// the rows

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A job the Status Center holds (running, or ended and still on its list).
    Op(u64),
    /// A record of the history of transfers: when it ended and what it was.
    Rec(i64, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum St {
    /// Waiting in line (or for the scheduled time).
    Queued,
    Running,
    Paused,
    /// Waiting for leaf's VPN.
    Vpn,
    Done,
    Cancelled,
    /// The reason, in leaf's words.
    Failed(String),
}

impl St {
    fn active(&self) -> bool {
        !matches!(self, St::Done | St::Cancelled | St::Failed(_))
    }

    /// Running on top, then what waits, then what ended.
    fn rank(&self) -> u8 {
        match self {
            St::Running => 0,
            St::Queued | St::Paused | St::Vpn => 1,
            _ => 2,
        }
    }

    fn word(&self) -> String {
        match self {
            St::Queued => t!("Waiting in line").to_string(),
            St::Running => t!("Downloading").to_string(),
            St::Paused => t!("Paused").to_string(),
            St::Vpn => t!("Waiting for the VPN").to_string(),
            St::Done => t!("Done").to_string(),
            St::Cancelled => t!("Cancelled").to_string(),
            St::Failed(why) if why.is_empty() => t!("Failed").to_string(),
            St::Failed(why) => format!("{} {why}", t!("Failed:")),
        }
    }
}

#[derive(Clone)]
pub struct Row {
    pub key: Key,
    pub name: String,
    pub via: Via,
    /// The server's name (or "Usenet").
    pub site: String,
    pub st: St,
    pub frac: Option<f32>,
    pub done: u64,
    pub total: u64,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    /// The folder it was asked to go into.
    pub folder: PathBuf,
    /// What it made (a finished one): the file, or for Usenet the folder.
    pub file: Option<PathBuf>,
    /// The address without a query or sign-in; for Usenet the NZB file.
    pub from: String,
    /// When it began (running ones) or ended (the history's), ms since 1970.
    pub when_ms: i64,
    pub op: Option<u64>,
    /// The history record that stands for this ended job too (the page shows the job once).
    pub rec_at: Option<i64>,
    /// "Retry" of the job's own card.
    pub retry: Option<ResultAction>,
    pub can_queue: bool,
}

fn site_of(via: Via, from: &str) -> String {
    match via {
        Via::Usenet => t!("Usenet").to_string(),
        _ => crate::downloads::host_of(from),
    }
}

fn is_waiting_text(c: &str) -> bool {
    c.starts_with(t!("Waiting for the downloads before it")) || c.starts_with(t!("Waiting for the Usenet downloads before it")) || c == t!("Waiting for the scheduled time")
}

fn is_vpn_text(c: &str) -> bool {
    c == t!("Waiting for the VPN") || c == crate::vpn::held_text()
}

/// The state of a job from what it says it is doing.
fn state_of(phase: &OpPhase, paused: bool, current: Option<&str>) -> St {
    match phase {
        OpPhase::Done => St::Done,
        OpPhase::Cancelled => St::Cancelled,
        OpPhase::Failed(e) => St::Failed(e.lines().next().unwrap_or_default().trim().to_string()),
        OpPhase::Running => match current {
            Some(c) if is_vpn_text(c) => St::Vpn,
            _ if paused => St::Paused,
            Some(c) if is_waiting_text(c) => St::Queued,
            _ => St::Running,
        },
    }
}

fn row_of_live(l: LiveDl, rec_at: Option<i64>) -> Row {
    let st = state_of(&l.phase, l.paused, l.current.as_deref());
    let (done, total) = l.bytes.unwrap_or_default();
    let frac = match st {
        St::Done => Some(1.0),
        _ => l.frac.or_else(|| (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0))),
    };
    Row {
        key: Key::Op(l.id),
        site: site_of(l.via, &l.from),
        name: l.name,
        via: l.via,
        speed: l.speed.filter(|s| *s >= 1.0 && st.rank() == 0),
        eta: l.eta_secs.filter(|_| st.rank() == 0),
        st,
        frac,
        done,
        total,
        folder: PathBuf::from(&l.to),
        file: l.saved.into_iter().next(),
        from: l.from,
        when_ms: l.started_ms,
        op: Some(l.id),
        rec_at,
        retry: l.retry,
        can_queue: l.can_queue,
    }
}

fn row_of_record(r: &Record, via: Via) -> Row {
    let st = match r.result {
        Outcome::Done => St::Done,
        Outcome::Cancelled => St::Cancelled,
        Outcome::Failed => St::Failed(r.reason.as_deref().and_then(|s| s.lines().next()).unwrap_or_default().trim().to_string()),
    };
    Row {
        key: Key::Rec(r.at_ms, r.what.clone()),
        site: site_of(via, &r.from),
        name: r.what.clone(),
        via,
        frac: matches!(st, St::Done).then_some(1.0),
        st,
        done: r.bytes,
        total: r.bytes,
        speed: None,
        eta: None,
        folder: PathBuf::from(&r.to),
        file: r.file.as_ref().map(PathBuf::from),
        from: r.from.clone(),
        when_ms: r.at_ms,
        op: None,
        rec_at: Some(r.at_ms),
        retry: None,
        can_queue: false,
    }
}

/// Every download of the manager: the Status Center's jobs, and the history's records that no job of the Status Center stands for
/// (a job that ended is in both: it is shown once, as the job, which still has its "Retry").
pub fn build_rows(live: Vec<LiveDl>, hist: &[Record]) -> Vec<Row> {
    let mut used = vec![false; hist.len()];
    let mut rows = Vec::with_capacity(live.len() + hist.len());
    for l in live {
        let mut rec_at = None;
        if l.phase != OpPhase::Running {
            // the newest record of this job (the history is newest first)
            if let Some(i) = (0..hist.len()).find(|&i| !used[i] && hist[i].what == l.name && hist[i].from == l.from && hist[i].download_via() == Some(l.via)) {
                used[i] = true;
                rec_at = Some(hist[i].at_ms);
            }
        }
        rows.push(row_of_live(l, rec_at));
    }
    for (i, r) in hist.iter().enumerate() {
        if let (false, Some(via)) = (used[i], r.download_via()) {
            rows.push(row_of_record(r, via));
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------------------------------------------
// filter, search, sort

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    All,
    Active,
    Finished,
    Failed,
}

impl Chip {
    fn admits(self, st: &St) -> bool {
        match self {
            Chip::All => true,
            Chip::Active => st.active(),
            Chip::Finished => matches!(st, St::Done | St::Cancelled),
            Chip::Failed => matches!(st, St::Failed(_)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Name,
    Site,
    Kind,
    State,
    Progress,
    Size,
    Speed,
    Left,
    Saved,
    When,
}

fn via_rank(v: Via) -> u8 {
    match v {
        Via::Web => 0,
        Via::Video => 1,
        Via::Usenet => 2,
        Via::Browser => 3,
    }
}

fn kind_word(v: Via) -> &'static str {
    match v {
        Via::Web => t!("Web"),
        Via::Video => t!("Video"),
        Via::Usenet => t!("Usenet"),
        Via::Browser => t!("From browser"),
    }
}

fn saved_text(r: &Row) -> String {
    r.file.as_deref().unwrap_or(&r.folder).to_string_lossy().into_owned()
}

fn size_of(r: &Row) -> u64 {
    r.total.max(r.done)
}

/// The rows to show: the ones the chip admits that have the words of `query` (name, site, address, folder), by `col` (`desc` = the
/// other way round). The first column's sort is the page's own order: running on top, then waiting (in the order they were
/// started), then the ended (the newest first).
pub fn arrange(rows: &[Row], chip: Chip, query: &str, col: Col, desc: bool) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(rf_core::entry::fold_case).collect();
    let mut v: Vec<usize> = (0..rows.len())
        .filter(|&i| {
            let r = &rows[i];
            chip.admits(&r.st)
                && (words.is_empty() || {
                    let hay = rf_core::entry::fold_case(&format!("{} {} {} {}", r.name, r.site, r.from, saved_text(r)));
                    words.iter().all(|w| hay.contains(w.as_str()))
                })
        })
        .collect();
    let ord = |a: &Row, b: &Row| {
        use std::cmp::Ordering::Equal;
        let by_name = || rf_core::sort::natural_cmp(&a.name, &b.name);
        match col {
            Col::Name => by_name(),
            Col::Site => rf_core::sort::natural_cmp(&a.site, &b.site).then_with(by_name),
            Col::Kind => via_rank(a.via).cmp(&via_rank(b.via)).then_with(by_name),
            Col::State => a.st.rank().cmp(&b.st.rank()).then_with(|| if a.st.rank() < 2 { a.when_ms.cmp(&b.when_ms) } else { b.when_ms.cmp(&a.when_ms) }).then_with(by_name),
            Col::Progress => a.frac.unwrap_or(-1.0).partial_cmp(&b.frac.unwrap_or(-1.0)).unwrap_or(Equal).then_with(by_name),
            Col::Size => size_of(a).cmp(&size_of(b)).then_with(by_name),
            Col::Speed => a.speed.unwrap_or(0.0).partial_cmp(&b.speed.unwrap_or(0.0)).unwrap_or(Equal).then_with(by_name),
            Col::Left => a.eta.unwrap_or(f64::MAX).partial_cmp(&b.eta.unwrap_or(f64::MAX)).unwrap_or(Equal).then_with(by_name),
            Col::Saved => rf_core::sort::natural_cmp(&saved_text(a), &saved_text(b)).then_with(by_name),
            Col::When => a.when_ms.cmp(&b.when_ms).then_with(by_name),
        }
    };
    v.sort_by(|&a, &b| {
        let o = ord(&rows[a], &rows[b]);
        if desc { o.reverse() } else { o }
    });
    v
}

/// The files "Remove and delete the file" sends to the Recycle Bin: only when that second choice was made, and only the file (or
/// Usenet folder) of a download that finished. A failed or cancelled one leaves its part files alone.
pub fn files_to_trash(rows: &[&Row], delete_files: bool) -> Vec<PathBuf> {
    if !delete_files {
        return Vec::new();
    }
    rows.iter().filter(|r| r.st == St::Done).filter_map(|r| r.file.clone()).collect()
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the view

pub enum DlEvent {
    /// Show this file (or folder) in its folder, selected.
    ShowInFolder(PathBuf),
    OpenFolder(PathBuf),
}

impl EventEmitter<DlEvent> for DownloadView {}

/// A download the Status Center card was clicked on: the page selects its row when it shows next.
static PENDING: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// "Show it on the Downloads page": the page opens with the row of job `op` selected.
pub fn reveal(op: u64, window: &mut Window, cx: &mut App) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(op);
    window.dispatch_action(Box::new(crate::actions::ShowDownloadList), cx);
    cx.refresh_windows();
}

pub struct DownloadView {
    tab: WeakEntity<FolderView>,
    rows: Vec<Row>,
    /// The rows shown: indices into `rows`.
    order: Vec<usize>,
    hist: Vec<Record>,
    hist_at: Option<std::time::Instant>,
    selected: HashSet<Key>,
    anchor: Option<Key>,
    chip: Chip,
    sort: (Col, bool),
    search: Option<Entity<InputState>>,
    _search_sub: Option<Subscription>,
    focus: FocusHandle,
    took_focus: bool,
    /// What the last command did, under the title.
    status: Option<String>,
    width: Pixels,
    _ops_sub: Option<Subscription>,
}

/// The columns (besides the name) a page of this width has room for, in the order they are drawn.
fn columns_for(width: f32) -> Vec<Col> {
    // (what is kept first when the page gets narrow)
    const PRIORITY: [Col; 9] = [Col::State, Col::Progress, Col::Size, Col::Speed, Col::Left, Col::Site, Col::When, Col::Kind, Col::Saved];
    let mut room = width - 280.;
    let mut keep = Vec::new();
    for c in PRIORITY {
        let w = col_w(c) + 8.;
        if room >= w {
            room -= w;
            keep.push(c);
        }
    }
    [Col::Site, Col::Kind, Col::State, Col::Progress, Col::Size, Col::Speed, Col::Left, Col::Saved, Col::When].into_iter().filter(|c| keep.contains(c)).collect()
}

fn col_w(c: Col) -> f32 {
    match c {
        Col::Name => 0.,
        Col::Site => 130.,
        Col::Kind => 90.,
        Col::State => 170.,
        Col::Progress => 100.,
        Col::Size => 80.,
        Col::Speed => 85.,
        Col::Left => 80.,
        Col::Saved => 190.,
        Col::When => 125.,
    }
}

fn col_label(c: Col) -> &'static str {
    match c {
        Col::Name => t!("Name"),
        Col::Site => t!("From"),
        Col::Kind => t!("Kind"),
        Col::State => t!("State"),
        Col::Progress => t!("Progress"),
        Col::Size => t!("Size"),
        Col::Speed => t!("Speed"),
        Col::Left => t!("Time left"),
        Col::Saved => t!("Saved to"),
        Col::When => t!("When"),
    }
}

impl DownloadView {
    pub fn new(tab: WeakEntity<FolderView>, cx: &mut Context<Self>) -> Self {
        let ops = cx.global::<OpsGlobal>().0.clone();
        // (the Status Center tells the page about progress, so the rows are never older than a report)
        let _ops_sub = Some(cx.observe(&ops, |_, _, cx| cx.notify()));
        Self {
            tab,
            rows: Vec::new(),
            order: Vec::new(),
            hist: Vec::new(),
            hist_at: None,
            selected: HashSet::new(),
            anchor: None,
            chip: Chip::All,
            sort: (Col::State, false),
            search: None,
            _search_sub: None,
            focus: cx.focus_handle(),
            took_focus: false,
            status: None,
            width: px(1000.),
            _ops_sub,
        }
    }

    pub fn status_text(&self) -> String {
        let active = self.rows.iter().filter(|r| r.st.active()).count();
        let n = self.rows.len();
        if self.selected.is_empty() {
            format!("{n} {} · {active} {}", if n == 1 { t!("download") } else { t!("downloads") }, t!("active"))
        } else {
            format!("{} / {n}", self.selected.len())
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.as_ref().map(|s| s.read(cx).value().to_string()).unwrap_or_default()
    }

    /// Reads the jobs and the history again and arranges the rows (at every draw: it is a few hundred rows at most).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let stale = self.hist_at.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3));
        if stale {
            self.hist = crate::xfer_history::recent();
            self.hist_at = Some(std::time::Instant::now());
        }
        let live = cx.global::<OpsGlobal>().0.read(cx).downloads();
        self.rows = build_rows(live, &self.hist);
        let q = self.query(cx);
        self.order = arrange(&self.rows, self.chip, &q, self.sort.0, self.sort.1);
        if let Some(id) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // (the job's card was clicked: the page shows it selected, whatever the filter hides)
            if self.rows.iter().any(|r| r.key == Key::Op(id)) {
                self.chip = Chip::All;
                self.selected = HashSet::from([Key::Op(id)]);
                self.anchor = Some(Key::Op(id));
                self.order = arrange(&self.rows, self.chip, &q, self.sort.0, self.sort.1);
            }
        }
        let shown: HashSet<&Key> = self.order.iter().map(|&i| &self.rows[i].key).collect();
        self.selected.retain(|k| shown.contains(k));
    }

    fn rows_changed(&mut self, cx: &mut Context<Self>) {
        // (after a command of ours: the history is read at once)
        self.hist_at = None;
        cx.notify();
    }

    fn sort_by(&mut self, col: Col, cx: &mut Context<Self>) {
        self.sort = if self.sort.0 == col { (col, !self.sort.1) } else { (col, matches!(col, Col::When | Col::Size | Col::Speed | Col::Progress)) };
        cx.notify();
    }

    fn selected_rows(&self) -> Vec<Row> {
        self.order.iter().map(|&i| &self.rows[i]).filter(|r| self.selected.contains(&r.key)).cloned().collect()
    }

    fn click(&mut self, ix: usize, ev: &MouseDownEvent) {
        let Some(row) = self.order.get(ix).map(|&i| self.rows[i].key.clone()) else { return };
        let (ctrl, shift) = (ev.modifiers.control, ev.modifiers.shift);
        let pos = |k: &Key| self.order.iter().position(|&i| &self.rows[i].key == k);
        if shift && let Some(a) = self.anchor.as_ref().and_then(pos) {
            let (lo, hi) = (a.min(ix), a.max(ix));
            let run: HashSet<Key> = self.order[lo..=hi].iter().map(|&i| self.rows[i].key.clone()).collect();
            self.selected = if ctrl { self.selected.union(&run).cloned().collect() } else { run };
        } else if ctrl {
            if !self.selected.remove(&row) {
                self.selected.insert(row.clone());
            }
            self.anchor = Some(row);
        } else {
            self.selected = HashSet::from([row.clone()]);
            self.anchor = Some(row);
        }
    }

    /// The arrow keys: one row on, or the run extended.
    fn step(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        if self.order.is_empty() {
            return;
        }
        let at = self.anchor.as_ref().and_then(|k| self.order.iter().position(|&i| &self.rows[i].key == k));
        let to = match at {
            Some(a) => (a as isize + delta).clamp(0, self.order.len() as isize - 1) as usize,
            None if delta < 0 => self.order.len() - 1,
            None => 0,
        };
        let key = self.rows[self.order[to]].key.clone();
        if extend && let Some(a) = at {
            let (lo, hi) = (a.min(to), a.max(to));
            self.selected = self.order[lo..=hi].iter().map(|&i| self.rows[i].key.clone()).collect();
            // (the anchor stays; the run grows from it)
            cx.notify();
            return;
        }
        self.selected = HashSet::from([key.clone()]);
        self.anchor = Some(key);
        cx.notify();
    }

    fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        if let Some(&i) = if last { self.order.last() } else { self.order.first() } {
            let key = self.rows[i].key.clone();
            self.selected = HashSet::from([key.clone()]);
            self.anchor = Some(key);
            cx.notify();
        }
    }

    // --- the actions on the rows

    fn ops(&self, cx: &App) -> Entity<crate::status_center::Ops> {
        cx.global::<OpsGlobal>().0.clone()
    }

    /// Pause (when something selected runs or waits) or resume (when all that is selected is paused).
    fn pause_resume(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let pause = rows.iter().any(|r| matches!(r.st, St::Running | St::Queued | St::Vpn));
        let ops = self.ops(cx);
        for r in &rows {
            let Some(id) = r.op else { continue };
            let want = if pause { matches!(r.st, St::Running | St::Queued | St::Vpn) } else { r.st == St::Paused };
            if want {
                ops.update(cx, |o, cx| o.toggle_pause(id, cx));
            }
        }
        self.rows_changed(cx);
    }

    fn pause_all(&mut self, pause: bool, cx: &mut Context<Self>) {
        let ops = self.ops(cx);
        for r in &self.rows {
            let Some(id) = r.op else { continue };
            let want = if pause { matches!(r.st, St::Running | St::Queued | St::Vpn) } else { r.st == St::Paused };
            if want {
                ops.update(cx, |o, cx| o.toggle_pause(id, cx));
            }
        }
        self.rows_changed(cx);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let ops = self.ops(cx);
        for r in self.selected_rows().iter().filter(|r| r.st.active()) {
            if let Some(id) = r.op {
                ops.update(cx, |o, cx| o.cancel_op(id, cx));
            }
        }
        self.rows_changed(cx);
    }

    /// Whether "Retry" can start this row again: its own card's button, or from what is remembered of it.
    fn can_retry(r: &Row) -> bool {
        if !matches!(r.st, St::Failed(_) | St::Cancelled) {
            return false;
        }
        r.retry.is_some() || (r.op.is_none() && matches!(r.via, Via::Web | Via::Usenet) && !r.from.is_empty())
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<Row> = self.selected_rows().into_iter().filter(Self::can_retry).collect();
        let mut started = 0;
        let mut gone = Vec::new();
        for r in &rows {
            if let Some((_, run)) = &r.retry {
                // (the card's own button: it also takes the failed card off the Status Center)
                run(cx);
                started += 1;
                gone.push(r.clone());
            } else if r.via == Via::Web {
                let url = crate::downloads::past_url(&r.from, &r.folder).unwrap_or_else(|| r.from.clone());
                crate::videodl::route(cx, crate::downloads::Request::new(url, r.folder.clone()), None, true);
                started += 1;
                gone.push(r.clone());
            } else if r.via == Via::Usenet && crate::usenet::is_nzb(Path::new(&r.from)) {
                crate::usenet::start(PathBuf::from(&r.from), r.folder.clone(), false, cx);
                started += 1;
                gone.push(r.clone());
            }
        }
        // (the ended row it replaces goes: the new job has its own row)
        forget_entries(&gone, cx);
        self.status = Some(if started == 1 { t!("Started again").to_string() } else { format!("{started} {}", t!("started again")) });
        self.rows_changed(cx);
    }

    fn queue(&mut self, act: QueueAct, cx: &mut Context<Self>) {
        for r in self.selected_rows().iter().filter(|r| r.can_queue && r.st.active()) {
            if let Some(id) = r.op {
                crate::status_center::queue_act(cx, id, act);
            }
        }
        self.rows_changed(cx);
    }

    /// The file a finished row made, when it is still there.
    fn made(r: &Row) -> Option<PathBuf> {
        r.file.clone().filter(|f| f.exists())
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.selected_rows().into_iter().next() else { return };
        match Self::made(&r) {
            Some(f) if f.is_dir() => cx.emit(DlEvent::OpenFolder(f)),
            Some(f) => {
                if let Err(e) = leaf_sys::shell::open(&f) {
                    log::warn!("downloads: {} could not be opened: {e}", f.display());
                    self.status = Some(t!("The file could not be opened").to_string());
                }
            }
            None if r.st == St::Done => self.status = Some(t!("The file is no longer where it was saved").to_string()),
            None => {}
        }
        cx.notify();
    }

    fn show_in_folder(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.selected_rows().into_iter().next() else { return };
        match Self::made(&r) {
            Some(f) => cx.emit(DlEvent::ShowInFolder(f)),
            None if !r.folder.as_os_str().is_empty() => cx.emit(DlEvent::OpenFolder(r.folder.clone())),
            None => {}
        }
    }

    fn copy_address(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<String> = self
            .selected_rows()
            .iter()
            .map(|r| if matches!(r.via, Via::Usenet) { r.from.clone() } else { crate::downloads::past_url(&r.from, &r.folder).unwrap_or_else(|| r.from.clone()) })
            .collect();
        if !urls.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(urls.join("\n")));
            self.status = Some(t!("Address copied").to_string());
            cx.notify();
        }
    }

    /// Takes the ended rows off the list. Only the list entries, unless `delete_files`: then the file each finished one made goes to the
    /// Recycle Bin (the normal delete path), after the dialog that asked.
    fn remove(&mut self, delete_files: bool, cx: &mut Context<Self>) {
        let rows: Vec<Row> = self.selected_rows().into_iter().filter(|r| !r.st.active()).collect();
        if rows.is_empty() {
            return;
        }
        let refs: Vec<&Row> = rows.iter().collect();
        let trash = files_to_trash(&refs, delete_files);
        forget_entries(&rows, cx);
        if !trash.is_empty() {
            crate::status_center::delete(cx, trash, false);
        }
        self.selected.clear();
        self.rows_changed(cx);
    }

    /// "Remove and delete the file…": asks first; the entry and the file both go.
    fn remove_and_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let refs: Vec<&Row> = rows.iter().filter(|r| !r.st.active()).collect();
        let files = files_to_trash(&refs, true);
        if refs.is_empty() {
            return;
        }
        let me = cx.entity().downgrade();
        let what = if files.len() == 1 { format!("\"{}\"", rf_core::history::display_name(&files[0])) } else { crate::i18n::items(files.len()) };
        let message = if files.is_empty() { t!("None of these has a file to delete. The entries are taken off the list.").to_string() } else { format!("{what} {}", t!("will be moved to the Trash, and the entries taken off the list.")) };
        crate::prompt::show(
            t!("Remove and delete the file"),
            Some(message),
            None,
            vec![
                crate::prompt::Button::new(t!("Remove and delete"), crate::prompt::Kind::Danger, move |_, _, cx| {
                    let _ = me.update(cx, |v, cx| v.remove(true, cx));
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<Row> = self.rows.iter().filter(|r| !r.st.active()).cloned().collect();
        let n = rows.len();
        forget_entries(&rows, cx);
        self.selected.clear();
        self.status = Some(format!("{n} {}", t!("finished entries taken off the list")));
        self.rows_changed(cx);
    }

    // --- adding

    fn folder(cx: &App) -> PathBuf {
        let dir = crate::downloads::default_folder(cx.global::<crate::settings::Settings>());
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::downloads::ask(self.tab.clone(), Self::folder(cx), window, cx);
    }

    fn add_several(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::downloads::ask_many(self.tab.clone(), Self::folder(cx), window, cx);
    }

    /// Links (or patterns) and NZB files in a piece of text, each started into the download folder. How many were added.
    fn add_text(&mut self, text: &str, cx: &mut Context<Self>) -> usize {
        let dest = Self::folder(cx);
        let mut n = 0;
        let (urls, _) = crate::downloads::expand_many(text);
        for u in urls {
            crate::videodl::route(cx, crate::downloads::Request::new(u, dest.clone()), None, true);
            n += 1;
        }
        for line in text.lines().map(|l| l.trim().trim_matches('"')).filter(|l| !l.is_empty()) {
            let p = PathBuf::from(line);
            if crate::usenet::is_nzb(&p) {
                crate::usenet::start(p, dest.clone(), true, cx);
                n += 1;
            }
        }
        n
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
        let n = self.add_text(&text, cx);
        self.status = Some(if n == 0 { t!("There is no link or NZB file on the clipboard").to_string() } else { format!("{n} {}", t!("added")) });
        self.rows_changed(cx);
    }

    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let dest = Self::folder(cx);
        let mut n = 0;
        if let Some(url) = crate::downloads::dropped_link(paths.paths()) {
            crate::videodl::route(cx, crate::downloads::Request::new(url, dest.clone()), None, false);
            n += 1;
        }
        for p in paths.paths().iter().filter(|p| crate::usenet::is_nzb(p)) {
            crate::usenet::start(p.clone(), dest.clone(), true, cx);
            n += 1;
        }
        self.status = Some(if n == 0 { t!("Only a link or an NZB file can be dropped here").to_string() } else { format!("{n} {}", t!("added")) });
        self.rows_changed(cx);
    }

    /// The limit each download has (the setting of Settings > Advanced > Downloads), one step on: the same list the setting offers.
    fn next_limit(cx: &mut App) {
        const STEPS: [u32; 7] = [0, 1, 2, 5, 10, 25, 50];
        let now = cx.global::<crate::settings::Settings>().download_speed_limit;
        let next = STEPS.iter().copied().find(|s| *s > now).unwrap_or(0);
        crate::settings::update(cx, |s| s.download_speed_limit = next);
    }

    fn open_settings(window: &mut Window, cx: &mut App) {
        crate::settings_view::open_page(crate::settings_view::Page::Advanced, cx);
        window.dispatch_action(Box::new(GoSettings), cx);
    }

    // --- the menu

    fn context_menu(&mut self, row: Option<Key>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(k) = row {
            if !self.selected.contains(&k) {
                self.selected = HashSet::from([k.clone()]);
                self.anchor = Some(k);
            }
            cx.notify();
        }
        let rows = self.selected_rows();
        let me = cx.entity().downgrade();
        let any = !rows.is_empty();
        let first = rows.first();
        let (running, paused) = (rows.iter().any(|r| matches!(r.st, St::Running | St::Queued | St::Vpn)), rows.iter().any(|r| r.st == St::Paused));
        let active = rows.iter().any(|r| r.st.active());
        let ended = rows.iter().any(|r| !r.st.active());
        let retry = rows.iter().any(Self::can_retry);
        let queue = rows.iter().any(|r| r.can_queue && r.st.active());
        let has_file = first.is_some_and(|r| r.st == St::Done && r.file.is_some());
        let place = first.is_some_and(|r| r.file.is_some() || !r.folder.as_os_str().is_empty());
        let item = |label: &'static str, icon: L, on: bool, f: fn(&mut DownloadView, &mut Window, &mut Context<DownloadView>)| {
            let me = me.clone();
            Item::action(label, MenuIcon::Lucide(icon), move |window, cx| {
                let _ = me.update(cx, |v, cx| f(v, window, cx));
            })
            .enabled(on)
        };
        let items = vec![
            item(if paused && !running { t!("Resume") } else { t!("Pause") }, if paused && !running { L::Play } else { L::Pause }, running || paused, |v, _, cx| v.pause_resume(cx)),
            item(t!("Cancel"), L::X, active, |v, _, cx| v.cancel(cx)),
            item(t!("Retry"), L::RotateCw, retry, |v, _, cx| v.retry(cx)),
            Item::Separator,
            item(t!("Open"), L::ExternalLink, has_file, |v, _, cx| v.open_file(cx)),
            item(t!("Show in folder"), L::FolderOpen, place, |v, _, cx| v.show_in_folder(cx)),
            item(t!("Copy the address"), L::Link, any, |v, _, cx| v.copy_address(cx)),
            Item::Separator,
            item(t!("Move up"), L::ArrowUp, queue, |v, _, cx| v.queue(QueueAct::Up, cx)),
            item(t!("Move down"), L::ArrowDown, queue, |v, _, cx| v.queue(QueueAct::Down, cx)),
            item(t!("High priority"), L::ChevronsUp, queue, |v, _, cx| v.queue(QueueAct::High, cx)),
            item(t!("Low priority"), L::ChevronsDown, queue, |v, _, cx| v.queue(QueueAct::Low, cx)),
            Item::Separator,
            item(t!("Remove from the list"), L::ListX, ended, |v, _, cx| v.remove(false, cx)),
            item(t!("Remove and delete the file…"), L::Trash, ended, |v, w, cx| v.remove_and_delete(w, cx)),
        ];
        fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }
}

/// Takes the ended rows' entries off: the Status Center's card and the history's record. Never a file.
fn forget_entries(rows: &[Row], cx: &mut App) {
    let at: HashSet<(i64, String)> = rows.iter().filter_map(|r| r.rec_at.map(|a| (a, r.name.clone()))).collect();
    for r in rows {
        if let Some(id) = r.op {
            crate::status_center::remove(cx, id);
        }
    }
    if !at.is_empty() {
        crate::xfer_history::forget(|rec| rec.download_via().is_some() && at.contains(&(rec.at_ms, rec.what.clone())));
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// drawing

fn chip(id: &'static str, label: String, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .text_sm()
        .cursor_pointer()
        .when(on, |d| d.bg(p.selection).text_color(p.text))
        .when(!on, |d| d.bg(p.hover).text_color(p.text_secondary).hover(|s| s.bg(p.pressed)))
        .child(label)
}

fn icon_for(r: &Row) -> (L, crate::icons::Category) {
    let ext = r.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let cat = if r.via == Via::Usenet { crate::icons::Category::Folder } else { crate::icons::category_for_ext(&ext) };
    (crate::icons::icon_for(cat), cat)
}

fn speed_word(r: &Row) -> String {
    r.speed.map(crate::transfer_view::speed_text).unwrap_or_default()
}

fn left_word(r: &Row) -> String {
    r.eta.map(|s| crate::status_center::fmt_duration(s as f32)).unwrap_or_default()
}

fn size_word(r: &Row) -> String {
    match (r.done, r.total) {
        (0, 0) => String::new(),
        (_, t) if t > 0 && r.st.active() => format::size(t),
        (d, t) => format::size(d.max(t)),
    }
}

impl Render for DownloadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let search = match &self.search {
            Some(s) => s.clone(),
            None => {
                let s = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search the downloads")));
                self._search_sub = Some(cx.subscribe(&s, |_, _, ev: &InputEvent, cx| {
                    if let InputEvent::Change = ev {
                        cx.notify();
                    }
                }));
                self.search = Some(s.clone());
                s
            }
        };
        self.refresh(cx);
        let width = f32::from(self.width);
        let compact = width < 900.;
        let cols = columns_for(width);
        let this = cx.entity();
        let width_probe = canvas(
            move |bounds, window, cx| {
                let changed = this.update(cx, |v, cx| {
                    let changed = v.width != bounds.size.width;
                    if changed {
                        v.width = bounds.size.width;
                        cx.notify();
                    }
                    changed
                });
                if changed {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let picked = self.selected_rows();
        let any = !picked.is_empty();
        let running = picked.iter().any(|r| matches!(r.st, St::Running | St::Queued | St::Vpn));
        let paused = picked.iter().any(|r| r.st == St::Paused);
        let can_cancel = picked.iter().any(|r| r.st.active());
        let can_retry = picked.iter().any(Self::can_retry);
        let ended = picked.iter().any(|r| !r.st.active());
        let any_ended = self.rows.iter().any(|r| !r.st.active());
        let any_live = self.rows.iter().any(|r| r.op.is_some() && r.st.active());
        let total_speed: f64 = self.rows.iter().filter_map(|r| r.speed).sum();
        let limit = cx.global::<crate::settings::Settings>().download_speed_limit;

        let (sort_col, sort_desc) = self.sort;
        let col_head = |id: &'static str, c: Col| {
            h_flex()
                .id(id)
                .h_full()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.text_color(p.text))
                .child(col_label(c))
                .when(sort_col == c, |d| d.child(Icon::new(if sort_desc { L::ChevronDown } else { L::ChevronUp }).size(px(12.))))
                .on_click(cx.listener(move |this, _, _, cx| this.sort_by(c, cx)))
        };
        let head_ids: [(&'static str, Col); 9] = [
            ("dl-h-site", Col::Site),
            ("dl-h-kind", Col::Kind),
            ("dl-h-state", Col::State),
            ("dl-h-prog", Col::Progress),
            ("dl-h-size", Col::Size),
            ("dl-h-speed", Col::Speed),
            ("dl-h-left", Col::Left),
            ("dl-h-saved", Col::Saved),
            ("dl-h-when", Col::When),
        ];
        let columns = h_flex()
            .h(px(30.))
            .mx_1p5()
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(p.text_secondary)
            .border_b_1()
            .border_color(p.divider)
            .child(div().w(px(16.)).flex_shrink_0())
            .child(col_head("dl-h-name", Col::Name).flex_1().min_w_0())
            .children(head_ids.into_iter().filter(|(_, c)| cols.contains(c)).map(|(id, c)| col_head(id, c).w(px(col_w(c))).flex_shrink_0()));

        let title_row = h_flex()
            .px_4()
            .pt_3()
            .pb_2()
            .gap_2()
            .flex_wrap()
            .items_center()
            .child(Icon::new(L::ListChecks).size(px(18.)).text_color(p.text_secondary))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(140.))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(t!("Downloads")))
                    .child(div().text_xs().text_color(p.text_secondary).child(if total_speed >= 1.0 {
                        format!("{} · {}", self.status_text(), crate::transfer_view::speed_text(total_speed))
                    } else {
                        self.status_text()
                    }))
                    .children(self.status.clone().map(|s| div().text_xs().text_color(p.text_secondary).child(s))),
            )
            .child(button("dl-add", L::Plus, t!("Add"), true, compact, &p).on_click(cx.listener(|this, _, window, cx| this.add(window, cx))))
            .child(button("dl-add-many", L::ListPlus, t!("Add several"), true, compact, &p).on_click(cx.listener(|this, _, window, cx| this.add_several(window, cx))))
            .child(button("dl-pause-all", L::Pause, t!("Pause all"), any_live, compact, &p).on_click(cx.listener(|this, _, _, cx| this.pause_all(true, cx))))
            .child(button("dl-resume-all", L::Play, t!("Resume all"), self.rows.iter().any(|r| r.st == St::Paused), compact, &p).on_click(cx.listener(|this, _, _, cx| this.pause_all(false, cx))))
            .child(button("dl-clear", L::ListX, t!("Clear finished"), any_ended, compact, &p).on_click(cx.listener(|this, _, _, cx| this.clear_finished(cx))))
            .child(
                h_flex()
                    .id("dl-limit")
                    .h(px(30.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .text_xs()
                    .text_color(p.text_secondary)
                    .bg(p.hover)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.pressed))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("The speed limit of each download (Settings > Advanced > Downloads). Click for the next step.")).build(w, cx))
                    .child(Icon::new(L::Gauge).size(px(14.)))
                    .child(if limit == 0 { t!("No limit").to_string() } else { format!("{limit} MB/s") })
                    .on_click(|_, _, cx| Self::next_limit(cx)),
            )
            .child(button("dl-settings", L::Settings, t!("Downloads settings"), true, true, &p).on_click(|_, window, cx| Self::open_settings(window, cx)))
            .child(crate::guide_page::help_button(&p, "dl-help", "downloadmgr"));

        let count = |c: Chip| self.rows.iter().filter(|r| c.admits(&r.st)).count();
        let chips = h_flex()
            .px_4()
            .pb_2()
            .gap_2()
            .flex_wrap()
            .items_center()
            .child(chip("dl-chip-all", format!("{} {}", t!("All"), count(Chip::All)), self.chip == Chip::All, &p).on_click(cx.listener(|this, _, _, cx| {
                this.chip = Chip::All;
                cx.notify();
            })))
            .child(chip("dl-chip-active", format!("{} {}", t!("Active"), count(Chip::Active)), self.chip == Chip::Active, &p).on_click(cx.listener(|this, _, _, cx| {
                this.chip = Chip::Active;
                cx.notify();
            })))
            .child(chip("dl-chip-done", format!("{} {}", t!("Finished"), count(Chip::Finished)), self.chip == Chip::Finished, &p).on_click(cx.listener(|this, _, _, cx| {
                this.chip = Chip::Finished;
                cx.notify();
            })))
            .child(chip("dl-chip-failed", format!("{} {}", t!("Failed"), count(Chip::Failed)), self.chip == Chip::Failed, &p).on_click(cx.listener(|this, _, _, cx| {
                this.chip = Chip::Failed;
                cx.notify();
            })))
            .child(
                h_flex()
                    .w(px(if compact { 150. } else { 220. }))
                    .h(px(30.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .bg(p.hover)
                    .child(Icon::new(L::Search).size(px(14.)).text_color(p.text_secondary))
                    .child(Input::new(&search).appearance(false).cleanable(true)),
            )
            .child(div().flex_1())
            .child(button("dl-toggle", if paused && !running { L::Play } else { L::Pause }, if paused && !running { t!("Resume") } else { t!("Pause") }, running || paused, true, &p).on_click(cx.listener(|this, _, _, cx| this.pause_resume(cx))))
            .child(button("dl-cancel", L::X, t!("Cancel"), can_cancel, true, &p).on_click(cx.listener(|this, _, _, cx| this.cancel(cx))))
            .child(button("dl-retry", L::RotateCw, t!("Retry"), can_retry, true, &p).on_click(cx.listener(|this, _, _, cx| this.retry(cx))))
            .child(button("dl-open", L::ExternalLink, t!("Open"), any && picked.first().is_some_and(|r| r.st == St::Done && r.file.is_some()), true, &p).on_click(cx.listener(|this, _, _, cx| this.open_file(cx))))
            .child(button("dl-folder", L::FolderOpen, t!("Show in folder"), any, true, &p).on_click(cx.listener(|this, _, _, cx| this.show_in_folder(cx))))
            .child(button("dl-copy", L::Link, t!("Copy the address"), any, true, &p).on_click(cx.listener(|this, _, _, cx| this.copy_address(cx))))
            .child(button("dl-remove", L::ListX, t!("Remove from the list"), ended, true, &p).on_click(cx.listener(|this, _, _, cx| this.remove(false, cx))));

        let body: AnyElement = if self.rows.is_empty() {
            let way = |icon: L, text: String| h_flex().gap_2().items_center().text_sm().text_color(p.text_secondary).child(Icon::new(icon).size(px(14.))).child(text);
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(L::ArrowDownToLine).size(px(40.)).text_color(p.text_tertiary))
                .child(div().text_color(p.text_secondary).child(t!("No downloads yet. Add one and it shows here.")))
                .child(way(L::Plus, t!("Press Add and enter a web address").to_string()))
                .child(way(L::ClipboardPaste, t!("Paste a link or an NZB file here (Ctrl+V)").to_string()))
                .child(way(L::MousePointerClick, t!("Drop a link or an NZB file on this page, or use leaf's browser extension").to_string()))
                .into_any_element()
        } else if self.order.is_empty() {
            div().p_6().text_sm().text_color(p.text_secondary).child(t!("No matching downloads")).into_any_element()
        } else {
            let n = self.order.len();
            let cols = cols.clone();
            uniform_list(
                "download-rows",
                n,
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    let p = Palette::for_window(window);
                    range
                        .filter_map(|row| this.order.get(row).copied().map(|i| (row, i)))
                        .map(|(row, i)| {
                            let r = &this.rows[i];
                            let sel = this.selected.contains(&r.key);
                            let key = r.key.clone();
                            let (icon, cat) = icon_for(r);
                            let state_color = match &r.st {
                                St::Failed(_) => p.danger,
                                St::Done => p.text_secondary,
                                _ => p.text,
                            };
                            let cell = |c: Col, content: AnyElement| {
                                div().w(px(col_w(c))).flex_shrink_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(p.text_secondary).child(content).into_any_element()
                            };
                            let cells: Vec<AnyElement> = cols
                                .iter()
                                .map(|&c| match c {
                                    Col::Site => cell(c, r.site.clone().into_any_element()),
                                    Col::Kind => cell(c, kind_word(r.via).into_any_element()),
                                    Col::State => div()
                                        .id(("dl-state", i))
                                        .w(px(col_w(c)))
                                        .flex_shrink_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(state_color)
                                        .child(r.st.word())
                                        .when(matches!(r.st, St::Failed(_)), {
                                            let word = r.st.word();
                                            move |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(word.clone()).build(w, cx))
                                        })
                                        .into_any_element(),
                                    Col::Progress => {
                                        let frac = r.frac.unwrap_or(0.0).clamp(0.0, 1.0);
                                        div()
                                            .w(px(col_w(c)))
                                            .flex_shrink_0()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(div().flex_1().h(px(5.)).rounded_full().bg(p.pressed).child(div().h_full().rounded_full().bg(if r.st == St::Paused { p.text_tertiary } else { p.accent }).w(relative(frac))))
                                            .child(div().w(px(34.)).text_xs().text_right().text_color(p.text_secondary).child(r.frac.map(|f| crate::i18n::pct(f * 100.0)).unwrap_or_default()))
                                            .into_any_element()
                                    }
                                    Col::Size => div().w(px(col_w(c))).flex_shrink_0().flex().justify_end().text_color(p.text_secondary).child(size_word(r)).into_any_element(),
                                    Col::Speed => div().w(px(col_w(c))).flex_shrink_0().flex().justify_end().text_color(p.text_secondary).child(speed_word(r)).into_any_element(),
                                    Col::Left => div().w(px(col_w(c))).flex_shrink_0().flex().justify_end().text_color(p.text_secondary).child(left_word(r)).into_any_element(),
                                    Col::Saved => cell(c, saved_text(r).into_any_element()),
                                    Col::When => cell(c, format::datetime(r.when_ms).into_any_element()),
                                    Col::Name => div().into_any_element(),
                                })
                                .collect();
                            let key_right = key.clone();
                            h_flex()
                                .id(("dl", row))
                                .w_full()
                                .h(theme::ROW_H)
                                .px_3()
                                .gap_2()
                                .items_center()
                                .rounded(theme::RADIUS_SM)
                                .text_sm()
                                .when(sel, |d| d.bg(p.selection))
                                .when(!sel, |d| d.hover(|s| s.bg(p.hover)))
                                .child(Icon::new(icon).size(px(16.)).flex_shrink_0().text_color(theme::kind_color(cat, p.dark)))
                                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(r.name.clone()))
                                .children(cells)
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        window.focus(&this.focus, cx);
                                        this.context_menu(Some(key_right.clone()), window, cx);
                                    }),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                        window.focus(&this.focus, cx);
                                        this.click(row, ev);
                                        if ev.click_count == 2 {
                                            this.open_file(cx);
                                        }
                                        cx.notify();
                                    }),
                                )
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .p_1p5()
            .into_any_element()
        };
        if !self.took_focus {
            self.took_focus = true;
            window.focus(&self.focus, cx);
        }
        let drop_bg = p.selection;
        v_flex()
            .id("download-view")
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.step(-1, false, cx)))
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.step(1, true, cx)))
            .on_action(cx.listener(|this, _: &ExtendPrev, _, cx| this.step(-1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_edge(false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select_edge(true, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.selected = this.order.iter().map(|&i| this.rows[i].key.clone()).collect();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &InvertSelection, _, cx| {
                let all: Vec<Key> = this.order.iter().map(|&i| this.rows[i].key.clone()).collect();
                this.selected = all.into_iter().filter(|k| !this.selected.contains(k)).collect();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.selected.clear();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_file(cx)))
            .on_action(cx.listener(|this, _: &DeleteItems, window, cx| {
                // (Delete takes the entries off the list; the file is only deleted by its own menu entry)
                if !this.search.as_ref().is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window)) {
                    this.remove(false, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PasteItems, window, cx| {
                if !this.search.as_ref().is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window)) {
                    this.paste(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.context_menu(None, window, cx);
                }),
            )
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx)))
            .child(width_probe)
            .child(title_row)
            .child(chips)
            .when(!self.rows.is_empty(), |d| d.child(columns))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, Row, St, files_to_trash};
    use crate::xfer_history::Via;
    use std::path::PathBuf;

    fn row(name: &str, st: St, file: Option<&str>, when: i64) -> Row {
        Row {
            key: Key::Rec(when, name.into()),
            name: name.into(),
            via: Via::Web,
            site: "example.com".into(),
            st,
            frac: None,
            done: 0,
            total: 0,
            speed: None,
            eta: None,
            folder: PathBuf::from("downloads"),
            file: file.map(PathBuf::from),
            from: format!("https://example.com/{name}"),
            when_ms: when,
            op: None,
            rec_at: Some(when),
            retry: None,
            can_queue: false,
        }
    }

    #[test]
    fn taking_an_entry_off_the_list_never_touches_the_file_unless_the_second_choice_was_made() {
        let dir = std::env::temp_dir().join(format!("leaf-dlview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.zip");
        let part = dir.join("b.zip.part");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(&part, b"y").unwrap();
        let done = row("a.zip", St::Done, file.to_str(), 2);
        let failed = row("b.zip", St::Failed("The file is not on the server".into()), part.to_str(), 1);
        let rows = [&done, &failed];
        // the plain removal: nothing is sent to the Recycle Bin
        assert!(files_to_trash(&rows, false).is_empty());
        assert!(file.exists() && part.exists());
        // the second choice: only the finished download's file, never a failed one's part file
        assert_eq!(files_to_trash(&rows, true), vec![file.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
