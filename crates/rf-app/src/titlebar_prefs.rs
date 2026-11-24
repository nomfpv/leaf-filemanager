//! The title bar's right-hand controls (the Canopy mockup's): the appearance drop, the main menu, the Visibility switch, the
//! dark / light moon and the Status Center dot. Each one is a real control over a setting or command that already exists; the
//! mockup's demo buttons ("Show context menu", "Show toast") are not app features and are left out.

use crate::t;
use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::fluent_menu::{Item, Menu, MenuIcon};
use crate::settings::{BackdropPref, Visibility};
use crate::theme::{self, Palette};

/// About the width the group takes in the title bar (the tab strip leaves it free).
pub const WIDTH: f32 = 236.;

/// The group's width now: the VPN dot adds one button when it is shown.
pub fn width(cx: &App) -> f32 {
    if vpn_shown(cx) { WIDTH + 34. } else { WIDTH }
}

/// The VPN dot is there once a Mullvad account is signed in (or while a tunnel runs): a PC without one has no use for it.
fn vpn_shown(cx: &App) -> bool {
    cx.global::<crate::settings::Settings>().vpn_signed_in || !matches!(leaf_vpn::state(), leaf_vpn::State::Off)
}

/// A 32 x 28 icon button of the group (the mockup's `.prefs button`).
fn icon_button(id: &'static str, p: &Palette, icon: L, tip: SharedString, popups_open: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .w(px(32.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .text_color(p.text_secondary)
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .when(!popups_open, move |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
        .child(Icon::new(icon).size(px(16.)))
        // the title bar arms a window move on mouse down: the click must stay with the button
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The group, left to right. `ops` is the Status Center the dot shows and opens.
pub fn render(p: &Palette, ops: &Entity<crate::status_center::Ops>, popups_open: bool, cx: &App) -> AnyElement {
    h_flex()
        .id("tb-prefs")
        .flex_shrink_0()
        .h_full()
        .items_center()
        .gap(px(2.))
        .px_1()
        .child(appearance_button(p, popups_open))
        .child(icon_button("tb-menu", p, L::Menu, t!("Menu").into(), popups_open).on_click(|_, window, cx| main_menu(window, cx)))
        .child(visibility_button(p, popups_open, cx))
        .child({
            // the moon in a dark window, the sun in a light one; a click is the "Light / dark theme" command
            let tip: SharedString = if p.dark { t!("Switch to the light theme").into() } else { t!("Switch to the dark theme").into() };
            icon_button("tb-theme", p, if p.dark { L::Moon } else { L::Sun }, tip, popups_open)
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::actions::ToggleTheme), cx))
        })
        .when(vpn_shown(cx), |d| d.child(vpn_dot(p, popups_open)))
        .child(status_dot(p, ops, popups_open, cx))
        .into_any_element()
}

/// leaf's VPN as a dot beside the Status Center's: hollow while off, the warning colour while connecting, green while on, red
/// after a failure. The tooltip says where (country, city and the exit address); a click opens its small menu.
fn vpn_dot(p: &Palette, popups_open: bool) -> AnyElement {
    let state = leaf_vpn::state();
    let (word, line) = crate::vpn::describe(&state);
    let tip: SharedString = match line {
        Some(l) => format!("{word}\n{l}").into(),
        None => word,
    };
    let fill: Option<Hsla> = match state {
        leaf_vpn::State::Off => None,
        leaf_vpn::State::Connecting { .. } => Some(p.warning),
        // (green only after the handshake AND Mullvad's own check that the traffic leaves through it)
        leaf_vpn::State::On { unverified: None, .. } => Some(p.success),
        leaf_vpn::State::On { .. } => Some(p.warning),
        leaf_vpn::State::Failed(_) => Some(p.danger),
    };
    let dot = div().size(px(12.)).rounded_full().border_1();
    let dot = match fill {
        Some(c) => dot.bg(c).border_color(p.border_strong),
        None => dot.border_color(p.text_secondary),
    };
    h_flex()
        .id("tb-vpn")
        .flex_shrink_0()
        .h(px(28.))
        .px(px(6.))
        .gap(px(5.))
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .text_size(px(11.))
        .text_color(p.text_secondary)
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .when(!popups_open, move |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
        .child(dot)
        .child("VPN")
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, window, cx| vpn_menu(window, cx))
        .into_any_element()
}

/// Connect or disconnect, the country (from Mullvad's list), and the VPN settings.
fn vpn_menu(window: &mut Window, cx: &mut App) {
    crate::vpn::want_relays(cx);
    let running = !matches!(leaf_vpn::state(), leaf_vpn::State::Off | leaf_vpn::State::Failed(_));
    let mut items = vec![if running {
        Item::action(t!("Disconnect"), MenuIcon::Lucide(L::Power), |_, cx| crate::vpn::disconnect(cx))
    } else {
        Item::action(t!("Connect"), MenuIcon::Lucide(L::Power), |_, cx| crate::vpn::connect(cx))
    }];
    let places = leaf_vpn::mullvad::places(&crate::vpn::relays());
    if !places.is_empty() {
        let now = cx.global::<crate::settings::Settings>().vpn_country.trim().to_ascii_lowercase();
        let mut countries = vec![Item::action(t!("Anywhere"), MenuIcon::None, |_, cx: &mut App| {
            crate::settings::update(cx, |s| {
                s.vpn_country.clear();
                s.vpn_city.clear();
            })
        })
        .checked(now.is_empty())];
        countries.extend(places.into_iter().map(|(name, code, _)| {
            let on = code == now;
            Item::action(name, MenuIcon::None, move |_, cx: &mut App| {
                let code = code.clone();
                crate::settings::update(cx, move |s| {
                    if s.vpn_country != code {
                        s.vpn_city.clear();
                    }
                    s.vpn_country = code;
                })
            })
            .checked(on)
        }));
        items.push(Item::sub(t!("Country"), MenuIcon::Lucide(L::Globe), countries));
    }
    items.push(Item::Separator);
    items.push(Item::action(t!("VPN settings"), MenuIcon::Lucide(L::Settings), |w, cx| {
        crate::settings_view::open_page(crate::settings_view::Page::Vpn, cx);
        w.dispatch_action(Box::new(crate::actions::GoSettings), cx)
    }));
    crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
}

/// The drop: the window's material (the same choices as Settings > Appearance > Window material) and the theme.
fn appearance_button(p: &Palette, popups_open: bool) -> AnyElement {
    icon_button("tb-appearance", p, L::Droplet, t!("Appearance").into(), popups_open)
        .on_click(|_, window, cx| appearance_menu(window, cx))
        .into_any_element()
}

fn appearance_menu(window: &mut Window, cx: &mut App) {
    let s = cx.global::<crate::settings::Settings>();
    let (backdrop, theme_name) = (s.backdrop, s.theme_name.trim().to_ascii_lowercase());
    let materials: Vec<(BackdropPref, SharedString)> = if crate::settings::materials_available() {
        vec![
            (BackdropPref::Theme, t!("Theme default").into()),
            (BackdropPref::Mica, "Mica".into()),
            (BackdropPref::MicaAlt, "Mica Alt".into()),
            (BackdropPref::Acrylic, t!("Acrylic").into()),
            (BackdropPref::Solid, t!("Solid (no see-through)").into()),
            (BackdropPref::Glass, t!("HyperGlass (experimental)").into()),
        ]
    } else {
        vec![(BackdropPref::Solid, t!("Solid").into()), (BackdropPref::Acrylic, t!("Acrylic").into()), (BackdropPref::Glass, t!("HyperGlass (experimental)").into())]
    };
    let shown = if crate::settings::materials_available() { backdrop } else { s.backdrop_in_use() };
    let mut items: Vec<Item> = materials
        .into_iter()
        .map(|(v, label)| Item::action(label, MenuIcon::None, move |_, cx: &mut App| crate::settings::update(cx, |s| s.backdrop = v)).checked(v == shown))
        .collect();
    let themes = crate::theme::data::list();
    items.push(Item::Separator);
    items.push(Item::sub(
        t!("Theme"),
        MenuIcon::Lucide(L::Palette),
        themes
            .into_iter()
            .map(|(id, name)| {
                let on = id == theme_name;
                Item::action(name, MenuIcon::None, move |_, cx: &mut App| {
                    let id = id.clone();
                    crate::settings::update(cx, move |s| s.theme_name = id)
                })
                .checked(on)
            })
            .collect(),
    ));
    items.push(Item::action(t!("Appearance settings"), MenuIcon::Lucide(L::Settings), |w, cx| w.dispatch_action(Box::new(crate::actions::GoSettings), cx)));
    crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
}

/// The main menu: the window and app commands (each one the registry's command, through its action).
fn main_menu(window: &mut Window, cx: &mut App) {
    use crate::actions::*;
    fn run<A: Action + Default>() -> impl Fn(&mut Window, &mut App) + 'static {
        |w, cx| w.dispatch_action(Box::new(A::default()), cx)
    }
    let items = vec![
        Item::action(t!("New tab"), MenuIcon::Lucide(L::Plus), run::<NewTab>()).hint("Ctrl+T"),
        Item::action(t!("New window"), MenuIcon::Lucide(L::AppWindow), run::<NewWindow>()).hint("Ctrl+N"),
        Item::action(t!("Reopen closed tab"), MenuIcon::Lucide(L::RotateCcw), run::<ReopenClosedTab>()).hint("Ctrl+Shift+T"),
        Item::action(t!("Save session…"), MenuIcon::Lucide(L::Save), run::<SaveSession>()),
        Item::Separator,
    ];
    let mut items = items;
    items.extend(command_groups(cx));
    items.extend([
        Item::Separator,
        Item::action(t!("Command palette"), MenuIcon::Lucide(L::Command), run::<ToggleCommandPalette>()).hint("Ctrl+Shift+P"),
        Item::action(t!("Sync jobs"), MenuIcon::Lucide(L::RefreshCw), run::<SyncJobs>()),
        Item::action(t!("Full screen"), MenuIcon::Lucide(L::Maximize), run::<ToggleFullscreen>()).hint("F11"),
        Item::action(t!("Always on top"), MenuIcon::Lucide(L::Pin), run::<ToggleAlwaysOnTop>()),
        Item::Separator,
        Item::action(t!("Settings"), MenuIcon::Lucide(L::Settings), run::<GoSettings>()).hint("Ctrl+,"),
        Item::action(t!("Guide"), MenuIcon::Lucide(L::CircleQuestionMark), run::<ShowGuide>()).hint("F1"),
        Item::action(t!("What leaf can do"), MenuIcon::Lucide(L::Sparkles), run::<ShowFeatures>()),
        Item::action(t!("Restart leaf"), MenuIcon::Lucide(L::RotateCw), run::<RestartApp>()),
    ]);
    crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
}

/// Every command of the registry as one submenu per group, so that no command is reached by typing only: the palette's rows that
/// are offered now, plus Redo and the tool bar switch. Made from the registry, so a new command is in the menu without a line here.
/// Inside a group the commands of one module (downloads, sync, agents...) stand together, with a line between the modules.
fn command_groups(cx: &App) -> Vec<Item> {
    use crate::registry::{self, Category, CommandSpec, Places};
    const EXTRA: [&str; 2] = ["Redo", "ToggleToolbar"];
    let rows: Vec<&'static CommandSpec> = registry::all()
        .filter(|c| (c.places.contains(Places::PALETTE) || EXTRA.contains(&c.id)) && c.hint.is_empty() && c.available(cx))
        .collect();
    let mut out = Vec::new();
    for cat in Category::ALL {
        let mut of: Vec<&'static CommandSpec> = rows.iter().copied().filter(|c| c.category == cat).collect();
        // stable: the registry's order stays inside a module
        of.sort_by_key(|c| c.module().map_or(0, |m| m as usize + 1));
        let Some(mut last) = of.first().map(|c| c.module()) else { continue };
        let mut items = Vec::with_capacity(of.len() + 4);
        for c in of {
            if c.module() != last {
                items.push(Item::Separator);
                last = c.module();
            }
            let item = Item::action(crate::i18n::t(c.title), MenuIcon::Lucide(c.icon.clone()), move |w: &mut Window, cx: &mut App| w.dispatch_action(c.make_action(), cx));
            let keys = c.shown_keys();
            items.push(if keys.is_empty() { item } else { item.hint(keys) });
        }
        let icon = match cat {
            Category::Tabs => L::PanelTop,
            Category::Navigation => L::FolderOpen,
            Category::Selection => L::SquareCheck,
            Category::Files => L::FilePlus,
            Category::CopyPath => L::Link,
            Category::View => L::Eye,
            Category::Panes => L::Columns2,
            Category::Tools => L::Wrench,
            Category::Viewer | Category::App => L::Info,
            Category::Custom => L::WandSparkles,
        };
        out.push(Item::sub(crate::i18n::t(cat.title()), MenuIcon::Lucide(icon), items));
    }
    out
}

/// Settings > Appearance > Visibility as a pill: its icon and the level, a click switches Standard / High.
fn visibility_button(p: &Palette, popups_open: bool, cx: &App) -> AnyElement {
    let high = cx.global::<crate::settings::Settings>().visibility == Visibility::High;
    let label: SharedString = if high { t!("High").into() } else { t!("Standard").into() };
    let tip: SharedString = format!("{}: {}", t!("Visibility"), label).into();
    div()
        .id("tb-visibility")
        .flex_shrink_0()
        .h(px(28.))
        .pl(px(6.))
        .pr(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(theme::RADIUS_SM)
        .border_1()
        .border_color(if high { p.border_strong } else { p.border })
        .text_size(px(11.))
        .text_color(if high { p.text } else { p.text_secondary })
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .when(!popups_open, move |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
        .child(contrast_glyph(if high { p.text } else { p.text_secondary }))
        .child(label)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, _, cx| crate::settings::update(cx, move |s| s.visibility = if high { Visibility::Standard } else { Visibility::High }))
        .into_any_element()
}

/// The Status Center's state as a dot: green while nothing runs, the accent while operations run, red while a failure is unseen.
/// A click opens (or closes) the Status Center, as its button in the address row does.
fn status_dot(p: &Palette, ops: &Entity<crate::status_center::Ops>, popups_open: bool, cx: &App) -> AnyElement {
    let (open, unseen, running) = ops.read(cx).dot_state();
    let (color, tip): (Hsla, SharedString) = if unseen {
        (p.danger, t!("Status Center: an operation failed").into())
    } else if running > 0 {
        (p.accent, t!("Status Center: {n} running").replace("{n}", &running.to_string()).into())
    } else {
        (p.success, t!("Status Center: nothing running").into())
    };
    let ops = ops.clone();
    div()
        .id("tb-status")
        .flex_shrink_0()
        .w(px(32.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .when(open, |d| d.bg(p.pressed))
        .when(!open, |d| d.hover(|s| s.bg(p.hover)))
        .when(!popups_open && !open, move |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
        // (the mockup's 14 px dot in a thin ring of the strong line colour)
        .child(div().size(px(14.)).rounded_full().bg(color).border_1().border_color(p.border_strong))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            let owner = window.window_handle();
            ops.update(cx, |o, cx| o.toggle(owner, cx))
        })
        .into_any_element()
}

/// The mockup's contrast mark: a 12 px ring with its right half filled (Lucide's has a different inner shape). The half is a filled
/// circle cut by a clip the width of half the ring.
fn contrast_glyph(color: Hsla) -> AnyElement {
    div()
        .size(px(16.))
        .flex_shrink_0()
        .relative()
        .child(div().absolute().left(px(1.35)).top(px(1.35)).size(px(13.3)).rounded_full().border(px(1.3)).border_color(color))
        .child(
            div()
                .absolute()
                .left(px(8.))
                .top(px(2.))
                .w(px(6.))
                .h(px(12.))
                .overflow_hidden()
                .child(div().absolute().left(px(-6.)).top_0().size(px(12.)).rounded_full().bg(color)),
        )
        .into_any_element()
}
