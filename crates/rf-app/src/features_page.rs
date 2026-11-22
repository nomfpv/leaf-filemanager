//! Settings -> What leaf can do: every command of the registry (`registry.rs`), grouped by category, with what it does and its
//! shortcut (the user's own one when set). A search box above narrows the list by title, description and keywords. Opened from the
//! page list in Settings and by the `ShowFeatures` command (in the command palette).

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::registry::{Category, CommandSpec, Places};
use crate::settings_view::{section, shell};
use crate::t;
use crate::theme::{self, Palette};

/// The page's search box (made at the first draw of Settings: it needs a window).
#[derive(Default)]
struct FeaturesUi {
    search: Option<Entity<InputState>>,
}
impl Global for FeaturesUi {}

/// `ShowFeatures` opens Settings on this page, in the window the command came from.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::ShowFeatures, cx| {
        crate::settings_view::open_page(crate::settings_view::Page::Features, cx);
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, cx| window.dispatch_action(Box::new(crate::actions::GoSettings), cx));
            }
        });
    });
}

/// Makes the search box once; Settings calls it on every draw.
pub fn ensure_search(window: &mut Window, cx: &mut App) {
    if cx.try_global::<FeaturesUi>().is_some_and(|u| u.search.is_some()) {
        return;
    }
    let e = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search commands")));
    // typing redraws the page (the box is not part of a view of its own)
    cx.observe(&e, |_, cx| cx.refresh_windows()).detach();
    cx.set_global(FeaturesUi { search: Some(e) });
}

/// Whether `c` has every word of the (folded) query in its title, description, keywords or group.
fn matches(c: &CommandSpec, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = rf_core::entry::fold_case(&format!(
        "{} {} {} {} {}",
        crate::i18n::t(c.title),
        crate::i18n::t(c.description),
        c.keywords.join(" "),
        crate::i18n::t(c.category.title()),
        c.id
    ));
    words.iter().all(|w| text.contains(w.as_str()))
}

fn row(p: &Palette, ix: usize, c: &'static CommandSpec, cx: &App) -> AnyElement {
    let custom = crate::keymap_editor::custom_keys(c.id, cx);
    let keys = match &custom {
        Some(k) => crate::registry::shown(k),
        None => c.shown_keys(),
    };
    let only = c.platforms.label();
    let desc = if only.is_empty() { crate::i18n::t(c.description).to_string() } else { format!("{} ({only})", crate::i18n::t(c.description)) };
    // a command of a part of leaf that is switched off is listed, marked, not hidden
    let off = !c.module_on(cx);
    let desc = if off { format!("{} {}", crate::i18n::t("Off, turn on in Settings > Features."), desc) } else { desc };
    h_flex()
        .id(("feat", ix))
        .min_h(px(52.))
        .px_4()
        .py_2()
        .gap_4()
        .items_center()
        .when(off, |d| d.opacity(0.6))
        .child(Icon::new(c.icon).size(px(16.)).flex_shrink_0().text_color(p.text_secondary))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(crate::i18n::t(c.title)))
                .child(div().text_xs().text_color(p.text_secondary).child(desc)),
        )
        .when(!keys.is_empty(), |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.border)
                    .text_xs()
                    .text_color(if custom.is_some() { p.accent } else { p.text_secondary })
                    .child(keys),
            )
        })
        .into_any_element()
}

pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let search = cx.try_global::<FeaturesUi>().and_then(|u| u.search.clone());
    let query = search.as_ref().map(|s| s.read(cx).value().trim().to_string()).unwrap_or_default();
    let words: Vec<String> = query.split_whitespace().map(rf_core::entry::fold_case).collect();
    let mut v: Vec<AnyElement> = Vec::new();
    v.push(
        div()
            .pb_2()
            .text_sm()
            .text_color(p.text_secondary)
            .child(t!("Every command leaf has, with what it does and its shortcut. Change a shortcut under Actions; find any command with Ctrl+Shift+P."))
            .into_any_element(),
    );
    if let Some(search) = &search {
        v.push(
            h_flex()
                .h(px(34.))
                .px_2()
                .gap_1()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .bg(p.hover)
                .border_1()
                .border_color(p.border)
                .child(Icon::new(L::Search).size(px(14.)).flex_shrink_0().text_color(p.text_secondary))
                .child(Input::new(search).appearance(false).cleanable(true))
                .into_any_element(),
        );
    }
    let mut ix = 0;
    let mut any = false;
    for cat in Category::ALL {
        let rows: Vec<&'static CommandSpec> =
            crate::registry::all().filter(|c| c.category == cat && c.places.contains(Places::PAGE) && c.when.is_none_or(|w| w(cx)) && matches(c, &words)).collect();
        if rows.is_empty() {
            continue;
        }
        any = true;
        v.push(section(p, crate::i18n::t(cat.title())));
        let mut card = shell(p);
        for (i, c) in rows.into_iter().enumerate() {
            if i > 0 {
                card = card.child(div().h(px(1.)).bg(p.divider));
            }
            card = card.child(row(p, ix, c, cx));
            ix += 1;
        }
        v.push(card.into_any_element());
    }
    if !any {
        v.push(div().py_4().text_sm().text_color(p.text_secondary).child(t!("No matching commands")).into_any_element());
    }
    v
}
