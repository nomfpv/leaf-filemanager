//! The optional Home widgets for people who work in folders and repositories: Git pulse, Code stats, Activity calendar and
//! Reclaimable space. Each works on one folder the user picked in Customize mode (`home::folder_of`), reads only, and computes
//! on a worker thread, and only while its widget is drawn on the Home page. The result is kept in memory ([`Cell`]) until the
//! refresh button throws it away, so drawing the page again costs nothing.
//!
//! The pure parts (language split, calendar bucketing, relative time) are plain functions at the top of each section.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::{gitignore, gitread, junk};
use leaf_sys::dupes;
use rf_core::{Location, format};

use super::{WidgetCx, folder_of, home_button};
use crate::folder_view::FolderView;
use crate::theme::{self, Palette};

// ---------------------------------------------------------------------------------------------------------------------
// the cache cell
// ---------------------------------------------------------------------------------------------------------------------

/// A result computed on a worker thread for one folder and kept until it is thrown away.
pub struct Cell<T> {
    folder: Option<PathBuf>,
    value: Option<T>,
    pending: Option<Arc<Mutex<Option<T>>>>,
    stop: Arc<AtomicBool>,
}

impl<T> Default for Cell<T> {
    fn default() -> Self {
        Cell { folder: None, value: None, pending: None, stop: Arc::new(AtomicBool::new(false)) }
    }
}

impl<T> Drop for Cell<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl<T: Clone + Send + 'static> Cell<T> {
    /// Another folder than the one the result is for: the result goes (a running computation is stopped).
    fn sync(&mut self, folder: &Path) {
        if self.folder.as_deref() != Some(folder) {
            self.clear();
            self.folder = Some(folder.to_path_buf());
        }
    }

    /// Throws the result away and stops a running computation.
    pub fn clear(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.stop = Arc::new(AtomicBool::new(false));
        self.value = None;
        self.pending = None;
    }

    /// Takes in what the worker has finished.
    fn poll(&mut self) {
        if let Some(p) = &self.pending {
            let got = p.lock().ok().and_then(|mut g| g.take());
            if let Some(v) = got {
                self.value = Some(v);
                self.pending = None;
            }
        }
    }

    fn idle(&self) -> bool {
        self.value.is_none() && self.pending.is_none()
    }

    fn running(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts `work` on its own thread; the page is redrawn while it runs and when it is done.
    fn begin(&mut self, cx: &mut Context<FolderView>, work: impl FnOnce(&AtomicBool) -> T + Send + 'static) {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));
        self.pending = Some(slot.clone());
        let stop = self.stop.clone();
        let (stop_w, done_w) = (stop.clone(), done.clone());
        let started = std::thread::Builder::new().name("home-widget".into()).spawn(move || {
            let r = work(&stop_w);
            if !stop_w.load(Ordering::Relaxed) {
                if let Ok(mut g) = slot.lock() {
                    *g = Some(r);
                }
            }
            done_w.store(true, Ordering::Relaxed);
        });
        if started.is_err() {
            self.pending = None;
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let finished = done.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed);
                if this.update(cx, |_, cx| cx.notify()).is_err() || finished {
                    break;
                }
            }
        })
        .detach();
    }
}

/// What the four widgets keep in memory.
#[derive(Default)]
pub struct Data {
    pulse: Cell<Pulse>,
    stats: Cell<Stats>,
    cal: Cell<Cal>,
    reclaim: Cell<Reclaim>,
    rings: Cell<Rings>,
    reclaim_progress: Option<Arc<dupes::Progress>>,
}

// ---------------------------------------------------------------------------------------------------------------------
// shared bits of drawing
// ---------------------------------------------------------------------------------------------------------------------

fn note(p: &Palette, s: impl Into<SharedString>) -> AnyElement {
    div().text_sm().text_color(p.text_tertiary).child(s.into()).into_any_element()
}

fn hint_no_folder(p: &Palette) -> AnyElement {
    note(p, "No folder to look at. Choose one with the folder button in Customize mode, or pin a folder.")
}

fn folder_line(p: &Palette, folder: &Path, extra: &str) -> AnyElement {
    let name = rf_core::history::display_name(folder);
    div()
        .text_xs()
        .text_color(p.text_tertiary)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(if extra.is_empty() { name } else { format!("{name}  \u{b7}  {extra}") })
        .into_any_element()
}

fn refresh_btn(id: &'static str, p: &Palette, cx: &mut Context<FolderView>, f: fn(&mut Data)) -> AnyElement {
    div()
        .id(id)
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Refresh").build(w, cx))
        .child(Icon::new(L::RefreshCw).size(px(14.)).text_color(p.text_secondary))
        .on_click(cx.listener(move |v, _, _, cx| {
            f(&mut v.home_data);
            cx.notify();
        }))
        .into_any_element()
}

/// Skips the folders a walk should not look into: `.git` and the folders a build or an install makes again.
fn skip_dir(name: &str) -> bool {
    name == ".git" || junk::kind_by_name(name).is_some()
}

const MAX_FILES: usize = 60_000;
const MAX_TIME: Duration = Duration::from_secs(5);

/// A capped walk below `root` (not into links, `.git` or build folders; what `.gitignore` ignores is left out), handing every
/// file to `visit`. Returns true when a cap stopped it (the result is partial).
fn walk(root: &Path, stop: &AtomicBool, mut visit: impl FnMut(&Path, &std::fs::Metadata) -> bool) -> bool {
    let matcher = gitignore::matcher_for(root);
    let ignored = |p: &Path, dir: bool| matcher.as_ref().is_some_and(|m| m.is_ignored(p, dir));
    let start = Instant::now();
    let mut n = 0usize;
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if stop.load(Ordering::Relaxed) {
                return true;
            }
            if n >= MAX_FILES || start.elapsed() > MAX_TIME {
                return true;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                if skip_dir(&e.file_name().to_string_lossy()) || ignored(&path, true) {
                    continue;
                }
                todo.push(path);
            } else if ft.is_file() {
                if ignored(&path, false) {
                    continue;
                }
                let Ok(m) = e.metadata() else { continue };
                n += 1;
                if !visit(&path, &m) {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------------------------------
// S10 Git pulse
// ---------------------------------------------------------------------------------------------------------------------

/// "5 min ago", "3 h ago", "2 d ago"... for a commit made at `then` (unix seconds) when it is `now`.
pub fn ago(then: i64, now: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=1_209_599 => format!("{} d ago", d / 86_400),
        1_209_600..=5_183_999 => format!("{} wk ago", d / 604_800),
        5_184_000..=62_207_999 => format!("{} mo ago", d / 2_592_000),
        _ => format!("{} y ago", d / 31_104_000),
    }
}

#[derive(Clone)]
struct Pulse {
    /// The repository the folder is in; none: it is in no repository (or git is missing).
    repo: Option<PathBuf>,
    branch: Option<String>,
    /// (ahead, behind) the upstream branch, when there is one and git answered in time.
    upstream: Option<(u32, u32)>,
    changed: usize,
    new: usize,
    deleted: usize,
    commits: Vec<gitread::Commit>,
}

fn compute_pulse(folder: &Path) -> Pulse {
    let repo = gitread::repo_root(folder);
    let Some(root) = repo.clone() else {
        return Pulse { repo: None, branch: None, upstream: None, changed: 0, new: 0, deleted: 0, commits: Vec::new() };
    };
    let (mut changed, mut new, mut deleted) = (0, 0, 0);
    for c in gitread::status(&root).unwrap_or_default() {
        match c.kind {
            gitread::Kind::New => new += 1,
            gitread::Kind::Deleted => deleted += 1,
            _ => changed += 1,
        }
    }
    Pulse {
        branch: gitread::current_branch(&root),
        upstream: gitread::ahead_behind(&root),
        changed,
        new,
        deleted,
        commits: gitread::log(&root, 5).unwrap_or_default(),
        repo,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn pulse_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    refresh_btn("pulse-refresh", p, cx, |d| d.pulse.clear())
}

pub fn pulse(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let Some(folder) = folder_of(v, "pulse", cx) else { return Some(hint_no_folder(p)) };
    let c = &mut v.home_data.pulse;
    c.sync(&folder);
    c.poll();
    if c.idle() {
        let f = folder.clone();
        c.begin(cx, move |_| compute_pulse(&f));
    }
    let Some(got) = c.value.clone() else { return Some(note(p, "Reading the repository...")) };
    let Some(root) = got.repo.clone() else {
        return Some(v_flex().gap_1().child(folder_line(p, &folder, "")).child(note(p, "This folder is not in a git repository.")).into_any_element());
    };
    let mut branch = h_flex().gap_2().items_center().child(Icon::new(L::GitBranch).size(px(16.)).text_color(p.accent)).child(
        div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).overflow_hidden().text_ellipsis().whitespace_nowrap().child(
            got.branch.clone().unwrap_or_else(|| "detached HEAD".to_string()),
        ),
    );
    if let Some((ahead, behind)) = got.upstream {
        let s = match (ahead, behind) {
            (0, 0) => "up to date".to_string(),
            (a, 0) => format!("{a} ahead"),
            (0, b) => format!("{b} behind"),
            (a, b) => format!("{a} ahead, {b} behind"),
        };
        branch = branch.child(div().text_xs().text_color(p.text_secondary).child(s));
    }
    let stat = |n: usize, label: &'static str, color: Hsla| {
        v_flex()
            .gap_0p5()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(if n > 0 { color } else { p.text_tertiary }).child(n.to_string()))
            .child(div().text_xs().text_color(p.text_secondary).child(label))
    };
    let counts = h_flex().gap_5().child(stat(got.changed, "changed", p.warning)).child(stat(got.new, "new", p.success)).child(stat(got.deleted, "deleted", p.danger));
    let now = now_secs();
    let mut log = v_flex().gap_1();
    for c in &got.commits {
        log = log.child(
            h_flex()
                .gap_3()
                .items_center()
                .text_sm()
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(c.subject.clone()))
                .child(div().flex_shrink_0().text_xs().text_color(p.text_tertiary).child(ago(c.unix, now))),
        );
    }
    let hover = p.hover;
    Some(
        v_flex()
            .id("pulse-open")
            .gap_3()
            .cursor_pointer()
            .rounded(theme::RADIUS_SM)
            .hover(move |s| s.bg(hover))
            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Open the change feed").build(w, cx))
            .child(branch)
            .child(counts)
            .child(log)
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(Location::Changes(root.clone()), cx)))
            .into_any_element(),
    )
}

// ---------------------------------------------------------------------------------------------------------------------
// S9 Code stats
// ---------------------------------------------------------------------------------------------------------------------

/// The language a file extension stands for, for text files worth counting; none for everything else (pictures, archives...).
pub fn language(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "py" | "pyw" => "Python",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "go" => "Go",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "lua" => "Lua",
        "dart" => "Dart",
        "zig" => "Zig",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "sql" => "SQL",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" | "bat" | "cmd" => "Scripts",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        "wgsl" | "glsl" | "hlsl" | "frag" | "vert" => "Shaders",
        "json" => "JSON",
        "toml" | "yaml" | "yml" | "ini" | "cfg" => "Config",
        "xml" | "svg" => "XML",
        "md" | "markdown" | "txt" | "rst" => "Docs",
        _ => return None,
    })
}

/// A language that counts as source code (for the list of the largest source files).
fn is_source(lang: &str) -> bool {
    !matches!(lang, "Docs" | "Config" | "JSON" | "XML")
}

/// The languages by bytes, biggest first; past `top` the rest is one "Other" row.
pub fn rank(by_lang: &HashMap<&'static str, u64>, top: usize) -> Vec<(String, u64)> {
    let mut all: Vec<(&str, u64)> = by_lang.iter().map(|(k, v)| (*k, *v)).filter(|(_, b)| *b > 0).collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut out: Vec<(String, u64)> = all.iter().take(top).map(|(k, v)| (k.to_string(), *v)).collect();
    let rest: u64 = all.iter().skip(top).map(|(_, v)| *v).sum();
    if rest > 0 {
        out.push(("Other".to_string(), rest));
    }
    out
}

#[derive(Clone)]
struct Stats {
    langs: Vec<(String, u64)>,
    lines: u64,
    files: usize,
    /// The five largest source files: (path, bytes).
    largest: Vec<(PathBuf, u64)>,
    partial: bool,
}

/// Files bigger than this are counted by their size only (their lines are not read).
const MAX_READ: u64 = 2 * 1024 * 1024;
/// The most bytes read for lines in all.
const MAX_READ_TOTAL: u64 = 256 * 1024 * 1024;

fn count_lines(bytes: &[u8]) -> Option<u64> {
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    Some(if bytes.last().is_some_and(|b| *b != b'\n') { n + 1 } else { n })
}

fn compute_stats(folder: &Path, stop: &AtomicBool) -> Stats {
    let mut by_lang: HashMap<&'static str, u64> = HashMap::new();
    let (mut lines, mut files, mut read_total) = (0u64, 0usize, 0u64);
    let mut largest: Vec<(PathBuf, u64)> = Vec::new();
    let partial = walk(folder, stop, |path, m| {
        let Some(lang) = path.extension().and_then(|e| e.to_str()).and_then(language) else { return true };
        let size = m.len();
        *by_lang.entry(lang).or_default() += size;
        files += 1;
        if size <= MAX_READ && read_total < MAX_READ_TOTAL {
            if let Ok(b) = std::fs::read(path) {
                read_total += b.len() as u64;
                lines += count_lines(&b).unwrap_or(0);
            }
        }
        if is_source(lang) {
            largest.push((path.to_path_buf(), size));
            if largest.len() > 32 {
                largest.sort_by(|a, b| b.1.cmp(&a.1));
                largest.truncate(5);
            }
        }
        true
    });
    largest.sort_by(|a, b| b.1.cmp(&a.1));
    largest.truncate(5);
    Stats { langs: rank(&by_lang, 6), lines, files, largest, partial }
}

pub fn stats_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    refresh_btn("stats-refresh", p, cx, |d| d.stats.clear())
}

/// A colour for the n-th language of the bar.
fn lang_color(n: usize, dark: bool) -> Hsla {
    hsla((n as f32 * 0.137 + 0.58).fract(), 0.55, if dark { 0.62 } else { 0.46 }, 1.0)
}

pub fn stats(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let Some(folder) = folder_of(v, "codestats", cx) else { return Some(hint_no_folder(p)) };
    let c = &mut v.home_data.stats;
    c.sync(&folder);
    c.poll();
    if c.idle() {
        let f = folder.clone();
        c.begin(cx, move |stop| compute_stats(&f, stop));
    }
    let Some(got) = c.value.clone() else { return Some(note(p, "Counting...")) };
    let total: u64 = got.langs.iter().map(|(_, b)| *b).sum();
    if total == 0 {
        return Some(v_flex().gap_1().child(folder_line(p, &folder, "")).child(note(p, "No source files found.")).into_any_element());
    }
    let mut bar = h_flex().w_full().h(px(8.)).rounded_full().overflow_hidden();
    for (i, (_, b)) in got.langs.iter().enumerate() {
        bar = bar.child(div().h_full().bg(lang_color(i, p.dark)).w(relative(*b as f32 / total as f32)));
    }
    let mut legend = v_flex().gap_0p5();
    for (i, (name, b)) in got.langs.iter().enumerate() {
        legend = legend.child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .child(div().size(px(8.)).flex_shrink_0().when(!p.square_marks, |d| d.rounded_full()).bg(lang_color(i, p.dark)))
                .child(div().flex_1().min_w_0().text_color(p.text).child(name.clone()))
                .child(div().text_color(p.text_secondary).child(format!("{:.0}%", *b as f64 * 100. / total as f64)))
                .child(div().w(px(64.)).text_color(p.text_tertiary).flex().justify_end().child(format::size(*b))),
        );
    }
    let mut big = v_flex().gap_0p5().child(div().text_xs().text_color(p.text_secondary).child("Largest source files"));
    for (i, (path, size)) in got.largest.iter().enumerate() {
        let parent = path.parent().map(Path::to_path_buf);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tip = path.to_string_lossy().into_owned();
        big = big.child(
            h_flex()
                .id(("stats-big", i))
                .gap_2()
                .text_xs()
                .rounded(theme::RADIUS_SM)
                .cursor_pointer()
                .hover(|s| s.bg(p.hover))
                .tooltip(move |win, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(win, cx))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                .child(div().text_color(p.text_tertiary).child(format::size(*size)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(d) = parent.clone() {
                        this.navigate(Location::Path(d), cx)
                    }
                })),
        );
    }
    let sum = format!("{} lines in {} files", group(got.lines), group(got.files as u64));
    Some(
        v_flex()
            .gap_2()
            .child(folder_line(p, &folder, if got.partial { "partial: the walk was capped" } else { "" }))
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(sum))
            .child(bar)
            .child(legend)
            .child(big)
            .into_any_element(),
    )
}

/// 12345 -> "12,345".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------------
// S3 Activity calendar
// ---------------------------------------------------------------------------------------------------------------------

/// Weeks the calendar shows.
const WEEKS: i32 = 12;

/// The first day (a Monday) of a calendar of [`WEEKS`] weeks that ends in the week of `today` (days counted from one fixed day;
/// `today_dow` is 0 for a Monday).
pub fn cal_start(today: i32, today_dow: i32) -> i32 {
    today - today_dow - 7 * (WEEKS - 1)
}

/// Files per day: `days` are the day numbers of the modification times; the answer has one count per day from `start` through
/// the end of the last week (days after `today` stay 0); days outside the calendar are not counted.
pub fn bucket(days: impl IntoIterator<Item = i32>, start: i32, today: i32) -> Vec<u32> {
    let mut out = vec![0u32; (WEEKS * 7) as usize];
    for d in days {
        if d >= start && d <= today {
            out[(d - start) as usize] += 1;
        }
    }
    out
}

/// 0 for a quiet day, 1 to 4 by the share of the busiest day.
pub fn level(count: u32, max: u32) -> usize {
    if count == 0 || max == 0 {
        return 0;
    }
    (1 + ((count - 1) as u64 * 4 / max as u64) as usize).min(4)
}

#[derive(Clone)]
struct Cal {
    counts: Vec<u32>,
    start: i32,
    today: i32,
    partial: bool,
}

fn local_day(secs: i64) -> Option<i32> {
    use chrono::{Datelike, TimeZone};
    chrono::Local.timestamp_opt(secs, 0).single().map(|t| t.date_naive().num_days_from_ce())
}

fn compute_cal(folder: &Path, stop: &AtomicBool) -> Cal {
    use chrono::Datelike;
    let now = chrono::Local::now().date_naive();
    let today = now.num_days_from_ce();
    let start = cal_start(today, now.weekday().num_days_from_monday() as i32);
    let mut days: Vec<i32> = Vec::new();
    let partial = walk(folder, stop, |_, m| {
        let secs = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
        if let Some(d) = secs.and_then(local_day).filter(|d| *d >= start && *d <= today) {
            days.push(d);
        }
        true
    });
    Cal { counts: bucket(days, start, today), start, today, partial }
}

pub fn activity_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    refresh_btn("activity-refresh", p, cx, |d| d.cal.clear())
}

pub fn activity(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let Some(folder) = folder_of(v, "activity", cx) else { return Some(hint_no_folder(p)) };
    let c = &mut v.home_data.cal;
    c.sync(&folder);
    c.poll();
    if c.idle() {
        let f = folder.clone();
        c.begin(cx, move |stop| compute_cal(&f, stop));
    }
    let Some(got) = c.value.clone() else { return Some(note(p, "Reading modification times...")) };
    let max = got.counts.iter().copied().max().unwrap_or(0);
    let total: u32 = got.counts.iter().sum();
    let mut weeks = h_flex().gap(px(3.));
    for wk in 0..WEEKS {
        let mut col = v_flex().gap(px(3.));
        for dow in 0..7 {
            let day = got.start + wk * 7 + dow;
            let cell = div().id(("cal", (wk * 7 + dow) as usize)).size(px(14.)).when(!p.square_marks, |d| d.rounded(px(3.)));
            if day > got.today {
                col = col.child(cell);
                continue;
            }
            let n = got.counts[(wk * 7 + dow) as usize];
            let lv = level(n, max);
            let fill = if lv == 0 { p.pressed } else { p.accent.opacity([0.0, 0.28, 0.5, 0.75, 1.0][lv]) };
            let date = chrono::NaiveDate::from_num_days_from_ce_opt(day).map(|d| d.format("%a %e %b %Y").to_string()).unwrap_or_default();
            let tip = format!("{} {}: {}", n, if n == 1 { "file modified" } else { "files modified" }, date.trim());
            // a click opens the search of this folder for the files modified that day
            let query = chrono::NaiveDate::from_num_days_from_ce_opt(day).map(|d| format!("modified:{}", d.format("%Y-%m-%d")));
            let root = folder.clone();
            let cell = cell.bg(fill).tooltip(move |win, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(win, cx));
            col = col.child(match query.filter(|_| n > 0) {
                Some(query) => cell
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(Location::Search { root: root.clone(), query: query.clone() }, cx))),
                None => cell,
            });
        }
        weeks = weeks.child(col);
    }
    let extra = format!("{} files in {} weeks{}", group(total as u64), WEEKS, if got.partial { ", partial: the walk was capped" } else { "" });
    Some(v_flex().gap_2().child(folder_line(p, &folder, &extra)).child(weeks).into_any_element())
}

// ---------------------------------------------------------------------------------------------------------------------
// S12 Reclaimable meter
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Reclaim {
    /// What deleting all but one copy of every group of identical files would free.
    dup_bytes: u64,
    dup_groups: usize,
    /// The build folders (`node_modules`, `target`...) and their size.
    junk_bytes: u64,
    junk_folders: usize,
}

/// The bytes of the files below `dir` (links are not followed).
fn dir_bytes(dir: &Path, stop: &AtomicBool) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if stop.load(Ordering::Relaxed) {
                return total;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                todo.push(e.path());
            } else if let Ok(m) = e.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// The same two scanners the "Find build junk" and "Find duplicates" pages use. The duplicate search hashes files.
fn compute_reclaim(folder: &Path, stop: &AtomicBool, prog: &dupes::Progress) -> Reclaim {
    let mut found: Vec<PathBuf> = Vec::new();
    junk::scan(folder, stop, &junk::git_tracks, &mut |j| found.push(j.path));
    let junk_bytes = found.iter().map(|p| dir_bytes(p, stop)).sum();
    let groups = dupes::find(&[folder.to_path_buf()], &dupes::Options { include_hidden: false, threads: 2 }, prog, stop).unwrap_or_default();
    Reclaim { dup_bytes: groups.iter().map(|g| g.wasted()).sum(), dup_groups: groups.len(), junk_bytes, junk_folders: found.len() }
}

pub fn reclaim_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    refresh_btn("reclaim-refresh", p, cx, |d| {
        d.reclaim.clear();
        d.reclaim_progress = None;
    })
}

pub fn reclaim(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let Some(folder) = folder_of(v, "reclaim", cx) else { return Some(hint_no_folder(p)) };
    let c = &mut v.home_data.reclaim;
    c.sync(&folder);
    c.poll();
    let got = c.value.clone();
    let running = c.running();

    let open_dups = {
        let f = folder.clone();
        cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::Duplicates(vec![f.clone()]))))
    };
    let open_junk = {
        let f = folder.clone();
        cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::BuildJunk(f.clone()))))
    };
    let buttons = h_flex()
        .gap_2()
        .flex_wrap()
        .child(home_button("reclaim-dups", L::Copy, "Find duplicates", p, open_dups))
        .child(home_button("reclaim-junk", L::Eraser, "Find build junk", p, open_junk));

    let body: AnyElement = if running {
        let seen = v.home_data.reclaim_progress.as_ref().map_or(0, |pr| pr.files.load(Ordering::Relaxed));
        let stop = home_button(
            "reclaim-stop",
            L::X,
            "Stop",
            p,
            cx.listener(|v, _: &ClickEvent, _, cx| {
                v.home_data.reclaim.clear();
                v.home_data.reclaim_progress = None;
                cx.notify();
            }),
        );
        v_flex().gap_2().child(note(p, format!("Measuring... {} files seen", group(seen)))).child(stop).into_any_element()
    } else if let Some(r) = got {
        let total = r.dup_bytes + r.junk_bytes;
        let frac = |b: u64| if total == 0 { 0. } else { b as f32 / total as f32 };
        let bar = h_flex()
            .w_full()
            .h(px(8.))
            .rounded_full()
            .overflow_hidden()
            .bg(p.pressed)
            .child(div().h_full().bg(p.accent).w(relative(frac(r.dup_bytes))))
            .child(div().h_full().bg(p.warning).w(relative(frac(r.junk_bytes))));
        let key = |color: Hsla, text: String| {
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .text_color(p.text_secondary)
                .child(div().size(px(8.)).flex_shrink_0().when(!p.square_marks, |d| d.rounded_full()).bg(color))
                .child(text)
        };
        v_flex()
            .gap_2()
            .child(div().text_2xl().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(format::size(total)))
            .child(bar)
            .child(key(p.accent, format!("{} in {} groups of duplicates", format::size(r.dup_bytes), r.dup_groups)))
            .child(key(p.warning, format!("{} in {} build folders", format::size(r.junk_bytes), r.junk_folders)))
            .child(note(p, "Both can overlap: a duplicate inside a build folder is counted twice."))
            .into_any_element()
    } else {
        let measure = home_button(
            "reclaim-measure",
            L::Play,
            "Measure",
            p,
            cx.listener({
                let f = folder.clone();
                move |v, _: &ClickEvent, _, cx| {
                    let prog = Arc::new(dupes::Progress::default());
                    v.home_data.reclaim_progress = Some(prog.clone());
                    let f = f.clone();
                    v.home_data.reclaim.begin(cx, move |stop| compute_reclaim(&f, stop, &prog));
                    cx.notify();
                }
            }),
        );
        v_flex().gap_2().child(note(p, "What the duplicate finder and the build junk finder could free here. Measuring hashes files, so it runs only when you press Measure.")).child(measure).into_any_element()
    };
    Some(v_flex().gap_3().child(folder_line(p, &folder, "")).child(body).child(buttons).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{HashMap, ago, bucket, cal_start, count_lines, group, language, level, rank};

    #[test]
    fn relative_time() {
        // pulse: relative time
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 3 * 86_400), "3 d ago");
        assert_eq!(ago(500, 100), "just now");

    }

    #[test]
    fn language_split() {
        // code stats: only text files count, the tail folds into "Other", ties by name
        let mut m: HashMap<&'static str, u64> = HashMap::new();
        for (ext, b) in [("rs", 600), ("RS", 100), ("py", 300), ("png", 9999), ("md", 50), ("js", 50)] {
            if let Some(l) = language(ext) {
                *m.entry(l).or_default() += b;
            }
        }
        assert_eq!(rank(&m, 10), vec![("Rust".to_string(), 700), ("Python".to_string(), 300), ("Docs".to_string(), 50), ("JavaScript".to_string(), 50)]);
        assert_eq!(rank(&m, 2).last().unwrap(), &("Other".to_string(), 100));
        assert_eq!(count_lines(b"a\nb\nc"), Some(3));
        assert_eq!(count_lines(b"a\nb\n"), Some(2));
        assert_eq!(count_lines(b"\0\x01"), None);
        assert_eq!(group(1234567), "1,234,567");

    }

    #[test]
    fn calendar_bucketing() {
        // activity calendar: day 100 is a Wednesday (dow 2); the grid starts on the Monday 11 weeks before this week's
        let today = 100;
        let start = cal_start(today, 2);
        assert_eq!(start, 100 - 2 - 77);
        let b = bucket([start, start, today, today + 1, start - 1], start, today);
        assert_eq!(b.len(), 84);
        assert_eq!((b[0], b[(today - start) as usize], b.iter().sum::<u32>()), (2, 1, 3));
        assert_eq!((level(0, 9), level(1, 9), level(9, 9), level(5, 9), level(3, 3)), (0, 1, 4, 2, 3));
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// S2 Age rings
// ---------------------------------------------------------------------------------------------------------------------

/// The ages the rings stand for, innermost first.
const RING_LABELS: [&str; 5] = ["Today", "This week", "This month", "This year", "Older"];

#[derive(Clone)]
struct Rings {
    /// Files by last change: today, within 7 days, within 30 days, within a year, older.
    counts: [u64; 5],
    partial: bool,
}

/// The ring a file of this age (seconds since its last change) belongs to.
fn ring_of(age_secs: i64) -> usize {
    match age_secs {
        i64::MIN..=86_399 => 0,
        86_400..=604_799 => 1,
        604_800..=2_591_999 => 2,
        2_592_000..=31_535_999 => 3,
        _ => 4,
    }
}

fn compute_rings(folder: &Path, stop: &AtomicBool) -> Rings {
    let now = now_secs();
    let mut counts = [0u64; 5];
    let partial = walk(folder, stop, |_, m| {
        let secs = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        counts[ring_of(now - secs)] += 1;
        true
    });
    Rings { counts, partial }
}

pub fn age_rings_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    refresh_btn("agerings-refresh", p, cx, |d| d.rings.clear())
}

pub fn age_rings(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let Some(folder) = folder_of(v, "agerings", cx) else { return Some(hint_no_folder(p)) };
    let c = &mut v.home_data.rings;
    c.sync(&folder);
    c.poll();
    if c.idle() {
        let f = folder.clone();
        c.begin(cx, move |stop| compute_rings(&f, stop));
    }
    let Some(got) = c.value.clone() else { return Some(note(p, "Reading modification times...")) };
    let total: u64 = got.counts.iter().sum();
    if total == 0 {
        return Some(note(p, "No files here."));
    }
    // each ring is a track; the arc on it is that age's share of the files (a ring with files shows at least a sliver)
    let size = 112.0f32;
    let (hole, width, gap) = (14.0f32, 9.0f32, 2.5f32);
    let fills: Vec<Hsla> = (0..5).map(|i| p.accent.opacity([1.0, 0.82, 0.64, 0.46, 0.3][i])).collect();
    let arcs: Vec<(f32, f32, f32, Hsla)> = (0..5)
        .map(|i| {
            let r0 = hole + i as f32 * (width + gap);
            let share = got.counts[i] as f32 / total as f32;
            let sweep = if got.counts[i] == 0 { 0.0 } else { share.max(0.02) } * std::f32::consts::TAU;
            (r0, r0 + width, sweep.min(std::f32::consts::TAU - 0.001), fills[i])
        })
        .collect();
    let track = p.pressed;
    let paint = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let c = size / 2.0;
            let at = |r: f32, ang: f32| point(bounds.origin.x + px(c + r * ang.sin()), bounds.origin.y + px(c - r * ang.cos()));
            let mut draw = |r0: f32, r1: f32, sweep: f32, color: Hsla| {
                if sweep <= 0.0 {
                    return;
                }
                let steps = ((sweep / 0.05).ceil() as usize).max(1);
                let mut pts = Vec::with_capacity(2 * steps + 2);
                for i in 0..=steps {
                    pts.push(at(r1, sweep * i as f32 / steps as f32));
                }
                for i in (0..=steps).rev() {
                    pts.push(at(r0, sweep * i as f32 / steps as f32));
                }
                let mut b = PathBuilder::fill();
                b.add_polygon(&pts, true);
                if let Ok(path) = b.build() {
                    window.paint_path(path, color);
                }
            };
            for (r0, r1, sweep, color) in &arcs {
                draw(*r0, *r1, std::f32::consts::TAU - 0.001, track);
                draw(*r0, *r1, *sweep, *color);
            }
        },
    )
    .absolute()
    .size_full();
    let mut legend = v_flex().gap(px(3.)).justify_center();
    for i in 0..5 {
        let n = got.counts[i];
        legend = legend.child(
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .child(div().size(px(9.)).rounded_full().bg(fills[i]))
                .child(div().w(px(76.)).text_color(p.text_secondary).child(RING_LABELS[i]))
                .child(div().text_color(p.text).child(group(n))),
        );
    }
    let extra = format!("{} files{}", group(total), if got.partial { ", partial: the walk was capped" } else { "" });
    Some(
        v_flex()
            .gap_2()
            .child(folder_line(p, &folder, &extra))
            .child(h_flex().gap_4().items_center().child(div().relative().size(px(size)).flex_shrink_0().child(paint)).child(legend))
            .into_any_element(),
    )
}
