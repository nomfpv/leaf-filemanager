//! The Home page: a dashboard of widgets on a grid ([`WIDGETS`], one static table in the spirit of `registry.rs`).
//!
//! The grid has one to four columns, as many as the pane's width holds; a widget is small (one column), medium (two) or wide (the
//! whole row) and snaps to those columns. Which widgets are shown, in which order and size, is the user's (`Settings::home_layout`,
//! changed in Customize mode on the page, by the "Customize Home" and "Reset Home" commands and in Settings); `None` is the default
//! set ([`DEFAULT`]).
//!
//! Nothing of a widget runs while Home is not shown: a widget is only its render function, which reads what it needs (places, tags)
//! when the page is drawn. A widget that is not in the layout is never called.

mod dev;
mod widgets;

pub use dev::Data;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::folder_view::FolderView;
use crate::settings::Settings;
use crate::t;
use crate::theme::{self, Palette};

/// How much of the grid a widget takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    /// One column.
    Small,
    /// Two columns.
    Medium,
    /// The whole row.
    Wide,
    /// Two columns and two rows (the mockups' large tile).
    Large,
}

impl Size {
    pub const ALL: [Size; 4] = [Size::Small, Size::Medium, Size::Wide, Size::Large];

    pub fn key(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::Wide => "wide",
            Size::Large => "large",
        }
    }

    pub fn parse(s: &str) -> Option<Size> {
        Size::ALL.into_iter().find(|z| z.key() == s)
    }

    /// The letter on its button in Customize mode, and the name in its tooltip.
    fn letter(self) -> &'static str {
        match self {
            Size::Small => "S",
            Size::Medium => "M",
            Size::Wide => "W",
            Size::Large => "L",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Size::Small => "Small",
            Size::Medium => "Medium",
            Size::Wide => "Wide",
            Size::Large => "Large",
        }
    }

    /// The columns it spans on a grid of `cols`.
    pub fn span(self, cols: u16) -> u16 {
        match self {
            Size::Small => 1,
            Size::Medium | Size::Large => 2.min(cols),
            Size::Wide => cols,
        }
        .max(1)
    }

    /// The rows it spans on the grid, for a widget whose own height is `base` rows: Large is at least two.
    pub fn rows(self, base: u16) -> u16 {
        match self {
            Size::Large => base.max(2),
            _ => base.max(1),
        }
    }
}

/// One widget of the saved layout, as the settings file has it (`[[home_layout]] id = "drives"  size = "wide"`). Plain strings, so a
/// size or a widget this build does not know is skipped when read ([`resolve`]) instead of making the whole layout unreadable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: String,
    #[serde(default)]
    pub size: String,
    /// The folder a widget that works on one was pointed at in Customize mode (empty: the first pinned folder).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
}

/// What a widget is drawn with.
pub struct WidgetCx {
    pub p: Palette,
    pub size: Size,
    /// The width inside the widget's padding.
    pub width: f32,
}

/// Draws a widget's body; `None`: nothing to show right now (no devices plugged in): the widget is left out of the page outside
/// Customize mode.
pub type RenderFn = fn(&mut FolderView, &WidgetCx, &mut Context<FolderView>) -> Option<AnyElement>;
/// Something of its own in the widget's heading, right of the title (Recent's "Clear").
pub type HeaderFn = fn(&Palette, &mut Context<FolderView>) -> AnyElement;

pub struct Widget {
    /// Its name in the settings file; never changes.
    pub id: &'static str,
    /// The English title (translated where it is drawn).
    pub title: &'static str,
    pub icon: L,
    /// The sizes it can take; the first one it is added with is `default_size`.
    pub sizes: &'static [Size],
    pub default_size: Size,
    /// Rows of the grid it takes in the sizes S, M and W (a fixed row unit, theme key `home_row`); content that does not fit scrolls.
    pub rows: u16,
    /// It works on one folder, chosen in Customize mode ([`folder_of`]).
    pub folder: bool,
    pub render: RenderFn,
    pub header: Option<HeaderFn>,
    /// The leaf version it came with.
    #[allow(dead_code)]
    pub since: &'static str,
}

const ANY: &[Size] = &[Size::Small, Size::Medium, Size::Wide];
const ANY_LARGE: &[Size] = &[Size::Small, Size::Medium, Size::Wide, Size::Large];

/// Every widget there is.
pub static WIDGETS: &[Widget] = &[
    Widget { id: "pinned", title: "Quick access", icon: L::Pin, sizes: ANY, default_size: Size::Wide, rows: 1, folder: false, render: widgets::pinned, header: None, since: "0.9.5" },
    Widget { id: "drives", title: "Drives", icon: L::HardDrive, sizes: ANY_LARGE, default_size: Size::Wide, rows: 1, folder: false, render: widgets::drives, header: None, since: "0.9.5" },
    Widget { id: "devices", title: "Devices", icon: L::Usb, sizes: ANY, default_size: Size::Medium, rows: 1, folder: false, render: widgets::devices, header: None, since: "0.9.5" },
    Widget { id: "network", title: "Network locations", icon: L::Network, sizes: ANY, default_size: Size::Medium, rows: 1, folder: false, render: widgets::network, header: None, since: "0.9.5" },
    Widget {
        id: "recent",
        title: "Recent",
        icon: L::Clock,
        sizes: &[Size::Medium, Size::Wide, Size::Large],
        default_size: Size::Wide,
        rows: 2,
        folder: false,
        render: widgets::recent,
        header: Some(widgets::recent_clear),
        since: "0.9.5",
    },
    Widget { id: "tags", title: "Tags", icon: L::Tag, sizes: ANY, default_size: Size::Medium, rows: 1, folder: false, render: widgets::tags, header: None, since: "0.9.5" },
    // the optional ones: none is in the default set, and none computes anything unless it is on the page
    Widget {
        id: "pulse",
        title: "Git pulse",
        icon: L::GitBranch,
        sizes: ANY_LARGE,
        default_size: Size::Medium,
        rows: 2,
        folder: true,
        render: dev::pulse,
        header: Some(dev::pulse_header),
        since: "0.9.6",
    },
    Widget {
        id: "codestats",
        title: "Code stats",
        icon: L::FileCode,
        sizes: ANY_LARGE,
        default_size: Size::Medium,
        rows: 2,
        folder: true,
        render: dev::stats,
        header: Some(dev::stats_header),
        since: "0.9.6",
    },
    Widget {
        id: "activity",
        title: "Activity calendar",
        icon: L::Calendar,
        sizes: ANY_LARGE,
        default_size: Size::Medium,
        rows: 2,
        folder: true,
        render: dev::activity,
        header: Some(dev::activity_header),
        since: "0.9.6",
    },
    Widget {
        id: "reclaim",
        title: "Reclaimable space",
        icon: L::Eraser,
        sizes: ANY,
        default_size: Size::Medium,
        rows: 2,
        folder: true,
        render: dev::reclaim,
        header: Some(dev::reclaim_header),
        since: "0.9.6",
    },
    Widget { id: "hosts", title: "Hosts", icon: L::Server, sizes: ANY, default_size: Size::Medium, rows: 1, folder: false, render: crate::hosts_ui::widget, header: None, since: "0.9.5" },
];

/// The default set, in order. Devices, network locations and hosts show themselves only while there are any.
pub const DEFAULT: &[&str] = &["pinned", "drives", "devices", "network", "hosts", "recent", "tags"];

/// The keys of the old hide list (`Settings::home_hidden`) and the widgets they were.
const OLD_KEYS: &[(&str, &str)] = &[("quick", "pinned"), ("drives", "drives"), ("devices", "devices"), ("network", "network"), ("tags", "tags"), ("recent", "recent")];

/// A widget of the layout: its row in [`WIDGETS`] and its size.
pub type Placed = (usize, Size);

fn index(id: &str) -> Option<usize> {
    WIDGETS.iter().position(|w| w.id == id)
}

pub fn default_layout() -> Vec<Placed> {
    DEFAULT.iter().filter_map(|id| index(id)).map(|i| (i, WIDGETS[i].default_size)).collect()
}

/// The layout the settings hold, made sense of: unknown widgets and repeats are skipped, a size the widget cannot take becomes its
/// default size. Nothing saved, or a saved layout of which nothing could be understood: the default set. (An empty saved layout is
/// the user's: every widget removed.)
pub fn resolve(saved: Option<&[Slot]>) -> Vec<Placed> {
    let Some(saved) = saved else { return default_layout() };
    let mut out: Vec<Placed> = Vec::with_capacity(saved.len());
    for slot in saved {
        let Some(i) = index(slot.id.trim()) else { continue };
        if out.iter().any(|(j, _)| *j == i) {
            continue;
        }
        let w = &WIDGETS[i];
        let size = Size::parse(slot.size.trim()).filter(|z| w.sizes.contains(z)).unwrap_or(w.default_size);
        out.push((i, size));
    }
    if out.is_empty() && !saved.is_empty() {
        return default_layout();
    }
    out
}

fn to_slots(layout: &[Placed]) -> Vec<Slot> {
    layout.iter().map(|&(i, z)| Slot { id: WIDGETS[i].id.to_string(), size: z.key().to_string(), folder: String::new() }).collect()
}

/// The layout the old hide list described: the default set without the hidden sections.
pub fn from_hidden(hidden: &[String]) -> Vec<Slot> {
    let gone = |id: &str| OLD_KEYS.iter().any(|(old, new)| *new == id && hidden.iter().any(|h| h == old));
    to_slots(&default_layout().into_iter().filter(|&(i, _)| !gone(WIDGETS[i].id)).collect::<Vec<_>>())
}

/// Moves the old hide list into the layout, once (called by `Settings::migrate`).
pub fn migrate(s: &mut Settings) {
    if s.home_hidden.is_empty() {
        return;
    }
    if s.home_layout.is_none() {
        s.home_layout = Some(from_hidden(&s.home_hidden));
    }
    s.home_hidden.clear();
}

/// The layout as it is shown.
pub fn layout(s: &Settings) -> Vec<Placed> {
    resolve(s.home_layout.as_deref())
}

/// Changes the layout and writes it back.
pub fn edit(s: &mut Settings, f: impl FnOnce(&mut Vec<Placed>)) {
    let mut l = layout(s);
    f(&mut l);
    let old = s.home_layout.take().unwrap_or_default();
    let mut slots = to_slots(&l);
    // the folders the widgets were pointed at stay with them
    for sl in &mut slots {
        if let Some(o) = old.iter().find(|o| o.id == sl.id) {
            sl.folder = o.folder.clone();
        }
    }
    s.home_layout = Some(slots);
}

/// Points a widget (on the page) at a folder.
pub fn set_folder(s: &mut Settings, id: &str, folder: &str) {
    edit(s, |_| {});
    if let Some(sl) = s.home_layout.as_mut().and_then(|l| l.iter_mut().find(|sl| sl.id == id)) {
        sl.folder = folder.to_string();
    }
}

/// The folder a widget works on: the one chosen in Customize mode, else the first pinned folder, else none.
pub fn folder_of(v: &FolderView, id: &str, cx: &App) -> Option<std::path::PathBuf> {
    let chosen = cx.global::<Settings>().home_layout.as_ref().and_then(|l| l.iter().find(|sl| sl.id == id)).map(|sl| sl.folder.trim().to_string());
    match chosen {
        Some(c) if !c.is_empty() => Some(c.into()),
        _ => v.places.read(cx).pinned.first().map(|f| f.path.clone()),
    }
}

/// Adds the widget (at the end, in its default size) or takes it out.
pub fn set_shown(s: &mut Settings, id: &str, on: bool) {
    let Some(i) = index(id) else { return };
    edit(s, |l| {
        l.retain(|(j, _)| *j != i);
        if on {
            l.push((i, WIDGETS[i].default_size));
        }
    });
}

/// Back to the default set.
pub fn reset(s: &mut Settings) {
    s.home_layout = None;
}

/// `ResetHome` (from anywhere); `CustomizeHome` is the window's (it opens Home in the current tab).
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::ResetHome, cx| {
        crate::settings::update(cx, reset);
        cx.refresh_windows();
    });
}

/// Columns of the grid for the width there is: one per 300 px or so, at most four.
pub fn columns(avail: f32, gap: f32) -> u16 {
    (((avail + gap) / (300. + gap)).floor() as i32).clamp(1, 4) as u16
}

const PAD: f32 = 24.;
const W_PAD: f32 = 16.;
/// The heading row of a widget.
const HEAD_H: f32 = 24.;

/// The Home page.
pub fn render(v: &mut FolderView, p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    let p = *p;
    let editing = v.home_edit;
    let placed = layout(cx.global::<Settings>());
    // Index: one ruled sheet whose widgets share their lines; else separate cards (Canopy: sheets with its shadow)
    let ruled = p.section_rule;
    let gap = if ruled { 0. } else { 12. };
    let radius = if ruled { 0. } else if p.gap > 0. { p.radius } else { p.tile_radius.max(4.) };
    let avail = (f32::from(v.content_width) - 2. * PAD).max(200.);
    let cols = columns(avail, gap);
    let col_w = (avail - gap * f32::from(cols - 1)) / f32::from(cols);

    let mut grid = div().grid().grid_cols(cols).gap(px(gap)).w_full().when(ruled, |d| d.border_t_1().border_l_1().border_color(p.border));
    let mut shown = 0;
    for (pos, &(i, size)) in placed.iter().enumerate() {
        let w = &WIDGETS[i];
        let span = size.span(cols);
        let width = col_w * f32::from(span) + gap * f32::from(span - 1) - 2. * W_PAD;
        let rows = size.rows(w.rows);
        let card_h = f32::from(rows) * p.home_row + f32::from(rows - 1) * gap;
        let body = (w.render)(v, &WidgetCx { p, size, width }, cx);
        if body.is_none() && !editing {
            continue;
        }
        shown += 1;
        let body = body.unwrap_or_else(|| div().text_sm().text_color(p.text_tertiary).child(t!("Nothing to show right now")).into_any_element());
        let head = h_flex()
            .gap_2()
            .items_center()
            .h(px(HEAD_H)).flex_shrink_0()
            .child(Icon::new(w.icon).size(px(14.)).text_color(p.text_tertiary))
            .child(div().flex_1().min_w_0().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(crate::i18n::t(w.title)))
            .when_some(w.header.filter(|_| !editing), |d, h| d.child(h(&p, cx)))
            .when(editing, |d| d.child(edit_controls(pos, placed.len(), i, size, &p, cx)));
        let body = div().id(("home-body", i)).flex_1().min_h_0().w_full().overflow_y_scroll().child(body);
        let mut card = v_flex().id(("home-w", i)).col_span(span).row_span(rows).h(px(card_h)).min_w_0().p(px(W_PAD)).gap_3().child(head).child(body);
        card = if ruled {
            card.border_r_1().border_b_1().border_color(p.border).bg(p.content_bg)
        } else if p.gap > 0. {
            card.rounded(px(radius)).bg(p.tile_bg).shadow(p.sheet_shadows())
        } else {
            card.rounded(px(radius)).border_1().border_color(p.border)
        };
        if editing {
            card = card.border_1().border_dashed().border_color(p.border_strong);
        }
        grid = grid.child(card);
    }

    let header = h_flex()
        .gap_2()
        .items_center()
        .child(div().flex_1())
        .when(editing, |d| {
            d.child(home_button("home-reset", L::RotateCcw, t!("Reset"), &p, cx.listener(|_, _, _, cx| crate::settings::update(cx, reset))))
        })
        .child(home_button(
            "home-customize",
            L::LayoutGrid,
            if editing { t!("Done") } else { t!("Customize") },
            &p,
            cx.listener(|v, _, _, cx| {
                v.home_edit = !v.home_edit;
                cx.notify();
            }),
        ));

    let missing: Vec<usize> = (0..WIDGETS.len()).filter(|i| !placed.iter().any(|(j, _)| j == i)).collect();
    let adder = editing.then(|| {
        let mut row = h_flex().flex_wrap().gap_2().items_center().child(div().text_sm().text_color(p.text_secondary).child(t!("Add a widget:")));
        if missing.is_empty() {
            row = row.child(div().text_sm().text_color(p.text_tertiary).child(t!("every widget is on the page")));
        }
        for i in missing {
            let id = WIDGETS[i].id;
            row = row.child(home_button(
                SharedString::from(format!("home-add-{id}")),
                L::Plus,
                crate::i18n::t(WIDGETS[i].title),
                &p,
                cx.listener(move |_, _, _, cx| crate::settings::update(cx, |s| set_shown(s, id, true))),
            ));
        }
        row
    });

    // the Home page measures its own width (the list's probe isn't drawn here)
    let this = cx.entity();
    let width_probe = canvas(
        move |bounds, window, cx| {
            let changed = this.update(cx, |v, cx| {
                let changed = v.content_width != bounds.size.width;
                if changed {
                    v.content_width = bounds.size.width;
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
    div()
        .id("home")
        .size_full()
        .relative()
        .overflow_y_scroll()
        .child(width_probe)
        .child(
            v_flex()
                .p(px(PAD))
                .pt_3()
                .gap_3()
                .child(header)
                .child(grid)
                .when(shown == 0 && !editing, |d| {
                    d.child(div().text_sm().text_color(p.text_tertiary).child(t!("Home is empty. Customize adds widgets to it.")))
                })
                .children(adder),
        )
        .into_any_element()
}

/// Size, move and remove, in a widget's heading in Customize mode.
fn edit_controls(pos: usize, len: usize, i: usize, size: Size, p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    let w = &WIDGETS[i];
    let mut row = h_flex().gap_1().items_center();
    if w.sizes.len() > 1 {
        for &z in w.sizes {
            let on = z == size;
            row = row.child(
                div()
                    .id(SharedString::from(format!("home-size-{}-{}", w.id, z.key())))
                    .w(px(24.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(p.control_radius.min(4.)))
                    .text_xs()
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.accent).text_color(p.accent_text))
                    .when(!on, |d| d.border_1().border_color(p.border).text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
                    .tooltip(move |win, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(z.name())).build(win, cx))
                    .child(z.letter())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        crate::settings::update(cx, |s| {
                            edit(s, |l| {
                                if let Some(e) = l.iter_mut().find(|(j, _)| *j == i) {
                                    e.1 = z;
                                }
                            })
                        })
                    })),
            );
        }
        row = row.child(div().w(px(6.)));
    }
    let icon_btn = |key: &str, icon: L, tip: &'static str, enabled: bool| {
        div()
            .id(SharedString::from(format!("home-{key}-{}", w.id)))
            .size(px(22.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(p.control_radius.min(4.)))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)))
            .tooltip(move |win, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(tip)).build(win, cx))
            .child(Icon::new(icon).size(px(14.)).text_color(if enabled { p.text_secondary } else { p.text_disabled }))
    };
    let pick_folder = w.folder.then(|| {
        let id = w.id;
        let chosen = cx.global::<Settings>().home_layout.as_ref().and_then(|l| l.iter().find(|sl| sl.id == id)).map(|sl| sl.folder.clone()).filter(|f| !f.is_empty());
        let tip = match chosen {
            Some(f) => format!("Folder: {f}. Click to choose another."),
            None => "Folder: the first pinned one. Click to choose another.".to_string(),
        };
        icon_btn("folder", L::FolderOpen, "Choose the folder", true)
            .tooltip(move |win, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(win, cx))
            .on_click(cx.listener(move |_, _, _, cx| {
                let (tx, rx) = futures::channel::oneshot::channel::<Option<std::path::PathBuf>>();
                let title = t!("Choose the folder for this widget");
                leaf_sys::sta::run(move |owner| {
                    let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
                });
                cx.spawn(async move |_this, cx| {
                    if let Ok(Some(path)) = rx.await {
                        let _ = cx.update(|cx| crate::settings::update(cx, |s| set_folder(s, id, &path.to_string_lossy())));
                    }
                })
                .detach();
            }))
    });
    let shift = move |by: isize| {
        move |_: &mut FolderView, _: &ClickEvent, _: &mut Window, cx: &mut Context<FolderView>| {
            crate::settings::update(cx, |s| edit(s, |l| move_by(l, i, by)));
        }
    };
    // with the controls focused (a click on them, or Tab), Left / Right or Alt+Left / Alt+Right move the widget as the buttons do
    let keys = cx.listener(move |_: &mut FolderView, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<FolderView>| {
        let by = match ev.keystroke.key.as_str() {
            "left" => -1,
            "right" => 1,
            _ => return,
        };
        if ev.keystroke.modifiers.control || ev.keystroke.modifiers.shift {
            return;
        }
        cx.stop_propagation();
        crate::settings::update(cx, |s| edit(s, |l| move_by(l, i, by)));
    });
    row.children(pick_folder)
        .id(SharedString::from(format!("home-controls-{}", w.id)))
        .key_context(crate::actions::HOME_CONTROLS_CTX)
        .focusable()
        .tab_stop(true)
        .on_key_down(keys)
        .child(icon_btn("earlier", L::ArrowLeft, "Move earlier", pos > 0).when(pos > 0, |d| d.on_click(cx.listener(shift(-1)))))
        .child(icon_btn("later", L::ArrowRight, "Move later", pos + 1 < len).when(pos + 1 < len, |d| d.on_click(cx.listener(shift(1)))))
        .child(icon_btn("remove", L::X, "Remove from Home", true).on_click(cx.listener(move |_, _, _, cx| crate::settings::update(cx, |s| set_shown(s, WIDGETS[i].id, false)))))
        .into_any_element()
}

/// Moves widget `i` by `by` places in the layout.
pub fn move_by(l: &mut [Placed], i: usize, by: isize) {
    let Some(at) = l.iter().position(|(j, _)| *j == i) else { return };
    let to = at as isize + by;
    if to >= 0 && (to as usize) < l.len() {
        l.swap(at, to as usize);
    }
}

fn home_button(
    id: impl Into<ElementId>,
    icon: L,
    label: impl Into<SharedString>,
    p: &Palette,
    run: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .gap_1p5()
        .items_center()
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .text_color(p.text_secondary)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .child(Icon::new(icon).size(px(14.)))
        .child(label.into())
        .on_click(run)
}

#[cfg(test)]
mod tests {
    use super::{Placed, Settings, Size, WIDGETS, default_layout, edit, layout, move_by, reset, set_shown};

    fn ids(l: &[Placed]) -> Vec<(&'static str, &'static str)> {
        l.iter().map(|&(i, z)| (WIDGETS[i].id, z.key())).collect()
    }

    #[test]
    fn layout_saves_loads_migrates_and_falls_back() {
        // nothing saved: the default set
        let s = Settings::parse_migrated("").unwrap();
        assert_eq!(layout(&s), default_layout());
        assert_eq!(ids(&layout(&s))[0], ("pinned", "wide"));

        // the old hide list becomes a layout once, and is gone
        let s = Settings::parse_migrated("home_hidden = [\"quick\", \"tags\"]\n").unwrap();
        assert!(s.home_hidden.is_empty());
        let got = ids(&layout(&s));
        assert!(!got.iter().any(|(id, _)| *id == "pinned" || *id == "tags"), "{got:?}");
        assert!(got.iter().any(|(id, _)| *id == "drives"));

        // a layout saved and read again is the same layout
        let mut s = Settings::parse_migrated("").unwrap();
        edit(&mut s, |l| {
            l.retain(|(i, _)| WIDGETS[*i].id != "network");
            let second = l[1].0;
            move_by(l, second, -1);
            l[0].1 = Size::Medium;
        });
        set_shown(&mut s, "network", true);
        let text = toml::to_string(&s).unwrap();
        let back = Settings::parse_migrated(&text).unwrap();
        assert_eq!(layout(&back), layout(&s));
        assert_eq!(ids(&layout(&back))[0], ("drives", "medium"));
        assert_eq!(ids(&layout(&back)).last().unwrap().0, "network");

        // the size Large: kept where the widget can take it, the default where it cannot, and an older layout reads as before
        let s = Settings::parse_migrated("[[home_layout]]
id = \"drives\"
size = \"large\"
[[home_layout]]
id = \"tags\"
size = \"large\"
[[home_layout]]
id = \"recent\"
size = \"wide\"
").unwrap();
        assert_eq!(ids(&layout(&s)), vec![("drives", "large"), ("tags", "medium"), ("recent", "wide")]);
        let back = Settings::parse_migrated(&toml::to_string(&s).unwrap()).unwrap();
        assert_eq!(layout(&back), layout(&s));
        assert_eq!((Size::Large.span(4), Size::Large.rows(1), Size::Wide.rows(1), Size::Small.span(4)), (2, 2, 1, 1));

        // every widget removed stays empty
        let mut s = Settings::parse_migrated("").unwrap();
        edit(&mut s, |l| l.clear());
        let back = Settings::parse_migrated(&toml::to_string(&s).unwrap()).unwrap();
        assert!(layout(&back).is_empty());

        // broken: unknown widgets, repeats and sizes are skipped; nothing usable at all, or not a list: the default
        let s = Settings::parse_migrated(
            "[[home_layout]]\nid = \"recent\"\nsize = \"small\"\n[[home_layout]]\nid = \"recent\"\n[[home_layout]]\nid = \"weather\"\nsize = \"huge\"\n",
        )
        .unwrap();
        assert_eq!(ids(&layout(&s)), vec![("recent", "wide")]);
        let s = Settings::parse_migrated("[[home_layout]]\nid = \"weather\"\n").unwrap();
        assert_eq!(layout(&s), default_layout());
        let s = Settings::parse_migrated("home_layout = 7\n").unwrap();
        assert_eq!(layout(&s), default_layout());

        // reset: the default again, and nothing written
        let mut s = s;
        edit(&mut s, |l| l.clear());
        reset(&mut s);
        assert!(!toml::to_string(&s).unwrap().contains("home_layout"));
    }
}
