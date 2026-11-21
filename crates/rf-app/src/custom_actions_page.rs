//! Settings -> Your actions: the list of the user's own commands (`actions.toml`), add / edit / remove, "Open actions.toml", and three
//! ready-made examples to add with one click. Nothing is installed by default.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use rf_core::actions::{self, CustomAction, Place};

use crate::custom_actions;
use crate::i18n::tx;
use crate::settings_view::{button, card_body, section, shell, switch};
use crate::t;
use crate::theme::Palette;

fn place_label(p: Place) -> &'static str {
    match p {
        Place::MenuFile => "Menu: files",
        Place::MenuFolder => "Menu: folders",
        Place::MenuBackground => "Menu: empty space",
        Place::Palette => "Command palette",
        Place::Toolbar => "Tool bar",
    }
}

fn store(list: Vec<CustomAction>, window: &mut Window, cx: &mut App) {
    if let Err(e) = custom_actions::save(list, cx) {
        crate::prompt::notice(tx("Could not save actions.toml"), format!("{e:#}"), window, cx);
    }
    cx.refresh_windows();
}

pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let list = custom_actions::list(cx);
    let mut v: Vec<AnyElement> = Vec::new();
    if custom_actions::broken(cx) {
        v.push(
            div()
                .pb_2()
                .text_sm()
                .text_color(p.danger)
                .child(t!("actions.toml cannot be read and is ignored. Fix it with Open actions.toml; saving here keeps a copy of it aside first."))
                .into_any_element(),
        );
    }
    let mut rows = v_flex().py_1();
    if list.is_empty() {
        rows = rows.child(div().px_4().py_3().text_sm().text_color(p.text_secondary).child(t!("No actions yet. Add one, or start from an example below.")));
    }
    for (i, a) in list.iter().enumerate() {
        let wheres = a.places.iter().map(|w| crate::i18n::t(place_label(*w))).collect::<Vec<_>>().join(", ");
        let (edit_a, rm_name) = (a.clone(), a.name.clone());
        rows = rows.child(
            h_flex()
                .min_h(px(52.))
                .px_4()
                .py_2()
                .gap_3()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(a.name.clone()))
                        .child(div().text_xs().text_color(p.text_secondary).truncate().child(a.command.clone()))
                        .child(div().text_xs().text_color(p.text_secondary).child(wheres)),
                )
                .child(button(("act-edit", i), p, t!("Edit"), move |window, cx| edit(Some(edit_a.clone()), window, cx)))
                .child(button(("act-rm", i), p, t!("Remove"), move |window, cx| {
                    let mut all = custom_actions::list(cx);
                    all.retain(|x| x.name != rm_name);
                    store(all, window, cx);
                })),
        );
    }
    v.push(
        shell(p)
            .child(card_body(
                p,
                L::Zap,
                t!("Your actions").into(),
                Some(t!("Your own commands, written in actions.toml. They show in the right-click menu, the command palette and the shortcut list.").into()),
                Some(
                    h_flex()
                        .gap_2()
                        .child(button("act-new", p, t!("Add action"), |window, cx| edit(None, window, cx)))
                        .child(button("act-file", p, t!("Open actions.toml"), |_, cx| custom_actions::open_file(cx)))
                        .into_any_element(),
                ),
            ))
            .child(div().h(px(1.)).bg(p.divider))
            .child(rows)
            .into_any_element(),
    );
    v.push(
        div()
            .pt_2()
            .text_xs()
            .text_color(p.text_secondary)
            .child(t!("In a command: {path} is the first selected item, {files} every selected item, {dir} the current folder, {name} the first item's name. Each is passed as one argument, so spaces are safe. Programs also get LEAF_PATH, LEAF_FILES, LEAF_DIR and LEAF_NAME as environment variables."))
            .into_any_element(),
    );
    v.push(section(p, t!("Examples")));
    let mut ex = v_flex().py_1();
    for (i, e) in actions::examples().into_iter().enumerate() {
        let have = list.iter().any(|a| a.name.eq_ignore_ascii_case(&e.name));
        let name = e.name.clone();
        let cmd = e.command.clone();
        ex = ex.child(
            h_flex()
                .min_h(px(48.))
                .px_4()
                .py_2()
                .gap_3()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child(name))
                        .child(div().text_xs().text_color(p.text_secondary).truncate().child(cmd)),
                )
                .child(if have {
                    div().text_xs().text_color(p.text_secondary).child(t!("Added")).into_any_element()
                } else {
                    button(("act-ex", i), p, t!("Add"), move |window, cx| {
                        let mut all = custom_actions::list(cx);
                        all.push(e.clone());
                        store(all, window, cx);
                    })
                }),
        );
    }
    v.push(shell(p).child(ex).into_any_element());
    v
}

/// The add / edit form. `current` is the action being edited (found again by its name when saved).
fn edit(current: Option<CustomAction>, window: &mut Window, cx: &mut App) {
    let init = current.clone().unwrap_or(CustomAction { places: vec![Place::MenuFile, Place::Palette], ..Default::default() });
    let name = cx.new(|cx| InputState::new(window, cx).default_value(init.name.clone()).placeholder(tx("Name")));
    let command = cx.new(|cx| InputState::new(window, cx).default_value(init.command.clone()).placeholder("code {path}"));
    let shortcut = cx.new(|cx| InputState::new(window, cx).default_value(init.shortcut.clone().unwrap_or_default()).placeholder("ctrl-alt-v"));
    let extensions = cx.new(|cx| InputState::new(window, cx).default_value(init.extensions.join(", ")).placeholder("png, jpg"));
    let icon = cx.new(|cx| InputState::new(window, cx).default_value(init.icon.clone().unwrap_or_default()).placeholder("code"));
    let places: Entity<Vec<Place>> = cx.new(|_| init.places.clone());
    let confirm = cx.new(|_| init.confirm);
    let terminal = cx.new(|_| init.terminal);
    let error: Entity<Option<String>> = cx.new(|_| None);
    let focus_first = std::cell::Cell::new(true);
    window.open_alert_dialog(cx, move |dialog, window, cx| {
        if focus_first.replace(false) {
            name.focus_handle(cx).focus(window, cx);
        }
        let pal = Palette::for_window(window);
        let (name_in, command_in, shortcut_in, ext_in, icon_in, places_in, confirm_in, terminal_in, err) =
            (name.clone(), command.clone(), shortcut.clone(), extensions.clone(), icon.clone(), places.clone(), confirm.clone(), terminal.clone(), error.clone());
        let current = current.clone();
        let mut wheres = h_flex().gap_x_4().gap_y_1().flex_wrap();
        for (i, w) in Place::ALL.into_iter().enumerate() {
            let on = places.read(cx).contains(&w);
            let ent = places.clone();
            wheres = wheres.child(
                h_flex().gap_2().items_center().child(switch(("act-where", i), &pal, on, move |_, cx| {
                    ent.update(cx, |l, cx| {
                        if let Some(at) = l.iter().position(|x| *x == w) {
                            l.remove(at);
                        } else {
                            l.push(w);
                        }
                        cx.notify();
                    });
                    cx.refresh_windows();
                }))
                .child(div().text_sm().child(crate::i18n::t(place_label(w)))),
            );
        }
        let flag = |id: &'static str, label: &'static str, ent: &Entity<bool>, cx: &App| {
            let on = *ent.read(cx);
            let ent = ent.clone();
            h_flex().gap_2().items_center().child(switch(id, &pal, on, move |_, cx| {
                ent.update(cx, |b, cx| {
                    *b = !*b;
                    cx.notify();
                });
                cx.refresh_windows();
            }))
            .child(div().text_sm().child(crate::i18n::t(label)))
        };
        dialog
            .title(tx("Your action"))
            .bg(pal.card_bg)
            .width(px(540.))
            .show_cancel(true)
            .child(
                v_flex()
                    .gap_2()
                    .child(tx("Name"))
                    .child(Input::new(&name))
                    .child(tx("Command"))
                    .child(Input::new(&command))
                    .child(div().text_xs().text_color(pal.text_secondary).child(tx("{path} first selected item, {files} all selected, {dir} current folder, {name} its name")))
                    .child(tx("Offered in"))
                    .child(wheres)
                    .child(h_flex().gap_2()
                        .child(v_flex().gap_2().flex_1().min_w_0().child(tx("Shortcut (optional)")).child(Input::new(&shortcut)))
                        .child(v_flex().gap_2().flex_1().min_w_0().child(tx("Only these file types (optional)")).child(Input::new(&extensions))))
                    .child(tx("Icon (optional: code, terminal, folder, hash, play)"))
                    .child(Input::new(&icon))
                    .child(h_flex().gap_x_6().child(flag("act-confirm", "Ask before running", &confirm, cx)).child(flag("act-terminal", "Run in a terminal window", &terminal, cx)))
                    .children(error.read(cx).clone().map(|m| div().text_color(pal.danger).child(m))),
            )
            .on_ok(move |_, _, cx| {
                let split = |s: &str| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>();
                let act = CustomAction {
                    name: name_in.read(cx).value().trim().to_string(),
                    command: command_in.read(cx).value().trim().to_string(),
                    places: places_in.read(cx).clone(),
                    shortcut: Some(shortcut_in.read(cx).value().trim().to_lowercase()).filter(|s| !s.is_empty()),
                    extensions: split(&ext_in.read(cx).value()),
                    confirm: *confirm_in.read(cx),
                    terminal: *terminal_in.read(cx),
                    icon: Some(icon_in.read(cx).value().trim().to_string()).filter(|s| !s.is_empty()),
                };
                let mut all = custom_actions::list(cx);
                let message = if act.name.is_empty() {
                    Some(tx("Give the action a name."))
                } else if actions::split_words(&act.command).map(|w| w.is_empty()).unwrap_or(true) {
                    Some(tx("Enter a command (with its quotes closed)."))
                } else if all.iter().any(|x| x.name.eq_ignore_ascii_case(&act.name) && current.as_ref().is_none_or(|c| !c.name.eq_ignore_ascii_case(&act.name))) {
                    Some(tx("An action with this name exists."))
                } else {
                    None
                };
                if let Some(m) = message {
                    err.update(cx, |e, cx| {
                        *e = Some(m.to_string());
                        cx.notify();
                    });
                    cx.refresh_windows();
                    return false;
                }
                match current.as_ref().and_then(|c| all.iter().position(|x| x.name == c.name)) {
                    Some(at) => all[at] = act,
                    None => all.push(act),
                }
                if let Err(e) = custom_actions::save(all, cx) {
                    err.update(cx, |m, cx| {
                        *m = Some(format!("{e:#}"));
                        cx.notify();
                    });
                    cx.refresh_windows();
                    return false;
                }
                true
            })
    });
}
