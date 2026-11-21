//! Settings -> Dosya türleri: the user's own programs for kinds of files (`rf_core::openrules`). A list per group of extensions; its
//! first program opens such a file on a double click, all of them stand first under "Open with". Everything is changed in place:
//! the extensions are asked for in a small dialog, a program is chosen in a file dialog or typed as a path, and the order is changed
//! with the arrows. Only rustyfiles follows the lists; Windows' own defaults are not touched.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::openrules::{self, OpenRule};

use crate::settings::{self, Settings};
use crate::settings_view::{button, card_body, section, shell, switch};
use crate::t;
use crate::theme::{self, Palette};

fn small_button(id: impl Into<ElementId>, icon: L, tip: &'static str, p: &Palette, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .id(id)
        .size(px(32.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .border_1()
        .border_color(p.border)
        .bg(p.hover)
        .cursor_pointer()
        .tab_index(0)
        .hover(|s| s.bg(p.pressed))
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
        .child(Icon::new(icon).size(px(14.)))
        .on_click(move |_, window, cx| run(window, cx))
        .into_any_element()
}

/// What the dialog says when the extensions typed cannot be taken; `Ok` with the extensions otherwise. `ix`: the list being changed
/// (`None`: a new one).
pub(crate) fn exts_answer(rules: &[OpenRule], ix: Option<usize>, text: &str) -> Result<Vec<String>, String> {
    let exts = openrules::parse_exts(text).map_err(|bad| format!("{}: {bad}", t!("This is not an extension")))?;
    if exts.is_empty() {
        return Err(t!("Type at least one extension.").to_string());
    }
    let taken = openrules::taken(rules, ix, &exts);
    if !taken.is_empty() {
        return Err(format!("{}: {}", t!("These extensions already have a list"), taken.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(", ")));
    }
    Ok(exts)
}

/// Asks for the extensions of a list: a new one (`None`), or list `ix`'s.
fn ask_exts(ix: Option<usize>, window: &mut Window, cx: &mut App) {
    let current = ix.and_then(|i| cx.global::<Settings>().open_rules.get(i).map(|r| r.exts.join(", "))).unwrap_or_default();
    crate::prompt::show(
        if ix.is_some() { t!("Change extensions") } else { t!("New file type") },
        Some(t!("Which extensions is this list for? Separate several with commas.").to_string()),
        Some((current, "dll, sys".to_string())),
        vec![
            crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |text, _, cx| {
                let exts = exts_answer(&cx.global::<Settings>().open_rules, ix, &text.unwrap_or_default())?;
                settings::update(cx, move |s| match ix {
                    Some(i) => {
                        if let Some(r) = s.open_rules.get_mut(i) {
                            r.exts = exts;
                        }
                    }
                    None => s.open_rules.push(openrules::new_rule(exts)),
                });
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

/// Puts a program at the end of list `ix` (a program that is already in it stays where it is).
fn add_program(ix: usize, path: String, cx: &mut App) {
    settings::update(cx, move |s| {
        if let Some(r) = s.open_rules.get_mut(ix)
            && !r.programs.iter().any(|p| p.eq_ignore_ascii_case(&path))
        {
            r.programs.push(path);
        }
    });
}

/// A program for list `ix`, chosen in a file dialog (off the UI thread; the list changes when the dialog answers).
fn pick_program(ix: usize, cx: &mut App) {
    let (tx, rx) = futures::channel::oneshot::channel::<Option<std::path::PathBuf>>();
    let (title, programs, all) = (t!("Choose a program"), t!("Programs"), t!("All files"));
    rf_win::sta::run(move |owner| {
        let _ = tx.send(rf_win::dialogs::pick(owner, title, rf_win::dialogs::Pick::File { filters: &[(programs, "*.exe;*.bat;*.cmd;*.com;*.lnk"), (all, "*.*")] }));
    });
    cx.spawn(async move |cx| {
        if let Ok(Some(path)) = rx.await {
            cx.update(|cx| add_program(ix, path.to_string_lossy().into_owned(), cx));
        }
    })
    .detach();
}

/// A program for list `ix`, typed or pasted as a path.
fn ask_program_path(ix: usize, window: &mut Window, cx: &mut App) {
    crate::prompt::show(
        t!("The program's path"),
        Some(t!("Type or paste the program's full path.").to_string()),
        Some((String::new(), r"C:\Program Files\Program\program.exe".to_string())),
        vec![
            crate::prompt::Button::new(t!("Add"), crate::prompt::Kind::Primary, move |text, _, cx| {
                let path = openrules::clean_program(&text.unwrap_or_default()).ok_or_else(|| t!("Type a path.").to_string())?;
                add_program(ix, path, cx);
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

fn delete_dialog(ix: usize, window: &mut Window, cx: &mut App) {
    crate::prompt::show(
        t!("Delete this list?"),
        Some(t!("Files of this type open with Windows' default program again.").to_string()),
        None,
        vec![
            crate::prompt::Button::new(t!("Delete"), crate::prompt::Kind::Danger, move |_, _, cx| {
                settings::update(cx, move |s| {
                    if ix < s.open_rules.len() {
                        s.open_rules.remove(ix);
                    }
                });
                Ok(())
            }),
            crate::prompt::Button::cancel(),
        ],
        window,
        cx,
    );
}

/// For a process that was started long ago and does not hear of a change of the settings (DirectLook stays in the background): takes
/// the lists from the settings file as it is now, just before a file is opened. A small file read, on the thread that opens.
pub(crate) fn follow_file() {
    if let Some(s) = Settings::load_readonly() {
        rf_win::openrules::set(s.open_rules);
    }
}

/// The line under a list's extensions: what a double click on such a file does.
pub(crate) fn rule_line(r: &OpenRule) -> String {
    match (r.programs.first(), r.double_click) {
        (None, _) => t!("No program yet: add one.").to_string(),
        (Some(first), true) => format!("{}: {}", t!("Double click"), openrules::program_label(first)),
        (Some(_), false) => t!("A double click opens with Windows' default; the programs are only under \"Open with\".").to_string(),
    }
}

fn program_row(p: &Palette, ix: usize, pi: usize, n: usize, path: &str, is_default: bool) -> AnyElement {
    let key = ix * 1000 + pi;
    h_flex()
        .h(px(52.))
        .pl(px(50.))
        .pr_4()
        .gap_3()
        .items_center()
        .border_t_1()
        .border_color(p.border)
        .child(Icon::new(L::AppWindow).size(px(16.)).flex_shrink_0().text_color(p.text_secondary))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().child(openrules::program_label(path)))
                        .when(is_default, |d| d.child(div().px_1p5().rounded(px(4.)).border_1().border_color(p.accent).text_xs().text_color(p.accent).child(t!("double click")))),
                )
                .child(div().text_xs().text_color(p.text_tertiary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(path.to_string())),
        )
        .when(pi > 0, |d| {
            d.child(small_button(("or-up", key), L::ChevronUp, t!("Move up"), p, move |_, cx| {
                settings::update(cx, move |s| {
                    if let Some(r) = s.open_rules.get_mut(ix) {
                        r.programs = openrules::moved(&r.programs, pi, -1);
                    }
                })
            }))
        })
        .when(pi + 1 < n, |d| {
            d.child(small_button(("or-down", key), L::ChevronDown, t!("Move down"), p, move |_, cx| {
                settings::update(cx, move |s| {
                    if let Some(r) = s.open_rules.get_mut(ix) {
                        r.programs = openrules::moved(&r.programs, pi, 1);
                    }
                })
            }))
        })
        .child(small_button(("or-rm", key), L::X, t!("Remove from the list"), p, move |_, cx| {
            settings::update(cx, move |s| {
                if let Some(r) = s.open_rules.get_mut(ix)
                    && pi < r.programs.len()
                {
                    r.programs.remove(pi);
                }
            })
        }))
        .into_any_element()
}

fn rule_card(p: &Palette, ix: usize, r: &OpenRule) -> AnyElement {
    let (on, n) = (r.double_click, r.programs.len());
    let head = h_flex()
        .gap_2()
        .child(button(("or-exts", ix), p, t!("Change extensions"), move |window, cx| ask_exts(Some(ix), window, cx)))
        .child(button(("or-del", ix), p, t!("Delete"), move |window, cx| delete_dialog(ix, window, cx)))
        .into_any_element();
    shell(p)
        .child(card_body(p, L::FileType, openrules::exts_line(r).into(), Some(rule_line(r).into()), Some(head)))
        .children(r.programs.iter().enumerate().map(|(pi, path)| program_row(p, ix, pi, n, path, pi == 0 && on)))
        .child(
            h_flex()
                .min_h(px(56.))
                .pl(px(50.))
                .pr_4()
                .gap_2()
                .items_center()
                .border_t_1()
                .border_color(p.border)
                .child(button(("or-add", ix), p, t!("Add program…"), move |_, cx| pick_program(ix, cx)))
                .child(button(("or-path", ix), p, t!("Type a path…"), move |window, cx| ask_program_path(ix, window, cx))),
        )
        // (a row of its own: beside the two buttons the words and the switch did not fit a narrow window)
        .child(
            h_flex()
                .min_h(px(52.))
                .pl(px(50.))
                .pr_4()
                .gap_3()
                .items_center()
                .border_t_1()
                .border_color(p.border)
                .child(div().flex_1().min_w_0().text_sm().child(t!("Open with the first program on a double click")))
                .child(switch(("or-dbl", ix), p, on, move |_, cx| {
                    settings::update(cx, move |s| {
                        if let Some(r) = s.open_rules.get_mut(ix) {
                            r.double_click = !on;
                        }
                    })
                })),
        )
        .into_any_element()
}

pub fn page(p: &Palette, s: &Settings) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::new();
    out.push(
        shell(p)
            .child(card_body(
                p,
                L::AppWindow,
                t!("Programs by file type").into(),
                Some(t!("Make your own list of programs for a file type: the first program in the list opens that type on a double click, and all of them stand at the top of \"Open with\" in the right-click menu. Programs are given by their paths, so a program Windows does not know works too. Only rustyfiles follows these lists; Windows' default programs do not change.").into()),
                Some(button("or-new", p, t!("New type"), |window, cx| ask_exts(None, window, cx))),
            ))
            .into_any_element(),
    );
    if s.open_rules.is_empty() {
        out.push(
            shell(p)
                .child(card_body(
                    p,
                    L::Sparkles,
                    t!("No lists yet").into(),
                    Some(t!("Example: for .dll, dnSpy first and Ghidra second. A double click opens with dnSpy, and Ghidra is one click away under \"Open with\".").into()),
                    None,
                ))
                .into_any_element(),
        );
    } else {
        out.push(section(p, t!("Lists")));
        for (i, r) in s.open_rules.iter().enumerate() {
            out.push(rule_card(p, i, r));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // (not `super::*`: the glob of the UI kit above carries its own `test` attribute)
    use rf_core::openrules::OpenRule;

    use super::{exts_answer, rule_line};

    fn rule(exts: &[&str], programs: &[&str], double_click: bool) -> OpenRule {
        OpenRule { exts: exts.iter().map(|s| s.to_string()).collect(), programs: programs.iter().map(|s| s.to_string()).collect(), double_click }
    }

    #[test]
    fn the_dialog_takes_extensions_and_says_what_is_wrong_with_the_rest() {
        let rules = [rule(&["dll", "sys"], &[], true), rule(&["js"], &[], true)];
        {
            assert_eq!(exts_answer(&rules, None, ".bin, DAT"), Ok(vec!["bin".to_string(), "dat".to_string()]));
            assert_eq!(exts_answer(&rules, Some(0), "dll, sys, bin"), Ok(vec!["dll".to_string(), "sys".to_string(), "bin".to_string()]), "a list keeps its own extensions");
            assert_eq!(exts_answer(&rules, None, "  "), Err("Type at least one extension.".to_string()));
            assert_eq!(exts_answer(&rules, None, "tar.gz"), Err("This is not an extension: tar.gz".to_string()));
            assert_eq!(exts_answer(&rules, None, "ts, js, dll"), Err("These extensions already have a list: .js, .dll".to_string()));
            assert_eq!(exts_answer(&rules, Some(1), "dll"), Err("These extensions already have a list: .dll".to_string()));
        }
    }

    #[test]
    fn a_lists_line_says_what_a_double_click_does() {
        {
            assert_eq!(rule_line(&rule(&["dll"], &[r"C:\t\dnSpy.exe", r"C:\t\ghidraRun.bat"], true)), "Double click: dnSpy");
            assert!(rule_line(&rule(&["exe"], &[r"C:\t\ghidraRun.bat"], false)).starts_with("A double click opens with Windows' default"));
            assert!(rule_line(&rule(&["dll"], &[], true)).starts_with("No program yet"));
        }
    }
}
