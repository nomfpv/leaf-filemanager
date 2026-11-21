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
        .into_any_element()
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
        Item::action(t!("Command palette"), MenuIcon::Lucide(L::Command), run::<ToggleCommandPalette>()).hint("Ctrl+Shift+P"),
        Item::action(t!("Sync jobs"), MenuIcon::Lucide(L::RefreshCw), run::<SyncJobs>()),
        Item::action(t!("Full screen"), MenuIcon::Lucide(L::Maximize), run::<ToggleFullscreen>()).hint("F11"),
        Item::action(t!("Always on top"), MenuIcon::Lucide(L::Pin), run::<ToggleAlwaysOnTop>()),
        Item::Separator,
        Item::action(t!("Settings"), MenuIcon::Lucide(L::Settings), run::<GoSettings>()).hint("Ctrl+,"),
        Item::action(t!("What leaf can do"), MenuIcon::Lucide(L::Sparkles), run::<ShowFeatures>()),
        Item::action(t!("Restart leaf"), MenuIcon::Lucide(L::RotateCw), run::<RestartApp>()),
    ];
    crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
}
