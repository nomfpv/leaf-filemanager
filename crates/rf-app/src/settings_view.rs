//! Settings page (Location::Settings), laid out like Files: a navigation column on the left,
//! one page of cards on the right. Every card is one setting: icon, title, optional
//! description, and its control (a switch, a drop-down that opens our Acrylic menu, buttons) —
//! or an expander with child settings.

use std::collections::HashSet;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::platform::gpui::hybrid;

use crate::fluent_menu::{self, Item, Menu, MenuIcon};
use crate::select_box::{Choice, select_box};
use crate::settings::{
    self, AnimPref, BackdropPref, ClickPref, DatePref, DefaultView, DeletePref, IdePref, RendererPref, Settings, StartupPref, ThemePref,
    Visibility,
};
use crate::t;
use crate::look::config::LookSettings;
use crate::theme::{self, Palette};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Appearance,
    Layout,
    Files,
    FileTypes,
    Actions,
    YourActions,
    Tags,
    Hosts,
    Vpn,
    Look,
    Nearby,
    Search,
    Advanced,
    Speed,
    Organizer,
    Numbers,
    Features,
    WhatsNew,
    About,
}

const PAGES: [(Page, L, &str); 20] = [
    (Page::General, L::Settings, "General"),
    (Page::Appearance, L::Palette, "Appearance"),
    (Page::Layout, L::LayoutGrid, "Layout"),
    (Page::Files, L::Folder, "Files and folders"),
    (Page::FileTypes, L::FileType, "File types"),
    (Page::Actions, L::Keyboard, "Actions"),
    (Page::YourActions, L::Zap, "Your actions"),
    (Page::Tags, L::Tag, "Tags"),
    (Page::Hosts, L::Server, "Hosts"),
    (Page::Vpn, L::ShieldCheck, "VPN"),
    (Page::Look, L::Eye, "Preview"),
    (Page::Search, L::Search, "HyperSearch"),
    (Page::Nearby, L::Send, "Nearby"),
    (Page::Advanced, L::Wrench, "Advanced"),
    (Page::Speed, L::Gauge, "Speed test"),
    (Page::Organizer, L::WandSparkles, "File organizer"),
    (Page::Numbers, L::Activity, "Your numbers"),
    (Page::Features, L::Sparkles, "What leaf can do"),
    (Page::WhatsNew, L::Bell, "What's new"),
    (Page::About, L::Info, "About"),
];

/// The pages that are experimental: marked with a small "Deneysel" pill in the page list and "(deneysel)" in their title. The mark is
/// NOT part of the page name in [`PAGES`], so the search and the translations still know the pages as "Dock" and "Masaüstü".
fn experimental(page: Page) -> bool {
    matches!(page, Page::Nearby)
}

/// The heading of a page: its name, and "(deneysel)" after it for an experimental page.
fn page_title(page: Page, name: &str) -> String {
    if experimental(page) { format!("{name} ({})", t!("experimental")) } else { name.to_string() }
}

/// The quiet pill next to an experimental page's name in the page list (the same for every such page).
fn experimental_pill(p: &Palette) -> Div {
    div().px(px(6.)).py(px(1.)).rounded_full().border_1().border_color(p.border).text_size(px(10.)).text_color(p.text_secondary).child(t!("Experimental"))
}

/// Which page is shown and which expanders are open (kept while the app runs).
struct Ui {
    page: Page,
    open: HashSet<&'static str>,
    /// The search box above the pages (made at the first draw: it needs a window).
    search: Option<Entity<gpui_kit::component::input::InputState>>,
    /// One focus handle per HyperGlass slider (made at the first draw, kept so the arrow keys have something to be focused on).
    hg_focus: Vec<FocusHandle>,
    /// How wide the Settings pane was at its last paint, logical px (0: not painted yet). The list of pages is drawn as a strip of
    /// icons when the pane is narrow ([`nav_is_strip`]).
    pane_w: f32,
}
impl Global for Ui {}

/// The pane's width under which the list of pages is a strip of icons, and the widths of the list in its two forms.
const NAV_STRIP_BELOW: f32 = 640.0;
const NAV_W: f32 = 232.0;
const NAV_STRIP_W: f32 = 60.0;

/// Whether the list of pages is drawn as a strip of icons: the pane is narrow, and the full list (232 px) would leave the settings
/// themselves a column too thin to read (the user's pictures: the app's sidebar and this list took 464 px of a 736 px window, and the
/// settings had 200). A pane that has not been measured yet gets the full list.
pub(crate) fn nav_is_strip(pane_w: f32) -> bool {
    pane_w > 0.0 && pane_w < NAV_STRIP_BELOW
}

fn ui(cx: &mut App) -> &mut Ui {
    if !cx.has_global::<Ui>() {
        cx.set_global(Ui { page: Page::General, open: HashSet::new(), search: None, hg_focus: Vec::new(), pane_w: 0.0 });
    }
    cx.global_mut::<Ui>()
}

/// A setting found while every page is built for the search: its texts and where it is.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    title: String,
    desc: Option<String>,
    page: Page,
    /// The expander it is inside (opened when the result is picked).
    expander: Option<&'static str>,
}

thread_local! {
    /// Some while the pages are being built to list their settings (see [`collect`]).
    static COLLECT: std::cell::RefCell<Option<Vec<Found>>> = const { std::cell::RefCell::new(None) };
    static COLLECT_AT: std::cell::Cell<(Page, Option<&'static str>)> = const { std::cell::Cell::new((Page::General, None)) };
}

thread_local! {
    /// The setting a search result was picked for (issue 126): its row is marked, and the page is scrolled to it once.
    static FIND: std::cell::RefCell<Option<FindTarget>> = const { std::cell::RefCell::new(None) };
    /// The settings page's scroll, for that one scroll (set by every [`render`]).
    static PAGE_SCROLL: std::cell::RefCell<Option<ScrollHandle>> = const { std::cell::RefCell::new(None) };
}

struct FindTarget {
    title: String,
    scrolled: bool,
}

/// How long the picked setting's row stays marked.
const FIND_MARK_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

/// A setting's row as drawn: when a search result was picked for it, with a soft accent wash, and the page scrolled (at its first paint)
/// so that the row stands near the top.
fn found_mark(title: &str, p: &Palette, row: Div) -> Div {
    if collecting() || !FIND.with(|f| f.borrow().as_ref().is_some_and(|t| t.title == title)) {
        return row;
    }
    row.relative().rounded(theme::RADIUS).bg(p.accent.opacity(0.18)).child(
        canvas(
            |_, _, _| (),
            |bounds, _, window, _| {
                let first = FIND.with(|f| f.borrow_mut().as_mut().is_some_and(|t| !std::mem::replace(&mut t.scrolled, true)));
                if let (true, Some(scroll)) = (first, PAGE_SCROLL.with(|s| s.borrow().clone())) {
                    let (view, offset) = (scroll.bounds(), scroll.offset());
                    let y = offset.y - (bounds.top() - view.top() - px(24.));
                    scroll.set_offset(point(offset.x, y.min(px(0.))));
                    window.refresh();
                }
            },
        )
        .absolute()
        .size_full(),
    )
}

pub(crate) fn collecting() -> bool {
    COLLECT.with(|c| c.borrow().is_some())
}

/// Notes a setting's texts (the card and row builders call it; a no-op outside a search).
fn note_setting(title: &str, desc: Option<&str>) {
    COLLECT.with(|c| {
        if let Some(list) = c.borrow_mut().as_mut() {
            let (page, expander) = COLLECT_AT.with(|a| a.get());
            list.push(Found { title: title.to_string(), desc: desc.map(str::to_string), page, expander });
        }
    });
}

/// The settings whose title or description has every word of `query` (case-insensitive).
fn matching(all: Vec<Found>, query: &str) -> Vec<Found> {
    let words: Vec<String> = query.split_whitespace().map(rf_core::entry::fold_case).collect();
    let mut seen = HashSet::new();
    all.into_iter()
        .filter(|f| {
            let text = rf_core::entry::fold_case(&format!("{} {}", f.title, f.desc.as_deref().unwrap_or("")));
            !words.is_empty() && words.iter().all(|w| text.contains(w.as_str()))
        })
        // (a setting drawn twice on a page is one result)
        .filter(|f| seen.insert((f.title.clone(), f.page as u8)))
        .collect()
}

/// Every setting of every page: the pages are built once with the builders noting their texts.
fn collect(p: &Palette, s: &Settings, cx: &mut App) -> Vec<Found> {
    COLLECT.with(|c| *c.borrow_mut() = Some(Vec::new()));
    for (page, _, _) in PAGES {
        COLLECT_AT.with(|a| a.set((page, None)));
        let _ = build_page(page, p, s, cx);
    }
    COLLECT.with(|c| c.borrow_mut().take()).unwrap_or_default()
}

fn build_page(page: Page, p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    match page {
        Page::General => general(p, s, cx),
        Page::Appearance => appearance(p, s, cx),
        Page::Layout => layout(p, s, cx),
        Page::Files => files(p, s, cx),
        Page::FileTypes => crate::open_rules_page::page(p, s),
        Page::Actions => actions(p, s, cx),
        Page::YourActions => crate::custom_actions_page::page(p, cx),
        Page::Tags => tags(p, cx),
        Page::Hosts => crate::hosts_ui::page(p, cx),
        Page::Vpn => crate::vpn_page::page(p, s, cx),
        Page::Look => look(p, s, cx),
        Page::Nearby => crate::nearby::page(p, s, cx),
        Page::Search => quick_search_cards(p, s, cx),
        // the developer tools are a part of Gelişmiş (they had a page of their own)
        Page::Advanced => {
            let mut v = advanced(p, s, cx);
            v.push(section(p, t!("Developer tools")));
            v.extend(dev(p, s, cx));
            v
        }
        Page::Speed => crate::bench_view::page(p, cx),
        Page::Organizer => crate::organizer::page::page(p, cx),
        Page::Numbers => crate::numbers_page::page(p, s, cx),
        Page::Features => crate::features_page::page(p, cx),
        Page::WhatsNew => crate::whats_new_page::page(p, cx),
        Page::About => about(p, cx),
    }
}

pub fn open_page(page: Page, cx: &mut App) {
    ui(cx).page = page;
}

// ---------------------------------------------------------------- controls

/// A Windows 11 switch with its state word ("Açık"/"Kapalı") on the left. Over glass the track and the thumb are the liquid ones
/// (`liquid::switch_track`); the word and the click are the same.
pub(crate) fn switch(id: impl Into<ElementId>, p: &Palette, on: bool, toggle: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    let id: ElementId = id.into();
    let track: AnyElement = if p.liquid {
        crate::liquid::switch_track(id, p, on, false, toggle)
    } else {
        let knob = if on { p.accent_text } else { p.text_secondary };
        h_flex()
            .id(id)
            .w(px(40.))
            .h(px(20.))
            .px(px(3.))
            .items_center()
            .rounded_full()
            .border_1()
            // Tab reaches the switch, Enter / Space flips it
            .tab_index(0)
            .when(on, |d| d.bg(p.accent).border_color(p.accent).justify_end())
            .when(!on, |d| d.border_color(p.text_secondary).hover(|s| s.bg(p.hover)))
            .child(div().size(px(12.)).rounded_full().bg(knob))
            .on_click(move |_, window, cx| toggle(window, cx))
            .into_any_element()
    };
    h_flex()
        .gap_3()
        .items_center()
        .child(div().text_sm().text_color(p.text_secondary).child(if on { t!("On") } else { t!("Off") }))
        .child(track)
        .into_any_element()
}

/// Switch bound to a boolean setting.
pub(crate) fn sw(id: &'static str, p: &Palette, on: bool, set: fn(&mut Settings, bool)) -> AnyElement {
    switch(id, p, on, move |_, cx| settings::update(cx, |s| set(s, !on)))
}

/// A button showing the current value; clicking opens the choices in our menu.
pub(crate) fn dropdown<T: Copy + PartialEq + 'static>(id: &'static str, p: &Palette, current: T, options: Vec<(T, String)>, set: fn(&mut Settings, T)) -> AnyElement {
    let label = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone()).unwrap_or_default();
    select_box(id, p, label, move |_| {
        options.iter().map(|(v, l)| {
            let v = *v;
            Choice::new(l.clone(), v == current, move |_, cx| settings::update(cx, |s| set(s, v)))
        }).collect()
    })
}

pub(crate) fn button(id: impl Into<ElementId>, p: &Palette, label: impl Into<SharedString>, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .id(id)
        .h(px(32.))
        .px_4()
        .flex()
        .items_center()
        .rounded(theme::RADIUS_SM)
        .bg(p.hover)
        .border_1()
        .border_color(p.border)
        .text_sm()
        .cursor_pointer()
        // Tab reaches the button, Enter / Space presses it
        .tab_index(0)
        .hover(|s| s.bg(p.pressed))
        .active(|s| s.opacity(0.8))
        .child(label.into())
        .on_click(move |_, window, cx| run(window, cx))
        .into_any_element()
}

// ---------------------------------------------------------------- layout pieces

/// The narrowest a setting's words (its title and the line under it) are drawn, logical px.
///
/// WHY (the user, with two pictures of the General page in a narrow window: "uygulamada çok yerde responsive problemi var", "şuraya
/// bak. böyle olur mu hiç?"). A setting's row was: picture, words, control, side by side, and the words were what gave way: the
/// control kept its width (a list is never narrower than 180 px), so in a narrow window the words were pressed into a column one
/// letter wide and a title stood letter under letter. Now the words never go under this width. When the row has no room for the words
/// and the control side by side, the CONTROL goes to a line of its own under them ([`beside`]), as a form does on a narrow page.
pub(crate) const WORDS_MIN: f32 = 120.0;

/// A setting's control, placed after its words: it keeps its own width, stays at the right end, and wraps to a line of its own when
/// the row is too narrow for both (the row is a wrapping one). It is never wider than the row.
pub(crate) fn beside(control: AnyElement) -> Div {
    h_flex().flex_shrink_0().max_w_full().ml_auto().child(control)
}

pub(crate) fn card_body(p: &Palette, icon: L, title: SharedString, desc: Option<SharedString>, control: Option<AnyElement>) -> Div {
    card_body_with(p, Icon::new(icon).size(px(18.)).flex_shrink_0().text_color(p.text).into_any_element(), title, desc, control)
}

/// `card_body` with any picture in place of the line icon.
pub(crate) fn card_body_with(p: &Palette, icon: AnyElement, title: SharedString, desc: Option<SharedString>, control: Option<AnyElement>) -> Div {
    note_setting(&title, desc.as_deref());
    let marked = title.clone();
    let row = h_flex()
        .min_h(px(64.))
        .px_4()
        .py_3()
        .gap_x_4()
        .gap_y_2()
        .items_center()
        .flex_wrap()
        .child(
            // (the picture and the words stay together: it is the control that wraps)
            h_flex()
                .flex_1()
                .min_w(px(WORDS_MIN + 34.))
                .gap_4()
                .items_center()
                .child(icon)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().text_sm().child(title))
                        .children(desc.map(|d| div().text_xs().text_color(p.text_secondary).child(d))),
                ),
        )
        .children(control.map(beside));
    found_mark(&marked, p, row)
}

pub(crate) fn shell(p: &Palette) -> Div {
    v_flex().w_full().rounded(theme::RADIUS).bg(p.hover).border_1().border_color(p.border)
}

/// One setting row.
pub(crate) fn card(p: &Palette, icon: L, title: &str, desc: Option<&str>, control: AnyElement) -> AnyElement {
    shell(p)
        .child(card_body(p, icon, title.to_string().into(), desc.map(|d| d.to_string().into()), Some(control)))
        .into_any_element()
}

/// A card that opens to show `children` (state kept per `key`).
fn expander(key: &'static str, p: &Palette, icon: L, title: &str, desc: Option<&str>, control: Option<AnyElement>, children: impl FnOnce() -> Vec<AnyElement>, cx: &mut App) -> AnyElement {
    let open = ui(cx).open.contains(key);
    let chevron = div()
        .id(SharedString::from(format!("exp-chev-{key}")))
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .hover(|s| s.bg(p.pressed))
        .child(Icon::new(if open { L::ChevronUp } else { L::ChevronDown }).size(px(14.)))
        .on_click(move |_, window, cx| {
            let u = ui(cx);
            if !u.open.remove(key) {
                u.open.insert(key);
            }
            window.refresh();
        });
    let header_control = h_flex().gap_2().items_center().children(control).child(chevron).into_any_element();
    let body = card_body(p, icon, title.to_string().into(), desc.map(|d| d.to_string().into()), Some(header_control));
    // (a search looks inside closed expanders too)
    let kids = if open || collecting() {
        let at = COLLECT_AT.with(|a| a.get());
        COLLECT_AT.with(|a| a.set((at.0, Some(key))));
        let kids = children();
        COLLECT_AT.with(|a| a.set(at));
        Some(kids)
    } else {
        None
    };
    shell(p).child(body).when_some(kids.filter(|_| open), |d, kids| d.child(div().h(px(1.)).bg(p.divider)).child(v_flex().py_1().children(kids))).into_any_element()
}

/// A child row inside an expander (no icon, indented like Files).
fn sub(p: &Palette, title: &str, desc: Option<&str>, control: AnyElement) -> AnyElement {
    note_setting(title, desc);
    let row = h_flex()
        .min_h(px(48.))
        .pl(px(56.))
        .pr_4()
        .py_2()
        .gap_x_4()
        .gap_y_2()
        .items_center()
        .flex_wrap()
        .child(
            v_flex()
                .flex_1()
                .min_w(px(WORDS_MIN))
                .child(div().text_sm().child(title.to_string()))
                .children(desc.map(|d| div().text_xs().text_color(p.text_secondary).child(d.to_string()))),
        )
        .child(beside(control));
    found_mark(title, p, row).into_any_element()
}

pub(crate) fn section(p: &Palette, title: &str) -> AnyElement {
    div().pt_5().pb_1().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(title.to_string()).into_any_element()
}

fn link_row(p: &Palette, id: &'static str, icon: L, title: &str, desc: Option<&str>, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    shell(p)
        .id(id)
        .hover(|s| s.bg(p.pressed))
        .on_click(move |_, window, cx| run(window, cx))
        .child(card_body(
            p,
            icon,
            title.to_string().into(),
            desc.map(|d| d.to_string().into()),
            Some(Icon::new(L::SquareArrowOutUpRight).size(px(14.)).text_color(p.text_secondary).into_any_element()),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------- page

/// `lens_owner`: this page owns the window's settings lens (see `FolderView::lens_owner`); only then is the selected entry glass.
pub fn render(p: &Palette, window: &mut Window, cx: &mut App, lens_owner: bool, scroll: &ScrollHandle) -> AnyElement {
    use gpui_kit::component::input::{Input, InputState};
    let page = ui(cx).page;
    PAGE_SCROLL.with(|h| *h.borrow_mut() = Some(scroll.clone()));
    let s = cx.global::<Settings>().clone();
    crate::features_page::ensure_search(window, cx);
    let search = match ui(cx).search.clone() {
        Some(e) => e,
        None => {
            let e = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search settings")));
            // typing redraws the page (the box is not part of a view of its own)
            cx.observe(&e, |_, cx| cx.refresh_windows()).detach();
            ui(cx).search = Some(e.clone());
            e
        }
    };
    let query = search.read(cx).value().trim().to_string();
    // In a narrow pane the list of pages is a strip of icons (each with its name as a tip), so that the settings keep a readable
    // width; the title and the search box are the full list's.
    let strip = nav_is_strip(ui(cx).pane_w);
    // (scrolls: the 13 entries need more than the 420 px the window may be as low as)
    let mut nav = v_flex().id("settings-nav").w(px(if strip { NAV_STRIP_W } else { NAV_W })).flex_shrink_0().h_full().overflow_y_scroll().px_3().pt_5().gap_0p5();
    if !strip {
        nav = nav.child(div().px_2().pb_3().text_xl().font_weight(FontWeight::SEMIBOLD).child(t!("Settings"))).child(
            h_flex()
                .h(px(32.))
                .mb_2()
                .px_2()
                .gap_1()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .bg(p.hover)
                .border_1()
                .border_color(p.border)
                .child(Icon::new(L::Search).size(px(14.)).flex_shrink_0().text_color(p.text_secondary))
                .child(Input::new(&search).appearance(false).cleanable(true)),
        );
    }
    // Over glass the selected entry is a lens the engine draws behind the scene (slot 1): no fill and no accent bar, a probe that
    // reports the row's place on every paint instead. The page is only built for the active tab; the workspace names the one pane that
    // owns the lens and takes the lens out when no pane does.
    let lens = lens_owner && crate::glass::shown(crate::look::window::hwnd_of(window));
    for (i, (pg, icon, label)) in PAGES.iter().copied().enumerate() {
        let on = pg == page;
        nav = nav.child(
            h_flex()
                .id(("set-nav", i))
                .h(px(36.))
                .when(!strip, |d| d.px_3())
                .when(strip, |d| {
                    let tip: SharedString = crate::i18n::t(label).into();
                    d.justify_center().tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                })
                .gap_3()
                .items_center()
                .relative()
                .rounded(theme::RADIUS_SM)
                .text_sm()
                .when(on && !lens, |d| d.bg(p.selection).child(theme::selection_pill(p)))
                .when(on && lens && crate::glass::lens_phase(crate::look::window::hwnd_of(window), crate::glass::SLOT_SETTINGS) != crate::glass::LensPhase::Rest, |d| {
                    d.child(crate::glass::lens_stand_in(p.dark, 36.))
                })
                .when(on && lens, |d| d.child(crate::glass::lens_probe(crate::glass::SLOT_SETTINGS, crate::glass::key_of(&i))))
                .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                .child(Icon::new(icon).size(px(16.)).text_color(if on { p.accent } else { p.text_secondary }))
                // a small "new" dot until the page has been opened once after an update
                .when(pg == Page::WhatsNew && !on && crate::whats_new_page::unseen(&s), |d| d.child(div().absolute().right(px(strip.then_some(14.).unwrap_or(12.))).top(px(14.)).size(px(7.)).rounded_full().bg(p.accent)))
                .when(!strip, |d| d.child(h_flex().gap_2().items_center().child(crate::i18n::t(label)).when(experimental(pg), |d| d.child(experimental_pill(p)))))
                .on_click({
                    let search = search.clone();
                    move |_, window, cx| {
                        open_page(pg, cx);
                        // (a page picked by hand ends the search)
                        search.update(cx, |s, cx| s.set_value("", window, cx));
                        window.refresh();
                    }
                }),
        );
    }
    // (Nearby's page on screen shows this PC to everyone while it is: `nearby::page_seen`)
    if query.is_empty() && page == Page::Nearby {
        crate::nearby::page_seen(window.window_handle(), cx);
    }
    if query.is_empty() && page == Page::WhatsNew {
        crate::whats_new_page::seen(&s, cx);
    }
    let (title, body): (String, Vec<AnyElement>) = if query.is_empty() {
        (PAGES.iter().find(|x| x.0 == page).map(|x| page_title(x.0, crate::i18n::t(x.2))).unwrap_or_default(), build_page(page, p, &s, cx))
    } else {
        let found = matching(collect(p, &s, cx), &query);
        let mut rows: Vec<AnyElement> = Vec::new();
        if found.is_empty() {
            rows.push(div().py_4().text_sm().text_color(p.text_secondary).child(t!("No setting matches")).into_any_element());
        }
        for (i, f) in found.into_iter().enumerate() {
            let (pg, key, target) = (f.page, f.expander, f.title.clone());
            let page_label = PAGES.iter().find(|x| x.0 == pg).map(|x| crate::i18n::t(x.2)).unwrap_or_default();
            let search = search.clone();
            rows.push(
                shell(p)
                    .id(("set-found", i))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.pressed))
                    .on_click(move |_, window, cx| {
                        open_page(pg, cx);
                        if let Some(k) = key {
                            ui(cx).open.insert(k);
                        }
                        // the page opens scrolled to the setting, which is marked for a moment (issue 126)
                        FIND.with(|f| *f.borrow_mut() = Some(FindTarget { title: target.clone(), scrolled: false }));
                        window
                            .spawn(cx, async move |cx| {
                                cx.background_executor().timer(FIND_MARK_FOR).await;
                                let _ = cx.update(|window, _| {
                                    FIND.with(|f| *f.borrow_mut() = None);
                                    window.refresh();
                                });
                            })
                            .detach();
                        search.update(cx, |s, cx| s.set_value("", window, cx));
                        window.refresh();
                    })
                    .child(
                        h_flex()
                            .min_h(px(52.))
                            .px_4()
                            .py_2()
                            .gap_4()
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(div().text_sm().child(f.title))
                                    .children(f.desc.map(|d| div().text_xs().text_color(p.text_secondary).child(d))),
                            )
                            .child(div().flex_shrink_0().text_xs().text_color(p.text_secondary).child(page_label))
                            .child(Icon::new(L::ChevronRight).size(px(14.)).flex_shrink_0().text_color(p.text_secondary)),
                    )
                    .into_any_element(),
            );
        }
        (t!("Search results").to_string(), rows)
    };
    h_flex()
        .size_full()
        .relative()
        // (the pane's own width, measured at paint: when it crosses the limit the pane is drawn again with the other form of the list)
        .child(
            canvas(
                move |bounds, _, cx| {
                    let w = f32::from(bounds.size.width);
                    let u = ui(cx);
                    if (u.pane_w - w).abs() > 0.5 {
                        let was = nav_is_strip(u.pane_w);
                        u.pane_w = w;
                        if nav_is_strip(w) != was {
                            // (after this paint: a window that is being drawn does not take a request to be drawn again)
                            cx.defer(|cx| cx.refresh_windows());
                        }
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .child(nav)
        .child(
            // the page scrolls with a scrollbar over its right edge (issue 86: on Windows 10 nothing showed there was more below)
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    div().id("settings-page").size_full().overflow_y_scroll().track_scroll(scroll).child(
                        v_flex()
                            .w_full()
                            .max_w(px(980.))
                            .px_6()
                            .pt_5()
                            .pb_8()
                            .gap_2()
                            .child(div().pb_2().text_xl().font_weight(FontWeight::SEMIBOLD).child(title))
                            .children(body),
                    ),
                )
                .child(
                    gpui_kit::component::scroll::Scrollbar::vertical(scroll)
                        .id("settings-scrollbar")
                        .mode(crate::folder_view::scrollbar_mode()),
                ),
        )
        .into_any_element()
}

/// Quick search: on/off, its shortcut (recorded like the keyboard shortcuts), web engine,
/// bookmarks.
fn quick_search_cards(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    let capture = crate::quick_search::CAPTURE_ACTION;
    let waiting = crate::keymap_editor::capturing(cx).as_deref() == Some(capture);
    let shown: SharedString = if waiting { t!("Press keys…").into() } else { crate::keymap_editor::pretty(&s.quick_search_keys).into() };
    let keys_desc = match crate::quick_search::problem() {
        _ if !cfg!(any(windows, target_os = "linux")) => t!("A shortcut from anywhere is not available on this system"),
        Some(leaf_sys::hotkey::HotkeyError::Unavailable) if s.quick_search => t!("This desktop offers no global shortcuts to apps (the GlobalShortcuts portal is missing)"),
        Some(leaf_sys::hotkey::HotkeyError::Taken) if s.quick_search => t!("Another app uses this shortcut (a launcher, an assistant…): choose another one"),
        Some(leaf_sys::hotkey::HotkeyError::Invalid) if s.quick_search => t!("This shortcut can't be used: choose a key with Ctrl, Alt, Shift or Win"),
        _ => t!("Click and press the new keys to change it"),
    };
    let warn = s.quick_search && crate::quick_search::problem().is_some();
    let pill = div()
        .id("qs-keys")
        .px_3()
        .py_1()
        .rounded(px(4.))
        .border_1()
        .border_color(if warn { p.danger } else { p.border })
        .text_sm()
        .cursor_pointer()
        .text_color(if waiting { p.accent } else { p.text })
        .hover(|d| d.bg(p.pressed))
        .when(waiting, |d| d.bg(p.selection))
        .child(shown)
        .on_click(move |_, window, cx| {
            crate::keymap_editor::start(capture.to_string(), cx);
            window.refresh();
        })
        .into_any_element();
    vec![
        card(
            p,
            L::Search,
            "HyperSearch",
            Some(if s.run_in_background {
                t!("A shortcut from anywhere: apps, files, folders, bookmarks and the web")
            } else {
                t!("A shortcut from anywhere: apps, files, folders, bookmarks and the web. To use it with no window open, turn on running in the background under Advanced")
            }),
            sw("quick-search", p, s.quick_search, |s, v| s.quick_search = v),
        ),
        card(p, L::Keyboard, t!("Shortcut"), Some(keys_desc), pill),
        card(
            p,
            L::Globe,
            t!("Web search"),
            None,
            dropdown(
                "web-search",
                p,
                s.web_search,
                vec![
                    (crate::settings::WebSearch::Google, "Google".into()),
                    (crate::settings::WebSearch::Bing, "Bing".into()),
                    (crate::settings::WebSearch::DuckDuckGo, "DuckDuckGo".into()),
                    (crate::settings::WebSearch::Yandex, "Yandex".into()),
                ],
                |s, v| s.web_search = v,
            ),
        ),
        card(
            p,
            L::Bookmark,
            t!("Search Chrome and Edge bookmarks too"),
            Some(t!("Bookmarks are read on this PC and sent nowhere")),
            sw("qs-bookmarks", p, s.quick_search_bookmarks, |s, v| s.quick_search_bookmarks = v),
        ),
        card(
            p,
            L::Clock,
            t!("Show recent items when empty"),
            Some(t!("When off, HyperSearch stays empty until something is typed")),
            sw("qs-recent", p, s.quick_search_recent, |s, v| s.quick_search_recent = v),
        ),
    ]
}

fn general(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    let mut v = vec![
        card(
            p,
            L::CalendarClock,
            t!("Date format"),
            Some(t!("Application: \"Today, 10:05\", \"Yesterday, 18:02\", older ones \"18 Nov 2026\"; System: always the system's date and time format; Universal: 2026-11-05 13:45")),
            dropdown(
                "datefmt",
                p,
                s.date_format,
                vec![
                    (DatePref::Application, t!("Application").into()),
                    (DatePref::System, t!("System").into()),
                    (DatePref::Universal, t!("Universal").into()),
                ],
                |s, v| s.date_format = v,
            ),
        ),
    ];
    let startup_dd = dropdown(
        "startup",
        p,
        s.startup,
        vec![
            (StartupPref::Continue, t!("Continue where you left off").into()),
            (StartupPref::NewTab, t!("Open a new tab").into()),
            (StartupPref::Pages, t!("Open specific pages").into()),
        ],
        |s, v| s.startup = v,
    );
    let pages = s.startup_pages.clone();
    let pp = *p;
    v.push(expander(
        "startup",
        p,
        L::Power,
        t!("Startup settings"),
        None,
        Some(startup_dd),
        move || {
            let mut rows: Vec<AnyElement> = pages
                .iter()
                .enumerate()
                .map(|(i, page)| {
                    let shown = if page.is_empty() { t!("Home").to_string() } else { page.clone() };
                    sub(
                        &pp,
                        &shown,
                        None,
                        button(("sp-del", i), &pp, t!("Remove"), move |_, cx| settings::update(cx, |s| {
                            if i < s.startup_pages.len() {
                                s.startup_pages.remove(i);
                            }
                        })),
                    )
                })
                .collect();
            rows.push(sub(
                &pp,
                t!("Specific pages"),
                Some(t!("Opens when \"Open specific pages\" is selected.")),
                button("sp-use", &pp, t!("Use currently open tabs"), |_, cx| {
                    settings::update(cx, |s| {
                        s.startup_pages = s.session_tabs.clone();
                        s.startup = StartupPref::Pages;
                    })
                }),
            ));
            rows
        },
        cx,
    ));
    v.push(card(
        p,
        L::Plus,
        t!("A new tab opens"),
        Some(t!("Ctrl+T, the + button and the tab menu. Duplicate tab (Ctrl+Shift+K) always opens the same folder.")),
        dropdown(
            "new-tab-opens",
            p,
            s.new_tab_opens,
            vec![(crate::settings::NewTabOpens::Home, t!("Home").to_string()), (crate::settings::NewTabOpens::Current, t!("The current folder").to_string())],
            |s, v| s.new_tab_opens = v,
        ),
    ));
    v.push(card(p, L::SquarePlus, t!("Always switch to the newly opened tab"), None, sw("focus-tab", p, s.focus_new_tab, |s, v| s.focus_new_tab = v)));
    v.push(card(
        p,
        L::AppWindow,
        t!("Open folders from other programs in a new window"),
        Some(t!("When the desktop or another program opens a folder, it gets a new window instead of a new tab in the open window.")),
        sw("launch-newwin", p, s.launch_in_new_window, |s, v| s.launch_in_new_window = v),
    ));
    v.push(card(
        p,
        L::Save,
        t!("Open saved sessions in this window"),
        Some(t!("A session opened from the tab menu replaces this window's tabs instead of opening a new window (the closed tabs come back with Ctrl+Shift+T).")),
        sw("session-here", p, s.sessions_in_this_window, |s, v| s.sessions_in_this_window = v),
    ));

    v.push(section(p, t!("Dual pane")));
    v.push(card(p, L::Columns2, t!("Open new tabs in dual pane"), None, sw("dual-new", p, s.dual_pane_new_tabs, |s, v| s.dual_pane_new_tabs = v)));
    v.push(card(
        p,
        L::SquareSplitHorizontal,
        t!("Dual pane orientation"),
        None,
        dropdown(
            "dual-dir",
            p,
            s.dual_pane_vertical,
            vec![(true, t!("Side by side").into()), (false, t!("Stacked").into())],
            |s, v| s.dual_pane_vertical = v,
        ),
    ));

    v.push(section(p, t!("Context menu")));
    let m = s.menu.clone();
    let pp = *p;
    v.push(expander(
        "ctx",
        p,
        L::SquareMenu,
        t!("Context menu options"),
        Some(t!("Which commands appear in the menu")),
        None,
        move || {
            // (one switch for each name the rows of the menu give, `menu_model::switches`: a new row with a switch shows up here)
            crate::menu_model::switches()
                .into_iter()
                .enumerate()
                .map(|(i, sw)| {
                    let on = m.get(sw.key);
                    sub(&pp, crate::i18n::t(sw.label), None, switch(("ctx-sw", i), &pp, on, move |_, cx| settings::update(cx, |s| s.menu.set(sw.key, !on))))
                })
                .collect()
        },
        cx,
    ));
    v.push(card(
        p,
        L::Ellipsis,
        t!("Move shell extensions to a submenu"),
        Some(t!("When off, items from other apps are listed at the end of the menu.")),
        sw("shell-sub", p, s.menu.shell_submenu, |s, v| s.menu.shell_submenu = v),
    ));

    v.push(section(p, t!("Scrolling")));
    v.push(card(p, L::ChevronsUpDown, t!("Smooth scrolling"), None, sw("smooth", p, s.smooth_scroll, |s, v| s.smooth_scroll = v)));
    v
}

/// Background tint presets (Files' "Background color").
const TINTS: [(&str, u32); 15] = [
    ("Goldenrod", 0xC19C00),
    ("Orange", 0xCA5010),
    ("Brick red", 0xA4262C),
    ("Red", 0xD13438),
    ("Rose", 0xE3008C),
    ("Purple", 0x881798),
    ("Iris", 0x8764B8),
    ("Blue", 0x0078D4),
    ("Sea", 0x038387),
    ("Mint", 0x00B294),
    ("Green", 0x107C10),
    ("Camouflage", 0x486860),
    ("Storm", 0x4C4A48),
    ("Blue gray", 0x515C6B),
    ("Gray", 0x7A7574),
];

/// The background material card. While Glass is in use (chosen, or forced for this run) a note under it says what the material
/// costs; if the engine gave up on a window, another note says why that window is on Mica.
/// The letter of a drive's root ("V:" or "V:\"), in capitals; `None` for any other path.
pub(crate) fn drive_letter(path: &std::path::Path) -> Option<char> {
    let s = path.to_string_lossy();
    let rest = s.strip_suffix(['\\', '/']).unwrap_or(&s);
    let mut c = rest.chars();
    match (c.next(), c.next(), c.next()) {
        (Some(l), Some(':'), None) if l.is_ascii_alphabetic() => Some(l.to_ascii_uppercase()),
        _ => None,
    }
}

/// Hides a drive from the lists of places, or shows it again: the setting's letters, each once, in order.
pub(crate) fn set_drive_hidden(s: &mut Settings, letter: char, hide: bool) {
    let mut mask = leaf_sys::drives::mask_of_letters(&s.hidden_drives);
    if let Some(bit) = leaf_sys::drives::letter_bit(letter) {
        if hide { mask |= bit } else { mask &= !bit }
    }
    s.hidden_drives = leaf_sys::drives::letters_of_mask(mask).into_iter().map(String::from).collect();
}

/// Which drives are listed: a button per drive letter that is there now (and per letter the setting names, there or not), pressed
/// in while the drive is hidden.
fn hidden_drives_card(p: &Palette, s: &Settings) -> AnyElement {
    let hidden = leaf_sys::drives::mask_of_letters(&s.hidden_drives);
    let by_explorer = leaf_sys::drives::policy_hidden();
    let present = leaf_sys::drives::drive_mask();
    let mut row = h_flex().flex_wrap().gap_2().items_center().justify_end();
    for (i, letter) in leaf_sys::drives::letters_of_mask(present | hidden).into_iter().enumerate() {
        let bit = leaf_sys::drives::letter_bit(letter).unwrap_or(0);
        let (off, forced) = (hidden & bit != 0, by_explorer & bit != 0);
        row = row.child(
            h_flex()
                .id(("drive-hide", i))
                .h(px(28.))
                .px_2p5()
                .gap_1p5()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .border_1()
                .border_color(if off { p.accent } else { p.border })
                .bg(p.hover)
                .text_sm()
                .when(off || forced, |d| d.text_color(p.text_secondary))
                .cursor_pointer()
                .hover(|d| d.bg(p.pressed))
                .child(Icon::new(if off || forced { L::EyeOff } else { L::Eye }).size(px(14.)))
                .child(format!("{letter}:"))
                .on_click(move |_, _, cx| settings::update(cx, move |s| set_drive_hidden(s, letter, !off))),
        );
    }
    card(
        p,
        L::HardDrive,
        t!("Hidden drives"),
        Some(t!("A drive you hide is not shown in the sidebar, on the Home page or in the address bar's list of drives; you can still open it by typing its address. Drives that the system hides are hidden here too.")),
        row.into_any_element(),
    )
}

/// The cloud and WSL places hidden from the sidebar one by one (its menu: "Hide this place"): a click shows one again.
fn hidden_places_card(p: &Palette, s: &Settings) -> AnyElement {
    let mut row = h_flex().flex_wrap().gap_2().items_center().justify_end();
    for (i, path) in s.hidden_places.iter().enumerate() {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        let key = path.clone();
        row = row.child(
            h_flex()
                .id(("place-hide", i))
                .h(px(28.))
                .px_2p5()
                .gap_1p5()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .border_1()
                .border_color(p.accent)
                .bg(p.hover)
                .text_sm()
                .text_color(p.text_secondary)
                .cursor_pointer()
                .hover(|d| d.bg(p.pressed))
                .child(Icon::new(L::EyeOff).size(px(14.)))
                .child(name)
                .on_click(move |_, _, cx| {
                    let key = key.clone();
                    settings::update(cx, move |s| s.hidden_places.retain(|h| *h != key))
                }),
        );
    }
    card(
        p,
        L::Cloud,
        t!("Hidden cloud and Linux places"),
        Some(t!("The places you hid from the sidebar one by one. Click one to show it again.")),
        row.into_any_element(),
    )
}


/// What the accent setting holds after the user wrote a colour of their own: the colour as it is kept, or what to tell them.
pub(crate) fn accent_answer(text: &str) -> Result<String, String> {
    leaf_sys::accent::parse_hex(text).map(leaf_sys::accent::to_hex).ok_or_else(|| t!("Write the color as #RRGGBB, for example #F7630C.").to_string())
}

/// The theme: the three built-in looks and the user's own (`<config>/themes/*.toml`), with where those go and a way there.
fn theme_card(p: &Palette, s: &Settings) -> AnyElement {
    let themes = theme::data::list();
    let current = s.theme_name.trim().to_ascii_lowercase();
    let label = themes.iter().find(|(id, _)| *id == current).map(|(_, n)| n.clone()).unwrap_or_else(|| "Canopy".to_string());
    let control = select_box("theme-name", p, label, move |_| {
        themes
            .iter()
            .map(|(id, name)| {
                let id = id.clone();
                Choice::new(name.clone(), id == current, move |_, cx| {
                    let id = id.clone();
                    settings::update(cx, move |s| s.theme_name = id)
                })
            })
            .collect()
    });
    card(
        p,
        L::Palette,
        t!("Theme"),
        Some(t!("Canopy: frosted glass over the window backdrop. Stem: calm and solid, a thin accent line marks where you are. Index: dense and ruled, with monospace numbers.")),
        control,
    )
}

/// Where user themes go, and a button that opens that folder (made when it is not there yet).
/// Settings > Appearance > Icons: the built-in line icons, the system's own, or an installed icon pack; and the folder packs go in.
fn icons_card(p: &Palette, s: &Settings) -> AnyElement {
    const SYSTEM: &str = "@system";
    let packs = crate::icon_pack::installed();
    let current: String = if s.system_icons {
        SYSTEM.into()
    } else if packs.iter().any(|(id, _)| *id == s.icon_pack) {
        s.icon_pack.clone()
    } else {
        String::new()
    };
    let mut options: Vec<(String, String)> = vec![(String::new(), t!("Line icons (built in)").to_string()), (SYSTEM.into(), t!("Icons from the system").to_string())];
    options.extend(packs);
    let label = options.iter().find(|(id, _)| *id == current).map(|(_, l)| l.clone()).unwrap_or_default();
    let dir = crate::icon_pack::icons_dir();
    let shown = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    let desc = format!("Line icons are quiet shapes in the theme's colours. Icons from the system are Windows' own. An icon pack is a folder in {shown} (see docs/icon-packs.md); what it has no picture for keeps the line icon.");
    let pick = select_box("icon-source", p, label, move |_| {
        options
            .iter()
            .map(|(id, l)| {
                let id = id.clone();
                Choice::new(l.clone(), id == current, move |_, cx| {
                    crate::icon_pack::forget();
                    settings::update(cx, |s| {
                        s.system_icons = id == SYSTEM;
                        s.icon_pack = if id == SYSTEM { String::new() } else { id.clone() };
                    });
                    cx.refresh_windows();
                })
            })
            .collect()
    });
    let open = button("open-icons", p, t!("Open icons folder"), move |_, cx| {
        if let Some(d) = dir.clone() {
            if let Err(e) = std::fs::create_dir_all(&d) {
                log::warn!("icons folder: {e}");
                return;
            }
            if let Err(e) = leaf_sys::shell::open(&d) {
                log::warn!("icons folder: {e:#}");
            }
            cx.refresh_windows();
        }
    });
    shell(p)
        .child(card_body(p, L::Shapes, t!("Icons").into(), Some(desc.into()), Some(h_flex().gap_2().items_center().child(open).child(pick).into_any_element())))
        .into_any_element()
}

fn themes_folder_card(p: &Palette) -> AnyElement {
    let dir = theme::data::user_dir();
    let shown = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    let desc = format!("Your own themes are .toml files in {shown}. A theme lists only what it changes; the rest comes from its base theme (see docs/themes.md).");
    shell(p)
        .child(card_body(
            p,
            L::FolderOpen,
            t!("Your own themes").into(),
            Some(desc.into()),
            Some(button("open-themes", p, t!("Open themes folder"), move |_, cx| {
                if let Some(d) = dir.clone() {
                    if let Err(e) = std::fs::create_dir_all(&d) {
                        log::warn!("themes folder: {e}");
                        return;
                    }
                    if let Err(e) = leaf_sys::shell::open(&d) {
                        log::warn!("themes folder: {e:#}");
                    }
                    // (a theme added there shows in the list the next time it is drawn)
                    cx.refresh_windows();
                }
            })),
        ))
        .into_any_element()
}

/// The accent colour: one of the theme's set, Windows' own, or any colour written as #RRGGBB.
fn accent_card(p: &Palette, s: &Settings) -> AnyElement {
    let current = leaf_sys::accent::parse_hex(&s.accent_color);
    let chip = |id: &'static str, label: &'static str, on: bool, p: &Palette| {
        div()
            .id(id)
            .h(px(28.))
            .px_3()
            .flex()
            .items_center()
            .rounded(theme::RADIUS_SM)
            .border_1()
            .border_color(if on { p.accent } else { p.border })
            .bg(p.hover)
            .text_sm()
            .cursor_pointer()
            .hover(|d| d.bg(p.pressed))
            .child(label)
    };
    // the theme's own set, in the colour each has in this mode
    let all = theme::data::all_themes();
    let accents = all
        .iter()
        .find(|t| t.id == s.theme_name.trim().to_ascii_lowercase())
        .or_else(|| all.iter().find(|t| t.id == theme::data::DEFAULT_THEME))
        .map(|t| theme::data::resolve(&theme::data::chain(t, &all), p.dark, false).accents)
        .unwrap_or_default();
    let windows = current.is_none() && s.accent.eq_ignore_ascii_case("windows");
    let named = |i: usize, name: &str| current.is_none() && !windows && (s.accent.eq_ignore_ascii_case(name) || (i == 0 && !accents.iter().any(|a| a.name.eq_ignore_ascii_case(&s.accent))));
    let mut row = h_flex().flex_wrap().gap_2().items_center().justify_end();
    for (i, a) in accents.iter().enumerate() {
        let on = named(i, &a.name);
        let name = a.name.clone();
        row = row.child(
            div()
                .id(("accent-choice", i))
                .size(px(24.))
                .rounded_full()
                .bg(gpui_kit::rgb(if p.dark { a.dark } else { a.light }))
                .border_2()
                .border_color(if on { p.text } else { gpui_kit::transparent_black() })
                .cursor_pointer()
                .tooltip({
                    let name = name.clone();
                    move |w, cx| gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                })
                .on_click(move |_, _, cx| {
                    let name = name.clone();
                    settings::update(cx, move |s| {
                        s.accent = name;
                        s.accent_color.clear();
                    })
                }),
        );
    }
    row = row.child(chip("accent-windows", t!("System color"), windows, p).on_click(|_, _, cx| {
        settings::update(cx, |s| {
            s.accent = "windows".into();
            s.accent_color.clear();
        })
    }));
    let custom = current.is_some();
    let shown = s.accent_color.clone();
    row = row.child(chip("accent-own", t!("My own color…"), custom, p).on_click(move |_, window, cx| {
        crate::prompt::show(
            t!("Accent color"),
            Some(t!("Write the color as #RRGGBB, for example #F7630C.").to_string()),
            Some((shown.clone(), "#F7630C".to_string())),
            vec![
                crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |text, _, cx| {
                    let color = accent_answer(&text.unwrap_or_default())?;
                    settings::update(cx, move |s| s.accent_color = color);
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }));
    card(
        p,
        L::Palette,
        t!("Accent color"),
        Some(t!("The color of buttons, the selection and highlights: one of the theme's, the system's own, or a color you write yourself.")),
        row.into_any_element(),
    )
}

fn backdrop_card(p: &Palette, s: &Settings) -> AnyElement {
    // On a Windows without materials (before Windows 11 22H2, issue 24) the windows are "Solid" whatever was chosen, and that is the
    // one choice shown: a list of four materials that all look the same would be a lie.
    let materials = crate::settings::materials_available();
    let choices = if materials {
        // the first choice names what the theme in use is made for (Canopy: Acrylic)
        let theme_own: String = match crate::theme::material_pref() {
            BackdropPref::Acrylic => t!("Theme default: Acrylic (blurs what is behind the window)").into(),
            BackdropPref::MicaAlt => t!("Theme default: Mica Alt").into(),
            BackdropPref::Solid => t!("Theme default: Solid").into(),
            BackdropPref::Glass => t!("Theme default: HyperGlass").into(),
            _ => t!("Theme default: Mica").into(),
        };
        vec![
            (BackdropPref::Theme, theme_own),
            (BackdropPref::Mica, "Mica".into()),
            (BackdropPref::MicaAlt, "Mica Alt".into()),
            (BackdropPref::Acrylic, t!("Acrylic").into()),
            (BackdropPref::Solid, t!("Solid (no see-through)").into()),
            // a product name: only the "(experimental)" is translated
            (BackdropPref::Glass, t!("HyperGlass (experimental)").into()),
        ]
    } else {
        // Windows 10: Acrylic and HyperGlass for the main window only (issue 78); Mica left the window see-through there
        vec![
            (BackdropPref::Solid, t!("Solid").into()),
            (BackdropPref::Acrylic, t!("Acrylic").into()),
            (BackdropPref::Glass, t!("HyperGlass (experimental)").into()),
        ]
    };
    // (with materials the list shows the setting itself, "Theme default" included; without, the one material in use)
    let shown = if materials { s.backdrop } else { s.backdrop_in_use() };
    let control = dropdown("backdrop", p, shown, choices, |s, v| s.backdrop = v);
    let mut notes: Vec<String> = Vec::new();
    if !crate::settings::materials_available() && s.backdrop_in_use() != BackdropPref::Solid {
        notes.push(t!("On this system the material is for the main window only; menus and pop-ups stay plain.").to_string());
    }
    if s.backdrop_in_use() == BackdropPref::Glass || crate::glass::forced() {
        notes.push(t!("Bends and frosts what is behind the window like real glass. It watches the screen live and redraws on every change, so it uses more GPU and battery than Mica.").to_string());
        // Only where the windows really are hidden from screenshots: with a capture session (Windows 11 24H2 and the border-free capture
        // permission) they are not, and the line would be untrue. Before the engine has chosen (no window has glass yet) the mode is
        // Off and the line is left out; the page is drawn again once the glass is on the window.
        if leaf_sys::glass::capture_mode() == leaf_sys::glass::CaptureMode::Affinity {
            notes.push(t!("On this system, or while the screenshot border permission is off, leaf windows do not appear in screenshots, recordings or screen sharing.").to_string());
        }
        if let Some(why) = crate::glass::failure() {
            notes.push(t!("Glass could not start: {reason}. Mica is used instead.").replace("{reason}", &why));
        }
    }
    shell(p)
        // (the line about Mica is left out where Windows has no Mica to choose)
        .child(card_body(
            p,
            L::Layers,
            t!("Background material").into(),
            crate::settings::materials_available().then(|| {
                t!("What shows through the window. Acrylic blurs the desktop behind it (Canopy's glass); Mica takes a tint from the desktop color; Mica Alt is more colorful; Solid shows nothing through.").into()
            }),
            Some(control),
        ))
        .children(notes.into_iter().map(|n| div().pl(px(56.)).pr_4().pb_3().text_xs().text_color(p.text_secondary).child(n)))
        .into_any_element()
}

// ---------------------------------------------------------------- HyperGlass settings

/// The five HyperGlass values, in the order of the card. The number is also the slider's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hg {
    Frost,
    Tint,
    Refraction,
    Saturation,
    Surface,
}

impl Hg {
    const ALL: [Hg; 5] = [Hg::Frost, Hg::Tint, Hg::Refraction, Hg::Saturation, Hg::Surface];

    fn get(self, s: &Settings) -> u8 {
        match self {
            Hg::Frost => s.hg_frost,
            Hg::Tint => s.hg_tint,
            Hg::Refraction => s.hg_refraction,
            Hg::Saturation => s.hg_saturation,
            Hg::Surface => s.hg_surface,
        }
        .min(100)
    }

    fn set(self, s: &mut Settings, v: u8) {
        let v = v.min(100);
        match self {
            Hg::Frost => s.hg_frost = v,
            Hg::Tint => s.hg_tint = v,
            Hg::Refraction => s.hg_refraction = v,
            Hg::Saturation => s.hg_saturation = v,
            Hg::Surface => s.hg_surface = v,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Hg::Frost => t!("Frost"),
            Hg::Tint => t!("Tint"),
            Hg::Refraction => t!("Edge refraction"),
            Hg::Saturation => t!("Colour saturation"),
            Hg::Surface => t!("Surface wash"),
        }
    }

    fn desc(self) -> &'static str {
        match self {
            Hg::Frost => t!("How blurred the picture behind the glass is. At the far left the glass is clear."),
            Hg::Tint => t!("How much of the theme's tone lies over the picture. In the dark theme it also dims the picture."),
            Hg::Refraction => t!("How much the edges of the glass bend the picture behind them."),
            Hg::Saturation => t!("How vivid the colours seen through the glass are."),
            Hg::Surface => t!("How strongly the app's own surfaces cover the glass. At the far left there is no cover and text can be hard to read."),
        }
    }
}

/// Width of a slider's track and the thumb's size, in logical px.
const SLIDER_W: f32 = 220.;
const THUMB: f32 = 18.;

/// The value (0..=100) under the pointer: `left` and `width` are the slider's box, the thumb's centre travels from half a thumb
/// inside the left edge to half a thumb inside the right one. `thumb` is the thumb's width AT REST (the classic round one, or the
/// liquid capsule): a thumb that grows while it is held keeps its centre, so the mapping does not change under the pointer.
#[cfg(test)]
fn slider_value_at(x: f32, left: f32, width: f32, thumb: f32) -> u8 {
    slider_position_at(x, left, width, thumb, 100)
}

/// [`slider_value_at`] for a slider over the positions 0..=`max`.
fn slider_position_at(x: f32, left: f32, width: f32, thumb: f32, max: u8) -> u8 {
    let run = (width - thumb).max(1.0);
    (((x - left - thumb / 2.0) / run).clamp(0.0, 1.0) * f32::from(max)).round() as u8
}

/// The slider being dragged right now (its id), `usize::MAX` for none. Together with `cx.has_active_drag()` it says whether to draw
/// the thumb as the held glass lens: the press state of the element is cleared by GPUI when a drag starts.
static HG_DRAG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

/// The slider the pointer went down on and has not been let go from (its id), `usize::MAX` for none. A mouse-up anywhere else on the page
/// reaches every slider as a mouse-up-out; only the one that was pressed has a gesture to end.
static SLIDER_HELD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

/// What a key does to a slider that has the focus: the new value, or `None` for a key the slider does not take. Arrows move by 1,
/// Shift or Page Up / Down by 10, Home and End jump to the ends.
#[cfg(test)]
fn slider_key(value: u8, key: &str, shift: bool) -> Option<u8> {
    slider_key_in(value, key, shift, 100, 10)
}

/// [`slider_key`] for a slider over the positions 0..=`max` whose big step (Shift, Page Up / Down) is `big`.
fn slider_key_in(value: u8, key: &str, shift: bool, max: u8, big: u8) -> Option<u8> {
    let step = if shift { big } else { 1 };
    let v = value.min(max);
    Some(match key {
        "left" | "down" => v.saturating_sub(step),
        "right" | "up" => v.saturating_add(step).min(max),
        "pagedown" => v.saturating_sub(big),
        "pageup" => v.saturating_add(big).min(max),
        "home" => 0,
        "end" => max,
        _ => return None,
    })
}

/// What a slider drags: which of the five.
#[derive(Clone)]
struct SliderDrag(usize);

struct SliderGhost;
impl Render for SliderGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A slider for one HyperGlass value: a thin track filled up to a round thumb. A click puts the thumb there, a drag moves it, the arrow
/// keys move it while it has the focus. Every step is applied and drawn at once and written to the file once the steps settle.
fn slider(which: Hg, p: &Palette, value: u8, focus: FocusHandle, dragging: bool) -> AnyElement {
    let spec = SliderSpec {
        id: which as usize,
        max: 100,
        big: 10,
        get: std::rc::Rc::new(move |cx| which.get(cx.global::<Settings>())),
        apply: std::rc::Rc::new(move |v, cx| {
            if which.get(cx.global::<Settings>()) != v {
                settings::update_live(cx, |s| which.set(s, v));
            }
        }),
        release: None,
    };
    slider_for(spec, p, value, focus, dragging)
}

/// What a slider is bound to: the positions 0..=`max` it offers, how to read the current one and what to do for each step. The
/// HyperGlass sliders are positions 0..=100; the Dock page's size slider (`dock::size_slider`) is positions 0..=15.
#[derive(Clone)]
pub(crate) struct SliderSpec {
    /// Tells the sliders apart (focus, drag, the lens): the HyperGlass ones are 0..=4.
    pub id: usize,
    pub max: u8,
    /// The step of Shift and of Page Up / Down.
    pub big: u8,
    /// The position the setting has now (read at a key press).
    pub get: std::rc::Rc<dyn Fn(&App) -> u8>,
    /// A step of a click, a drag or a key: the new position.
    pub apply: SliderStep,
    /// The pointer was let go: nothing more is coming for now. (Not called for keys: see the key handler.)
    pub release: Option<SliderEnd>,
}

/// What a slider does for a step (the new position) and at the end of a gesture.
pub(crate) type SliderStep = std::rc::Rc<dyn Fn(u8, &mut App)>;
pub(crate) type SliderEnd = std::rc::Rc<dyn Fn(&mut App)>;

/// The slider of a [`SliderSpec`], with its box and thumb as the HyperGlass sliders have them.
pub(crate) fn slider_for(spec: SliderSpec, p: &Palette, value: u8, focus: FocusHandle, dragging: bool) -> AnyElement {
    let id = spec.id;
    let max = spec.max.max(1);
    let value = value.min(max);
    let frac = f32::from(value) / f32::from(max);
    let liquid = p.liquid;
    // the width the pointer mapping leaves the thumb at rest (a thumb that grows while held keeps its centre)
    let thumb_w = if liquid { crate::liquid::SL_REST_W } else { THUMB };
    // (only the liquid thumb needs to know that the pointer is down on the slider; the classic slider stays as it was)
    let group: Option<SharedString> = liquid.then(|| format!("hg-slider-{id}").into());
    let bounds = std::rc::Rc::new(std::cell::Cell::new(Bounds::<Pixels>::default()));
    let (apply_down, apply_drag, apply_key) = (spec.apply.clone(), spec.apply.clone(), spec.apply.clone());
    let (b_down, f_down) = (bounds.clone(), focus.clone());
    let (b_drag, b_probe) = (bounds.clone(), bounds.clone());
    let root = div()
        .id(("hg-slider", id))
        .when_some(group.clone(), |d, g| d.group(g))
        .relative()
        .w(px(SLIDER_W))
        .h(px(24.))
        .flex_shrink_0()
        .cursor_pointer()
        .rounded(px(12.))
        .border_1()
        .border_color(transparent_black())
        .track_focus(&focus)
        .focus(|s| s.border_color(p.accent))
        // (the probe records where the slider is; the mouse events do not carry the element's box)
        .child(canvas(move |b, _, _| b_probe.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full());
    let root = if let Some(group) = group {
        // a 4 px track and a capsule thumb that turns into a glass lens while it is held (no clip anywhere: it overhangs the box)
        let (track, lane) = crate::liquid::slider_parts(id, group, p, SLIDER_W - 2.0 * crate::liquid::SL_BORDER, frac, dragging);
        root.child(track).child(lane)
    } else {
        root.child(
            // the track, filled up to the thumb
            div()
                .absolute()
                .left(px(THUMB / 2.0))
                .right(px(THUMB / 2.0))
                .top(px(10.))
                .h(px(4.))
                .rounded_full()
                .bg(p.pressed)
                .child(div().h_full().rounded_full().bg(p.accent).w(relative(frac))),
        )
        .child(
            // The thumb's lane is the slider's inner width less one thumb, and the thumb sits at a share of it. Measured from the
            // box the children are placed in (not from SLIDER_W), so it agrees with the track and with the pointer mapping whatever
            // the border takes: the root's 1 px focus border made a thumb placed by SLIDER_W stick out by 2 px at the far right.
            div().absolute().top(px(3.)).left_0().right(px(THUMB)).h(px(THUMB)).child(
                div()
                    .absolute()
                    .top_0()
                    .left(relative(frac))
                    .size(px(THUMB))
                    .rounded_full()
                    .bg(p.card_bg)
                    .border_1()
                    .border_color(p.border)
                    .child(div().m(px(4.)).size(px(8.)).rounded_full().bg(p.accent)),
            ),
        )
    };
    let root = root
        .on_mouse_down(MouseButton::Left, move |e, window, cx| {
            window.focus(&f_down, cx);
            SLIDER_HELD.store(id, std::sync::atomic::Ordering::Relaxed);
            let b = b_down.get();
            apply_down(slider_position_at(f32::from(e.position.x), f32::from(b.left()), f32::from(b.size.width), thumb_w, max), cx);
            cx.stop_propagation();
        })
        .on_drag(SliderDrag(id), |_, _, _, cx| cx.new(|_| SliderGhost))
        .on_drag_move(move |e: &DragMoveEvent<SliderDrag>, window, cx| {
            if e.drag(cx).0 == id {
                if liquid && HG_DRAG.swap(id, std::sync::atomic::Ordering::Relaxed) != id {
                    // the thumb turns into the held lens at the first step of the drag, even when the value did not change
                    window.refresh();
                }
                // the same box as a click uses (the probe's), so a click and a drag agree on the value under the pointer
                let b = b_drag.get();
                apply_drag(slider_position_at(f32::from(e.event.position.x), f32::from(b.left()), f32::from(b.size.width), thumb_w, max), cx);
            }
        })
        // (captured, so the arrows reach the slider before the file list's own shortcuts do)
        .capture_key_down({
            let get = spec.get.clone();
            let big = spec.big;
            move |ev: &KeyDownEvent, _, cx| {
                let now = get(cx);
                if let Some(v) = slider_key_in(now, ev.keystroke.key.as_str(), ev.keystroke.modifiers.shift, max, big) {
                    apply_key(v, cx);
                    // (no release here: a held key is a run of key presses, and a slider whose end does costly work (the dock's band
                    // and icons) would do it for each one. Whoever listens settles by itself once the steps stop.)
                    cx.stop_propagation();
                }
            }
        });
    let release = spec.release.clone();
    if !liquid && release.is_none() {
        return root.into_any_element();
    }
    // the thumb goes back to rest when the pointer is let go, on the slider or off it; a slider with something to do at the end of a drag
    // does it then
    let (release_in, release_out) = (release.clone(), release);
    root.on_mouse_up(MouseButton::Left, move |_, window, cx| {
        HG_DRAG.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        window.refresh();
        if let Some(r) = &release_in
            && SLIDER_HELD.swap(usize::MAX, std::sync::atomic::Ordering::Relaxed) == id
        {
            r(cx);
        }
    })
    .on_mouse_up_out(MouseButton::Left, move |_, window, cx| {
        HG_DRAG.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
        window.refresh();
        if let Some(r) = &release_out
            && SLIDER_HELD.swap(usize::MAX, std::sync::atomic::Ordering::Relaxed) == id
        {
            r(cx);
        }
    })
    .into_any_element()
}

/// The focus handle of slider `i`, made at the first call.
pub(crate) fn hg_focus(cx: &mut App, i: usize) -> FocusHandle {
    while ui(cx).hg_focus.len() <= i {
        let h = cx.focus_handle();
        ui(cx).hg_focus.push(h);
    }
    ui(cx).hg_focus[i].clone()
}

/// The HyperGlass card: five sliders for the look of the glass, each with its value, and a button back to the shipped look. Shown
/// under the backdrop card while the backdrop is Glass.
fn hyperglass_card(p: &Palette, s: &Settings, cx: &mut App) -> AnyElement {
    let reset = button("hg-reset", p, t!("Reset to defaults"), |_, cx| settings::update(cx, |s| s.reset_hyperglass()));
    let mut rows: Vec<AnyElement> = Vec::new();
    if !cx.has_active_drag() {
        // No drag is on: whatever slider the flag still names lost its mouse-up (the page went away with the button down), and must not
        // show as held when some other drag starts.
        HG_DRAG.store(usize::MAX, std::sync::atomic::Ordering::Relaxed);
    }
    for which in Hg::ALL {
        let value = which.get(s);
        // dragged right now: the drag is on and it is this slider's (a flag left over from a drag that ended does not count)
        let dragging = cx.has_active_drag() && HG_DRAG.load(std::sync::atomic::Ordering::Relaxed) == which as usize;
        let control = h_flex()
            .gap_3()
            .items_center()
            .child(slider(which, p, value, hg_focus(cx, which as usize), dragging))
            .child(div().w(px(32.)).flex_shrink_0().text_right().text_sm().text_color(p.text_secondary).child(value.to_string()))
            .into_any_element();
        rows.push(sub(p, which.title(), Some(which.desc()), control));
    }
    // not a look but a cost: the glass of a window that is not in front stops following the screen (off by default)
    rows.push(sub(
        p,
        t!("Freeze the glass while not in front"),
        Some(t!("While the window is not in front and the pointer is not over it, the glass keeps its last picture: nothing is captured or drawn, whatever changes behind it. It flows again when the window comes to the front.")),
        sw("hg-freeze", p, s.hg_freeze_behind, |s, v| s.hg_freeze_behind = v),
    ));
    shell(p)
        .child(card_body(p, L::Sparkles, t!("HyperGlass settings").into(), Some(t!("Adjust how the glass looks. Changes show at once in every open window.").into()), Some(reset)))
        .child(div().h(px(1.)).bg(p.divider))
        .child(v_flex().py_1().children(rows))
        .into_any_element()
}

fn appearance(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    let mut v = vec![
        theme_card(p, s),
        card(
            p,
            L::SunMoon,
            t!("Mode"),
            None,
            dropdown(
                "theme",
                p,
                s.theme,
                vec![(ThemePref::System, t!("Use system setting").into()), (ThemePref::Light, crate::i18n::tx("Light").into()), (ThemePref::Dark, t!("Dark").into()), (ThemePref::Black, t!("Black (OLED)").into())],
                |s, v| s.theme = v,
            ),
        ),
        card(
            p,
            L::Eye,
            t!("Visibility"),
            Some(t!("High: stronger text, lines and selection, and nearly opaque glass. Standard: the calmer original.")),
            dropdown("visibility", p, s.visibility, vec![(Visibility::High, t!("High").into()), (Visibility::Standard, t!("Standard").into())], |s, v| s.visibility = v),
        ),
        accent_card(p, s),
        icons_card(p, s),
        backdrop_card(p, s),
        themes_folder_card(p),
        card(
            p,
            L::AppWindow,
            t!("Tabs as wide as their names"),
            Some(t!("When off, all tabs are equally wide. When on, each tab fits its name, from 72 to 220 pixels.")),
            sw("tabs-fit", p, s.tabs_fit_names, |s, v| s.tabs_fit_names = v),
        ),
        card(
            p,
            L::SquareTerminal,
            t!("Terminal button on tabs"),
            Some(t!("A button on each folder's tab that opens a terminal in that folder.")),
            sw("tab-terminal", p, s.tab_terminal_button, |s, v| s.tab_terminal_button = v),
        ),
        card(
            p,
            L::Activity,
            t!("Animations"),
            Some(t!("Short transitions when tabs open and close. Never delays anything.")),
            dropdown(
                "animations",
                p,
                s.animations,
                vec![
                    (AnimPref::System, t!("Follow the system").into()),
                    (AnimPref::On, t!("On").into()),
                    (AnimPref::Off, t!("Off").into()),
                ],
                |s, v| s.animations = v,
            ),
        ),
        card(
            p,
            L::PanelRight,
            t!("Show scrollbar"),
            Some(t!("On the right of the Details and Grid lists. Drag it, or click a spot to jump there.")),
            sw("scrollbar", p, s.show_scrollbar, |s, v| s.show_scrollbar = v),
        ),
    ];
    // HyperGlass look, right under the backdrop card (index 5) while Glass is the material
    if s.backdrop_in_use() == BackdropPref::Glass || crate::glass::forced() {
        v.insert(6, hyperglass_card(p, s, cx));
    }
    // tint swatches
    let tint = s.tint;
    let pp = *p;
    let swatch_now: AnyElement = match tint {
        Some(c) => div().size(px(20.)).rounded(px(4.)).bg(rgb(c)).border_1().border_color(p.border).into_any_element(),
        None => div().text_sm().text_color(p.text_secondary).child(t!("Default")).into_any_element(),
    };
    v.push(expander(
        "tint",
        p,
        L::Paintbrush,
        t!("Background color"),
        None,
        Some(swatch_now),
        move || {
            let tile = |id: usize, label: String, color: Option<u32>, selected: bool| {
                v_flex()
                    .id(("tint", id))
                    .w(px(96.))
                    .gap_1()
                    .items_center()
                    .p_1()
                    .rounded(theme::RADIUS_SM)
                    .border_1()
                    .border_color(if selected { pp.accent } else { transparent_black() })
                    .hover(|s| s.bg(pp.pressed))
                    .child(
                        div()
                            .w_full()
                            .h(px(44.))
                            .rounded(theme::RADIUS_SM)
                            .border_1()
                            .border_color(pp.border)
                            .when_some(color, |d, c| d.bg(hsla_alpha(c, 0.55)))
                            .when(color.is_none(), |d| d.bg(pp.content_bg)),
                    )
                    .child(div().text_xs().text_color(pp.text_secondary).child(label))
                    .on_click(move |_, _, cx| settings::update(cx, |s| s.tint = color))
            };
            let mut grid = div().px_4().py_3().flex().flex_wrap().gap_2();
            grid = grid.child(tile(0, t!("Default").to_string(), None, tint.is_none()));
            for (i, (name, c)) in TINTS.iter().enumerate() {
                grid = grid.child(tile(i + 1, crate::i18n::t(name).to_string(), Some(*c), tint == Some(*c)));
            }
            vec![grid.into_any_element()]
        },
        cx,
    ));
    // background image
    let img = s.background_image.clone();
    let opacity = s.background_opacity;
    let pp = *p;
    let img_desc = img.as_ref().map(|i| i.display().to_string());
    v.push(expander(
        "bgimg",
        p,
        L::Image,
        t!("Background image"),
        Some(img_desc.as_deref().unwrap_or(t!("None"))),
        Some(button("bg-browse", p, t!("Browse"), |window, cx| pick_into(window, cx, t!("Background image"), PickTarget::BackgroundImage))),
        move || {
            vec![
                sub(
                    &pp,
                    t!("Visibility"),
                    None,
                    dropdown(
                        "bg-opacity",
                        &pp,
                        (opacity * 100.).round() as u32,
                        vec![(10, "%10".into()), (20, "%20".into()), (35, "%35".into()), (50, "%50".into())],
                        |s, v| s.background_opacity = v as f32 / 100.,
                    ),
                ),
                sub(&pp, t!("Remove image"), None, button("bg-clear", &pp, t!("Remove"), |_, cx| settings::update(cx, |s| s.background_image = None))),
            ]
        },
        cx,
    ));
    // font
    let fonts = font_choices(cx);
    let current_font = s.font.clone().unwrap_or_default();
    let mut font_opts: Vec<(usize, String)> = vec![(0, t!("Default").to_string())];
    font_opts.extend(fonts.iter().enumerate().map(|(i, f)| (i + 1, f.clone())));
    let cur_ix = fonts.iter().position(|f| *f == current_font).map(|i| i + 1).unwrap_or(0);
    let fonts2 = fonts.clone();
    v.push(card(
        p,
        L::Type,
        t!("Font"),
        None,
        select_box("font", p, font_opts.iter().find(|(i, _)| *i == cur_ix).map(|x| x.1.clone()).unwrap_or_default(), move |_| {
            font_opts
                .iter()
                .map(|(i, l)| {
                    let name = if *i == 0 { None } else { fonts2.get(i - 1).cloned() };
                    Choice::new(l.clone(), *i == cur_ix, move |_, cx| {
                        let n = name.clone();
                        settings::update(cx, |s| s.font = n)
                    })
                })
                .collect()
        }),
    ));
    // the text size (issue 89): smaller for a low-resolution screen, larger to read more easily
    v.push(card(
        p,
        L::CaseSensitive,
        t!("Text size"),
        Some(t!("The size of the text in the file manager's windows")),
        dropdown(
            "text-size",
            p,
            u32::from(s.text_percent),
            vec![(87, t!("Small").into()), (100, t!("Normal").into()), (112, t!("Large").into())],
            |s, v| s.text_percent = v as u16,
        ),
    ));
    v.push(card(p, L::PanelBottom, t!("Show status bar"), None, sw("statusbar", p, s.show_statusbar, |s, v| s.show_statusbar = v)));
    v.push(card(p, L::PanelRight, t!("Preview pane"), Some("Alt+P"), sw("preview", p, s.preview_open, |s, v| s.preview_open = v)));
    v.push(card(
        p,
        L::PanelBottom,
        t!("Preview pane position"),
        Some(t!("Right: beside the file list. Bottom: under the list, across the whole width; drag its top edge to set the height")),
        dropdown(
            "preview-position",
            p,
            s.preview_position,
            vec![(rf_core::previewpane::PreviewPos::Right, t!("Right").into()), (rf_core::previewpane::PreviewPos::Bottom, t!("Bottom").into())],
            |s, v| s.preview_position = v,
        ),
    ));
    v.push(card(
        p,
        L::Info,
        t!("File details in the preview"),
        Some(t!("Shows the type, size, dates and location under the preview")),
        sw("preview-details", p, s.preview_details, |s, v| s.preview_details = v),
    ));
    let hidden = s.sidebar_hidden.clone();
    let libs = s.show_libraries;
    let (tree_on, tree_follow) = (s.sidebar_tree, s.sidebar_tree_follow);
    let pp = *p;
    v.push(expander(
        "sidebar",
        p,
        L::PanelLeft,
        t!("Sidebar sections"),
        None,
        None,
        move || {
            let shown = |k: &str| !hidden.iter().any(|h| h == k);
            vec![
                sub(&pp, t!("Drives"), None, switch("sb-drives", &pp, shown("drives"), {
                    let on = shown("drives");
                    move |_, cx| settings::update(cx, |s| show_section(s, "drives", !on))
                })),
                sub(&pp, t!("Cloud and Linux"), None, switch("sb-cloud", &pp, shown("cloud"), {
                    let on = shown("cloud");
                    move |_, cx| settings::update(cx, |s| show_section(s, "cloud", !on))
                })),
                sub(&pp, t!("Libraries"), Some(t!("Shows the same folders as pinned items")), switch("sb-libs", &pp, libs, move |_, cx| {
                    settings::update(cx, |s| s.show_libraries = !libs)
                })),
                sub(&pp, t!("Network"), None, switch("sb-net", &pp, shown("network"), {
                    let on = shown("network");
                    move |_, cx| settings::update(cx, |s| show_section(s, "network", !on))
                })),
                sub(&pp, t!("Hosts"), Some(t!("The servers saved in Settings > Hosts, while there are any")), switch("sb-hosts", &pp, shown("hosts"), {
                    let on = shown("hosts");
                    move |_, cx| settings::update(cx, |s| show_section(s, "hosts", !on))
                })),
                sub(&pp, t!("Tags"), None, switch("sb-tags", &pp, shown("tags"), {
                    let on = shown("tags");
                    move |_, cx| settings::update(cx, |s| show_section(s, "tags", !on))
                })),
                // the folder tree (crate::sidebar_tree)
                sub(&pp, t!("Folder tree"), Some(t!("Adds a Folders section under the sidebar that shows the drives and the user's folder as a tree that opens and closes")), switch("sb-tree", &pp, tree_on, move |_, cx| {
                    settings::update(cx, |s| s.sidebar_tree = !tree_on)
                })),
                sub(&pp, t!("Follow the open folder"), Some(t!("As you browse, the tree opens the branch to the open folder, marks it selected and scrolls it into view")), switch("sb-tree-follow", &pp, tree_follow, move |_, cx| {
                    settings::update(cx, |s| s.sidebar_tree_follow = !tree_follow)
                })),
            ]
        },
        cx,
    ));
    let (bar_hidden, bar_extra) = (s.toolbar_hidden.clone(), s.toolbar_extra.clone());
    let commands = s.toolbar_commands.clone();
    // the switch and the buttons it shows stand together (issue 111)
    v.push(card(p, L::PanelTop, t!("Show toolbar"), Some("Ctrl+Shift+B"), sw("toolbar", p, s.show_toolbar, |s, v| s.show_toolbar = v)));
    v.push(expander(
        "toolbar-buttons",
        p,
        L::PanelTop,
        t!("Toolbar buttons"),
        None,
        None,
        move || {
            // (key, id, label, off unless asked for)
            const BUTTONS: [(&str, &str, &str, bool); 16] = [
                ("new", "tb-new", "New", false),
                ("newfolder", "tb-newfolder", "New folder", true),
                ("cut", "tb-cut", "Cut", false),
                ("copy", "tb-copy", "Copy", false),
                ("paste", "tb-paste", "Paste", false),
                ("rename", "tb-rename", "Rename", false),
                ("share", "tb-share", "Share", false),
                ("delete", "tb-delete", "Delete", false),
                ("props", "tb-props", "Properties", false),
                ("terminal", "tb-terminal", "Open in terminal", true),
                ("hidden", "tb-hidden", "Show/hide hidden items", true),
                ("refresh", "tb-refresh", "Refresh", true),
                ("filter", "tb-filter", "Filter", false),
                ("sort", "tb-sort", "Sort and group", false),
                ("view", "tb-view", "Appearance", false),
                ("preview", "tb-preview", "Preview pane", false),
            ];
            let mut rows: Vec<_> = BUTTONS
                .iter()
                .map(|&(key, id, label, optional)| {
                    let shown = if optional { bar_extra.iter().any(|k| k == key) } else { !bar_hidden.iter().any(|k| k == key) };
                    sub(
                        &pp,
                        crate::i18n::t(label),
                        None,
                        switch(id, &pp, shown, move |_, cx| {
                            settings::update(cx, |s| {
                                let list = if optional { &mut s.toolbar_extra } else { &mut s.toolbar_hidden };
                                list.retain(|k| k != key);
                                // (the optional ones are listed when on, the others when off)
                                if shown != optional {
                                    list.push(key.to_string());
                                }
                            })
                        }),
                    )
                })
                .collect();
            rows.extend(commands.into_iter().enumerate().map(|(i, command)| {
                sub(&pp, command.name(), Some(&command.program), crate::quick_commands::controls(i, command.clone(), &pp).into_any_element())
            }));
            let add = crate::i18n::tx("Add command");
            rows.push(sub(
                &pp,
                crate::i18n::tx("Custom commands"),
                None,
                button("add-quick-command", &pp, add, |window, cx| crate::quick_commands::edit(None, window, cx)),
            ));
            rows
        },
        cx,
    ));
    let home_on: Vec<&'static str> = crate::home::layout(s).iter().map(|&(i, _)| crate::home::WIDGETS[i].id).collect();
    v.push(expander(
        "home-sections",
        p,
        L::House,
        t!("Home page widgets"),
        Some(t!("Sizes and order: Customize on the Home page")),
        None,
        move || {
            let mut rows: Vec<AnyElement> = crate::home::WIDGETS
                .iter()
                .map(|w| {
                    let (id, on) = (w.id, home_on.contains(&w.id));
                    sub(&pp, crate::i18n::t(w.title), None, switch(SharedString::from(format!("hm-{id}")), &pp, on, move |_, cx| settings::update(cx, |s| crate::home::set_shown(s, id, !on))))
                })
                .collect();
            rows.push(sub(&pp, t!("Default widgets"), None, button("home-reset-settings", &pp, t!("Reset"), |_, cx| settings::update(cx, crate::home::reset))));
            rows
        },
        cx,
    ));
    v
}

fn hsla_alpha(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = a;
    h
}

/// Installed UI-friendly fonts (curated, only those present).
fn font_choices(cx: &App) -> Vec<String> {
    let all: HashSet<String> = cx.text_system().all_font_names().into_iter().collect();
    [
        "Segoe UI Variable Text",
        "Segoe UI",
        "Inter",
        "Aptos",
        "Calibri",
        "Arial",
        "Verdana",
        "Tahoma",
        "Roboto",
        "Open Sans",
        "Noto Sans",
        "Source Sans 3",
        "Cascadia Code",
        "Consolas",
    ]
    .iter()
    .filter(|f| all.contains(**f))
    .map(|f| f.to_string())
    .collect()
}

fn layout(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    use rf_core::SortKey;
    let mut v = vec![
        card(
            p,
            L::FolderSync,
            t!("Use the same layout and sorting in all folders"),
            Some(t!("When off, each folder remembers its own view and sorting.")),
            sw("sync-layout", p, s.sync_layout, |s, v| s.sync_layout = v),
        ),
        card(
            p,
            L::LayoutGrid,
            t!("Default layout"),
            Some(t!("Adaptive: folders that mostly contain images or videos open in grid view.")),
            dropdown(
                "view",
                p,
                s.default_view,
                vec![
                    (DefaultView::Details, t!("Details").into()),
                    (DefaultView::Grid, t!("Grid").into()),
                    (DefaultView::Columns, t!("Columns").into()),
                    (DefaultView::Adaptive, t!("Adaptive").into()),
                ],
                |s, v| s.default_view = v,
            ),
        ),
        section(p, t!("Sort and group")),
        card(
            p,
            L::ArrowUpDown,
            t!("Sort"),
            None,
            dropdown(
                "sort",
                p,
                s.default_sort,
                vec![
                    (SortKey::Name, t!("Name").into()),
                    (SortKey::Modified, t!("Date modified").into()),
                    (SortKey::Created, t!("Date created").into()),
                    (SortKey::Type, t!("Type").into()),
                    (SortKey::Size, t!("Size").into()),
                ],
                |s, v| s.default_sort = v,
            ),
        ),
        card(p, L::ArrowDownWideNarrow, t!("Descending order"), None, sw("sort-desc", p, s.default_sort_desc, |s, v| s.default_sort_desc = v)),
        card(p, L::FolderUp, t!("Folders first"), None, sw("dirs-first", p, s.folders_first, |s, v| s.folders_first = v)),
        card(
            p,
            L::ListTree,
            t!("Group by"),
            None,
            dropdown(
                "group",
                p,
                group_ix(&s.default_group),
                vec![
                    (0, t!("None").into()),
                    (1, t!("Name").into()),
                    (2, t!("Type").into()),
                    (3, t!("Date modified").into()),
                    (4, t!("Size").into()),
                    (5, t!("Date created").into()),
                    (6, t!("Year").into()),
                    (7, t!("Month").into()),
                    (8, t!("Day").into()),
                    (9, t!("Tag").into()),
                ],
                |s, v| s.default_group = crate::folder_view::GroupBy::ALL[v.min(9)].1.into(),
            ),
        ),
        section(p, t!("Details view")),
    ];
    let c = s.columns.clone();
    let pp = *p;
    v.push(expander(
        "columns",
        p,
        L::Columns3,
        t!("Columns"),
        Some(t!("Name column is always visible")),
        None,
        move || {
            type Set = fn(&mut Settings, bool);
            let rows: [(&str, bool, Set); 12] = [
                (t!("Date modified"), c.modified, |s, v| s.columns.modified = v),
                (t!("Date created"), c.created, |s, v| s.columns.created = v),
                (t!("Type"), c.kind, |s, v| s.columns.kind = v),
                (t!("Size"), c.size, |s, v| s.columns.size = v),
                (t!("File tag"), c.tags, |s, v| s.columns.tags = v),
                (t!("Git status"), c.git, |s, v| s.columns.git = v),
                (t!("Sync status"), c.sync, |s, v| s.columns.sync = v),
                (t!("Last commit message"), c.git_message, |s, v| s.columns.git_message = v),
                (t!("Commit author"), c.git_author, |s, v| s.columns.git_author = v),
                (t!("Commit date"), c.git_date, |s, v| s.columns.git_date = v),
                (t!("Commit hash"), c.git_hash, |s, v| s.columns.git_hash = v),
                (t!("Permissions and owner (Linux, SFTP)"), c.permissions, |s, v| s.columns.permissions = v),
            ];
            rows.into_iter()
                .enumerate()
                .map(|(i, (label, on, set))| sub(&pp, label, None, switch(("col-sw", i), &pp, on, move |_, cx| settings::update(cx, |s| set(s, !on)))))
                .collect()
        },
        cx,
    ));
    v
}

pub fn group_ix(g: &str) -> usize {
    crate::folder_view::GroupBy::ALL.iter().position(|(_, name)| *name == g).unwrap_or(0)
}

fn files(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    let mut v = vec![section(p, t!("Appearance"))];
    let (h, sys, dot) = (s.show_hidden, s.show_system_files, s.show_dot_files);
    let pp = *p;
    v.push(expander(
        "hidden",
        p,
        L::EyeOff,
        t!("Hidden items"),
        None,
        None,
        move || {
            vec![
                sub(&pp, t!("Show hidden items"), Some("Ctrl+H"), switch("h1", &pp, h, move |_, cx| settings::update(cx, |s| s.show_hidden = !h))),
                sub(
                    &pp,
                    t!("Show protected operating system files"),
                    Some(t!("System files such as desktop.ini and thumbs.db")),
                    switch("h2", &pp, sys, move |_, cx| settings::update(cx, |s| s.show_system_files = !sys)),
                ),
                sub(
                    &pp,
                    t!("Show files that start with a dot"),
                    Some(t!("When off, items such as .git and .config are treated as hidden")),
                    switch("h3", &pp, dot, move |_, cx| settings::update(cx, |s| s.show_dot_files = !dot)),
                ),
            ]
        },
        cx,
    ));
    v.push(card(p, L::FileType, t!("Show file extensions"), None, sw("ext", p, s.show_extensions, |s, v| s.show_extensions = v)));
    v.push(card(p, L::Image, t!("Show thumbnails"), None, sw("thumbs", p, s.show_thumbnails, |s, v| s.show_thumbnails = v)));
    v.push(card(
        p,
        L::BadgeCheck,
        t!("Show other programs' icon overlays"),
        Some(t!("The status marks programs such as Google Drive, OneDrive and TortoiseGit put in the corner of icons in the file list.")),
        sw("overlays", p, s.show_overlays, |s, v| s.show_overlays = v),
    ));
    v.push(card(p, L::SquareCheck, t!("Show checkboxes when selecting"), None, sw("checkboxes", p, s.show_checkboxes, |s, v| s.show_checkboxes = v)));

    v.push(section(p, t!("Behaviors")));
    v.push(card(
        p,
        L::MousePointerClick,
        t!("Open items with a single click"),
        None,
        dropdown(
            "single-click",
            p,
            s.single_click,
            vec![(ClickPref::Never, t!("Never").into()), (ClickPref::Folders, t!("Folders only").into()), (ClickPref::Always, t!("Always").into())],
            |s, v| s.single_click = v,
        ),
    ));
    v.push(card(p, L::SquarePlus, t!("Open folders in a new tab"), None, sw("dir-newtab", p, s.open_folders_in_new_tab, |s, v| s.open_folders_in_new_tab = v)));
    v.push(card(
        p,
        L::Trash,
        t!("Confirm before deleting"),
        None,
        dropdown(
            "confirm-del",
            p,
            s.confirm_delete,
            vec![
                (DeletePref::Always, t!("Always").into()),
                (DeletePref::PermanentOnly, t!("Only for permanent deletion").into()),
                (DeletePref::Never, t!("Never").into()),
            ],
            |s, v| s.confirm_delete = v,
        ),
    ));
    v.push(card(p, L::TriangleAlert, t!("Warn when the extension changes"), None, sw("ext-warn", p, s.warn_extension_change, |s, v| s.warn_extension_change = v)));
    v.push(card(p, L::MousePointer, t!("Select on hover"), None, sw("hover-sel", p, s.select_on_hover, |s, v| s.select_on_hover = v)));
    v.push(card(
        p,
        L::ListFilter,
        t!("Typing filters this folder"),
        Some(t!("When off, typing selects the item whose name starts with those letters.")),
        sw("type-filter", p, s.type_to_filter, |s, v| s.type_to_filter = v),
    ));
    v.push(card(p, L::ArrowUp, t!("Double-click empty space to go up a folder"), None, sw("dbl-up", p, s.double_click_up, |s, v| s.double_click_up = v)));
    v.push(card(p, L::FolderUp, t!("Select the previous folder when going up"), None, sw("prev-sel", p, s.select_previous_on_up, |s, v| s.select_previous_on_up = v)));
    v.push(card(
        p,
        L::TextCursorInput,
        t!("Show the full path in the address bar"),
        Some(t!("The address bar shows the path as text instead of clickable folder parts; one click still edits it")),
        sw("addr-full", p, s.address_full_path, |s, v| s.address_full_path = v),
    ));
    v.push(card(
        p,
        L::PanelTop,
        t!("Show the full path on tabs"),
        Some(t!("Tabs are titled with the whole path instead of the folder name; long paths are shortened in the middle. The tooltip always shows the full path")),
        sw("tab-full", p, s.tab_full_path, |s, v| s.tab_full_path = v),
    ));
    v.push(card(
        p,
        L::AppWindow,
        t!("Show the full path in the window title"),
        None,
        sw("title-full", p, s.title_full_path, |s, v| s.title_full_path = v),
    ));
    v.push(card(
        p,
        L::SquareTerminal,
        t!("Run commands from the address bar"),
        Some(t!("Text typed into the address bar that is not an existing place is looked up as a program (cmd, powershell, wt…) and started in the open folder; confirmed with Enter")),
        sw("addr-cmd", p, s.address_commands, |s, v| s.address_commands = v),
    ));
    v.push(card(
        p,
        L::FileArchive,
        t!("Open archives like folders"),
        Some(t!("Double-click a ZIP, 7z or tar file to browse it; files you open inside are extracted to a temporary folder")),
        sw("browse-arc", p, s.browse_archives, |s, v| s.browse_archives = v),
    ));
    if s.browse_archives {
        use crate::settings::ArchiveOpen as A;
        v.push(card(
            p,
            L::AppWindow,
            t!("Where archives open"),
            None,
            dropdown("archives-open-in", p, s.archives_open_in, vec![(A::Here, t!("In the same tab").to_string()), (A::NewTab, t!("In a new tab").to_string()), (A::NewWindow, t!("In a new window").to_string())], |s, v| {
                s.archives_open_in = v
            }),
        ));
    }
    {
        use crate::settings::QuickCompress as Q;
        let mut options = vec![(Q::Menu, t!("Only open the menu").to_string()), (Q::Zip, "ZIP".into()), (Q::SevenZ, "7z".into()), (Q::TarZst, "tar.zst".into()), (Q::TarGz, "tar.gz".into()), (Q::Tar, "tar".into())];
        if crate::archive_view::winrar().is_some() || s.quick_compress == Q::Rar {
            options.push((Q::Rar, "RAR (WinRAR)".into()));
        }
        v.push(card(
            p,
            L::Archive,
            t!("Clicking Compress"),
            Some(t!("A click on Compress in the right-click menu compresses the selection in this format at once; the other ways open when you hover over it")),
            dropdown("quick-compress", p, s.quick_compress, options, |s, v| s.quick_compress = v),
        ));
    }
    v.push(card(
        p,
        L::FolderOutput,
        t!("Open the folder after extracting"),
        Some(t!("When an archive is extracted into a new folder, that folder opens in a new tab")),
        sw("open-extracted", p, s.open_after_extract, |s, v| s.open_after_extract = v),
    ));
    v.push(card(
        p,
        L::Search,
        t!("Use the system search index"),
        Some(t!("In indexed folders, file contents are searched too and filters like kind:picture or size:>10MB work")),
        sw("search-index", p, s.search_index, |s, v| s.search_index = v),
    ));
    v.push(card(
        p,
        L::Zap,
        t!("Fast search index"),
        Some(&name_index_status()),
        sw("name-index", p, s.name_index, |s, v| s.name_index = v),
    ));
    if s.name_index {
        v.push(index_exclusions(p, s));
    }
    v.push(card(
        p,
        L::HardDrive,
        t!("Size format"),
        None,
        dropdown(
            "size-fmt",
            p,
            s.decimal_sizes,
            vec![(false, t!("Binary (1 KB = 1024 bytes)").into()), (true, t!("Decimal (1 KB = 1000 bytes)").into())],
            |s, v| s.decimal_sizes = v,
        ),
    ));
    v.push(card(
        p,
        L::Clock,
        t!("Tint fresh changes"),
        Some(t!("A faint tint on the rows of files changed in the last ten minutes; it fades as they get older.")),
        sw("fresh-tint", p, s.fresh_tint, |s, v| s.fresh_tint = v),
    ));
    let pp = *p;
    v.push(expander(
        "folder-sizes",
        p,
        L::FolderSearch,
        t!("Calculate folder sizes"),
        Some(t!("The Size column shows the total size of folders")),
        Some(sw("calc-sizes", p, s.calculate_folder_sizes, |s, v| s.calculate_folder_sizes = v)),
        move || {
            vec![
                h_flex()
                    .mx_4()
                    .my_2()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .bg(hsla_alpha(0xFCE100, 0.12))
                    .child(Icon::new(L::CircleAlert).size(px(14.)).text_color(rgb(0xFCE100)))
                    .child(div().text_xs().text_color(pp.text).child(t!("Large folders can increase disk and CPU usage; calculation runs in the background and the interface stays responsive.")))
                    .into_any_element(),
            ]
        },
        cx,
    ));

    v.push(card(
        p,
        L::HardDrive,
        t!("Show size on disk for folders"),
        Some(t!("The Size column shows folders by the space they take on disk (rounded up to whole clusters) instead of the sum of their file sizes; sorting follows it")),
        sw("folder-on-disk", p, s.folder_size_on_disk, |s, v| s.folder_size_on_disk = v),
    ));

    // Files' folder aliases: "@name" in the address box
    v.push(section(p, t!("Drives")));
    v.push(hidden_drives_card(p, s));
    if !s.hidden_places.is_empty() {
        v.push(hidden_places_card(p, s));
    }
    v.push(section(p, t!("Folder aliases")));
    let aliases: Vec<(String, std::path::PathBuf)> = s.folder_aliases.iter().map(|(n, p)| (n.clone(), p.clone())).collect();
    let count = aliases.len();
    v.push(expander(
        "aliases",
        p,
        L::AtSign,
        t!("Aliases"),
        Some(t!("Type @name in the address bar to go to a folder. To add one, right-click a folder and choose \"Add alias…\".")),
        Some(div().text_sm().text_color(p.text_secondary).child(count.to_string()).into_any_element()),
        move || {
            if aliases.is_empty() {
                return vec![sub(&pp, t!("No aliases yet"), None, div().into_any_element())];
            }
            aliases
                .into_iter()
                .enumerate()
                .map(|(i, (name, path))| {
                    let key = name.clone();
                    sub(
                        &pp,
                        &format!("@{name}"),
                        Some(&path.to_string_lossy()),
                        button(("alias-rm", i), &pp, t!("Remove"), move |_, cx| {
                            let key = key.clone();
                            settings::update(cx, move |s| {
                                s.folder_aliases.remove(&key);
                            });
                        }),
                    )
                })
                .collect()
        },
        cx,
    ));
    v
}

fn actions(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    let mut grid = div().px_4().py_3().flex().flex_wrap().gap_x_6().gap_y_0p5();
    let capturing = crate::keymap_editor::capturing(cx);
    let entries = crate::registry::shortcut_entries();
    let conflicts = crate::keymap_editor::conflicts(&entries, &|a| crate::keymap_editor::custom_keys(a, cx).or_else(|| crate::custom_actions::file_shortcut(a, cx)));
    for (i, (label, keys, action)) in entries.into_iter().enumerate() {
        let custom = crate::keymap_editor::custom_keys(&action, cx).or_else(|| crate::custom_actions::file_shortcut(&action, cx));
        // another command has the same keys: both are marked, hovering says which
        let clash = conflicts.get(&action).map(|other| format!("{} {}", t!("Same shortcut:"), crate::i18n::t(other)));
        let waiting = capturing.as_deref() == Some(action.as_str());
        let shown: SharedString = if waiting {
            t!("Press keys…").into()
        } else if let Some(k) = &custom {
            crate::keymap_editor::pretty(k).into()
        } else if keys.is_empty() {
            "—".into()
        } else {
            keys.into()
        };
        let act = action.clone();
        let reset_act = action.clone();
        grid = grid.child(
            h_flex()
                .id(("kb", i))
                .w(px(380.))
                .h(px(32.))
                .px_2()
                .rounded(theme::RADIUS_SM)
                .justify_between()
                .items_center()
                .text_sm()
                .cursor_pointer()
                .hover(|s| s.bg(p.pressed))
                .when(waiting, |d| d.bg(p.selection))
                .on_click(move |_, window, cx| {
                    crate::keymap_editor::start(act.clone(), cx);
                    window.refresh();
                })
                .child(crate::i18n::t(label))
                .child(
                    h_flex()
                        .id(("kb-keys", i))
                        .gap_1()
                        .items_center()
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(p.border)
                                .text_xs()
                                .text_color(if clash.is_some() && !waiting {
                                    p.danger
                                } else if waiting || custom.is_some() {
                                    p.accent
                                } else {
                                    p.text_secondary
                                })
                                .when(clash.is_some() && !waiting, |d| d.border_color(p.danger))
                                .child(shown),
                        )
                        .when_some(clash.filter(|_| !waiting), |d, tip| {
                            d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                        })
                        // (a shortcut that came from actions.toml is changed there: nothing here to reset)
                        .when(crate::keymap_editor::custom_keys(&reset_act, cx).is_some() && !waiting, |d| {
                            d.child(
                                div()
                                    .id(("kb-reset", i))
                                    .px_1()
                                    .rounded(theme::RADIUS_SM)
                                    .text_xs()
                                    .text_color(p.text_secondary)
                                    .hover(|s| s.bg(p.pressed))
                                    .child("↺")
                                    .on_click(move |_, window, cx| {
                                        cx.stop_propagation();
                                        crate::keymap_editor::reset(&reset_act, cx);
                                        window.refresh();
                                    }),
                            )
                        }),
                ),
        );
    }
    vec![
        card(
            p,
            L::Clock,
            t!("Show past folders in the command palette"),
            Some(t!("As you type, folders you went to before are listed with the commands. Ctrl+P always searches past folders only.")),
            sw("palette-folders", p, s.palette_folder_history, |s, v| s.palette_folder_history = v),
        ),
        shell(p)
            .child(card_body(
                p,
                L::Keyboard,
                t!("Keyboard shortcuts").into(),
                Some(t!("To change a shortcut, click a command and press the new keys (Esc: cancel). ↺ restores the default.").into()),
                None,
            ))
            .child(div().h(px(1.)).bg(p.divider))
            .child(grid)
            .into_any_element(),
    ]
}

fn tags(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let defs = cx.global::<crate::tags::Tags>().0.defs.clone();
    let mut list = v_flex().py_1();
    for d in defs {
        let (id, name, color) = (d.id, d.name.clone(), d.color);
        let rename_name = name.clone();
        list = list.child(
            h_flex()
                .min_h(px(48.))
                .pl(px(56.))
                .pr_4()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .id(("tag-color", id as usize))
                        .size(px(18.))
                        .rounded_full()
                        .bg(rgb(color))
                        .cursor_pointer()
                        .on_click(move |ev: &ClickEvent, window, cx| {
                            let items = TAG_COLORS
                                .iter()
                                .map(|(n, c)| {
                                    let c = *c;
                                    Item::action(crate::i18n::t(n), MenuIcon::Dot(rgb(c).into()), move |_, cx| crate::tags::update(cx, |t| t.recolor(id, c)))
                                        .checked(c == color)
                                })
                                .collect();
                            fluent_menu::show(Menu { quick: Vec::new(), items }, ev.position(), window, cx);
                        }),
                )
                .child(div().flex_1().text_sm().child(crate::i18n::td(&name)))
                .child(button(("tag-edit", id as usize), p, t!("Rename"), move |window, cx| {
                    tag_name_dialog(Some((id, rename_name.clone())), window, cx)
                }))
                .child(button(("tag-del", id as usize), p, t!("Delete"), move |window, cx| {
                    crate::prompt::show(
                        t!("Delete tag?"),
                        Some(t!("The tag is removed from all items. Files are not deleted.").to_string()),
                        None,
                        vec![
                            crate::prompt::Button::new(t!("Delete"), crate::prompt::Kind::Danger, move |_, _, cx| {
                                crate::tags::update(cx, |t| t.remove(id));
                                Ok(())
                            }),
                            crate::prompt::Button::cancel(),
                        ],
                        window,
                        cx,
                    )
                })),
        );
    }
    vec![
        shell(p)
            .child(card_body(
                p,
                L::Tag,
                t!("Tags").into(),
                Some(t!("Group files independently of folders using colored tags. Click a color to change it.").into()),
                Some(button("tag-new", p, t!("New tag"), |window, cx| tag_name_dialog(None, window, cx))),
            ))
            .child(div().h(px(1.)).bg(p.divider))
            .child(list)
            .into_any_element(),
    ]
}

const TAG_COLORS: [(&str, u32); 8] = [
    ("Red", 0xFF453A),
    ("Orange", 0xFF9F0A),
    ("Yellow", 0xFFD60A),
    ("Green", 0x30D158),
    ("Turquoise", 0x40C8E0),
    ("Blue", 0x0A84FF),
    ("Purple", 0xBF5AF2),
    ("Gray", 0x8E8E93),
];

fn tag_name_dialog(existing: Option<(u32, String)>, window: &mut Window, cx: &mut App) {
    let title = if existing.is_some() { t!("Rename tag") } else { t!("New tag") };
    let initial = existing.as_ref().map(|e| e.1.clone()).unwrap_or_default();
    crate::prompt::show(
        title,
        None,
        Some((initial, t!("Tag name").to_string())),
        vec![
            crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |value, _, cx| {
                let name = value.unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    return Err(t!("Enter a name").to_string());
                }
                match existing {
                    Some((id, _)) => crate::tags::update(cx, |t| t.rename(id, &name)),
                    None => crate::tags::update(cx, |t| {
                        t.add(&name, TAG_COLORS[t.defs.len() % TAG_COLORS.len()].1);
                    }),
                }
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

fn dev(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    // The editors that are installed (`leaf_sys::editors`): each is a button that chooses it, and "automatic" is the first of them.
    // The look for them asks the disk and the registry, so it runs on another thread and the page is drawn again when the list
    // is another one; while the page is open it is repeated now and then (an editor installed meanwhile shows up).
    static LOOKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let (editors, fresh) = leaf_sys::editors::peek();
    if !fresh && !LOOKING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        cx.spawn(async move |cx| {
            let changed = cx.background_executor().spawn(async { leaf_sys::editors::refresh() }).await;
            LOOKING.store(false, std::sync::atomic::Ordering::Relaxed);
            if changed {
                let _ = cx.update(|cx| cx.refresh_windows());
            }
        })
        .detach();
    }
    let ide_label = if s.ide_path.is_empty() {
        match editors.first() {
            Some(editor) => format!("{}: {}", t!("Automatic"), editor.name),
            None => t!("VS Code (automatic)").to_string(),
        }
    } else {
        match leaf_sys::editors::name_of(&editors, std::path::Path::new(&s.ide_path)) {
            // (one of the installed editors: its name says it; its path beside it broke over three lines)
            Some(name) => name.to_string(),
            None => s.ide_path.clone(),
        }
    };
    let choices = editors.iter().enumerate().map(|(i, editor)| {
        let exe = editor.exe.display().to_string();
        button(("ide-editor", i), p, editor.name, move |_, cx| {
            let exe = exe.clone();
            settings::update(cx, move |s| s.ide_path = exe)
        })
    });
    let installed = leaf_sys::shell::installed_terminals();
    let mut terminal_cards = vec![card(
        p,
        L::SquareTerminal,
        t!("Terminal"),
        Some(t!("What \"Open in terminal\" starts: from the context menu, Ctrl+Shift+`, the command palette and the button next to the address. Only the terminals found on this PC are listed.")),
        dropdown(
            "terminal-choice",
            p,
            s.terminal,
            installed.iter().map(|t| (*t, crate::i18n::t(t.label()).to_string())).collect(),
            |s, v| s.terminal = v,
        ),
    )];
    if s.terminal == rf_core::terminal::Terminal::Custom {
        let shown = if s.terminal_custom.is_empty() { t!("Not set: Automatic is used").to_string() } else { s.terminal_custom.clone() };
        let current = s.terminal_custom.clone();
        terminal_cards.push(card(
            p,
            L::Pencil,
            t!("Custom terminal command"),
            Some(&shown),
            button("terminal-custom", p, t!("Edit"), move |window, cx| {
                crate::prompt::show(
                    t!("Custom terminal command"),
                    Some(t!("The program and its arguments; {path} stands for the folder, for example: wezterm start --cwd \"{path}\"").to_string()),
                    Some((current.clone(), t!("program arguments").to_string())),
                    vec![
                        crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                            let v = value.unwrap_or_default().trim().to_string();
                            settings::update(cx, move |s| s.terminal_custom = v);
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            }),
        ));
    }
    let mut cards = terminal_cards;
    cards.extend(vec![
        card(
            p,
            L::Code,
            t!("\"Open in editor\" button in the status bar"),
            None,
            dropdown(
                "ide-btn",
                p,
                s.ide_button,
                vec![
                    (IdePref::GitRepos, t!("Show in Git repositories").into()),
                    (IdePref::Always, t!("Always show").into()),
                    (IdePref::Never, t!("Don't show").into()),
                ],
                |s, v| s.ide_button = v,
            ),
        ),
        card(
            p,
            L::SquareTerminal,
            t!("Editor"),
            Some(&ide_label),
            h_flex()
                .gap_2()
                .flex_wrap()
                .justify_end()
                .children(choices)
                .child(button("ide-browse", p, t!("Browse"), |window, cx| pick_into(window, cx, t!("Choose editor"), PickTarget::Ide)))
                .child(button("ide-reset", p, t!("Automatic"), |_, cx| settings::update(cx, |s| s.ide_path.clear())))
                .into_any_element(),
        ),
        card(
            p,
            L::Lock,
            t!("Lock mark on secret files"),
            Some(t!("A small lock after the name of a file that looks like it holds a secret: .env, private keys, credentials, password stores. Copy as context never takes these in.")),
            sw("secret-marks", p, s.secret_marks, |s, v| s.secret_marks = v),
        ),
        card(
            p,
            L::GitCompare,
            t!("Git marks after names"),
            Some(t!("Inside a git repository, a small letter after the name of what changed: M changed, A added, ? new and not tracked, U in conflict, D deleted, R renamed. Folders show it when something inside changed.")),
            sw("git-marks", p, s.git_marks, |s, v| s.git_marks = v),
        ),
        card(
            p,
            L::Hash,
            t!("Show token estimate"),
            Some(t!("The status line shows about how many tokens the selected files are (bytes / 4), for pasting into a chat.")),
            sw("token-estimate", p, s.show_token_estimate, |s, v| s.show_token_estimate = v),
        ),
        card(
            p,
            L::Brain,
            t!("Agent layer"),
            Some(t!("Shows what coding agents (Claude Code) are doing: the Agents page, a small dot on the folder and the tab of a running session, and a line in the status bar. Reads the agent's own session files on this PC, never changes them. Off: nothing is read and nothing is shown.")),
            sw("ai-layer", p, s.ai_layer, |s, v| s.ai_layer = v),
        ),
        card(
            p,
            L::Brain,
            t!("Agent budget ring"),
            Some(t!("A small ring in the status bar: how much of this daily amount of new agent tokens (input, output, cache writes; an estimate from the session files) the last 24 hours used. It never warns and never blocks anything.")),
            dropdown(
                "agent-budget-k",
                p,
                s.agent_budget_k,
                [0u32, 500, 1000, 2000, 5000, 10000, 20000, 50000].into_iter().map(|k| (k, if k == 0 { t!("No ring").to_string() } else { crate::agent_usage::short(u64::from(k) * 1000) })).collect(),
                |s, v| s.agent_budget_k = v,
            ),
        ),
        {
            let current = s.lane_folder.clone();
            card(
                p,
                L::GitBranchPlus,
                t!("Lane folders"),
                Some(&format!("{} {}", t!("Where New lane puts a lane's worktree:"), s.lane_folder)),
                button("lane-folder", p, t!("Edit"), move |window, cx| {
                    crate::prompt::show(
                        t!("Lane folders"),
                        Some(t!("<repo> is the repository's folder, <name> the lane's name. A relative pattern starts in the repository's parent folder.").to_string()),
                        Some((current.clone(), "<repo>-wt/<name>".to_string())),
                        vec![
                            crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                                let v = value.unwrap_or_default().trim().to_string();
                                if !v.contains("<name>") {
                                    return Err(t!("The pattern needs <name>, so that every lane gets a folder of its own").to_string());
                                }
                                settings::update(cx, move |s| s.lane_folder = v);
                                Ok(())
                            }),
                            crate::prompt::Button::cancel(),
                        ],
                        window,
                        cx,
                    );
                }),
            )
        },
        card(
            p,
            L::ListTree,
            t!("Selection details"),
            Some(t!("The status line adds the lines of the selected text files, the play time of audio and video and the pixels of pictures; a click on it shows the breakdown by type. Read in the background, with a limit for big selections.")),
            sw("selection-details", p, s.selection_details, |s, v| s.selection_details = v),
        ),
        card(
            p,
            L::FileText,
            t!("Copy as context: biggest file"),
            Some(t!("Files above this size are listed as left out.")),
            dropdown(
                "context-file-kb",
                p,
                s.context_max_file_kb,
                [64u32, 128, 256, 512, 1024].into_iter().map(|k| (k, kb_text(k))).collect(),
                |s, v| s.context_max_file_kb = v,
            ),
        ),
        card(
            p,
            L::FileText,
            t!("Copy as context: most text"),
            Some(t!("The copy stops with a message when the text would pass this.")),
            dropdown(
                "context-total-kb",
                p,
                s.context_max_total_kb,
                [512u32, 1024, 2048, 4096, 8192].into_iter().map(|k| (k, kb_text(k))).collect(),
                |s, v| s.context_max_total_kb = v,
            ),
        ),
        card(
            p,
            L::FolderClock,
            t!("Checkpoints to keep"),
            Some(t!("For each folder or repository. Taking one more removes the oldest.")),
            dropdown(
                "checkpoint-keep",
                p,
                s.checkpoint_keep,
                [5u32, 10, 20, 50, 100].into_iter().map(|n| (n, n.to_string())).collect(),
                |s, v| s.checkpoint_keep = v,
            ),
        ),
        card(
            p,
            L::FolderClock,
            t!("Checkpoint size limit"),
            Some(t!("Outside a git repository a checkpoint is a copy of the folder in leaf's data folder; a bigger folder is refused. In a repository git keeps each content once and there is no limit.")),
            dropdown(
                "checkpoint-cap",
                p,
                s.checkpoint_cap_mb,
                [256u32, 512, 1024, 2048, 4096, 8192, 16384].into_iter().map(|m| (m, if m >= 1024 { format!("{} GB", m / 1024) } else { format!("{m} MB") })).collect(),
                |s, v| s.checkpoint_cap_mb = v,
            ),
        ),
    ]);
    cards
}

fn kb_text(k: u32) -> String {
    if k >= 1024 { format!("{} MB", k / 1024) } else { format!("{k} KB") }
}

/// leaf-look ayarı anahtarı (ayrı dosya `leaf-look.toml`; değişince leaf-look'a haber verilir).
fn lsw(id: &'static str, p: &Palette, on: bool, set: fn(&mut LookSettings, bool)) -> AnyElement {
    switch(id, p, on, move |_, cx| look_update(cx, |s| set(s, !on)))
}

fn look_update(cx: &mut App, f: impl FnOnce(&mut LookSettings)) {
    crate::look::config::update(cx, f);
    crate::look::ipc::send(crate::look::ipc::Msg::Reload, false);
}

fn ldropdown<T: Copy + PartialEq + 'static>(id: &'static str, p: &Palette, current: T, options: Vec<(T, String)>, set: fn(&mut LookSettings, T)) -> AnyElement {
    let label = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone()).unwrap_or_default();
    select_box(id, p, label, move |_| {
        options.iter().map(|(v, l)| {
            let v = *v;
            Choice::new(l.clone(), v == current, move |_, cx| look_update(cx, |s| set(s, v)))
        }).collect()
    })
}

/// leaf-look (Hızlı Bakış, ayrı program) ayarları.
fn look(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings::{LookBackdrop, LookImageTheme};
    // leaf-look da yazabilir: sayfa her açılışta dosyadan okunur
    let l = LookSettings::load();
    let enabled = s.look_enabled;
    let mut v = vec![card(
        p,
        L::Eye,
        t!("Use Preview"),
        Some(t!("Preview with Space. Runs as a separate program: it doesn't slow leaf down, and if one closes the other is unaffected.")),
        switch("dl-enabled", p, enabled, move |_, cx| {
            settings::update(cx, |s| s.look_enabled = !enabled);
            if enabled {
                crate::look::ipc::send(crate::look::ipc::Msg::Quit, false);
            } else {
                crate::look::ipc::send(crate::look::ipc::Msg::Wake, true);
            }
        }),
    )];
    // QuickLook instead (asked on Discord): shown with leaf-look off too, as that is how it is meant to be used
    let ql = s.space_quicklook;
    v.push(win_only(card(
        p,
        L::ScanEye,
        t!("Space opens QuickLook"),
        Some(if crate::quicklook::installed() {
            t!("Files go to QuickLook, and an open preview follows the selection. You can turn Preview off above.")
        } else {
            t!("QuickLook is not installed: get it from the Microsoft Store or GitHub (QL-Win/QuickLook).")
        }),
        switch("space-ql", p, ql, move |_, cx| settings::update(cx, |s| s.space_quicklook = !ql)),
    )));
    if !enabled {
        return v;
    }
    v.push(card(
        p,
        L::Monitor,
        t!("Preview with Space outside leaf too (other file managers, the desktop)"),
        Some(t!("Preview keeps running in the background and opens when you sign in (icon in the system tray), even while leaf is closed")),
        switch("look-wide", p, l.look_system_wide, {
            let on = l.look_system_wide;
            move |_, cx| {
                look_update(cx, |s| s.look_system_wide = !on);
                if let Some(exe) = crate::look::ipc::exe() {
                    if let Err(e) = leaf_sys::dialogs::set_start_with_windows_as("leaf-look", &exe, &["--background"], !on) {
                        log::warn!("Preview start with Windows: {e}");
                    }
                }
                crate::look::ipc::send(crate::look::ipc::Msg::Wake, true);
            }
        }),
    ));
    if l.look_system_wide && !l.look_ignore_quicklook && crate::look::system::quicklook_running() {
        v.push(card(
            p,
            L::TriangleAlert,
            t!("QuickLook is running"),
            Some(t!("The system-wide shortcut was not started so that two apps don't listen for the same key")),
            button("look-force", p, t!("Turn on anyway"), |window, cx| {
                look_update(cx, |s| s.look_ignore_quicklook = true);
                window.refresh();
            }),
        ));
    }
    let _ = cx;
    v.extend([
        section(p, t!("Window")),
        card(
            p,
            L::Layers,
            t!("Background"),
            None,
            ldropdown(
                "look-backdrop",
                p,
                l.look_backdrop,
                vec![
                    // (Mica and Acrylic are Windows' names)
                    (LookBackdrop::Mica, if cfg!(windows) { "Mica".into() } else { t!("Frosted").into() }),
                    (LookBackdrop::Acrylic, if cfg!(windows) { "Acrylic".into() } else { t!("Blurred").into() }),
                    (LookBackdrop::Tabbed, if cfg!(windows) { t!("Mica (tabbed)").into() } else { t!("Frosted (tabbed)").into() }),
                    (LookBackdrop::None, t!("Solid color").into()),
                ],
                |s, v| s.look_backdrop = v,
            ),
        ),
        card(p, L::PictureInPicture, t!("Always on top"), None, lsw("look-top", p, l.look_topmost, |s, v| s.look_topmost = v)),
        card(p, L::PanelBottom, t!("Show in the taskbar"), None, lsw("look-taskbar", p, l.look_show_in_taskbar, |s, v| s.look_show_in_taskbar = v)),
        card(
            p,
            L::AppWindow,
            t!("Preview icon in the system tray"),
            Some(t!("Not shown while leaf runs in the background: then leaf's icon is the only one")),
            lsw("look-tray", p, l.look_tray_icon, |s, v| s.look_tray_icon = v),
        ),
        card(p, L::MousePointerClick, t!("Focus the window when it opens"), Some(t!("When off, the keyboard stays in the other program: arrow keys move between files")), lsw("look-focus", p, l.look_focus_on_open, |s, v| s.look_focus_on_open = v)),
        card(p, L::CircleX, t!("Close when another window is activated"), Some(t!("Pinned windows stay open")), lsw("look-lostfocus", p, l.look_close_on_lost_focus, |s, v| s.look_close_on_lost_focus = v)),
        card(p, L::Zap, t!("Transition animation"), None, lsw("look-transition", p, l.look_transition, |s, v| s.look_transition = v)),
        card(p, L::RefreshCw, t!("Reload button"), None, lsw("look-reload", p, l.look_show_reload, |s, v| s.look_show_reload = v)),
        card(p, L::FolderSync, t!("Reload when the file changes"), None, lsw("look-autoreload", p, l.look_auto_reload, |s, v| s.look_auto_reload = v)),
        section(p, t!("Contains")),
        card(
            p,
            L::Image,
            t!("Image background"),
            None,
            ldropdown(
                "look-imgtheme",
                p,
                l.look_image_theme,
                vec![(LookImageTheme::Dark, t!("Dark").into()), (LookImageTheme::Light, t!("On").into())],
                |s, v| s.look_image_theme = v,
            ),
        ),
        card(p, L::RotateCw, t!("Loop videos"), None, lsw("look-loop", p, l.look_loop, |s, v| s.look_loop = v)),
        card(p, L::List, t!("Line numbers in text"), Some(t!("Alt+L in the preview")), lsw("look-lines", p, l.look_text_line_numbers, |s, v| s.look_text_line_numbers = v)),
        card(p, L::TextAlignJustify, t!("Word wrap in text"), Some(t!("Alt+Z in the preview")), lsw("look-wrap", p, l.look_text_wrap, |s, v| s.look_text_wrap = v)),
    ]);
    // text font and size
    let font_label = if l.look_text_font.is_empty() { t!("Default").to_string() } else { l.look_text_font.clone() };
    let cur_font = l.look_text_font.clone();
    let fonts = font_choices(cx);
    v.push(card(
        p,
        L::Type,
        t!("Text font"),
        None,
        select_box("look-font", p, font_label, move |_| {
            std::iter::once(String::new())
                .chain(fonts.iter().cloned())
                .map(|f| {
                    let label = if f.is_empty() { t!("Default").to_string() } else { f.clone() };
                    let sel = f == cur_font;
                    Choice::new(label, sel, move |_, cx| {
                        let f = f.clone();
                        look_update(cx, |s| s.look_text_font = f)
                    })
                })
                .collect()
        }),
    ));
    let cur_size = l.look_text_font_size.round() as u32;
    v.push(card(
        p,
        L::ALargeSmall,
        t!("Text size"),
        None,
        select_box("look-font-size", p, format!("{cur_size} px"), move |_| {
            [11u32, 12, 13, 14, 15, 16, 18, 20, 24]
                .into_iter()
                .map(|n| Choice::new(format!("{n} px"), n == cur_size, move |_, cx| look_update(cx, |s| s.look_text_font_size = n as f32)))
                .collect()
        }),
    ));
    // which file types leaf-look previews
    v.push(card(
        p,
        L::ListFilter,
        t!("Files to preview"),
        None,
        ldropdown(
            "look-ext-mode",
            p,
            l.look_ext_allow_mode,
            vec![(false, t!("All except those listed").to_string()), (true, t!("Only those listed").to_string())],
            |s, v| s.look_ext_allow_mode = v,
        ),
    ));
    let allow = l.look_ext_allow_mode;
    let list = if allow { l.look_ext_allowlist.clone() } else { l.look_ext_blocklist.clone() };
    let shown = if list.trim().is_empty() { "—".to_string() } else { list.clone() };
    v.push(card(
        p,
        L::CircleSlash,
        if allow { t!("Extensions to preview") } else { t!("Extensions not to preview") },
        Some(&shown),
        button("look-ext-edit", p, t!("Edit…"), move |window, cx| {
            let ok = crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |text, _, cx| {
                let text = normalize_exts(&text.unwrap_or_default());
                look_update(cx, |s| if allow { s.look_ext_allowlist = text } else { s.look_ext_blocklist = text });
                Ok(())
            });
            crate::prompt::show(
                if allow { t!("Extensions to preview") } else { t!("Extensions not to preview") },
                Some(t!("Separate with semicolons, for example: .iso; .insv; .psd").to_string()),
                Some((list.clone(), ".iso; .insv".to_string())),
                vec![ok, crate::prompt::Button::cancel()],
                window,
                cx,
            );
        }),
    ));
    v
}

/// "iso, .PSD ;; insv" → ".iso;.psd;.insv": dots added, lowercase, no duplicates.
fn normalize_exts(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for part in text.split([';', ',', ' ']) {
        let p = part.trim().trim_start_matches('*').to_lowercase();
        if p.is_empty() || p == "." {
            continue;
        }
        let e = if p.starts_with('.') { p } else { format!(".{p}") };
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out.join(";")
}

#[cfg(test)]
mod ext_tests {
    #[test]
    fn extension_lists_are_normalized() {
        assert_eq!(super::normalize_exts("iso, .PSD ;; insv *.Mkv .iso"), ".iso;.psd;.insv;.mkv");
        assert_eq!(super::normalize_exts("  "), "");
    }
}

/// A card that only means something on Windows (start with Windows, WSL, the default file manager, QuickLook): elsewhere it is an empty
/// strip that cancels the page's gap, so nothing shows and nothing is left over.
fn win_only(card: AnyElement) -> AnyElement {
    if cfg!(windows) { card } else { div().h_0().mt(px(-8.)).into_any_element() }
}

fn advanced(p: &Palette, s: &Settings, cx: &App) -> Vec<AnyElement> {
    let start_on = leaf_sys::dialogs::starts_with_windows();
    let load = hybrid::load_snapshot();
    let pct = |v: Option<f32>| v.map(crate::i18n::pct).unwrap_or_else(|| "—".into());
    let live = format!(
        "{} {} · {} {} · {} {} · {} {:.1} ms · {} {}",
        t!("Now:"),
        hybrid::current_backend_label(),
        t!("GPU load"),
        pct(load.gpu_percent),
        t!("CPU load"),
        pct(load.cpu_percent),
        t!("last frame"),
        load.last_frame_ms,
        hybrid::switch_count(),
        t!("switches")
    );
    let dfm_on = crate::DEFAULT_FM.load(std::sync::atomic::Ordering::Relaxed);
    // a copy run from a cargo build folder without the launcher (leaf-open.exe) beside it is not turned into the folder handler:
    // each rebuild replaces the exe, a clean removes it, and Windows would be left with no program for folders until leaf starts
    // again. With the launcher, folders open in Explorer meanwhile.
    let dfm_dev = !dfm_on
        && std::env::current_exe().is_ok_and(|e| is_build_folder_exe(&e) && !leaf_sys::default_fm::launcher_beside(&e))
        && std::env::var_os("LEAF_DFM_ALLOW_BUILD").is_none();
    // while leaf is the default file manager: archives can be given to it too (only the
    // user can pick the default app, in Windows' own page)
    let archives = (dfm_on && cfg!(windows)).then(|| {
        card(
            p,
            L::FileArchive,
            t!("Open archives with leaf"),
            Some(t!("The default app for zip, 7z, rar and tar is chosen in the system settings. Make leaf the default on the page that opens.")),
            button("dfm-arch", p, t!("Default apps"), |_, _| {
                let page = format!("ms-settings:defaultapps?registeredAppUser={}", leaf_sys::default_fm::APP_NAME);
                if let Err(e) = leaf_sys::shell::open(std::path::Path::new(&page)) {
                    log::warn!("default apps page: {e:#}");
                }
            }),
        )
    });
    let mut out = vec![
        link_row(p, "export", L::Upload, t!("Export settings"), None, |window, cx| pick_into(window, cx, t!("Export settings"), PickTarget::Export)),
        link_row(p, "import", L::Download, t!("Import settings"), None, |window, cx| pick_into(window, cx, t!("Import settings"), PickTarget::Import)),
        link_row(p, "edit-file", L::FilePen, t!("Edit settings file"), Some(t!("settings.toml opens in a text editor")), |_, _| {
            if let Some(path) = settings::path() {
                std::thread::spawn(move || {
                    #[cfg(windows)]
                    let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
                    #[cfg(not(windows))]
                    let _ = leaf_sys::shell::open(&path);
                });
            }
        }),
        card(
            p,
            L::Power,
            t!("Open leaf when you sign in"),
            // (Linux: the session's autostart folder)
            if cfg!(windows) { None } else { Some(t!("Adds leaf to your session's autostart (~/.config/autostart). Turn it off to remove it.")) },
            switch("start-win", p, start_on, move |window, _| {
                if let Ok(exe) = std::env::current_exe() {
                    if let Err(e) = leaf_sys::dialogs::set_start_with_windows(&exe, !start_on) {
                        log::warn!("start with Windows: {e}");
                    }
                }
                window.refresh();
            }),
        ),
        card(
            p,
            L::PanelBottom,
            t!("Keep running in the background when the window is closed"),
            Some(t!("An icon stays in the system tray; new windows open instantly")),
            sw("background", p, s.run_in_background, |s, v| s.run_in_background = v),
        ),
        card(
            p,
            L::Bell,
            t!("Notify when long operations finish"),
            Some(t!("When copying, moving or compressing took over 10 seconds and leaf isn't in front")),
            sw("notify-ops", p, s.notify_long_ops, |s, v| s.notify_long_ops = v),
        ),
        card(
            p,
            L::ListChecks,
            t!("Transfer progress"),
            Some(t!("Where the progress of a running copy, move or delete is shown. The ring on the Status Center button works in every case.")),
            dropdown(
                "transfer-progress",
                p,
                s.transfer_progress,
                vec![
                    (crate::settings::TransferProgress::StatusBar, t!("Status bar").into()),
                    (crate::settings::TransferProgress::FloatingCard, t!("Floating card").into()),
                    (crate::settings::TransferProgress::StatusCenter, t!("Status Center only").into()),
                ],
                |s, v| s.transfer_progress = v,
            ),
        ),
        section(p, t!("Performance")),
        card(
            p,
            L::Zap,
            t!("Fast file operations"),
            Some(t!("Copying, moving to another drive and permanent deletion use our own parallel engine. When off, the system's engine does everything.")),
            sw("fast-ops", p, s.fast_file_ops, |s, v| s.fast_file_ops = v),
        ),
        win_only(card(
            p,
            L::SquareTerminal,
            t!("Leave no download mark when copying into WSL"),
            Some(t!("In WSL, Windows writes a separate \"Zone.Identifier\" file beside a file downloaded from the internet. When on, copying deletes it.")),
            sw("wsl-zone", p, s.wsl_drop_zone, |s, v| s.wsl_drop_zone = v),
        )),
        card(
            p,
            L::ShieldCheck,
            t!("Verify copies"),
            Some(t!("Each file is compared with its source after copying. Full verification reads the copy back from the disk and takes longer.")),
            dropdown(
                "copy-verify",
                p,
                s.copy_verify,
                vec![
                    (crate::settings::CopyVerify::Off, t!("Off").into()),
                    (crate::settings::CopyVerify::Quick, t!("Quick: size and date").into()),
                    (crate::settings::CopyVerify::Full, t!("Full: content").into()),
                ],
                |s, v| s.copy_verify = v,
            ),
        ),
        card(
            p,
            L::Gauge,
            t!("Copy speed limit"),
            Some(t!("So copies to a network share or a USB stick don't choke the connection.")),
            dropdown(
                "copy-limit",
                p,
                s.copy_speed_limit,
                std::iter::once((0, t!("Unlimited").to_string())).chain([5u32, 10, 25, 50, 100, 250].into_iter().map(|m| (m, format!("{m} MB/s")))).collect(),
                |s, v| s.copy_speed_limit = v,
            ),
        ),
        section(p, t!("Downloads")),
        card(
            p,
            L::Network,
            t!("Connections per download"),
            Some(t!("A large file starts with this many connections when the server allows it; small files use fewer. More are opened while the speed still rises, up to 32.")),
            dropdown("download-segments", p, s.download_segments, [1u32, 2, 4, 6, 8, 12, 16, 24, 32].into_iter().map(|n| (n, n.to_string())).collect(), |s, v| s.download_segments = v),
        ),
        card(
            p,
            L::Download,
            t!("Downloads at once"),
            Some(t!("The others wait in the Status Center until one is done.")),
            dropdown("download-parallel", p, s.download_parallel, [1u32, 2, 3, 4, 6, 8].into_iter().map(|n| (n, n.to_string())).collect(), |s, v| s.download_parallel = v),
        ),
        card(
            p,
            L::Gauge,
            t!("Download speed limit"),
            Some(t!("For each download, over all its connections.")),
            dropdown(
                "download-limit",
                p,
                s.download_speed_limit,
                std::iter::once((0, t!("Unlimited").to_string())).chain([1u32, 2, 5, 10, 25, 50].into_iter().map(|m| (m, format!("{m} MB/s")))).collect(),
                |s, v| s.download_speed_limit = v,
            ),
        ),
        card(
            p,
            L::FolderDown,
            t!("Download folder"),
            Some(crate::downloads::default_folder(s).to_string_lossy().into_owned().as_str()),
            h_flex()
                .gap_2()
                .children((!s.download_folder.trim().is_empty()).then(|| button("download-folder-reset", p, t!("Default"), |_, cx| crate::settings::update(cx, |s| s.download_folder.clear()))))
                .child(button("download-folder", p, t!("Change…"), |_, cx| {
                    let (tx, rx) = futures::channel::oneshot::channel::<Option<std::path::PathBuf>>();
                    let title = t!("Choose the folder for downloads");
                    leaf_sys::sta::run(move |owner| {
                        let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
                    });
                    cx.spawn(async move |cx| {
                        if let Ok(Some(path)) = rx.await {
                            let _ = cx.update(|cx| crate::settings::update(cx, |s| s.download_folder = path.to_string_lossy().into_owned()));
                        }
                    })
                    .detach();
                }))
                .into_any_element(),
        ),
        card(
            p,
            L::FolderDown,
            t!("Sort downloads by file type"),
            Some(t!("A download into the download folder is put in a subfolder by its file type (Videos, Music, ...). \"Download here\" into a folder you choose is never sorted.")),
            sw("download-sort", p, s.download_sort, |s, v| s.download_sort = v),
        ),
        download_categories_card(p, s),
        download_servers_card(p, s),
        download_proxy_card(p, s),
        download_schedule_card(p, s),
        crate::videodl::settings_card(p, s),
        crate::usenet::servers_card(p, s),
        card(
            p,
            L::Globe,
            t!("Capture browser downloads"),
            Some(t!("Downloads started in Chrome, Edge, Brave or Firefox go to leaf instead, with the browser's sign-in for that one download. Needs the leaf extension in the browser.")),
            sw("capture-browser", p, s.capture_browser, |s, v| s.capture_browser = v),
        ),
        card(
            p,
            L::Globe,
            t!("Connect browsers"),
            Some(crate::capture::status_text().as_str()),
            h_flex()
                .gap_2()
                .children((!crate::capture::registered().is_empty()).then(|| button("capture-disconnect", p, t!("Disconnect"), |_, cx| crate::capture::disconnect(cx))))
                .child(button("capture-connect", p, t!("Connect browsers"), |_, cx| crate::capture::connect(cx)))
                .into_any_element(),
        ),
        card(
            p,
            L::Gauge,
            t!("Smallest download to capture"),
            Some(t!("Smaller files stay with the browser (when the browser knows the size).")),
            dropdown(
                "capture-min",
                p,
                s.capture_min_mb,
                std::iter::once((0, t!("Every size").to_string())).chain([1u32, 5, 10, 50, 100].into_iter().map(|m| (m, format!("{m} MB")))).collect(),
                |s, v| s.capture_min_mb = v,
            ),
        ),
        card(
            p,
            L::Clock,
            t!("Download history"),
            Some(format!("{} {}", crate::downloads::history_len(), t!("finished or failed downloads are remembered (the last 50)")).as_str()),
            button("download-history-clear", p, t!("Clear"), |_, cx| {
                crate::downloads::clear_history();
                cx.refresh_windows();
            }),
        ),
        card(
            p,
            L::Activity,
            t!("Performance info in the status bar"),
            Some(t!("Folder listing time and renderer (GPU / CPU)")),
            sw("perf-info", p, s.show_perf_info, |s, v| s.show_perf_info = v),
        ),
        shell(p)
            .child(card_body(
                p,
                L::Activity,
                t!("Rendering mode").into(),
                Some(t!("Automatic: the graphics card is checked at start and, if it works, the interface is drawn on the GPU. While the GPU is busy (games, rendering) it moves to the CPU and back when the GPU is free. You won't notice the switch.").into()),
                Some(dropdown(
                    "renderer",
                    p,
                    s.renderer,
                    vec![(RendererPref::Auto, t!("Automatic").into()), (RendererPref::Gpu, "GPU".into()), (RendererPref::Cpu, "CPU".into())],
                    |s, v| s.renderer = v,
                )),
            ))
            .child(div().pl(px(56.)).pr_4().pb_3().text_xs().text_color(p.text_tertiary).child(live))
            .into_any_element(),
        section(p, t!("Experimental")),
        card(
            p,
            L::AppWindow,
            t!("Set as default file manager"),
            Some(if dfm_dev {
                t!("This copy of leaf runs from a build folder (target) and leaf-open.exe is not next to it. Every rebuild replaces leaf and a clean removes it, which would leave Windows with no program to open folders. Build leaf-open too (it is built with leaf), or turn this on from an installed leaf.")
            } else if cfg!(windows) {
                t!("Folders and Win+E open with leaf. Applies to this user only and can be reverted at any time. It also covers Win+E and 'Show in folder' from other programs, and turning it off restores what was there before.")
            } else {
                t!("Folders open with leaf (your mimeapps.list names it for inode/directory). Applies to this user only; turning it off gives folders back to the program that had them.")
            }),
            switch("dfm", p, dfm_on, move |window, _| {
                if dfm_dev {
                    log::warn!("default file manager: not turned on from a build folder");
                    return;
                }
                let exe = std::env::current_exe().map(|e| e.display().to_string()).unwrap_or_default();
                std::thread::spawn(move || {
                    let classes = leaf_sys::default_fm::CLASSES;
                    let r = if dfm_on { leaf_sys::default_fm::disable(classes) } else { leaf_sys::default_fm::enable(&exe, classes) };
                    if let Err(e) = r {
                        log::warn!("default file manager: {e:#}");
                    }
                    crate::DEFAULT_FM.store(leaf_sys::default_fm::is_enabled(classes), std::sync::atomic::Ordering::Relaxed);
                });
                window.refresh();
            }),
        ),
    ];
    out.extend(archives);
    out.extend([
        card(
            p,
            L::FolderTree,
            t!("Show the flatten option"),
            Some(t!("Moves the files from all subfolders into a single level. Ctrl+Z undoes it.")),
            sw("flatten", p, s.show_flatten, |s, v| s.show_flatten = v),
        ),
        card(
            p,
            L::ScrollText,
            t!("Log file"),
            Some(t!("Warnings and errors are written to leaf.log (1 MB at most; a full one becomes .old). It can be attached when reporting a problem.")),
            button("log-open", p, t!("Show in folder"), |_, cx| {
                if let Some(path) = crate::logfile::path() {
                    crate::workspace::route_ipc(format!("{}{}", crate::workspace::SELECT_PREFIX, path.display()), cx);
                }
            }),
        ),
    ]);
    // the thumbnail cache's limit and a clear button (thumb_cache.rs)
    out.push(section(p, t!("Cache")));
    out.extend(crate::thumb_cache::cards(p, s, cx));
    out
}

fn open_url(url: &'static str) {
    std::thread::spawn(move || {
        let _ = leaf_sys::shell::open(std::path::Path::new(url));
    });
}

/// Update status and the button that fits it (Settings → About).
fn update_card(p: &Palette, s: &Settings) -> AnyElement {
    use crate::installer::State;
    let state = crate::installer::state();
    let usable = s.update_check && crate::installer::feed_is_valid(&s.update_feed);
    let err = crate::installer::last_error();
    let text = match &state {
        State::NotInstalled => t!("This copy wasn't installed with the setup, so it can't update itself.").to_string(),
        State::Idle if !err.is_empty() => format!("{} {err}", t!("The check failed:")),
        State::Idle if !s.update_check => t!("Update checks are off: leaf contacts no server.").to_string(),
        State::Idle if !usable => t!("Write the address of the release feed to check for updates.").to_string(),
        State::Idle => t!("Updates come from your release feed.").to_string(),
        State::Checking => t!("Checking…").to_string(),
        State::Downloading(v, pct) => format!("{} {v} · %{pct}", t!("Downloading:")),
        State::Ready(v) => format!("{} {v}. {}", t!("Downloaded:"), t!("It is applied when you restart.")),
    };
    let control = match state {
        State::NotInstalled => div().into_any_element(),
        State::Ready(_) => button("upd-restart", p, t!("Restart and update"), |_, _| {
            std::thread::spawn(crate::installer::restart_to_update);
        }),
        State::Checking | State::Downloading(..) => div().into_any_element(),
        _ if !usable => div().into_any_element(),
        _ => button("upd-check", p, t!("Check for updates"), |window, cx| {
            check_updates_now(cx);
            window.refresh();
        }),
    };
    let icon = match state {
        State::Downloading(..) => L::Download,
        State::Ready(_) => L::CircleCheck,
        _ => L::RefreshCw,
    };
    // behind the row: a light sweeping while checking, a bar filling while downloading, full when done
    let tint = p.accent;
    let layer = match state {
        State::Checking => Some(
            glow(tint, 0.22)
                .w(relative(0.3))
                .with_animation(
                    "upd-sweep",
                    Animation::new(std::time::Duration::from_millis(1200)).repeat().with_easing(ease_in_out),
                    |el, t| el.left(relative(-0.3 + 1.3 * t)),
                )
                .into_any_element(),
        ),
        State::Downloading(_, pct) => Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(relative(pct as f32 / 100.0))
                .overflow_hidden()
                .bg(tint.opacity(0.18))
                .child(
                    // a soft highlight running along the filled part
                    glow(tint, 0.2)
                        .w(px(120.))
                        .with_animation(
                            "upd-run",
                            Animation::new(std::time::Duration::from_millis(1000)).repeat(),
                            |el, t| el.left(relative(-0.2 + 1.2 * t)),
                        ),
                )
                .into_any_element(),
        ),
        State::Ready(_) => Some(div().absolute().inset_0().bg(tint.opacity(0.14)).into_any_element()),
        _ => None,
    };
    shell(p)
        .relative()
        .overflow_hidden()
        .when(matches!(state, State::Ready(_)), |d| d.border_color(tint.opacity(0.6)))
        .children(layer)
        .child(card_body(p, icon, t!("Updates").into(), Some(text.into()), Some(control)))
        .into_any_element()
}

/// A soft band of light: transparent at both edges, `alpha` in the middle.
fn glow(c: Hsla, alpha: f32) -> Div {
    let half = |from: f32, to: f32| {
        div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(c.opacity(from), 0.), linear_color_stop(c.opacity(to), 1.)))
    };
    div().absolute().top_0().bottom_0().flex().child(half(0., alpha)).child(half(alpha, 0.))
}

/// "Check for updates": runs off the UI thread and redraws while it checks and downloads.
pub(crate) fn check_updates_now(cx: &mut App) {
    cx.spawn(async move |cx| {
        let (on, feed) = cx.update(|cx| {
            let s = cx.global::<Settings>();
            (s.update_check, s.update_feed.clone())
        });
        let run = cx.background_executor().spawn(async move { crate::installer::check(on, &feed) });
        crate::installer::redraw_while_busy(cx).await;
        run.await;
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// The switch "Check for leaf updates" (off by default) and the feed address it reads (empty by default).
fn update_options_card(p: &Palette, s: &Settings) -> AnyElement {
    let feed = if s.update_feed.trim().is_empty() { t!("Not set").to_string() } else { s.update_feed.trim().to_string() };
    shell(p)
        .child(card_body(
            p,
            L::RefreshCw,
            t!("Check for leaf updates").into(),
            Some(t!("Off by default. When on, and a feed address is set, the button above asks that address for a newer leaf. Nothing is contacted otherwise.").into()),
            Some(sw("update-check", p, s.update_check, |s, v| s.update_check = v)),
        ))
        .child(card_body(
            p,
            L::Globe,
            t!("Release feed").into(),
            Some(feed.into()),
            Some(button("update-feed", p, t!("Edit"), |window, cx| {
                let initial = cx.global::<Settings>().update_feed.clone();
                crate::prompt::show(
                    t!("Release feed"),
                    Some(t!("An https address, or a folder on this PC, that holds the files `vpk pack` made. Empty = no updates.").to_string()),
                    Some((initial, "https://example.com/leaf/releases".to_string())),
                    vec![
                        crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                            let text = value.unwrap_or_default().trim().to_string();
                            if !text.is_empty() && !crate::installer::feed_is_valid(&text) {
                                return Err(t!("Write an https:// address or a folder path").to_string());
                            }
                            settings::update(cx, move |s| s.update_feed = text);
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            })),
        ))
        .into_any_element()
}

const REPO_URL: &str = "https://github.com/nomfpv/leaf-filemanager";
const ISSUES_URL: &str = "https://github.com/nomfpv/leaf-filemanager/issues";

fn about(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let ver = crate::installer::version_full();
    let version = format!("{} {ver}", t!("Version:"));
    let pp = *p;
    let s = cx.global::<Settings>().clone();
    vec![
        card(
            p,
            L::FolderOpen,
            "leaf",
            Some(&version),
            button("copy-ver", p, t!("Copy"), move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(format!("leaf {ver}")))),
        ),
        update_card(p, &s),
        update_options_card(p, &s),
        section(p, t!("Project")),
        link_row(p, "gh-repo", L::Code, t!("Source code"), Some("github.com/nomfpv/leaf-filemanager"), |_, _| open_url(REPO_URL)),
        link_row(p, "gh-issue", L::MessageSquare, t!("Report a bug or suggest something"), None, |_, _| open_url(ISSUES_URL)),
        section(p, t!("Help and support")),
        link_row(p, "open-log", L::FolderOpen, t!("Open log and settings folder"), settings::app_dir().map(|d| d.display().to_string()).as_deref(), |_, _| {
            if let Some(d) = settings::app_dir() {
                std::thread::spawn(move || {
                    // the real Explorer, past our own folder verb (which would open it here again)
                    if !leaf_sys::default_fm::open_in_explorer(&d.to_string_lossy()) {
                        let _ = std::process::Command::new("explorer.exe").arg(d).spawn();
                    }
                });
            }
        }),
        section(p, t!("License")),
        div()
            .px_1()
            .text_xs()
            .text_color(p.text_secondary)
            .child(t!("leaf is free software under the MIT License."))
            .into_any_element(),
        div()
            .px_1()
            .text_xs()
            .text_color(p.text_secondary)
            .child("Based on rustyfiles by Özer Güzel (MIT)")
            .into_any_element(),
        expander(
            "libs",
            p,
            L::Library,
            t!("Third-party libraries"),
            None,
            None,
            move || {
                [
                    ("GPUI", "Apache-2.0"),
                    ("gpui-kit", "Apache-2.0"),
                    ("Velopack", "MIT"),
                    ("windows-rs", "MIT / Apache-2.0"),
                    ("Lucide icons", "ISC"),
                    ("zip, sevenz-rust2, tar, flate2, zstd", "MIT / Apache-2.0"),
                    ("notify", "CC0 / Artistic-2.0"),
                    ("chrono, serde, toml", "MIT / Apache-2.0"),
                ]
                .iter()
                .map(|(n, l)| sub(&pp, n, Some(l), div().into_any_element()))
                .collect()
            },
            cx,
        ),
    ]
}


/// Shows or hides a sidebar section (`Settings::sidebar_hidden`).
fn show_section(s: &mut Settings, key: &str, show: bool) {
    s.sidebar_hidden.retain(|h| h != key);
    if !show {
        s.sidebar_hidden.push(key.to_string());
    }
}

enum PickTarget {
    BackgroundImage,
    Ide,
    Export,
    Import,
}

/// Runs a Windows file dialog on the shell thread and applies the result.
fn pick_into(window: &mut Window, cx: &mut App, title: &str, target: PickTarget) {
    let handle = window.window_handle();
    let title = title.to_string();
    let (tx, rx) = futures::channel::oneshot::channel::<Option<std::path::PathBuf>>();
    let images = [(t!("Pictures"), "*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.gif")];
    let exe = [(t!("Programs"), "*.exe;*.cmd;*.bat")];
    let toml = [(t!("Settings file"), "*.toml")];
    let (images, exe, toml) = (
        images.map(|(a, b)| (a.to_string(), b.to_string())),
        exe.map(|(a, b)| (a.to_string(), b.to_string())),
        toml.map(|(a, b)| (a.to_string(), b.to_string())),
    );
    let kind = match target {
        PickTarget::BackgroundImage => 0,
        PickTarget::Ide => 1,
        PickTarget::Export => 2,
        PickTarget::Import => 3,
    };
    leaf_sys::sta::run(move |owner| {
        use leaf_sys::dialogs::{Pick, pick};
        fn f(v: &[(String, String)]) -> Vec<(&str, &str)> {
            v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
        }
        let r = match kind {
            0 => pick(owner, &title, Pick::File { filters: &f(&images) }),
            1 => pick(owner, &title, Pick::File { filters: &f(&exe) }),
            2 => pick(owner, &title, Pick::Save { filters: &f(&toml), default_name: "leaf-settings.toml" }),
            _ => pick(owner, &title, Pick::File { filters: &f(&toml) }),
        };
        let _ = tx.send(r);
    });
    cx.spawn(async move |cx| {
        let Ok(Some(path)) = rx.await else { return };
        let _ = cx.update(|cx| match kind {
            0 => settings::update(cx, |s| s.background_image = Some(path)),
            1 => settings::update(cx, |s| s.ide_path = path.display().to_string()),
            2 => {
                let live = toml::to_string_pretty(cx.global::<Settings>()).unwrap_or_default();
                let _ = handle.update(cx, |_, window, cx| {
                    let rx = crate::prompt::ask(t!("Export settings"), t!("Include your tags too? Settings, your actions, hosts and themes are always included; no passwords are kept in any of them.").to_string(), window, cx);
                    cx.spawn(async move |_| {
                        let with_tags = rx.await.unwrap_or(false);
                        std::thread::spawn(move || {
                            let Some(dir) = settings::app_dir() else { return };
                            let b = crate::settings_bundle::build(&dir, live, with_tags);
                            if let Ok(text) = crate::settings_bundle::to_text(&b) {
                                let _ = std::fs::write(&path, text);
                            }
                        });
                    })
                    .detach();
                });
            }
            _ => {
                let Some(dir) = settings::app_dir() else { return };
                let _ = handle.update(cx, |_, window, cx| {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let bundle = match crate::settings_bundle::parse(&text) {
                        Ok(b) => b,
                        Err(e) => return crate::prompt::notice(t!("Import settings"), format!("{}: {e}", path.display()), window, cx),
                    };
                    let plan = crate::settings_bundle::plan(&dir, &bundle, true);
                    let msg = format!("{}\n\n{}", crate::settings_bundle::summary(&plan), t!("The old files are kept in the backup folder of the leaf config folder. Hosts, actions and tags apply after leaf restarts. Continue?"));
                    let h2 = window.window_handle();
                    let rx = crate::prompt::ask(t!("Import settings"), msg, window, cx);
                    cx.spawn(async move |cx| {
                        if !rx.await.unwrap_or(false) {
                            return;
                        }
                        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                        let done = crate::settings_bundle::apply(&dir, &bundle, true, stamp);
                        let _ = h2.update(cx, |_, window, cx| match done {
                            Ok(backup) => {
                                let new = std::fs::read_to_string(dir.join("settings.toml")).ok().and_then(|t| Settings::parse_lenient_pub(t.trim_start_matches('\u{feff}')).ok());
                                if let Some(new) = new {
                                    settings::update(cx, |s| *s = new);
                                }
                                crate::prompt::notice(t!("Import settings"), format!("{} {}", t!("Done. Old files:"), backup.display()), window, cx);
                            }
                            Err(e) => crate::prompt::notice(t!("Import settings"), e.to_string(), window, cx),
                        });
                    })
                    .detach();
                });
            }
        });
    })
    .detach();
}

/// "Local drives' file names are kept in an index … · C:\ 2.1 million entries, 84 MB · E:\ indexing…"
fn name_index_status() -> String {
    let mut s = t!("File names on the local drives are kept in an index, so searching subfolders takes milliseconds. The index is built in the background and kept up to date.").to_string();
    let parts: Vec<String> = leaf_sys::indexer::status()
        .into_iter()
        .map(|(root, state, live, mem)| {
            let r = root.to_string_lossy().trim_end_matches('\\').to_string();
            match state {
                leaf_sys::indexer::State::Building => format!("{r} {}", t!("indexing…")),
                leaf_sys::indexer::State::Failed => format!("{r} {}", t!("couldn't be indexed")),
                leaf_sys::indexer::State::Ready => {
                    t!("{root} {n} items, {mb} MB").replace("{root}", &r).replace("{n}", &crate::bench_view::count(live as u64)).replace("{mb}", &(mem >> 20).to_string())
                }
            }
        })
        .collect();
    if !parts.is_empty() {
        s.push_str(" · ");
        s.push_str(&parts.join(" · "));
    }
    s
}

/// The folders left out of the search index: the user's (removable) and the built-in ones.
fn index_exclusions(p: &Palette, s: &Settings) -> AnyElement {
    let mut list = v_flex().py_1();
    for (i, dir) in s.index_excluded.iter().enumerate() {
        let d = dir.clone();
        list = list.child(
            h_flex()
                .min_h(px(40.))
                .pl(px(56.))
                .pr_4()
                .gap_3()
                .items_center()
                .child(Icon::new(L::FolderX).size(px(14.)).text_color(p.text_secondary))
                .child(div().flex_1().min_w_0().text_sm().text_ellipsis().overflow_hidden().whitespace_nowrap().child(dir.clone()))
                .child(
                    div()
                        .id(("idx-excl-rm", i))
                        .p_1()
                        .rounded(theme::RADIUS_SM)
                        .hover(|st| st.bg(p.pressed))
                        .child(Icon::new(L::X).size(px(14.)).text_color(p.text_secondary))
                        .on_click(move |_, _, cx| {
                            let d = d.clone();
                            settings::update(cx, |s| s.index_excluded.retain(|e| *e != d));
                        }),
                ),
        );
    }
    let defaults = leaf_sys::nameindex::default_excluded_names().join(", ");
    shell(p)
        .child(
            h_flex()
                .min_h(px(48.))
                .pl(px(56.))
                .pr_4()
                .py_2()
                .gap_4()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(t!("Folders left out of the index")))
                        .child(div().text_xs().text_color(p.text_secondary).child(format!(
                            "{} {defaults}{}. {}",
                            t!("Always:"),
                            if cfg!(windows) { ", Windows" } else { "" },
                            t!("Searching inside these folders walks the disk. You can also right-click a folder and choose \"Leave out of the search index\".")
                        ))),
                )
                .child(button("idx-excl-add", p, t!("Add folder…"), |window, cx| {
                    let owner = crate::platform::owner_of(window);
                    if let Some(dir) = leaf_sys::dialogs::pick(owner, t!("Folder to leave out of the index"), leaf_sys::dialogs::Pick::Folder) {
                        let d = dir.to_string_lossy().trim_end_matches('\\').to_string();
                        settings::update(cx, |s| {
                            if !s.index_excluded.iter().any(|e| rf_core::pathkey::name_eq(e, &d)) {
                                s.index_excluded.push(d);
                            }
                        });
                    }
                })),
        )
        .when(!s.index_excluded.is_empty(), |d| d.child(div().h(px(1.)).bg(p.divider)).child(list))
        .into_any_element()
}

#[cfg(test)]
mod search_tests {
    use super::{Found, Page, matching};

    #[test]
    fn settings_are_found_by_every_word_of_the_query() {
        let f = |title: &str, desc: Option<&str>, page: Page| Found { title: title.into(), desc: desc.map(str::to_string), page, expander: None };
        let all = vec![
            f("Open archives like folders", Some("ZIP, 7z and tar files"), Page::Files),
            f("Preview pane", None, Page::Layout),
            f("Preview pane", None, Page::Layout),
            f("İlerleme çubuğu", None, Page::Appearance),
        ];
        let titles = |q: &str| matching(all.clone(), q).into_iter().map(|f| f.title).collect::<Vec<_>>();
        assert_eq!(titles("archive"), vec!["Open archives like folders"]);
        // a word of the description; every word must be there
        assert_eq!(titles("zip folders"), vec!["Open archives like folders"]);
        assert!(titles("zip pane").is_empty());
        // the same setting drawn twice is one result; a Turkish İ folds too
        assert_eq!(titles("PREVIEW"), vec!["Preview pane"]);
        assert_eq!(titles("ilerleme"), vec!["İlerleme çubuğu"]);
        assert!(titles("   ").is_empty());
    }
}

#[cfg(test)]
mod hyperglass_tests {
    use super::{Hg, SLIDER_W, THUMB, slider_key, slider_value_at};
    use crate::settings::Settings;

    /// The list of pages gives way before the settings do: in a narrow pane it is a strip of icons, and the settings' column is
    /// then wide enough for a setting's words and its widest control side by side or one under the other.
    #[test]
    fn only_a_drives_root_has_a_drive_letter() {
        use std::path::Path;
        assert_eq!(super::drive_letter(Path::new(r"V:\")), Some('V'));
        assert_eq!(super::drive_letter(Path::new("c:")), Some('C'));
        assert_eq!(super::drive_letter(Path::new("D:/")), Some('D'));
        for other in [r"C:\Users", r"\\server\share", "", "C", r"1:\", r"C:\\"] {
            assert_eq!(super::drive_letter(Path::new(other)), None, "{other:?}");
        }
    }

    #[test]
    fn hiding_and_showing_a_drive_keeps_the_letters_once_and_in_order() {
        let mut s = crate::settings::Settings::default();
        super::set_drive_hidden(&mut s, 'v', true);
        super::set_drive_hidden(&mut s, 'D', true);
        super::set_drive_hidden(&mut s, 'V', true);
        assert_eq!(s.hidden_drives, ["D", "V"]);
        super::set_drive_hidden(&mut s, 'd', false);
        assert_eq!(s.hidden_drives, ["V"]);
        super::set_drive_hidden(&mut s, 'Z', false);
        super::set_drive_hidden(&mut s, '1', true);
        assert_eq!(s.hidden_drives, ["V"], "a drive that was not hidden, and what is no drive, change nothing");
        // a list written by hand is cleaned the first time it is changed
        s.hidden_drives = vec!["v:".into(), "x".into(), "V".into(), "?".into()];
        super::set_drive_hidden(&mut s, 'A', true);
        assert_eq!(s.hidden_drives, ["A", "V", "X"]);
    }

    #[test]
    fn a_colour_of_the_users_own_is_kept_as_rrggbb_or_refused_with_words() {
        assert_eq!(super::accent_answer("f7630c"), Ok("#F7630C".to_string()));
        assert_eq!(super::accent_answer(" #abc "), Ok("#AABBCC".to_string()));
        assert!(super::accent_answer("turuncu").is_err());
        assert!(super::accent_answer("").is_err());
        // every theme offers its own six accents, each named once
        for t in crate::theme::data::builtins() {
            let names: std::collections::HashSet<String> = t.accents.iter().map(|a| a.name.to_ascii_lowercase()).collect();
            assert_eq!(names.len(), 6, "{}", t.id);
        }
    }

    #[test]
    fn a_narrow_pane_gets_the_strip_of_icons_and_a_readable_column() {
        assert!(!super::nav_is_strip(0.0), "a pane that was not measured yet gets the full list");
        assert!(super::nav_is_strip(480.0), "the narrowest window with the app's sidebar open");
        assert!(super::nav_is_strip(super::NAV_STRIP_BELOW - 1.0));
        assert!(!super::nav_is_strip(super::NAV_STRIP_BELOW));
        assert!(!super::nav_is_strip(1200.0));
        // the settings' column with the strip, in the narrowest pane the app's window allows (480): room for a list of the widest
        // kind (320) on a line of its own inside a card's padding, and for the words at their narrowest
        let column = 480.0 - super::NAV_STRIP_W - 48.0;
        assert!(column - 32.0 >= 320.0, "{column}");
        assert!(column - 32.0 >= super::WORDS_MIN + 34.0);
        // and with the full list just above the limit the column is no narrower than that
        assert!(super::NAV_STRIP_BELOW - super::NAV_W - 48.0 >= column - 16.0);
    }

    #[test]
    fn the_thumbs_travel_maps_to_0_through_100() {
        // a slider at x = 100: the thumb's centre runs from 100 + 9 to 100 + 220 - 9 (the classic round thumb), or from + 11 (the liquid one)
        for thumb in [THUMB, crate::liquid::SL_REST_W] {
            let at = |x: f32| slider_value_at(x, 100.0, SLIDER_W, thumb);
            assert_eq!(at(100.0), 0, "left of the track");
            assert_eq!(at(100.0 + thumb / 2.0), 0);
            assert_eq!(at(100.0 + SLIDER_W - thumb / 2.0), 100);
            assert_eq!(at(100.0 + SLIDER_W + 500.0), 100, "far right of the track, as a drag goes on past the end");
            assert_eq!(at(-300.0), 0);
            let mid = at(100.0 + SLIDER_W / 2.0);
            assert_eq!(mid, 50, "the middle of the track is the middle of the range");
            // the thumb drawn for a value is under the pointer that gave it
            for v in [0u8, 1, 37, 52, 99, 100] {
                let centre = 100.0 + f32::from(v) / 100.0 * (SLIDER_W - thumb) + thumb / 2.0;
                assert_eq!(at(centre), v);
            }
        }
        // the liquid thumb's drawn centre (liquid::slider_centre) is the pointer position that gives the value; it grows about its
        // centre while it is held, so the mapping always uses the resting width
        for v in 0..=100u8 {
            let centre = 100.0 + crate::liquid::slider_centre(SLIDER_W, f32::from(v) / 100.0);
            assert_eq!(slider_value_at(centre, 100.0, SLIDER_W, crate::liquid::SL_REST_W), v);
        }
        // a box too small to hold the thumb does not divide by zero
        assert_eq!(slider_value_at(5.0, 0.0, 0.0, THUMB), 0);
    }

    #[test]
    fn keys_move_a_slider_by_one_ten_or_to_an_end() {
        assert_eq!(slider_key(50, "right", false), Some(51));
        assert_eq!(slider_key(50, "up", false), Some(51));
        assert_eq!(slider_key(50, "left", false), Some(49));
        assert_eq!(slider_key(50, "down", false), Some(49));
        assert_eq!(slider_key(50, "right", true), Some(60));
        assert_eq!(slider_key(50, "left", true), Some(40));
        assert_eq!(slider_key(50, "pageup", false), Some(60));
        assert_eq!(slider_key(50, "pagedown", false), Some(40));
        assert_eq!(slider_key(50, "home", false), Some(0));
        assert_eq!(slider_key(50, "end", false), Some(100));
        // at the ends they stay (and are still taken, so the file list does not move)
        assert_eq!(slider_key(0, "left", false), Some(0));
        assert_eq!(slider_key(3, "left", true), Some(0));
        assert_eq!(slider_key(100, "right", false), Some(100));
        assert_eq!(slider_key(95, "pageup", false), Some(100));
        // a value the file had out of range counts as 100
        assert_eq!(slider_key(200, "left", false), Some(99));
        // other keys are not the slider's
        assert_eq!(slider_key(50, "a", false), None);
        assert_eq!(slider_key(50, "enter", false), None);
        assert_eq!(slider_key(50, "tab", false), None);
    }

    #[test]
    fn each_slider_reads_and_writes_its_own_setting() {
        let mut s = Settings::default();
        for (i, which) in Hg::ALL.into_iter().enumerate() {
            which.set(&mut s, 10 + i as u8);
        }
        assert_eq!([s.hg_frost, s.hg_tint, s.hg_refraction, s.hg_saturation, s.hg_surface], [10, 11, 12, 13, 14]);
        for (i, which) in Hg::ALL.into_iter().enumerate() {
            assert_eq!(which.get(&s), 10 + i as u8);
            assert_eq!(which as usize, i, "the slider id is the position on the card");
        }
        // a number out of range is cut on both sides
        Hg::Frost.set(&mut s, 250);
        assert_eq!(s.hg_frost, 100);
        s.hg_tint = 180;
        assert_eq!(Hg::Tint.get(&s), 100);
    }

    #[test]
    fn every_label_and_description_is_there() {
        for which in Hg::ALL {
            assert!(!which.title().is_empty() && !which.desc().is_empty());
        }
    }
}

/// The download categories: one row each with its folder and extensions, "Edit" (and "Remove" inside it), "Add" and "Defaults".
fn download_categories_card(p: &Palette, s: &Settings) -> AnyElement {
    use crate::settings::DownloadCategory;
    let edit = |index: Option<usize>, window: &mut Window, cx: &mut App| {
        let current = index.and_then(|i| cx.global::<Settings>().download_categories.get(i).cloned());
        let initial = current.as_ref().map(DownloadCategory::to_line).unwrap_or_default();
        let mut buttons = vec![crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |value, _, cx| {
            let line = value.unwrap_or_default();
            let Some(c) = DownloadCategory::from_line(&line).filter(|c| leaf_download::sanitize(&c.folder) == c.folder) else {
                return Err(t!("Write a folder name, a colon and the extensions: Videos: mp4 mkv avi").to_string());
            };
            settings::update(cx, move |s| match index.filter(|i| *i < s.download_categories.len()) {
                Some(i) => s.download_categories[i] = c,
                None => s.download_categories.push(c),
            });
            Ok(())
        })];
        if let Some(i) = index {
            buttons.push(crate::prompt::Button::new(t!("Remove"), crate::prompt::Kind::Danger, move |_, _, cx| {
                settings::update(cx, move |s| {
                    if i < s.download_categories.len() {
                        s.download_categories.remove(i);
                    }
                });
                Ok(())
            }));
        }
        buttons.push(crate::prompt::Button::cancel());
        crate::prompt::show(
            t!("Download category"),
            Some(t!("The subfolder, a colon, then the file extensions it takes.").to_string()),
            Some((initial, "Videos: mp4 mkv avi".to_string())),
            buttons,
            window,
            cx,
        );
    };
    let mut shell = shell(p).child(card_body(p, L::Folder, t!("Categories").into(), Some(t!("Which file types go into which subfolder when sorting is on.").into()), None));
    for (i, c) in s.download_categories.iter().enumerate() {
        shell = shell.child(card_body(
            p,
            L::Folder,
            c.folder.clone().into(),
            Some(c.exts.join(" ").into()),
            Some(button(("download-category", i), p, t!("Edit"), move |window, cx| edit(Some(i), window, cx))),
        ));
    }
    shell
        .child(card_body(
            p,
            L::Plus,
            t!("Add a category").into(),
            None,
            Some(
                h_flex()
                    .gap_2()
                    .child(button("download-category-defaults", p, t!("Defaults"), |_, cx| settings::update(cx, |s| s.download_categories = DownloadCategory::defaults())))
                    .child(button("download-category-add", p, t!("Add"), move |window, cx| edit(None, window, cx)))
                    .into_any_element(),
            ),
        ))
        .into_any_element()
}

/// The queue's schedule: switch, start, stop, weekdays and what happens when the queue is done.
fn download_schedule_card(p: &Palette, s: &Settings) -> AnyElement {
    use crate::settings::WhenDone;
    let times = |current: u16| -> Vec<(u16, String)> {
        let mut v: Vec<(u16, String)> = (0..48u16).map(|i| (i * 30, format!("{:02}:{:02}", i / 2, i % 2 * 30))).collect();
        if !v.iter().any(|(m, _)| *m == current) {
            v.push((current, format!("{:02}:{:02}", current / 60, current % 60)));
        }
        v
    };
    let names = [t!("Mon"), t!("Tue"), t!("Wed"), t!("Thu"), t!("Fri"), t!("Sat"), t!("Sun")];
    let days = h_flex().gap_1().children(names.into_iter().enumerate().map(|(i, name)| {
        let on = s.download_schedule_days >> i & 1 == 1;
        div()
            .id(("download-day", i))
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .rounded(theme::RADIUS_SM)
            .border_1()
            .border_color(p.border)
            .text_sm()
            .cursor_pointer()
            .when(on, |d| d.bg(p.accent).text_color(p.accent_text))
            .when(!on, |d| d.bg(p.hover).hover(|s| s.bg(p.pressed)))
            .child(name)
            .on_click(move |_, _, cx| settings::update(cx, move |s| s.download_schedule_days ^= 1 << i))
            .into_any_element()
    }));
    let when_options: Vec<(WhenDone, String)> = [
        (WhenDone::Nothing, t!("Nothing")),
        (WhenDone::Close, t!("Close leaf")),
        (WhenDone::Sleep, t!("Put this PC to sleep")),
        (WhenDone::Shutdown, t!("Shut this PC down")),
    ]
    .into_iter()
    .filter(|(w, _)| crate::download_schedule::available(*w))
    .map(|(w, l)| (w, l.to_string()))
    .collect();
    shell(p)
        .child(card_body(
            p,
            L::Clock,
            t!("Run the download queue at set times").into(),
            Some(t!("Outside the window downloads wait, and running ones pause until it opens again.").into()),
            Some(sw("download-schedule", p, s.download_schedule, |s, v| s.download_schedule = v)),
        ))
        .when(s.download_schedule, |d| {
            d.child(card_body(p, L::Clock, t!("Start at").into(), None, Some(dropdown("download-start", p, s.download_schedule_start, times(s.download_schedule_start), |s, v| s.download_schedule_start = v))))
                .child(card_body(
                    p,
                    L::Clock,
                    t!("Stop at").into(),
                    Some(t!("A time before the start is after midnight.").into()),
                    Some(dropdown("download-stop", p, s.download_schedule_stop, times(s.download_schedule_stop), |s, v| s.download_schedule_stop = v)),
                ))
                .child(card_body(p, L::Calendar, t!("On these days").into(), Some(t!("The days the window starts on.").into()), Some(days.into_any_element())))
        })
        .child(card_body(
            p,
            L::Download,
            t!("When the downloads are done").into(),
            Some(t!("Sleep and shut down give a 60 second notice you can cancel.").into()),
            Some(dropdown("download-when-done", p, s.download_when_done, when_options, |s, v| s.download_when_done = v)),
        ))
        .into_any_element()
}

/// The servers' own settings: the default row (connections to one server over all downloads) and a row per server name.
fn download_servers_card(p: &Palette, s: &Settings) -> AnyElement {
    use crate::settings::ServerRule;
    let edit = |index: Option<usize>, window: &mut Window, cx: &mut App| {
        let current = index.and_then(|i| cx.global::<Settings>().download_servers.get(i).cloned());
        let initial = current.as_ref().map(ServerRule::to_line).unwrap_or_default();
        let mut buttons = vec![crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |value, _, cx| {
            let Some(r) = ServerRule::from_line(&value.unwrap_or_default()) else {
                return Err(t!("Write the server name, the connections (up to 64), the speed limit in MB/s and the user agent, separated by semicolons").to_string());
            };
            settings::update(cx, move |s| match index.filter(|i| *i < s.download_servers.len()) {
                Some(i) => s.download_servers[i] = r,
                None => s.download_servers.push(r),
            });
            Ok(())
        })];
        if let Some(i) = index {
            buttons.push(crate::prompt::Button::new(t!("Remove"), crate::prompt::Kind::Danger, move |_, _, cx| {
                settings::update(cx, move |s| {
                    if i < s.download_servers.len() {
                        s.download_servers.remove(i);
                    }
                });
                Ok(())
            }));
        }
        buttons.push(crate::prompt::Button::cancel());
        crate::prompt::show(
            t!("Server settings"),
            Some(t!("Server name (it also covers the servers below it); connections (0: the default row); speed limit in MB/s (0: none); user agent (empty: leaf's).").to_string()),
            Some((initial, "example.com; 4; 2; Mozilla/5.0".to_string())),
            buttons,
            window,
            cx,
        );
    };
    let mut shell = shell(p).child(card_body(
        p,
        L::Network,
        t!("Connections to one server").into(),
        Some(t!("The most connections open to one server, over all downloads together (default 32, up to 64). Fewer are used when the server asks for it; a server that takes only one connection gets one. This is the row for every server that has none of its own below.").into()),
        Some(dropdown("download-per-host", p, s.download_per_host, [0u32, 2, 4, 8, 16, 24, 32, 48, 64].into_iter().map(|n| (n, if n == 0 { t!("No limit").to_string() } else { n.to_string() })).collect(), |s, v| s.download_per_host = v)),
    ));
    for (i, r) in s.download_servers.iter().enumerate() {
        let mut parts = vec![if r.connections == 0 { t!("default connections").to_string() } else { format!("{} {}", r.connections, t!("connections")) }];
        parts.push(if r.limit_mb == 0 { t!("no speed limit").to_string() } else { format!("{} MB/s", r.limit_mb) });
        if !r.user_agent.trim().is_empty() {
            parts.push(r.user_agent.clone());
        }
        shell = shell.child(card_body(p, L::Globe, r.host.clone().into(), Some(parts.join(" · ").into()), Some(button(("download-server", i), p, t!("Edit"), move |window, cx| edit(Some(i), window, cx)))));
    }
    shell
        .child(card_body(p, L::Plus, t!("Settings for one server").into(), Some(t!("Connections, speed limit and user agent for a server name.").into()), Some(button("download-server-add", p, t!("Add"), move |window, cx| edit(None, window, cx)))))
        .into_any_element()
}

/// The proxy downloads go through: none, the system's or one of the user's own (HTTP or SOCKS5). Its password goes to the keyring.
fn download_proxy_card(p: &Palette, s: &Settings) -> AnyElement {
    use crate::settings::{DownloadProxy, ProxyType};
    let manual = s.download_proxy == DownloadProxy::Manual;
    let server = if s.download_proxy_host.trim().is_empty() { t!("Not set").to_string() } else { format!("{}:{}", s.download_proxy_host.trim(), s.download_proxy_port) };
    let user = if s.download_proxy_user.trim().is_empty() { t!("No sign-in").to_string() } else { s.download_proxy_user.trim().to_string() };
    let shell = shell(p).child(card_body(
        p,
        L::Globe,
        t!("Proxy for downloads").into(),
        Some(t!("The system's proxy is the one Windows or the environment names. Your own proxy can be HTTP or SOCKS5. The proxy's password is sent to the proxy unencrypted (that is how these proxies work). Downloads through leaf's VPN do not use the proxy: the VPN wins.").into()),
        Some(dropdown(
            "download-proxy",
            p,
            s.download_proxy,
            vec![(DownloadProxy::System, t!("System").to_string()), (DownloadProxy::None, t!("None").to_string()), (DownloadProxy::Manual, t!("Manual").to_string())],
            |s, v| s.download_proxy = v,
        )),
    ));
    if !manual {
        return shell.into_any_element();
    }
    shell
        .child(card_body(
            p,
            L::Network,
            t!("Type").into(),
            None,
            Some(dropdown("download-proxy-type", p, s.download_proxy_type, vec![(ProxyType::Http, "HTTP".to_string()), (ProxyType::Socks5, "SOCKS5".to_string())], |s, v| s.download_proxy_type = v)),
        ))
        .child(card_body(
            p,
            L::Globe,
            t!("Server").into(),
            Some(server.into()),
            Some(button("download-proxy-server", p, t!("Edit"), |window, cx| {
                let s = cx.global::<Settings>();
                let initial = if s.download_proxy_host.trim().is_empty() { String::new() } else { format!("{}:{}", s.download_proxy_host.trim(), s.download_proxy_port) };
                crate::prompt::show(
                    t!("Proxy server"),
                    Some(t!("The proxy's name or address and its port, for example proxy.example.com:8080").to_string()),
                    Some((initial, "proxy.example.com:8080".to_string())),
                    vec![
                        crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                            let text = value.unwrap_or_default();
                            let (host, port) = text.trim().rsplit_once(':').ok_or_else(|| t!("Write the name, a colon and the port").to_string())?;
                            let port: u16 = port.parse().ok().filter(|p| *p > 0).ok_or_else(|| t!("The port is a number from 1 to 65535").to_string())?;
                            let host = host.trim().to_string();
                            if host.is_empty() || host.contains(['/', ' ', '@']) {
                                return Err(t!("Write the proxy's name or address without http:// and without a login").to_string());
                            }
                            settings::update(cx, move |s| {
                                s.download_proxy_host = host;
                                s.download_proxy_port = port;
                            });
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            })),
        ))
        .child(card_body(
            p,
            L::User,
            t!("Sign-in").into(),
            Some(user.into()),
            Some(button("download-proxy-user", p, t!("Edit"), |window, cx| {
                let initial = cx.global::<Settings>().download_proxy_user.clone();
                crate::prompt::show(
                    t!("Proxy user name"),
                    Some(t!("Leave empty when the proxy takes no sign-in. The password is kept in the system's keyring, not in a file; set it again when the user name changes.").to_string()),
                    Some((initial, t!("User name").to_string())),
                    vec![
                        crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |value, _, cx| {
                            let user = value.unwrap_or_default().trim().to_string();
                            settings::update(cx, move |s| s.download_proxy_user = user);
                            Ok(())
                        }),
                        crate::prompt::Button::cancel(),
                    ],
                    window,
                    cx,
                );
            })),
        ))
        .child(card_body(
            p,
            L::KeyRound,
            t!("Password").into(),
            Some(t!("Kept in the system's keyring.").into()),
            Some(
                h_flex()
                    .gap_2()
                    .child(button("download-proxy-forget", p, t!("Forget"), |_, cx| {
                        if let Some(id) = proxy_id(cx) {
                            std::thread::spawn(move || {
                                if let Err(e) = leaf_remote::vault().forget(&id) {
                                    log::warn!("proxy: {e}");
                                }
                            });
                        }
                    }))
                    .child(button("download-proxy-password", p, t!("Set"), |window, cx| {
                        let Some(id) = proxy_id(cx) else {
                            return crate::prompt::notice(t!("Proxy password"), t!("Enter the proxy's server and the user name first.").to_string(), window, cx);
                        };
                        if let leaf_remote::Keyring::Missing(why) = leaf_remote::vault().state() {
                            return crate::prompt::notice(t!("Proxy password"), format!("{}\n{why}", t!("leaf cannot keep the password here.")), window, cx);
                        }
                        crate::prompt::show_secret(
                            t!("Proxy password"),
                            Some(t!("Saved in the system's keyring for this proxy and user name.").to_string()),
                            t!("Password").to_string(),
                            vec![
                                crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |value, _, _| {
                                    let secret = leaf_remote::Secret::new(value.unwrap_or_default()).remembered(true);
                                    let id = id.clone();
                                    std::thread::spawn(move || leaf_remote::vault().keep_if_asked(&id, &secret));
                                    Ok(())
                                }),
                                crate::prompt::Button::cancel(),
                            ],
                            window,
                            cx,
                        );
                    }))
                    .into_any_element(),
            ),
        ))
        .into_any_element()
}

/// The keyring entry of the manual proxy's password (none until the server and the user name are set).
fn proxy_id(cx: &App) -> Option<leaf_remote::SecretId> {
    let s = cx.global::<Settings>();
    let (user, host) = (s.download_proxy_user.trim(), s.download_proxy_host.trim());
    (!user.is_empty() && !host.is_empty()).then(|| crate::downloads::proxy_secret_id(user, host, s.download_proxy_port))
}

/// Whether `exe` lies in a cargo build folder: a `target` folder on its path, or one holding cargo's `CACHEDIR.TAG`.
fn is_build_folder_exe(exe: &std::path::Path) -> bool {
    exe.ancestors().skip(1).any(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("target")) || d.join("CACHEDIR.TAG").is_file())
}
