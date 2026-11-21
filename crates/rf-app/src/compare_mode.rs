//! Folder compare in dual pane: the two panes' listings are compared (`rf_core::compare`), each row is marked and tinted by what it
//! is against the other pane, and a bar above the panes shows the counts, the filter chips and "Select differing". It follows the
//! panes: a listing that changes or a pane that goes to another folder compares again; it ends when the tab leaves dual pane.
//! Nothing here changes a file.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
}

impl Look {
    /// The index into the theme's four compare colours (`Palette::compare`); identical items have none.
    pub fn color_ix(self) -> Option<usize> {
        match self {
            Look::OnlyHere => Some(0),
            Look::Newer => Some(1),
            Look::Older => Some(2),
            Look::Different => Some(3),
            Look::Same => None,
        }
    }

    pub fn glyph(self) -> Option<&'static str> {
        match self {
            Look::OnlyHere => Some("only here"),
            Look::Newer => Some("newer"),
            Look::Older => Some("older"),
            Look::Different => Some("size differs"),
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
}

impl State {
    pub fn new(left: Entity<FolderView>, right: Entity<FolderView>) -> Self {
        Self { left, right, chip: Chip::All, counts: Counts::default(), ready: false, sig: None }
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
            compare::compare(&items(l), &items(r))
        };
        self.counts = result.counts();
        let chip = self.chip;
        for (view, marks, left_side) in [(&self.left, &result.left, true), (&self.right, &result.right, false)] {
            let looks: HashMap<String, Look> = marks.iter().map(|(k, m)| (k.clone(), look_of(*m, left_side))).collect();
            let only: Option<Arc<HashSet<String>>> = match chip {
                Chip::All => None,
                Chip::Differences => Some(Arc::new(looks.iter().filter(|(_, l)| **l != Look::Same).map(|(k, _)| k.clone()).collect())),
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
        format!("{} only left, {} only right, {} newer, {} different size, {} identical", c.only_left, c.only_right, newer, c.different, c.same)
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
                .child(button("cmp-select", t!("Select differing").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_select_differing(cx))))
                .child(button("cmp-stop", t!("Stop comparing").to_string()).on_click(cx.listener(|this, _, _, cx| this.compare_stop(cx)))),
        )
        .into_any_element()
}
