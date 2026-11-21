//! The "Changes" view of one file: what git says differs between the file and HEAD, as a unified diff with line numbers, added and
//! removed lines tinted (theme keys `diff_added`, `diff_removed`, `diff_hunk`), "+12 -3" in the header and a switch between only
//! the changes and the whole file. It lives in the preview pane (a tab beside the normal preview) and is the same one the change
//! feed shows. Read-only: only `leaf_dev::gitread` is used, off the UI thread, and nothing here writes to the repository.

use std::path::{Path, PathBuf};

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::gitread::{self, FileDiff, LineKind};

use crate::t;
use crate::theme::{self, Palette};

/// Lines of unchanged text kept round a change in "Only changes".
pub(crate) const CONTEXT: u32 = 3;
/// Enough context to take in the whole file.
pub(crate) const WHOLE: u32 = 1_000_000;
const ROW_H: f32 = 18.;
/// About the width of one character of the mono font at the diff's text size.
const CHAR_W: f32 = 7.3;

/// Where a file stands in its repository, when the caller already knows (the change feed does).
#[derive(Clone)]
pub struct Hint {
    pub root: PathBuf,
    pub rel: String,
    pub from: Option<String>,
    /// Another difference than against HEAD (a checkpoint's restore preview): asked with "whole file", off the UI thread.
    pub custom: Option<Custom>,
}

/// Works out the difference of one file another way than `git diff HEAD`; the flag asks for the whole file.
pub type Custom = std::sync::Arc<dyn Fn(bool) -> Option<FileDiff> + Send + Sync>;

#[derive(Clone)]
enum Row {
    Hunk(SharedString),
    Line { old: Option<u32>, new: Option<u32>, kind: LineKind, text: SharedString },
    Note(SharedString),
}

enum Load {
    Loading,
    /// Not in a repository, or git is missing or failed.
    None,
    Ready,
}

pub struct DiffPane {
    path: PathBuf,
    hint: Option<Hint>,
    state: Load,
    /// "Whole file" instead of "Only changes".
    whole: bool,
    only: Option<FileDiff>,
    full: Option<FileDiff>,
    rows: std::rc::Rc<Vec<Row>>,
    /// Sideways scroll of the text column in px (the number gutter stays), and the longest line in characters.
    hscroll: f32,
    longest: usize,
    /// Last known width of the text column, for the scroll limit.
    view_w: std::rc::Rc<std::cell::Cell<f32>>,
    _task: Option<Task<()>>,
}

impl DiffPane {
    pub fn new(path: PathBuf, hint: Option<Hint>, cx: &mut Context<Self>) -> Self {
        let mut this = Self { path, hint, state: Load::Loading, whole: false, only: None, full: None, rows: Default::default(), hscroll: 0., longest: 0, view_w: Default::default(), _task: None };
        this.load(false, cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&mut self, whole: bool, cx: &mut Context<Self>) {
        let (path, hint) = (self.path.clone(), self.hint.clone());
        let work = cx.background_spawn(async move {
            if let Some(f) = hint.as_ref().and_then(|h| h.custom.clone()) {
                return f(whole);
            }
            let (root, rel, from) = match hint {
                Some(h) => (h.root, h.rel, h.from),
                None => {
                    // (a cheap look up the folders first: most files are in no repository at all)
                    leaf_dev::gitignore::repo_root(path.parent()?)?;
                    let (root, rel) = gitread::locate(&path)?;
                    (root, rel, None)
                }
            };
            gitread::diff_file(&root, &rel, from.as_deref(), if whole { WHOLE } else { CONTEXT })
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let d = work.await;
            let _ = this.update(cx, |v, cx| {
                match d {
                    Some(d) => {
                        if whole {
                            v.full = Some(d);
                        } else {
                            v.only = Some(d);
                        }
                        v.state = Load::Ready;
                    }
                    None => v.state = Load::None,
                }
                v.rebuild();
                cx.notify();
            });
        }));
    }

    /// The file differs from HEAD (a change to show), counting a binary file.
    pub fn has_changes(&self) -> bool {
        matches!(self.state, Load::Ready) && self.only.as_ref().or(self.full.as_ref()).is_some_and(|d| !d.hunks.is_empty() || d.binary)
    }

    /// (added, removed) lines, once known.
    pub fn counts(&self) -> Option<(u32, u32)> {
        self.only.as_ref().or(self.full.as_ref()).map(|d| (d.added, d.removed))
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.state, Load::Loading)
    }

    fn current(&self) -> Option<&FileDiff> {
        if self.whole { self.full.as_ref() } else { self.only.as_ref() }
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        if let Some(d) = self.current() {
            if d.binary {
                rows.push(Row::Note(t!("A binary file: no lines to show.").into()));
            }
            for h in &d.hunks {
                if !self.whole {
                    rows.push(Row::Hunk(h.header.clone().into()));
                }
                for l in &h.lines {
                    rows.push(Row::Line { old: l.old_no, new: l.new_no, kind: l.kind, text: l.text.replace('\t', "    ").into() });
                }
            }
            if d.truncated {
                rows.push(Row::Note(t!("The rest of this diff is not shown: it is too long.").into()));
            }
        }
        self.longest = rows.iter().filter_map(|r| if let Row::Line { text, .. } = r { Some(text.chars().count()) } else { None }).max().unwrap_or(0);
        self.hscroll = 0.;
        self.rows = std::rc::Rc::new(rows);
    }

    /// The widest the text can scroll (px): the longest line at about the width of one mono character, less the visible width.
    fn max_hscroll(&self) -> f32 {
        (self.longest as f32 * CHAR_W + 16. - self.view_w.get()).max(0.)
    }

    fn scroll_by(&mut self, dx: f32, cx: &mut Context<Self>) {
        let v = (self.hscroll - dx).clamp(0., self.max_hscroll());
        if (v - self.hscroll).abs() > 0.1 {
            self.hscroll = v;
            cx.notify();
        }
    }

    fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        crate::settings::update(cx, |s| s.diff_wrap_lines = wrap);
        self.hscroll = 0.;
        cx.notify();
    }

    fn set_whole(&mut self, whole: bool, cx: &mut Context<Self>) {
        if self.whole == whole {
            return;
        }
        self.whole = whole;
        if self.current().is_none() {
            self._task = None;
            self.load(whole, cx);
        }
        self.rebuild();
        cx.notify();
    }
}

fn toggle_btn(id: &'static str, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
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

/// "+12" and "-3" as the header shows them (the minus is U+2212).
pub fn counts_text(added: u32, removed: u32) -> String {
    format!("+{added} \u{2212}{removed}")
}

impl Render for DiffPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let [c_add, c_rem, c_hunk] = p.diff;
        let (added, removed) = self.counts().unwrap_or((0, 0));
        let wrap = cx.global::<crate::settings::Settings>().diff_wrap_lines;
        let header = h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .border_b_1()
            .border_color(p.divider)
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.success).child(format!("+{added}")))
            .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.danger).child(format!("\u{2212}{removed}")))
            .child(div().flex_1())
            .child(toggle_btn("diff-wrap", t!("Wrap lines"), wrap, &p).on_click(cx.listener(move |v, _, _, cx| v.set_wrap(!wrap, cx))))
            .child(
                h_flex()
                    .gap_0p5()
                    .child(toggle_btn("diff-only", t!("Only changes"), !self.whole, &p).on_click(cx.listener(|v, _, _, cx| v.set_whole(false, cx))))
                    .child(toggle_btn("diff-whole", t!("Whole file"), self.whole, &p).on_click(cx.listener(|v, _, _, cx| v.set_whole(true, cx)))),
            );
        let body: AnyElement = if self.current().is_none() {
            let msg = if self.is_loading() || matches!(self.state, Load::Ready) { t!("Loading…") } else { t!("No changes to show") };
            div().flex_1().flex().items_center().justify_center().text_xs().text_color(p.text_tertiary).child(msg).into_any_element()
        } else {
            let rows = self.rows.clone();
            let count = rows.len();
            // the number columns are as wide as the biggest number needs
            let top = rows.iter().filter_map(|r| if let Row::Line { old, new, .. } = r { (*old).max(*new) } else { None }).max().unwrap_or(0);
            let num_w = (top.to_string().len().max(2) as f32) * 7. + 8.;
            let hscroll = self.hscroll;
            let build = std::rc::Rc::new(move |range: std::ops::Range<usize>| -> Vec<AnyElement> {
                range
                    .map(|i| match &rows[i] {
                        Row::Hunk(h) => div()
                            .id(("diff-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .bg(c_hunk)
                            .text_color(p.text_secondary)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(h.clone())
                            .into_any_element(),
                        Row::Note(n) => div()
                            .id(("diff-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .text_color(p.text_tertiary)
                            .child(n.clone())
                            .into_any_element(),
                        Row::Line { old, new, kind, text } => {
                            let (bg, sign) = match kind {
                                LineKind::Added => (Some(c_add), "+"),
                                LineKind::Removed => (Some(c_rem), "\u{2212}"),
                                LineKind::Context => (None, " "),
                            };
                            let num = |n: &Option<u32>| div().w(px(num_w)).flex_shrink_0().px_1().text_right().text_color(p.text_tertiary).child(n.map(|n| n.to_string()).unwrap_or_default());
                            let gutter = h_flex()
                                .flex_shrink_0()
                                .child(num(old))
                                .child(num(new))
                                .child(div().w(px(12.)).flex_shrink_0().text_color(p.text_secondary).child(sign));
                            if wrap {
                                h_flex()
                                    .id(("diff-row", i))
                                    .w_full()
                                    .min_h(px(ROW_H))
                                    .items_start()
                                    .when_some(bg, |d, c| d.bg(c))
                                    .child(gutter)
                                    .child(div().flex_1().min_w_0().text_color(p.text).whitespace_normal().child(text.clone()))
                                    .into_any_element()
                            } else {
                                // the text column is cut off at its edge and slid sideways; the gutter stays where it is
                                h_flex()
                                    .id(("diff-row", i))
                                    .h(px(ROW_H))
                                    .w_full()
                                    .items_center()
                                    .when_some(bg, |d, c| d.bg(c))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .child(gutter)
                                    .child(div().flex_1().min_w_0().h_full().overflow_hidden().relative().child(
                                        div().absolute().top_0().left(px(-hscroll)).h_full().flex().items_center().text_color(p.text).child(text.clone()),
                                    ))
                                    .into_any_element()
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            });
            let width = self.view_w.clone();
            let inner: AnyElement = if wrap {
                let b = build.clone();
                v_flex().id("diff-rows-wrap").flex_1().min_h_0().overflow_y_scroll().text_xs().font_family(crate::platform::fonts::mono()).children(b(0..count)).into_any_element()
            } else {
                let b = build.clone();
                uniform_list("diff-rows", count, move |range: std::ops::Range<usize>, _, _| b(range)).flex_1().min_h_0().text_xs().font_family(crate::platform::fonts::mono()).into_any_element()
            };
            div()
                .id("diff-body")
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(inner)
                .child(canvas(move |b, _, _| width.set(f32::from(b.size.width) - num_w * 2. - 12.), |_, _, _, _| {}).absolute().size_full())
                .when(!wrap, |d| {
                    d.on_scroll(cx.listener(|v, ev: &ScrollEvent, _, cx| {
                        let dl = ev.delta.pixel_delta(px(ROW_H));
                        let dx = if ev.modifiers.shift { f32::from(dl.y) } else { f32::from(dl.x) };
                        if dx != 0. {
                            v.scroll_by(dx, cx);
                        }
                    }))
                })
            .into_any_element()
        };
        v_flex().size_full().overflow_hidden().rounded(theme::RADIUS_SM).bg(p.tile_bg).border_1().border_color(p.border).child(header).child(body)
    }
}

/// Where the change feed's files stand (it knows the repository and a rename's old name): the preview pane asks here first, and a
/// file in here opens on its Changes tab.
static HINTS: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, Hint>>> = std::sync::Mutex::new(None);

/// The feed gives its files (replacing what it gave before).
pub fn remember_hints(list: Vec<(PathBuf, Hint)>) {
    if let Ok(mut g) = HINTS.lock() {
        *g = Some(list.into_iter().collect());
    }
}

pub fn forget_hints() {
    if let Ok(mut g) = HINTS.lock() {
        *g = None;
    }
}

pub fn hint_of(path: &Path) -> Option<Hint> {
    HINTS.lock().ok()?.as_ref()?.get(path).cloned()
}
