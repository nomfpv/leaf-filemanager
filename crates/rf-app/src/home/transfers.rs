//! The "Transfers" widget (a small live speed graph while something runs, the latest finished transfers below it) and the full
//! history as a section of Settings > "Your numbers" with "Clear history". The data is `xfer_history`; nothing here runs unless the
//! widget is on the page, and while a transfer runs the widget wakes itself once a second.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;

use super::WidgetCx;
use crate::folder_view::FolderView;
use crate::settings::Settings;
use crate::t;
use crate::theme::{self, Palette};
use crate::xfer_history::{self as hist, Kind, Outcome, Record};

/// Rows the widget lists.
const SHOWN: usize = 8;

static TICKING: AtomicBool = AtomicBool::new(false);

/// Draws the page again in a second (once, however many times it is asked).
fn tick(cx: &mut Context<FolderView>) {
    if TICKING.swap(true, Ordering::Relaxed) {
        return;
    }
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        TICKING.store(false, Ordering::Relaxed);
        let _ = this.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Copy => t!("Copy"),
        Kind::Move => t!("Move"),
        Kind::Download => t!("Download"),
        Kind::Upload => t!("Upload"),
    }
}

fn kind_icon(k: Kind) -> L {
    match k {
        Kind::Copy => L::Copy,
        Kind::Move => L::ArrowRight,
        Kind::Download | Kind::Upload => L::Download,
    }
}

fn duration(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0 => format!("{ms} ms"),
        1..=59 => format!("{s} s"),
        60..=3599 => format!("{} min {:02} s", s / 60, s % 60),
        _ => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
    }
}

fn speed(r: &Record) -> String {
    r.speed().map_or_else(String::new, |v| format!("{}/s", format::size(v as u64)))
}

fn outcome_color(o: Outcome, p: &Palette) -> Hsla {
    match o {
        Outcome::Done => p.success,
        Outcome::Cancelled => p.text_tertiary,
        Outcome::Failed => p.danger,
    }
}

fn outcome_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Done => t!("Done"),
        Outcome::Cancelled => t!("Cancelled"),
        Outcome::Failed => t!("Failed"),
    }
}

/// The live graph: one line per running transfer, its title, the speed and a row of bars (the last minute).
fn live(p: &Palette, runs: Vec<(String, Vec<f32>, Option<f64>)>) -> AnyElement {
    let mut col = v_flex().gap_2();
    for (i, (title, bars, rate)) in runs.into_iter().enumerate() {
        let mut graph = h_flex().id(("xfer-live", i)).h(px(34.)).w_full().gap(px(1.)).items_end();
        for b in bars {
            graph = graph.child(div().flex_1().h(px((b.clamp(0.0, 1.0) * 34.).max(1.))).rounded_sm().bg(p.accent));
        }
        col = col.child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text_secondary).child(title))
                        .child(div().flex_shrink_0().text_color(p.text).child(rate.map_or_else(String::new, |v| format!("{}/s", format::size(v as u64))))),
                )
                .child(graph),
        );
    }
    col.into_any_element()
}

pub fn widget(_: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let p = &w.p;
    let runs = cx.try_global::<crate::status_center::OpsGlobal>().map(|g| crate::status_center::live_paces(g.0.read(cx))).unwrap_or_default();
    if !runs.is_empty() {
        tick(cx);
    }
    let recent: Vec<Record> = hist::recent().into_iter().take(SHOWN).collect();
    let mut body = v_flex().gap_2();
    if !runs.is_empty() {
        body = body.child(live(p, runs));
    }
    if recent.is_empty() {
        let note = if hist::enabled() { t!("Finished copies, moves, downloads and uploads are listed here.") } else { t!("The history of transfers is off (Settings > Your numbers).") };
        return Some(body.child(div().text_sm().text_color(p.text_tertiary).child(note)).into_any_element());
    }
    let roomy = w.width >= 420.;
    let mut list = v_flex().gap_0p5();
    for (i, r) in recent.iter().enumerate() {
        list = list.child(
            h_flex()
                .id(("xfer-row", i))
                .h(theme::ROW_H)
                .px_2()
                .gap_2()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .text_sm()
                .tooltip({
                    let tip = format!("{} → {}", r.from, r.to);
                    move |win, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(win, cx)
                })
                .child(Icon::new(kind_icon(r.kind)).size(px(14.)).text_color(outcome_color(r.result, p)))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(r.what.clone()))
                .when(roomy, |d| d.child(div().w(px(80.)).flex_shrink_0().text_xs().text_color(p.text_secondary).child(if r.bytes > 0 { format::size(r.bytes) } else { String::new() })))
                .when(roomy, |d| d.child(div().w(px(80.)).flex_shrink_0().text_xs().text_color(p.text_secondary).child(speed(r))))
                .child(div().w(px(110.)).flex_shrink_0().text_xs().text_color(p.text_tertiary).child(format::datetime(r.at_ms))),
        );
    }
    Some(body.child(list).into_any_element())
}

/// The widget's heading: opens the full history.
pub fn header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    div()
        .id("xfer-all")
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .text_color(p.text_secondary)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .child(t!("All"))
        .on_click(cx.listener(|_, _, _, cx| {
            crate::settings_view::open_page(crate::settings_view::Page::Numbers, cx);
            crate::workspace::show_settings(cx);
        }))
        .into_any_element()
}

/// The heading of "Your numbers": the share card, and the page with all the figures.
pub fn numbers_header(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    h_flex()
        .gap_1()
        .child(
            div()
                .id("numbers-share")
                .px_2()
                .py_0p5()
                .rounded(theme::RADIUS_SM)
                .text_xs()
                .text_color(p.text_secondary)
                .cursor_pointer()
                .hover(|s| s.bg(p.hover).text_color(p.text))
                .child(t!("Share card"))
                .on_click(|_, window, cx| crate::share_card::open(window, cx)),
        )
        .child(header(p, cx))
        .into_any_element()
}

/// The full history on the "Your numbers" page: the switch, the list (newest first) and "Clear history".
pub fn page_section(p: &Palette, s: &Settings) -> Vec<AnyElement> {
    use crate::settings_view::{button, card, section, shell, sw};
    let mut v: Vec<AnyElement> = vec![
        section(p, t!("Transfers")),
        card(
            p,
            L::Clock,
            t!("Keep a history of transfers"),
            Some(t!("Writes what was copied, moved, downloaded or uploaded (names, folders, size, time, speed, result) to one small file in the settings folder. Never file contents, never sent anywhere.")),
            sw("record-transfers", p, s.record_transfers, |s, v| s.record_transfers = v),
        ),
    ];
    let all = hist::recent();
    let mut list = shell(p);
    if all.is_empty() {
        list = list.child(div().px_4().py_3().text_sm().text_color(p.text_tertiary).child(t!("No transfers recorded.")));
    }
    for (i, r) in all.iter().enumerate() {
        if i > 0 {
            list = list.child(div().h(px(1.)).bg(p.divider));
        }
        let facts = [Some(kind_name(r.kind).to_string()), (r.bytes > 0).then(|| format::size(r.bytes)), Some(duration(r.millis)), r.speed().map(|_| speed(r)), Some(outcome_word(r.result).to_string()), Some(format::datetime(r.at_ms))];
        let facts = facts.into_iter().flatten().collect::<Vec<_>>().join("  \u{b7}  ");
        list = list.child(
            v_flex()
                .px_4()
                .py_2()
                .gap_0p5()
                .child(h_flex().gap_2().items_center().child(Icon::new(kind_icon(r.kind)).size(px(14.)).text_color(outcome_color(r.result, p))).child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(r.what.clone())))
                .child(div().text_xs().text_color(p.text_secondary).child(facts))
                .child(div().text_xs().text_color(p.text_tertiary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(format!("{} \u{2192} {}", r.from, r.to))),
        );
    }
    v.push(list.into_any_element());
    v.push(
        v_flex()
            .pt_2()
            .items_start()
            .child(button("xfer-clear", p, t!("Clear history"), |_, cx| {
                hist::clear();
                cx.refresh_windows();
            }))
            .into_any_element(),
    );
    v
}

/// The "Your numbers" widget: the counters of `usage.rs` in four figures; a click opens the page with all of them (and "Reset").
pub fn numbers(_: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    use crate::usage::Counter as C;
    let p = &w.p;
    let (usage, _) = crate::usage::snapshot();
    if !cx.global::<Settings>().count_usage && usage.since == 0 {
        return Some(div().text_sm().text_color(p.text_tertiary).child(t!("Counting is off (Settings > Your numbers).")).into_any_element());
    }
    let handled = usage.total(C::FilesCopied) + usage.total(C::FilesMoved) + usage.total(C::FilesDeleted) + usage.total(C::FilesRenamed);
    let figures = [
        (handled.to_string(), t!("Files handled")),
        (format::size(usage.total(C::BytesCopied)), t!("Data copied or moved")),
        (usage.total(C::Searches).to_string(), t!("Searches")),
        {
            let days = usage.days_used.max(usage.days.len() as u32);
            (days.to_string(), if days == 1 { t!("day used") } else { t!("days used") })
        },
    ];
    let mut grid = div().id("home-numbers").grid().grid_cols(if w.width >= 320. { 4 } else { 2 }).gap_2().cursor_pointer();
    for (n, label) in figures {
        grid = grid.child(
            v_flex()
                .min_w_0()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).text_color(p.text).overflow_hidden().text_ellipsis().whitespace_nowrap().child(n))
                .child(div().text_xs().text_color(p.text_secondary).child(label)),
        );
    }
    Some(grid.on_click(cx.listener(|_, _, _, cx| {
        crate::settings_view::open_page(crate::settings_view::Page::Numbers, cx);
        crate::workspace::show_settings(cx);
    })).into_any_element())
}
