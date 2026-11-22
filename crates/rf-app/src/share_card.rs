//! "Share card": a picture (PNG) of a few of the user's own totals (Your numbers) in leaf's look, with the leaf logo, to post
//! somewhere. A small dialog first lists every line with its value and a tick box; only the ticked lines are drawn. Then the
//! picture is copied or saved. Everything on it is counted by leaf on this PC (usage counters and the transfer history); no path,
//! folder name, file name, user name or host name is ever used, and nothing is sent anywhere. The drawing is `card_img`.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::*;
use rf_core::format;
use rf_core::usage::{Counter as C, Usage};

use crate::card_img as img;
use crate::prompt::{self, Button, Check, Choice, Kind};
use crate::t;
use crate::theme::Palette;

/// One line of the card.
#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub label: String,
    pub value: String,
    /// Ticked when the dialog opens.
    pub default_on: bool,
}

/// How many days in a row, ending today (or yesterday: the day is not over), something was counted. `days` is oldest first.
pub fn streak(days: &[i32], today: i32) -> u32 {
    let mut want = match days.last() {
        Some(&d) if d == today || d == today - 1 => d,
        _ => return 0,
    };
    let mut n = 0;
    for &d in days.iter().rev() {
        if d == want {
            n += 1;
            want -= 1;
        } else if d < want {
            break;
        }
    }
    n
}

fn hours(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if h > 0 { format!("{h} h {m:02} min") } else { format!("{m} min") }
}

/// The lines the card can have, with the values as they are now. A line whose value is nothing yet is left out.
pub fn stats(usage: &Usage, today: i32, fastest: Option<f64>) -> Vec<Stat> {
    let days: Vec<i32> = usage.days.iter().map(|d| d.day).collect();
    let rows = [
        (t!("Files moved"), usage.total(C::FilesMoved), usage.total(C::FilesMoved).to_string(), true),
        (t!("Files copied"), usage.total(C::FilesCopied), usage.total(C::FilesCopied).to_string(), true),
        (t!("Data copied or moved"), usage.total(C::BytesCopied), format::size(usage.total(C::BytesCopied)), true),
        (t!("Fastest transfer"), fastest.map_or(0, |v| v as u64), fastest.map_or_else(String::new, |v| format!("{}/s", format::size(v as u64))), true),
        (t!("Days in a row"), u64::from(streak(&days, today)), format!("{}", streak(&days, today)), true),
        (t!("Days used"), u64::from(usage.days_used.max(days.len() as u32)), usage.days_used.max(days.len() as u32).to_string(), true),
        (t!("Folders opened"), usage.total(C::FoldersOpened), usage.total(C::FoldersOpened).to_string(), false),
        (t!("Searches"), usage.total(C::Searches), usage.total(C::Searches).to_string(), false),
        (t!("Time with Leaf in front"), usage.total(C::FocusSecs) / 60 * 60, hours(usage.total(C::FocusSecs)), false),
    ];
    rows.into_iter().filter(|r| r.1 > 0).map(|(label, _, value, default_on)| Stat { label: label.to_string(), value, default_on }).collect()
}

/// The current numbers: the counters, and the fastest finished transfer of the history (its speed only).
fn current_stats() -> Vec<Stat> {
    let (usage, today) = crate::usage::snapshot();
    let fastest = crate::xfer_history::recent().iter().filter(|r| r.result == crate::xfer_history::Outcome::Done).filter_map(|r| r.speed()).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    stats(&usage, today, fastest)
}

/// The picture's colours: the theme's.
#[derive(Clone)]
struct Colors {
    bg: String,
    text: String,
    dim: String,
    accent: String,
    line: String,
}

impl Colors {
    fn of(p: &Palette) -> Self {
        Self { bg: img::hex(p.card_bg), text: img::hex(p.text), dim: img::hex(p.text_secondary), accent: img::hex(p.accent), line: img::hex(p.border) }
    }
}

/// The picture: `wide` is 1200 x 630, else 1080 x 1080. Blocks the thread (fonts): not for the UI thread.
fn render(stats: &[Stat], wide: bool, c: &Colors) -> Option<Vec<u8>> {
    let (w, h) = if wide { (1200.0_f32, 630.0_f32) } else { (1080.0, 1080.0) };
    let n = stats.len().max(1);
    let rows = n.div_ceil(2) as f32;
    // the area of the figures
    let (ax, ay, aw, ah) = if wide { (470.0, 70.0, 660.0, 430.0) } else { (90.0, 440.0, 900.0, 500.0) };
    let (cw, rh) = (aw / 2.0, (ah / rows).min(if wide { 150.0 } else { 140.0 }));
    let mut body = String::new();
    for (i, s) in stats.iter().enumerate() {
        let (col, row) = ((i % 2) as f32, (i / 2) as f32);
        // (a long value gets a smaller size so that it stays in its cell)
        let vsize = (rh * 0.42).min((cw - 24.0) / (s.value.chars().count().max(3) as f32 * 0.6));
        let x = ax + col * cw;
        let y = ay + row * rh;
        body.push_str(&format!(
            "<text x=\"{x}\" y=\"{}\" font-size=\"{vsize}\" font-weight=\"700\" fill=\"{}\">{}</text><text x=\"{x}\" y=\"{}\" font-size=\"{}\" fill=\"{}\">{}</text>",
            y + rh * 0.52,
            c.text,
            img::esc(&s.value),
            y + rh * 0.80,
            (rh * 0.18).clamp(14.0, 24.0),
            c.dim,
            img::esc(&s.label),
        ));
    }
    let (lx, ly, ls) = if wide { (70.0, 70.0, 250.0) } else { ((w - 220.0) / 2.0, 60.0, 220.0) };
    let (wx, wy, anchor) = if wide { (70.0, 410.0, "start") } else { (w / 2.0, 330.0, "middle") };
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" font-family=\"{sans}\">\
<defs><radialGradient id=\"g\" cx=\"0.9\" cy=\"0.05\" r=\"0.9\"><stop offset=\"0\" stop-color=\"{ac}\" stop-opacity=\"0.30\"/><stop offset=\"1\" stop-color=\"{ac}\" stop-opacity=\"0\"/></radialGradient></defs>\
<rect width=\"{w}\" height=\"{h}\" fill=\"{bg}\"/><rect width=\"{w}\" height=\"{h}\" fill=\"url(#g)\"/>\
<rect x=\"1\" y=\"1\" width=\"{bw}\" height=\"{bh}\" fill=\"none\" stroke=\"{line}\" stroke-width=\"2\"/>\
<text x=\"{wx}\" y=\"{wy}\" font-size=\"84\" font-weight=\"700\" fill=\"{ac}\" text-anchor=\"{anchor}\">leaf</text>\
<text x=\"{wx}\" y=\"{ty}\" font-size=\"26\" fill=\"{dim}\" text-anchor=\"{anchor}\">{tag}</text>\
{body}\
<text x=\"{fx}\" y=\"{fy}\" font-size=\"18\" fill=\"{dim}\" text-anchor=\"{fanchor}\" opacity=\"0.8\">{foot}</text></svg>",
        sans = img::SANS,
        ac = c.accent,
        bg = c.bg,
        line = c.line,
        dim = c.dim,
        bw = w - 2.0,
        bh = h - 2.0,
        ty = wy + 44.0,
        tag = img::esc(t!("A fast file manager")),
        fx = if wide { 70.0 } else { w / 2.0 },
        fy = h - 40.0,
        fanchor = if wide { "start" } else { "middle" },
        foot = img::esc(t!("Counted by leaf on this PC")),
    );
    let mut pix = img::draw_svg(&svg, w as u32, h as u32)?;
    img::put_logo(&mut pix, lx, ly, ls)?;
    img::png(&pix)
}

/// Shows the dialog (`Share card` command and the button on Your numbers).
pub fn open(window: &mut Window, cx: &mut App) {
    let stats = current_stats();
    if stats.is_empty() {
        let note = if crate::usage::enabled() { t!("Nothing is counted yet. Use leaf for a while, then come back.") } else { t!("Counting is off (Settings > Your numbers).") };
        return prompt::notice(t!("Share card"), note.to_string(), window, cx);
    }
    let p = Palette::for_window(window);
    let colors = Rc::new(Colors::of(&p));
    let stats = Rc::new(stats);
    let ticks: Vec<Rc<Cell<bool>>> = stats.iter().map(|s| Rc::new(Cell::new(s.default_on))).collect();
    let checks: Vec<Check> = stats.iter().zip(&ticks).map(|(s, on)| Check { label: s.label.clone().into(), note: s.value.clone().into(), on: on.clone(), enabled: true }).collect();
    let shape = Rc::new(Cell::new(0usize));
    let choices = vec![Choice { label: t!("Shape").into(), options: vec![t!("Wide (1200 x 630)").into(), t!("Square (1080 x 1080)").into()], selected: shape.clone() }];
    let message = t!("The picture shows the ticked lines and the leaf logo. No folder, file or user names are on it, and it is not sent anywhere.").to_string();
    let chosen = {
        let (stats, ticks) = (stats.clone(), ticks.clone());
        move || -> Vec<Stat> { stats.iter().zip(&ticks).filter(|(_, on)| on.get()).map(|(s, _)| s.clone()).collect() }
    };
    let make = {
        let (chosen, colors, shape) = (chosen.clone(), colors.clone(), shape.clone());
        move |save: bool, cx: &mut App| -> Result<(), String> {
            let picked = chosen();
            if picked.is_empty() {
                return Err(t!("Tick at least one line.").to_string());
            }
            let wide = shape.get() == 0;
            // (the colours are plain text: they cross to the thread as a copy)
            let c = Colors::clone(&colors);
            cx.spawn(async move |cx| {
                let png = cx.background_spawn(async move { render(&picked, wide, &c) }).await;
                cx.update(|cx| match png {
                    Some(png) if save => img::save_png(png, "leaf-numbers.png", cx),
                    Some(png) => img::copy_png(png, cx),
                    None => log::warn!("share card: the picture could not be drawn"),
                });
            })
            .detach();
            Ok(())
        }
    };
    let copy = {
        let make = make.clone();
        Button::new(t!("Copy picture"), Kind::Primary, move |_, _, cx| make(false, cx))
    };
    let save = Button::new(t!("Save as..."), Kind::Normal, move |_, _, cx| make(true, cx));
    prompt::show_with_checks(t!("Share card"), Some(message), None, choices, checks, vec![copy, save, Button::cancel()], window, cx);
}

/// The command: asks the window in front.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::ShareCard, cx| {
        let Some(handle) = cx.active_window() else { return };
        let _ = handle.update(cx, |_, window, cx| open(window, cx));
    });
}
