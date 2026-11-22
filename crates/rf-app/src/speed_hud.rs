//! The speed overlay: a small readout in the bottom right corner of the window, off by default (Settings > Advanced, or the command
//! "Speed overlay"). It shows what leaf measures anyway: how long the current folder took to open (and how many items it holds),
//! how long the last frame took, and how much memory leaf holds. Off, nothing here runs: the window asks one setting and moves on.
//! On, it is drawn again with the workspace's own 800 ms heartbeat, and the memory is read at most once a second.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;

use crate::settings::Settings;
use crate::t;
use crate::theme::Palette;

/// The memory last read and when (read again after a second).
static MEMORY: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

/// Whether the overlay is on.
pub fn on(cx: &App) -> bool {
    cx.global::<Settings>().speed_hud
}

/// leaf's own memory in use (the working set), read at most once a second.
fn memory() -> Option<u64> {
    let mut g = MEMORY.lock().ok()?;
    if let Some((at, v)) = *g
        && at.elapsed() < Duration::from_secs(1)
    {
        return Some(v);
    }
    let v = leaf_sys::bench::process_memory(std::process::id()).map(|m| m.0)?;
    *g = Some((Instant::now(), v));
    Some(v)
}

/// The text of the readout: `open_ms` and `items` of the folder in front, then the frame time and the memory.
pub fn readout(open_ms: Option<u128>, items: usize) -> String {
    let mut parts = Vec::new();
    parts.push(match open_ms {
        Some(ms) => format!("{} {ms} ms ({} {})", t!("open"), crate::bench_view::count(items as u64), t!("items")),
        None => format!("{} \u{2026}", t!("open")),
    });
    let frame = crate::platform::gpui::hybrid::load_snapshot().last_frame_ms;
    if frame > 0.0 {
        parts.push(format!("{} {frame:.1} ms", t!("frame")));
    }
    if let Some(m) = memory() {
        parts.push(format!("{} {}", t!("memory"), format::size(m)));
    }
    parts.join("  \u{b7}  ")
}

/// The corner readout; `None` while it is off.
pub fn overlay(p: &Palette, open_ms: Option<u128>, items: usize, cx: &App) -> Option<AnyElement> {
    if !on(cx) {
        return None;
    }
    let mono = (!p.font_mono.is_empty()).then(|| p.font_mono);
    Some(
        div()
            .id("speed-hud")
            .absolute()
            .right(px(12.))
            .bottom(px(40.))
            .px_2()
            .py_0p5()
            .rounded(crate::theme::RADIUS_SM)
            .bg(p.card_bg.opacity(0.9))
            .border_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.text_secondary)
            .when_some(mono, |d, f| d.font_family(f))
            .child(readout(open_ms, items))
            .into_any_element(),
    )
}

/// The command "Speed overlay": on or off.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &crate::actions::ToggleSpeedHud, cx| {
        crate::settings::update(cx, |s| s.speed_hud = !s.speed_hud);
        cx.refresh_windows();
    });
}
