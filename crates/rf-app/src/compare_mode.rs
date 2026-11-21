//! Folder compare in dual pane: the two panes' listings are compared (`rf_core::compare`), each row is marked and tinted by what it
//! is against the other pane, and a bar above the panes shows the counts, the filter chips and "Select differing". It follows the
//! panes: a listing that changes or a pane that goes to another folder compares again; it ends when the tab leaves dual pane.
//! Nothing here changes a file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::compare::{self, Counts, Item, Mark};
use rf_core::pathkey;

use crate::folder_view::FolderView;
use crate::t;
use crate::theme::Palette;
use crate::workspace::Workspace;

/// What one item of a pane is against the other pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    OnlyHere,
    Newer,
    Older,
    Different,
    Same,
    /// Checked by content: the same bytes.
    SameContent,
    /// Checked by content: other bytes.
    ContentDiffers,
}

impl Look {
    /// The index into the theme's four compare colours (`Palette::compare`); identical items have none.
    pub fn color_ix(self) -> Option<usize> {
        match self {
            Look::OnlyHere => Some(0),
            Look::Newer => Some(1),
            Look::Older => Some(2),
            Look::Different => Some(3),
            Look::ContentDiffers => Some(3),
            Look::Same | Look::SameContent => None,
        }
    }

    /// Identical items (by listing or by content) are shown dimmed and are not "differing".
    pub fn is_same(self) -> bool {
        matches!(self, Look::Same | Look::SameContent)
    }

    pub fn glyph(self) -> Option<&'static str> {
        match self {
            Look::OnlyHere => Some("only here"),
            Look::Newer => Some("newer"),
            Look::Older => Some("older"),
            Look::Different => Some("size differs"),
            Look::SameContent => Some("same content"),
            Look::ContentDiffers => Some("content differs"),
            Look::Same => None,
        }
    }
}

/// Which items the panes list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chip {
    All,
    Differences,
    OnlyLeft,
    OnlyRight,
    Newer,
}

fn look_of(m: Mark, left_side: bool) -> Look {
    match (m, left_side) {
        (Mark::Same, _) => Look::Same,
        (Mark::OnlyLeft, true) | (Mark::OnlyRight, false) => Look::OnlyHere,
        (Mark::NewerLeft, true) | (Mark::NewerRight, false) => Look::Newer,
        (Mark::NewerRight, true) | (Mark::NewerLeft, false) => Look::Older,
        (Mark::DifferentSize, _) => Look::Different,
        (Mark::SameContent, _) => Look::SameContent,
        (Mark::ContentDiffers, _) => Look::ContentDiffers,
        // (a name absent from a side is never marked as only the other's)
        (Mark::OnlyLeft, false) | (Mark::OnlyRight, true) => Look::OnlyHere,
    }
}

/// What the compare was made from: it is made again when any of it changes.
#[derive(PartialEq, Eq, Clone)]
struct Sig {
    left: (String, u64, usize),
    right: (String, u64, usize),
    chip: Chip,
    ready: bool,
}

fn items(v: &FolderView) -> Vec<Item<'_>> {
    v.entries.iter().map(|e| Item { name: &e.name, size: e.size, modified_ms: e.modified_ms, is_dir: e.is_dir() }).collect()
}

pub struct State {
    pub left: Entity<FolderView>,
    pub right: Entity<FolderView>,
    pub chip: Chip,
    pub counts: Counts,
    /// Both panes show a folder (not Home, Settings…).
    pub ready: bool,
    sig: Option<Sig>,
    /// Content checks done so far (folded name -> same bytes); dropped when a pane's listing or place changes.
    content: HashMap<String, bool>,
    /// Also check the pairs the listing calls identical (same size and time).
    pub content_all: bool,
    /// While a content check runs: (done, total) and the flag that stops it.
    pub content_progress: Option<(usize, usize)>,
    content_cancel: Option<Arc<AtomicBool>>,
    pub(crate) content_task: Option<Task<()>>,
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop_content();
    }
}

impl State {
    pub fn new(left: Entity<FolderView>, right: Entity<FolderView>) -> Self {
        Self { left, right, chip: Chip::All,
            counts: Counts::default(),
            ready: false,
            sig: None,
            content: HashMap::new(),
            content_all: false,
            content_progress: None,
            content_cancel: None,
            content_task: None,
        }
    }

    fn signature(&self, cx: &App) -> Sig {
        let one = |v: &Entity<FolderView>| {
            let v = v.read(cx);
            (v.location().path().map(pathkey::path_key).unwrap_or_default(), v.entries_generation(), v.entries.len())
        };
        let ready = self.left.read(cx).location().path().is_some() && self.right.read(cx).location().path().is_some();
        Sig { left: one(&self.left), right: one(&self.right), chip: self.chip, ready }
    }

    /// Compares again when a pane's listing or place, or the chip, changed since the last time; marks and filters both panes.
    pub fn sync(&mut self, cx: &mut App) {
        let sig = self.signature(cx);
        if self.sig.as_ref() == Some(&sig) {
            return;
        }
        if self.sig.as_ref().is_some_and(|o| o.left != sig.left || o.right != sig.right || o.ready != sig.ready) {
            self.stop_content();
            self.content.clear();
        }
        self.sig = Some(sig.clone());
        self.ready = sig.ready;
        if !sig.ready {
            self.counts = Counts::default();
            for v in [&self.left, &self.right] {
                v.update(cx, |v, cx| v.set_compare(None, None, cx));
            }
            return;
        }
        let result = {
            let l = self.left.read(cx);
            let r = self.right.read(cx);
            let mut out = compare::compare(&items(l), &items(r));
            for (k, same) in &self.content {
                out.set_content(k, *same);
            }
            out
        };
        self.counts = result.counts();
        let chip = self.chip;
        for (view, marks, left_side) in [(&self.left, &result.left, true), (&self.right, &result.right, false)] {
            let looks: HashMap<String, Look> = marks.iter().map(|(k, m)| (k.clone(), look_of(*m, left_side))).collect();
            let only: Option<Arc<HashSet<String>>> = match chip {
                Chip::All => None,
                Chip::Differences => Some(Arc::new(looks.iter().filter(|(_, l)| !l.is_same()).map(|(k, _)| k.clone()).collect())),
                Chip::Newer => Some(Arc::new(looks.iter().filter(|(_, l)| matches!(l, Look::Newer | Look::Older)).map(|(k, _)| k.clone()).collect())),
                // (only the named pane is narrowed to its own items: the other keeps its whole listing)
                Chip::OnlyLeft if left_side => Some(Arc::new(looks.iter().filter(|(_, l)| **l == Look::OnlyHere).map(|(k, _)| k.clone()).collect())),
                Chip::OnlyRight if !left_side => Some(Arc::new(looks.iter().filter(|(_, l)| **l == Look::OnlyHere).map(|(k, _)| k.clone()).collect())),
                Chip::OnlyLeft | Chip::OnlyRight => None,
            };
            let looks = Arc::new(looks);
            view.update(cx, |v, cx| v.set_compare(Some(looks), only, cx));
        }
    }

    /// Stops a running content check (the pairs done so far keep their verdicts).
    pub fn stop_content(&mut self) {
        if let Some(c) = self.content_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.content_task = None;
        self.content_progress = None;
    }

    /// The pairs a content check would hash now: (folded name, left path, right path).
    pub fn content_pairs(&self, cx: &App) -> Vec<(String, PathBuf, PathBuf)> {
        let (l, r) = (self.left.read(cx), self.right.read(cx));
        let (Some(lp), Some(rp)) = (l.location().path().map(|p| p.to_path_buf()), r.location().path().map(|p| p.to_path_buf())) else { return Vec::new() };
        let (li, ri) = (items(l), items(r));
        let out = compare::compare(&li, &ri);
        let keys = compare::content_candidates(&out, &li, &ri, self.content_all);
        let rname: HashMap<String, &str> = ri.iter().map(|i| (pathkey::fold(i.name), i.name)).collect();
        keys.into_iter()
            .filter(|k| !self.content.contains_key(k))
            .filter_map(|k| {
                let ln = li.iter().find(|i| pathkey::fold(i.name) == k)?.name;
                let rn = rname.get(&k)?;
                Some((k, lp.join(ln), rp.join(rn)))
            })
            .collect()
    }

    /// Takes a finished verdict in; the next sync marks it.
    pub fn content_verdict(&mut self, key: String, same: bool, done: usize, total: usize) {
        self.content.insert(key, same);
        self.content_progress = Some((done, total));
        self.sig = None;
    }

    pub fn content_finished(&mut self) {
        self.content_progress = None;
        self.content_cancel = None;
        self.sig = None;
    }

    /// Takes the marks and the filter off both panes.
    pub fn clear(&self, cx: &mut App) {
        for v in [&self.left, &self.right] {
            v.update(cx, |v, cx| v.set_compare(None, None, cx));
        }
    }
}

/// The bar above the panes.
pub fn bar(state: &State, p: &Palette, cx: &mut Context<Workspace>) -> AnyElement {
    let c = state.counts;
    let chip = |id: &'static str, label: String, which: Chip| {
        let on = state.chip == which;
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded(px(p.control_radius))
            .text_size(px(p.fs_small))
            .cursor_pointer()
            .border_1()
            .border_color(if on { p.accent } else { p.border })
            .when(on, |d| d.bg(p.selection).text_color(p.text))
            .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.compare_set_chip(which, cx)))
    };
    let button = |id: &'static str, label: String| {
        div().id(id).px_2().py_1().rounded(px(p.control_radius)).text_size(px(p.fs_small)).cursor_pointer().text_color(p.text).bg(p.hover).child(label)
    };
    let summary = if state.ready {
        let newer = c.newer_left + c.newer_right;
        let mut s = format!("{} only left, {} only right, {} newer, {} different size, {} identical", c.only_left, c.only_right, newer, c.different, c.same);
        if c.same_content + c.content_differs > 0 {
            s.push_str(&format!(", {} same content, {} content differs", c.same_content, c.content_differs));
        }
        if let Some((d, n)) = state.content_progress {
            s.push_str(&format!(" - comparing contents: {d} of {n}"));
        }
        s
    } else {
        t!("Open a folder in each pane to compare them").to_string()
    };
    v_flex()
        .w_full()
        .px_4()
        .py_2()
        .gap_2()
        .flex_shrink_0()
        .border_b_1()
        .border_color(p.divider)
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .flex_wrap()
                .child(div().text_size(px(p.fs_small)).font_weight(FontWeight::SEMIBOLD).child(t!("Comparing folders")))
                .child(div().text_size(px(p.fs_small)).text_color(p.text_secondary).child(summary)),
        )
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(chip("cmp-diff", t!("Only differences").to_string(), Chip::Differences))
                .child(chip("cmp-left", t!("Only left").to_string(), Chip::OnlyLeft))
                .child(chip("cmp-right", t!("Only right").to_string(), Chip::OnlyRight))
                .child(chip("cmp-newer", t!("Newer").to_string(), Chip::Newer))
                .child(div().w(px(8.)))
                .child(div().w(px(8.)))
                .child(if state.content_progress.is_some() {
                    button("cmp-contents", t!("Cancel content check").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_contents_cancel(cx))).into_any_element()
                } else {
                    button("cmp-contents", t!("Compare contents").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_contents(cx))).into_any_element()
                })
                .child(
                    div()
                        .id("cmp-all")
                        .px_2()
                        .py_1()
                        .rounded(px(p.control_radius))
                        .text_size(px(p.fs_small))
                        .cursor_pointer()
                        .border_1()
                        .border_color(if state.content_all { p.accent } else { p.border })
                        .text_color(if state.content_all { p.text } else { p.text_secondary })
                        .child(t!("Include identical-looking files").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.compare_contents_all_toggle(cx))),
                )
                .child(div().w(px(8.)))
                .child(button("cmp-select", t!("Select differing").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_select_differing(cx))))
                .child(button("cmp-stop", t!("Stop comparing").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_stop(cx)))),
        )
        .into_any_element()
}

/// Starts the content check of the pairs a listing could not settle: one background thread hashes pair by pair (cancellable), the
/// bar shows "n of m" and each verdict marks its pair at once. No file is changed.
pub fn start_content(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(st) = ws.compare.as_mut() else { return };
    if st.content_progress.is_some() {
        return;
    }
    let pairs = st.content_pairs(cx);
    if pairs.is_empty() {
        return;
    }
    let total = pairs.len();
    let cancel = Arc::new(AtomicBool::new(false));
    st.content_cancel = Some(cancel.clone());
    st.content_progress = Some((0, total));
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<(String, Option<bool>)>();
    std::thread::Builder::new()
        .name("rf-compare-content".into())
        .spawn(move || {
            for (key, l, r) in pairs {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let stop = || cancel.load(Ordering::Relaxed);
                let a = leaf_sys::hash::compute(&l, &mut |_, _| {}, &stop);
                let b = if a.is_ok() { leaf_sys::hash::compute(&r, &mut |_, _| {}, &stop) } else { Err(anyhow::anyhow!("skipped")) };
                let same = match (a, b) {
                    (Ok(a), Ok(b)) => Some(a.size == b.size && a.sha256 == b.sha256),
                    _ => None,
                };
                if tx.unbounded_send((key, same)).is_err() {
                    break;
                }
            }
        })
        .ok();
    let task = cx.spawn(async move |this, cx| {
        use futures::StreamExt;
        let mut done = 0usize;
        while let Some((key, same)) = rx.next().await {
            done += 1;
            let ok = this.update(cx, |ws, cx| {
                if let Some(st) = ws.compare.as_mut() {
                    match same {
                        Some(s) => st.content_verdict(key, s, done, total),
                        None => st.content_progress = Some((done, total)),
                    }
                }
                cx.notify();
            });
            if ok.is_err() {
                return;
            }
        }
        let _ = this.update(cx, |ws, cx| {
            if let Some(st) = ws.compare.as_mut() {
                st.content_finished();
            }
            cx.notify();
        });
    });
    if let Some(st) = ws.compare.as_mut() {
        st.content_task = Some(task);
    }
    cx.notify();
}
