//! Settings > "Your numbers": the counters of `usage.rs` as totals and a small chart of the last 90 days.
//! The numbers are counted on this PC, kept in one file in the config folder and never sent anywhere.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use crate::settings::Settings;
use crate::settings_view::{button, card, section, shell, sw};
use crate::t;
use crate::theme::Palette;
use crate::usage::Counter;
use rf_core::usage::DAYS_KEPT;

fn label(c: Counter) -> &'static str {
    match c {
        Counter::FilesCopied => t!("Files copied"),
        Counter::FilesMoved => t!("Files moved"),
        Counter::FilesDeleted => t!("Files deleted"),
        Counter::FilesRenamed => t!("Files renamed"),
        Counter::BytesCopied => t!("Data copied"),
        Counter::FoldersOpened => t!("Folders opened"),
        Counter::Searches => t!("Searches"),
        Counter::BulkRenames => t!("Bulk renames"),
        Counter::ArchivesMade => t!("Archives made"),
        Counter::ArchivesExtracted => t!("Archives extracted"),
        Counter::Launches => t!("Times Leaf was started"),
        Counter::FocusSecs => t!("Time with Leaf in front"),
    }
}

fn value(c: Counter, n: u64) -> String {
    match c {
        Counter::BytesCopied => rf_core::format::size(n),
        Counter::FocusSecs => {
            let (h, m) = (n / 3600, n % 3600 / 60);
            if h > 0 { format!("{h} h {m:02} min") } else { format!("{m} min") }
        }
        _ => n.to_string(),
    }
}

/// What the chart counts for one day: the files handled.
const CHART: [Counter; 4] = [Counter::FilesCopied, Counter::FilesMoved, Counter::FilesDeleted, Counter::FilesRenamed];

pub fn page(p: &Palette, s: &Settings, _cx: &mut App) -> Vec<AnyElement> {
    let (usage, today) = crate::usage::snapshot();
    let mut v: Vec<AnyElement> = vec![
        div()
            .pb_2()
            .text_sm()
            .text_color(p.text_secondary)
            .child(t!("These numbers are counted on this PC, kept in one small file in the settings folder, and never sent anywhere."))
            .into_any_element(),
        card(
            p,
            L::Activity,
            t!("Count my usage"),
            Some(t!("Counts what you do with Leaf (files copied, folders opened, searches, ...) for this page. Off: nothing is counted.")),
            sw("count-usage", p, s.count_usage, |s, v| s.count_usage = v),
        ),
    ];
    if !s.count_usage {
        v.push(div().pt_2().text_sm().text_color(p.text_tertiary).child(t!("Counting is off. The numbers below are from before.")).into_any_element());
    }
    // files handled per day, the last 90 days
    v.push(section(p, t!("Files handled per day (last 90 days)")));
    let per_day: Vec<u64> = (0..DAYS_KEPT)
        .map(|i| CHART.iter().map(|&c| usage.per_day(c, today, DAYS_KEPT)[i]).sum())
        .collect();
    let max = per_day.iter().copied().max().unwrap_or(0).max(1);
    let mut bars = h_flex().h(px(72.)).w_full().gap(px(1.)).items_end();
    for (i, &n) in per_day.iter().enumerate() {
        let h = if n == 0 { 1.0 } else { (n as f32 / max as f32 * 72.0).max(2.0) };
        bars = bars.child(div().id(("num-day", i)).flex_1().h(px(h)).rounded_sm().bg(if n == 0 { p.divider } else { p.accent }));
    }
    v.push(
        shell(p)
            .p_4()
            .gap_2()
            .child(bars)
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(p.text_tertiary)
                    .child(t!("90 days ago"))
                    .child(format!("{} {}", t!("Busiest day:"), max))
                    .child(t!("Today")),
            )
            .into_any_element(),
    );
    v.push(section(p, t!("Totals")));
    let mut card_ = shell(p);
    for (i, c) in Counter::ALL.into_iter().enumerate() {
        if i > 0 {
            card_ = card_.child(div().h(px(1.)).bg(p.divider));
        }
        card_ = card_.child(
            h_flex()
                .px_4()
                .py_2()
                .justify_between()
                .text_sm()
                .child(div().text_color(p.text_secondary).child(label(c)))
                .child(div().text_color(p.text).font_weight(FontWeight::SEMIBOLD).child(value(c, usage.total(c)))),
        );
    }
    v.push(card_.into_any_element());

    v.push(
        v_flex()
            .pt_4()
            .items_start()
            .child(button("numbers-reset", p, t!("Reset"), |_, cx| {
                crate::usage::reset();
                cx.refresh_windows();
            }))
            .into_any_element(),
    );
    v.extend(crate::home::transfers_page_section(p, s));
    v
}
