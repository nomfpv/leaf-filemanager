//! Settings -> What's new: the commands of the registry (`registry.rs`) grouped by the version they came with (`since`), newest
//! first. The Settings entry carries a small dot until the page has been opened once after an update (`Settings::whats_new_seen`).

use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::registry::{CommandSpec, Places};
use crate::settings::Settings;
use crate::settings_view::{section, shell};
use crate::t;
use crate::theme::Palette;

/// "0.9.10" -> (0, 9, 10); what is not a number counts as 0.
fn version_key(v: &str) -> (u32, u32, u32) {
    let mut n = v.split('.').map(|p| p.trim().parse::<u32>().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

/// The commands that count for the page, grouped by `since`, newest version first (the commands inside a group keep the registry's order).
fn groups<'a>(rows: impl Iterator<Item = &'a CommandSpec>) -> Vec<(&'a str, Vec<&'a CommandSpec>)> {
    let mut out: Vec<(&str, Vec<&CommandSpec>)> = Vec::new();
    for c in rows {
        match out.iter_mut().find(|(v, _)| *v == c.since) {
            Some((_, g)) => g.push(c),
            None => out.push((c.since, vec![c])),
        }
    }
    out.sort_by(|a, b| version_key(b.0).cmp(&version_key(a.0)));
    out
}

fn listed() -> impl Iterator<Item = &'static CommandSpec> {
    crate::registry::all().filter(|c| c.custom.is_empty() && c.places.contains(Places::PAGE))
}

/// The version the dot and the page speak of: the running one, or a newer `since` of a command that is already in (a build between
/// two releases).
fn current() -> String {
    let mut cur = env!("CARGO_PKG_VERSION").to_string();
    for c in listed() {
        if version_key(c.since) > version_key(&cur) {
            cur = c.since.to_string();
        }
    }
    cur
}

/// Whether the Settings entry wears its dot: the page has not been opened since this version came.
pub fn unseen(s: &Settings) -> bool {
    s.whats_new_seen != current()
}

/// The page is on screen: from now on it is seen (done after the draw; a draw does not change settings).
pub fn seen(s: &Settings, cx: &mut App) {
    if unseen(s) {
        cx.defer(|cx| crate::settings::update(cx, |s| s.whats_new_seen = current()));
    }
}

fn row(p: &Palette, ix: usize, c: &'static CommandSpec) -> AnyElement {
    let keys = c.shown_keys();
    h_flex()
        .id(("new", ix))
        .min_h(px(52.))
        .px_4()
        .py_2()
        .gap_4()
        .items_center()
        .child(Icon::new(c.icon).size(px(16.)).flex_shrink_0().text_color(p.text_secondary))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(crate::i18n::t(c.title)))
                .child(div().text_xs().text_color(p.text_secondary).child(crate::i18n::t(c.description))),
        )
        .when(!keys.is_empty(), |d| d.child(div().flex_shrink_0().px_2().py_0p5().rounded(px(4.)).border_1().border_color(p.border).text_xs().text_color(p.text_secondary).child(keys)))
        .into_any_element()
}

pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let mut v: Vec<AnyElement> = vec![div()
        .pb_2()
        .text_sm()
        .text_color(p.text_secondary)
        .child(t!("The commands that came with each version of leaf, newest first. Every command, old and new, is under What leaf can do."))
        .into_any_element()];
    let mut ix = 0;
    let oldest = groups(listed().filter(|c| c.available(cx))).into_iter().map(|(v, _)| v).min_by_key(|v| version_key(v));
    for (ver, rows) in groups(listed().filter(|c| c.available(cx))) {
        let title = if Some(ver) == oldest && groups(listed()).len() > 1 { format!("{ver} {}", t!("and before")) } else { format!("{} {ver}", t!("Version")) };
        v.push(section(p, &title));
        let mut card = shell(p);
        for (i, c) in rows.into_iter().enumerate() {
            if i > 0 {
                card = card.child(div().h(px(1.)).bg(p.divider));
            }
            card = card.child(row(p, ix, c));
            ix += 1;
        }
        v.push(card.into_any_element());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::version_key;

    #[test]
    fn versions_sort_by_number_not_by_text() {
        assert!(version_key("0.9.10") > version_key("0.9.5"));
        assert!(version_key("0.10.0") > version_key("0.9.9"));
        assert_eq!(version_key("1"), (1, 0, 0));
    }
}
