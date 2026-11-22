//! Settings > Features (one switch per part of leaf, `modules.rs`), the first-run setup that doubles as a feature guide, and the
//! first-use tip strips.
//!
//! The setup is a page of the Settings location without the page list ([`Page::Setup`]): Welcome, Look, What you use leaf for,
//! Features, System, Helpers, Done. Nothing is written until the user reaches Done; Skip keeps the defaults (every part on, nothing
//! written to the system). Only the theme is shown live while choosing, and Skip puts it back.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::modules::{self, Module, PROFILES, Spec};
use crate::settings::{self, Settings, ThemePref};
use crate::settings_view::{self, Page, button, card_body, section, shell, switch};
use crate::t;
use crate::theme::{self, Palette};

// ---------------------------------------------------------------- the module cards (Settings > Features, and the setup)

/// One module: icon, name, one line, the helper programs' state, and its switch. `toggle` flips it (the setup stages the change,
/// Settings makes it).
pub fn module_card(p: &Palette, spec: &'static Spec, on: bool, s: &Settings, toggle: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    let mut lines: Vec<String> = vec![crate::i18n::t(spec.desc).to_string()];
    lines.push(format!("{} {}", crate::i18n::t("Off hides:"), crate::i18n::t(spec.hides)));
    if let Some(n) = spec.needs {
        lines.push(format!("{} {}.", crate::i18n::t("Needs"), crate::i18n::t(modules::spec(n).name)));
    }
    if !spec.restart_note.is_empty() {
        lines.push(crate::i18n::t(spec.restart_note).to_string());
    }
    for h in spec.helpers {
        let (found, version) = h.found(s);
        lines.push(match (found, version) {
            (true, Some(v)) => format!("{} {}, {} {v}", h.name(), crate::i18n::t("found"), crate::i18n::t("version")),
            (true, None) => format!("{} {}", h.name(), crate::i18n::t("found")),
            (false, _) => format!("{} {}: {} `{}`", h.name(), crate::i18n::t("not found"), crate::i18n::t("install with"), h.install()),
        });
    }
    shell(p)
        .child(card_body(p, spec.icon, crate::i18n::t(spec.name).into(), Some(lines.join("\n").into()), Some(switch(("module", spec.module as usize), p, on, toggle))))
        .into_any_element()
}

/// A Settings page of a module that is off: one line and a "Turn on" button.
pub fn off_card(p: &Palette, m: Module) -> AnyElement {
    let spec = modules::spec(m);
    shell(p)
        .child(card_body(
            p,
            spec.icon,
            format!("{}: {}", crate::i18n::t(spec.name), crate::i18n::t("off")).into(),
            Some(crate::i18n::t(spec.desc).to_string().into()),
            Some(button(("module-on", m as usize), p, t!("Turn on"), move |_, cx| settings::update(cx, |s| modules::set(s, m, true)))),
        ))
        .into_any_element()
}

/// The page of Settings > Features.
pub fn features_page(p: &Palette, s: &Settings, _cx: &mut App) -> Vec<AnyElement> {
    let mut v: Vec<AnyElement> = vec![div()
        .pb_2()
        .text_sm()
        .text_color(p.text_secondary)
        .child(t!("Switch whole parts of leaf off when you do not use them: their commands, menu rows, sidebar sections and Home widgets go away and nothing of them runs. What they saved stays, and turning one on brings everything back."))
        .into_any_element()];
    for spec in modules::MODULES {
        let (m, on) = (spec.module, modules::own_on(s, spec.module));
        v.push(module_card(p, spec, on, s, move |_, cx| settings::update(cx, |s| modules::set(s, m, !on))));
    }
    v.push(section(p, t!("Tips and setup")));
    v.push(
        shell(p)
            .child(card_body(
                p,
                L::Info,
                t!("Show first-use tips").into(),
                Some(t!("A small strip the first time you open a part of leaf, with two or three pointers.").into()),
                Some(settings_view::sw("tips", p, !s.tips_off, |s, v| s.tips_off = !v)),
            ))
            .into_any_element(),
    );
    v.push(
        shell(p)
            .child(card_body(
                p,
                L::Sparkles,
                t!("Run setup again").into(),
                Some(t!("The guide that opens on the first start: look, what you use leaf for, features, helper programs.").into()),
                Some(button("setup-again", p, t!("Run setup"), |_, cx| begin(cx))),
            ))
            .into_any_element(),
    );
    v
}

// ---------------------------------------------------------------- first-use tips

/// The tip strip of a module's page: what it is, where to start, where the settings are. Dismissed once for good.
pub fn tip_strip(p: &Palette, m: Module) -> AnyElement {
    let spec = modules::spec(m);
    let id = spec.id;
    h_flex()
        .id(("tip", m as usize))
        .w_full()
        .gap_3()
        .px_4()
        .py_3()
        .mb_2()
        .items_start()
        .rounded(theme::RADIUS)
        .bg(p.selection)
        .border_1()
        .border_color(p.border)
        .child(Icon::new(L::Info).size(px(16.)).flex_shrink_0().text_color(p.accent))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::t(spec.name)))
                .children(spec.tips.iter().map(|t| div().text_xs().text_color(p.text_secondary).child(crate::i18n::t(*t)))),
        )
        .child(
            v_flex()
                .gap_1()
                .flex_shrink_0()
                .child(button(("tip-ok", m as usize), p, t!("Got it"), move |_, cx| settings::update(cx, |s| s.tips_seen.push(id.to_string()))))
                .child(button(("tip-off", m as usize), p, t!("No tips"), |_, cx| settings::update(cx, |s| s.tips_off = true))),
        )
        .into_any_element()
}

/// `content` with the module's tip strip above it, when one is due (the pages of leaf's own that belong to a module).
pub fn with_tip(content: AnyElement, loc: &rf_core::history::Location, p: &Palette, cx: &App) -> AnyElement {
    let Some(m) = modules::of_location(loc).filter(|m| modules::tip_due(cx.global::<Settings>(), *m)) else { return content };
    v_flex().size_full().child(div().px_3().pt_2().child(tip_strip(p, m))).child(div().flex_1().min_h_0().child(content)).into_any_element()
}

// ---------------------------------------------------------------- the setup

const STEPS: usize = 7;
const THEMES: [(&str, &str, &str); 3] = [
    ("canopy", "Canopy", "Soft sheets with shadows: the calm default."),
    ("stem", "Stem", "Flat and light, with thin lines."),
    ("index", "Index", "Ruled and dense, with a monospace look."),
];

struct Setup {
    step: usize,
    /// The staged switches, one per module (own switch, `Module as usize`).
    mods: Vec<bool>,
    profile: Option<usize>,
    /// "Make leaf the default file manager" as chosen here, and as it was when the setup opened.
    dfm: bool,
    dfm_was: bool,
    /// The look when the setup opened: what Skip puts back.
    theme0: (String, ThemePref),
    /// The theme shown now is a preview that was not applied: every way out but "Apply and finish" puts `theme0` back ([`revert_preview`]).
    previewing: bool,
}
impl Global for Setup {}

fn with<R>(cx: &mut App, f: impl FnOnce(&mut Setup) -> R) -> R {
    f(cx.global_mut::<Setup>())
}

/// Opens the setup (first start, "Run setup" in Settings, the "Setup guide" command).
pub fn begin(cx: &mut App) {
    let s = cx.global::<Settings>();
    let dfm = crate::DEFAULT_FM.load(std::sync::atomic::Ordering::Relaxed);
    let st = Setup {
        step: 0,
        mods: Module::ALL.iter().map(|m| modules::own_on(s, *m)).collect(),
        profile: None,
        dfm,
        dfm_was: dfm,
        theme0: (s.theme_name.clone(), s.theme),
        previewing: false,
    };
    cx.set_global(st);
    settings_view::open_page(Page::Setup, cx);
    cx.refresh_windows();
}

/// The "Setup guide" command: Settings, on the setup.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::SetupGuide, cx| {
        begin(cx);
        cx.defer(|cx| {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, cx| window.dispatch_action(Box::new(crate::actions::GoSettings), cx));
            }
        });
    });
}

/// The theme and mode shown now, not written: Skip puts the first ones back.
fn preview(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    if cx.has_global::<Setup>() {
        with(cx, |st| st.previewing = true);
    }
    cx.update_global::<Settings, _>(|s, _| f(s));
    cx.global::<Settings>().apply_side_effects();
    settings::apply_component_theme(cx);
    cx.refresh_windows();
}

/// Puts the look back when the setup is left any way but "Apply and finish": Skip, another Settings page, a closed window, any other
/// change of the settings meanwhile (which would write the preview). Nothing happens when no preview is showing.
pub fn revert_preview(cx: &mut App) {
    if !cx.has_global::<Setup>() || !with(cx, |st| std::mem::take(&mut st.previewing)) {
        return;
    }
    let (name, mode) = with(cx, |st| st.theme0.clone());
    preview(cx, |s| {
        s.theme_name = name;
        s.theme = mode;
    });
    with(cx, |st| st.previewing = false);
}

/// Leaves the setup for the page that was asked for.
fn leave(page: Page, window: &mut Window, cx: &mut App) {
    settings_view::open_page(page, cx);
    window.refresh();
}

fn skip(window: &mut Window, cx: &mut App) {
    revert_preview(cx);
    // (nothing else is written: every part stays on, the system is not touched)
    settings::update(cx, |s| s.setup_done = true);
    cx.defer(|cx| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, cx| window.dispatch_action(Box::new(crate::actions::GoHome), cx));
        }
    });
    leave(Page::General, window, cx);
}

/// Reaching Done: the modules, the look and the one system switch are applied.
fn apply(cx: &mut App) {
    // (the look shown is the one chosen now: it stays and is written with the rest)
    let (mods, dfm, dfm_was) = with(cx, |st| {
        st.previewing = false;
        (st.mods.clone(), st.dfm, st.dfm_was)
    });
    settings::update(cx, |s| {
        for (m, on) in Module::ALL.iter().zip(&mods) {
            modules::set(s, *m, *on);
        }
        s.setup_done = true;
    });
    // the same code path as Settings > Advanced (its refusal for a build-folder copy included)
    if dfm != dfm_was {
        settings_view::set_default_file_manager(dfm, settings_view::default_fm_refused());
    }
}

fn choice(p: &Palette, id: impl Into<ElementId>, icon: L, title: &'static str, desc: &'static str, on: bool, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .id(id)
        .w(px(210.))
        .p_4()
        .rounded(theme::RADIUS)
        .border_1()
        .border_color(if on { p.accent } else { p.border })
        .bg(if on { p.selection } else { p.hover })
        .cursor_pointer()
        .tab_index(0)
        .hover(|s| s.bg(p.pressed))
        .on_click(move |_, window, cx| run(window, cx))
        .child(
            v_flex()
                .gap_2()
                .child(Icon::new(icon).size(px(20.)).text_color(if on { p.accent } else { p.text }))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::t(title)))
                .child(div().text_xs().text_color(p.text_secondary).child(crate::i18n::t(desc))),
        )
        .into_any_element()
}

fn heading(p: &Palette, title: &'static str, sub: &'static str) -> AnyElement {
    v_flex()
        .gap_1()
        .pb_2()
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::t(title)))
        .child(div().text_sm().text_color(p.text_secondary).child(crate::i18n::t(sub)))
        .into_any_element()
}

/// The setup page (replaces the whole Settings page, its list of pages included).
pub fn render(p: &Palette, s: &Settings, cx: &mut App) -> AnyElement {
    if !cx.has_global::<Setup>() {
        begin(cx);
    }
    let (step, mods, profile, dfm) = with(cx, |st| (st.step, st.mods.clone(), st.profile, st.dfm));
    let mut body: Vec<AnyElement> = Vec::new();
    match step {
        0 => {
            body.push(heading(p, "Welcome to leaf", "A fast file manager for Windows and Linux: tabs, two panes, previews, search, and the servers, backups and tools you choose to switch on."));
            body.push(div().text_sm().text_color(p.text_secondary).child(t!("This takes a minute. Nothing changes until the last page, and you can skip it: everything stays on and nothing is written to your system. You can run it again from Settings > Features or the command palette (Setup guide).")).into_any_element());
        }
        1 => {
            body.push(heading(p, "Look", "Pick a theme and a mode. You see it at once; Skip puts the old look back."));
            let cur = s.theme_name.trim().to_ascii_lowercase();
            body.push(
                h_flex()
                    .flex_wrap()
                    .items_stretch()
                    .gap_3()
                    .children(THEMES.iter().enumerate().map(|(i, (id, name, desc))| {
                        let id = *id;
                        choice(p, ("setup-theme", i), L::Palette, name, desc, cur == id, move |window, cx| {
                            preview(cx, |s| s.theme_name = id.to_string());
                            window.refresh();
                        })
                    }))
                    .into_any_element(),
            );
            let mode = |i: usize, label: &'static str, icon: L, m: ThemePref| {
                choice(p, ("setup-mode", i), icon, label, "", s.theme == m, move |window, cx| {
                    preview(cx, |s| s.theme = m);
                    window.refresh();
                })
            };
            body.push(section(p, t!("Mode")));
            body.push(
                h_flex()
                    .flex_wrap()
                    .items_stretch()
                    .gap_3()
                    .child(mode(0, "Light", L::Sun, ThemePref::Light))
                    .child(mode(1, "Dark", L::Moon, ThemePref::Dark))
                    .child(mode(2, "Follow system", L::Monitor, ThemePref::System))
                    .into_any_element(),
            );
        }
        2 => {
            body.push(heading(p, "What do you use leaf for?", "This sets the switches on the next page. You can change each one there."));
            body.push(
                h_flex()
                    .flex_wrap()
                    .items_stretch()
                    .gap_3()
                    .children(PROFILES.iter().enumerate().map(|(i, pr)| {
                        choice(p, ("setup-profile", i), pr.icon, pr.name, pr.desc, profile == Some(i), move |window, cx| {
                            with(cx, |st| {
                                st.profile = Some(i);
                                st.mods = modules::profile_switches(&PROFILES[i]).into_iter().map(|(_, on)| on).collect();
                            });
                            window.refresh();
                        })
                    }))
                    .into_any_element(),
            );
        }
        3 => {
            body.push(heading(p, "Features", "Each part of leaf you do not switch on stays out of your way: no commands, no menu rows, nothing running."));
            for spec in modules::MODULES {
                let i = spec.module as usize;
                let on = mods.get(i).copied().unwrap_or(true);
                body.push(module_card(p, spec, on, s, move |window, cx| {
                    with(cx, |st| {
                        if let Some(b) = st.mods.get_mut(i) {
                            *b = !*b;
                        }
                    });
                    window.refresh();
                }));
            }
        }
        4 => {
            body.push(heading(p, "System", "The one thing here that changes your system. It is off unless you turn it on."));
            let refused = settings_view::default_fm_refused();
            let desc = if refused {
                settings_view::default_fm_refusal()
            } else if cfg!(windows) {
                t!("Folders and Win+E open with leaf. Applies to this user only and can be reverted at any time: turn this off here or in Settings > Advanced, and what was there before comes back.")
            } else {
                t!("Folders open with leaf. Applies to this user only and can be undone at any time: turn it off here or in Settings > Advanced.")
            };
            body.push(shell(p).child(card_body(p, L::AppWindow, t!("Make leaf the default file manager").into(), Some(desc.into()), Some(switch("setup-dfm", p, dfm && !refused, move |window, cx| {
                if !refused {
                    with(cx, |st| st.dfm = !st.dfm);
                }
                window.refresh();
            })))).into_any_element());
        }
        5 => {
            body.push(heading(p, "Helper programs", "leaf can use these when they are installed. It installs nothing itself."));
            for h in [modules::Helper::Git, modules::Helper::YtDlp, modules::Helper::Ffmpeg, modules::Helper::Rclone] {
                let (found, version) = h.found(s);
                let state = match (found, version) {
                    (true, Some(v)) => format!("{} {}, {} {v}", h.name(), crate::i18n::t("found"), crate::i18n::t("version")),
                    (true, None) => format!("{} {}", h.name(), crate::i18n::t("found")),
                    (false, _) => format!("{} {}: {} `{}`", h.name(), crate::i18n::t("not found"), crate::i18n::t("install with"), h.install()),
                };
                body.push(
                    shell(p)
                        .child(card_body(
                            p,
                            if found { L::Check } else { L::Info },
                            state.into(),
                            Some(crate::i18n::t(h.unlocks()).to_string().into()),
                            None,
                        ))
                        .into_any_element(),
                );
            }
        }
        _ => {
            body.push(heading(p, "You are set", "Try one of these first."));
            let link = |id: &'static str, icon: L, title: &'static str, desc: &'static str, run: fn(&mut Window, &mut App)| {
                shell(p)
                    .id(id)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.pressed))
                    .on_click(move |_, window, cx| run(window, cx))
                    .child(card_body(p, icon, crate::i18n::t(title).into(), Some(crate::i18n::t(desc).into()), Some(Icon::new(L::ChevronRight).size(px(14.)).text_color(p.text_secondary).into_any_element())))
                    .into_any_element()
            };
            body.push(link("setup-home", L::House, "Open Home", "Your drives, pinned folders and recent places on one page.", |window, cx| {
                leave(Page::General, window, cx);
                window.dispatch_action(Box::new(crate::actions::GoHome), cx);
            }));
            body.push(link("setup-features", L::Sparkles, "Settings > Features", "Turn parts of leaf on or off any time.", |window, cx| leave(Page::Modules, window, cx)));
            body.push(link("setup-palette", L::Command, "The command palette", "Ctrl+Shift+P finds every command by name.", |window, cx| {
                leave(Page::General, window, cx);
                window.dispatch_action(Box::new(crate::actions::GoHome), cx);
                window.dispatch_action(Box::new(crate::actions::ToggleCommandPalette), cx);
            }));
            body.push(link("setup-guide", L::BookOpen, "The guide", "Short steps for things you may want to do. F1 opens it any time.", |window, cx| leave(Page::Guide, window, cx)));
            body.push(link("setup-can-do", L::Search, "What leaf can do", "Every command with its shortcut.", |window, cx| leave(Page::Features, window, cx)));
        }
    }

    let last = step + 1 >= STEPS;
    let footer = h_flex()
        .gap_3()
        .pt_4()
        .items_center()
        .child(div().text_xs().text_color(p.text_tertiary).child(format!("{} / {STEPS}", step + 1)))
        .child(div().flex_1())
        .when(!last, |d| d.child(button("setup-skip", p, t!("Skip"), skip)))
        .when(step > 0 && !last, |d| {
            d.child(button("setup-back", p, t!("Back"), |window, cx| {
                with(cx, |st| st.step = st.step.saturating_sub(1));
                window.refresh();
            }))
        })
        .child(button(
            "setup-next",
            p,
            if last { t!("Finish") } else if step == STEPS - 2 { t!("Apply and finish") } else { t!("Next") },
            move |window, cx| {
                if last {
                    leave(Page::General, window, cx);
                } else if step == STEPS - 2 {
                    apply(cx);
                    with(cx, |st| st.step += 1);
                    window.refresh();
                } else {
                    with(cx, |st| st.step += 1);
                    window.refresh();
                }
            },
        ));
    div()
        .id("setup-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .max_w(px(760.))
                .mx_auto()
                .px_6()
                .pt_8()
                .pb_8()
                .gap_2()
                .children(body)
                .child(footer),
        )
        .into_any_element()
}
