//! Settings > Guide ("How do I ..."): short task-oriented topics (`guide_topics.rs`), searchable. Opened by the `ShowGuide` command
//! (F1, the title bar's main menu, the setup's last page), by a "?" on a page that has a topic ([`help_button`]) and by the command
//! palette's "How do I: ..." rows ([`matching`]). The page lists the topics by group; a click opens one: numbered steps, a
//! "Do it" button where the task can be started from here, and the limits under "Good to know".

use std::collections::HashSet;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::*;

use crate::guide_topics::{Go, TOPICS, Topic};
use crate::settings_view::{Page, button, section, shell};
use crate::t;
use crate::theme::{self, Palette};

/// The page's state: its search box (made at the first draw: it needs a window), the open topics, and the topic a "?" asked for.
#[derive(Default)]
struct GuideUi {
    search: Option<Entity<InputState>>,
    open: HashSet<&'static str>,
    /// Shown alone, open, until "All topics" is pressed.
    focus: Option<&'static str>,
}
impl Global for GuideUi {}

fn ui(cx: &mut App) -> &mut GuideUi {
    if !cx.has_global::<GuideUi>() {
        cx.set_global(GuideUi::default());
    }
    cx.global_mut::<GuideUi>()
}

/// `ShowGuide` opens Settings on this page, in the window the command came from.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::ShowGuide, cx| {
        crate::settings_view::open_page(Page::Guide, cx);
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, cx| window.dispatch_action(Box::new(crate::actions::GoSettings), cx));
            }
        });
    });
}

/// The next time the guide is shown it shows this topic, open (a "?" on a page, a row of the command palette).
pub fn set_focus(id: &'static str, cx: &mut App) {
    let g = ui(cx);
    g.focus = Some(id);
    g.open.insert(id);
}

/// Makes the search box once; Settings calls it on every draw.
pub fn ensure_search(window: &mut Window, cx: &mut App) {
    if ui(cx).search.is_some() {
        return;
    }
    let e = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search the guide: ftp, zip, nas, screenshot…")));
    // typing redraws the page (the box is not part of a view of its own)
    cx.observe(&e, |_, cx| cx.refresh_windows()).detach();
    ui(cx).search = Some(e);
}

fn words_of(query: &str) -> Vec<String> {
    query.split_whitespace().map(rf_core::entry::fold_case).collect()
}

/// Whether every word of the (folded) query is in the topic's title, steps, limits, words or group.
fn has_words(topic: &Topic, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = rf_core::entry::fold_case(&format!(
        "{} {} {} {} {}",
        crate::i18n::t(topic.title),
        topic.words.join(" "),
        topic.steps.iter().map(|s| crate::i18n::t(*s)).collect::<Vec<_>>().join(" "),
        topic.good.iter().map(|s| crate::i18n::t(*s)).collect::<Vec<_>>().join(" "),
        crate::i18n::t(topic.group),
    ));
    words.iter().all(|w| text.contains(w.as_str()))
}

/// The topics a typed query finds, best first (a title match before a match in the steps): at most `max`. For the command palette.
pub fn matching(query: &str, max: usize) -> Vec<&'static Topic> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let words = words_of(&q);
    let mut by: Vec<(i32, usize)> = TOPICS
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let title = crate::command_palette::score(&crate::i18n::t(t.title).to_lowercase(), &q);
            // (a typed word that is one of the topic's own words, "zip", "nas": as good as a title match)
            let own = t.words.iter().any(|w| w.starts_with(q.as_str())).then_some(90);
            let s = title.max(own).or_else(|| has_words(t, &words).then_some(10))?;
            Some((s, i))
        })
        .collect();
    by.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    by.into_iter().take(max).map(|(_, i)| &TOPICS[i]).collect()
}

/// Starts what the "Do it" button of `topic` stands for.
fn do_it(go: Go, window: &mut Window, cx: &mut App) {
    match go {
        Go::No => {}
        Go::Cmd(id) => {
            if let Some(c) = crate::registry::by_id(id) {
                window.dispatch_action(c.make_action(), cx);
            }
        }
        Go::Page(p) => {
            crate::settings_view::open_page(p, cx);
            window.refresh();
        }
    }
}

/// Whether the button of `topic` can work now: its command is offered (its part of leaf is on, the system has it).
fn can_do(go: Go, cx: &App) -> bool {
    match go {
        Go::No => false,
        Go::Cmd(id) => crate::registry::by_id(id).is_some_and(|c| c.available(cx)),
        Go::Page(_) => true,
    }
}

fn topic_card(p: &Palette, ix: usize, topic: &'static Topic, open: bool, cx: &mut App) -> AnyElement {
    let off = topic.needs.filter(|m| !crate::modules::enabled(cx, *m));
    let head = h_flex()
        .id(("guide-head", ix))
        .min_h(px(44.))
        .px_4()
        .py_2()
        .gap_3()
        .items_center()
        .cursor_pointer()
        .hover(|s| s.bg(p.pressed))
        .child(Icon::new(if open { L::ChevronDown } else { L::ChevronRight }).size(px(14.)).flex_shrink_0().text_color(p.text_secondary))
        .child(div().flex_1().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::t(topic.title)))
        .child(div().flex_shrink_0().text_xs().text_color(p.text_tertiary).child(crate::i18n::t(topic.group)))
        .on_click({
            let id = topic.id;
            move |_, window, cx| {
                let g = ui(cx);
                if !g.open.remove(id) {
                    g.open.insert(id);
                }
                window.refresh();
            }
        });
    let mut card = shell(p).child(head);
    if open {
        let mut body = v_flex().gap_2().px_4().pb_4().pl(px(46.));
        if let Some(m) = off {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(p.text_secondary)
                    .child(format!("{} {}: {}", crate::i18n::t("This needs"), crate::i18n::t(crate::modules::spec(m).name), crate::i18n::t("it is off. Turn it on in Settings > Features."))),
            );
        }
        for (i, s) in topic.steps.iter().enumerate() {
            body = body.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().w(px(18.)).flex_shrink_0().text_sm().text_color(p.accent).child(format!("{}.", i + 1)))
                    .child(div().flex_1().min_w_0().text_sm().child(crate::i18n::t(s))),
            );
        }
        if can_do(topic.go, cx) {
            let (go, label) = (topic.go, if topic.go_label.is_empty() { t!("Do it") } else { topic.go_label });
            body = body.child(h_flex().pt_1().child(button(("guide-do", ix), p, crate::i18n::t(label), move |window, cx| do_it(go, window, cx))));
        }
        if !topic.good.is_empty() {
            body = body.child(div().pt_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Good to know")));
            for g in topic.good {
                body = body.child(h_flex().gap_2().items_start().child(div().text_xs().text_color(p.text_tertiary).child("•")).child(
                    div().flex_1().min_w_0().text_xs().text_color(p.text_secondary).child(crate::i18n::t(g)),
                ));
            }
        }
        card = card.child(div().h(px(1.)).bg(p.divider)).child(div().pt_3().child(body));
    }
    card.into_any_element()
}

/// The page body.
pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let search = ui(cx).search.clone();
    let query = search.as_ref().map(|s| s.read(cx).value().trim().to_string()).unwrap_or_default();
    let words = words_of(&query);
    // a typed query ends what a "?" asked for
    let focus = ui(cx).focus.filter(|_| query.is_empty());
    let mut v: Vec<AnyElement> = vec![div()
        .pb_2()
        .text_sm()
        .text_color(p.text_secondary)
        .child(t!("Short steps for things you may want to do. Type a few words, or click a topic. Press F1 anywhere to come back here."))
        .into_any_element()];
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
    if let Some(id) = focus.and_then(crate::guide_topics::by_id) {
        let ix = TOPICS.iter().position(|t| t.id == id.id).unwrap_or(0);
        v.push(div().h(px(8.)).into_any_element());
        let open = ui(cx).open.contains(id.id);
        v.push(topic_card(p, ix, id, open, cx));
        v.push(h_flex().pt_2().child(button("guide-all", p, t!("All topics"), |window, cx| {
            ui(cx).focus = None;
            window.refresh();
        })).into_any_element());
        return v;
    }
    let mut groups: Vec<&'static str> = Vec::new();
    for t in TOPICS {
        if !groups.contains(&t.group) {
            groups.push(t.group);
        }
    }
    let mut any = false;
    // a search that narrows to a few topics opens them
    let few = !words.is_empty() && matching_count(&words) <= 3;
    for group in groups {
        let rows: Vec<(usize, &'static Topic)> = TOPICS.iter().enumerate().filter(|(_, t)| t.group == group && has_words(t, &words)).collect();
        if rows.is_empty() {
            continue;
        }
        any = true;
        v.push(section(p, crate::i18n::t(group)));
        let mut col = v_flex().w_full().gap_2();
        for (ix, topic) in rows {
            // a query opens the topics it found; otherwise only the ones clicked open
            let open = ui(cx).open.contains(topic.id) || few;
            col = col.child(topic_card(p, ix, topic, open, cx));
        }
        v.push(col.into_any_element());
    }
    if !any {
        v.push(div().py_4().text_sm().text_color(p.text_secondary).child(t!("No topic matches")).into_any_element());
    }
    v
}

/// How many topics have all the words (a search that narrows to a few opens them).
fn matching_count(words: &[String]) -> usize {
    TOPICS.iter().filter(|t| has_words(t, words)).count()
}

/// A small "?" button that opens the guide on `topic`: for the header of a page that has one.
pub fn help_button(p: &Palette, id: impl Into<ElementId>, topic: &'static str) -> AnyElement {
    let tip: SharedString = crate::i18n::t("How do I…? Opens the guide").into();
    div()
        .id(id)
        .size(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(p.border)
        .text_color(p.text_secondary)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover))
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        .child(Icon::new(L::CircleQuestionMark).size(px(14.)))
        .on_click(move |_, window, cx| {
            set_focus(topic, cx);
            window.dispatch_action(Box::new(crate::actions::ShowGuide), cx);
        })
        .into_any_element()
}

/// A section heading of a Settings page with the "?" of `topic` at its right.
pub fn section_with_help(p: &Palette, title: &str, id: &'static str, topic: &'static str) -> AnyElement {
    h_flex().w_full().items_center().justify_between().child(section(p, title)).child(div().pt_4().child(help_button(p, id, topic))).into_any_element()
}
