//! The worktree branch map (PLAN D13), on the Agents page: for each repository that has lanes, its base branch as a vertical trunk and
//! every lane as a branch off it, in the lane's colour: filled squares for the commits it is ahead (what a merge would bring), hollow
//! squares for the commits the base is ahead (what it is missing), the number of changed files, and the dot of an agent working in it.
//! Read only, from the lane records and the progress `lanes` already keeps fresh (no git runs here).

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::t;
use crate::theme::Palette;

/// Most commit squares drawn on a branch (the number says the rest).
const MAX_SQ: u32 = 10;

fn squares(n: u32, filled: bool, color: Hsla, p: &Palette) -> Div {
    let mut d = h_flex().gap(px(2.)).items_center();
    for _ in 0..n.min(MAX_SQ) {
        d = d.child(div().size(px(6.)).flex_shrink_0().rounded(px(1.5)).when(filled, |d| d.bg(color)).when(!filled, |d| d.border_1().border_color(p.border_strong)));
    }
    if n > MAX_SQ {
        d = d.child(div().text_xs().text_color(p.text_tertiary).child(format!("+{}", n - MAX_SQ)));
    }
    d
}

/// The map, `None` when there is no lane (nothing shows, as with the layer off).
pub fn view(cx: &App, p: &Palette) -> Option<Div> {
    let lanes = crate::lanes::list(cx);
    if lanes.is_empty() {
        return None;
    }
    let mut repos: Vec<(std::path::PathBuf, String)> = Vec::new();
    for l in &lanes {
        if !repos.iter().any(|(r, _)| leaf_agents::folder_key(r) == leaf_agents::folder_key(&l.repo)) {
            repos.push((l.repo.clone(), l.base.clone()));
        }
    }
    let mut col = v_flex().px_3().py_2().gap(px(2.)).border_b_1().border_color(p.divider);
    col = col.child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Branches")));
    for (repo, base) in repos {
        col = col.child(
            h_flex()
                .h(px(22.))
                .gap_2()
                .items_center()
                .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(p.text_secondary))
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(base))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child(rf_core::history::display_name(&repo))),
        );
        let mine: Vec<_> = lanes.iter().filter(|l| leaf_agents::folder_key(&l.repo) == leaf_agents::folder_key(&repo)).collect();
        for (i, l) in mine.iter().enumerate() {
            let color = crate::lanes::color(l.color, p);
            let last = i + 1 == mine.len();
            let pr = crate::lanes::progress(cx, &l.path).flatten();
            let agent = crate::agents::tab_state(cx, &l.path);
            let row = h_flex()
                .h(px(24.))
                .gap_2()
                .items_center()
                // the trunk runs down the left; the branch leaves it with a short stub
                .child(
                    div().w(px(8.)).h_full().flex_shrink_0().relative()
                        .child(div().absolute().left(px(3.5)).top(px(0.)).w(px(1.)).h(if last { px(12.) } else { px(24.) }).bg(p.border_strong))
                        .child(div().absolute().left(px(3.5)).top(px(11.5)).w(px(12.)).h(px(1.)).bg(color)),
                )
                .child(div().w(px(12.)).flex_shrink_0())
                .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(color))
                .child(div().min_w(px(90.)).max_w(px(220.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(l.name.clone()))
                .children(pr.map(|pr| squares(pr.ahead, true, color, p)))
                .children(pr.map(|pr| squares(pr.behind, false, color, p)))
                .children(pr.filter(|pr| pr.ahead > 0 || pr.behind > 0).map(|pr| div().text_xs().text_color(p.text_tertiary).child(format!("\u{2191}{} \u{2193}{}", pr.ahead, pr.behind))))
                .children(pr.filter(|pr| pr.changed > 0).map(|pr| div().text_xs().text_color(p.text_secondary).child(format!("{} {}", pr.changed, if pr.changed == 1 { t!("file changed") } else { t!("files changed") }))))
                .children(agent.map(|st| div().size(px(6.)).flex_shrink_0().rounded_full().bg(crate::agents::state_color(st, p))));
            col = col.child(row);
        }
    }
    Some(col)
}
