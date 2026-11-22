//! "About this PC and leaf": a page in the style of a terminal's system summary. The leaf as text art on the left; on the right the
//! system, processor, memory, graphics adapters, drives (sizes only), display, leaf's version and build, and leaf's own numbers (how
//! long the last folder took to open, how many files are indexed, how much memory leaf holds). Monospace, theme colours only.
//! "Copy as text" and "Copy as picture" put the same lines on the clipboard. No computer name, user name, serial number or address
//! is read or shown. The facts that need the registry or the drives are read once, off the UI thread (`leaf_sys::bench`).

use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use rf_core::format;

use crate::card_img as img;
use crate::settings_view::{button, shell};
use crate::t;
use crate::theme::Palette;

/// The leaf, as text.
const ART: [&str; 11] = [
    "          /\\          ",
    "         /  \\         ",
    "   /\\   /    \\   /\\   ",
    "  /  \\_/  ||  \\_/  \\  ",
    " /    o   ||   o    \\ ",
    " \\     \\  ||  /     / ",
    "  \\_.-'  \\||/  '-._/  ",
    "      '-. || .-'      ",
    "         \\||/         ",
    "          ||          ",
    "          \\/          ",
];

/// What was read from the system (once).
#[derive(Clone, Default)]
struct Facts {
    os: String,
    cpu: String,
    ram_gb: u32,
    gpus: Vec<String>,
    drives: Vec<u64>,
    displays: Vec<String>,
}

struct State {
    facts: Option<Facts>,
}
impl Global for State {}

/// How long the last folder took to open (ms; 0 = none yet) and how many items it held: noted where the listing ends.
static OPEN_MS: AtomicU64 = AtomicU64::new(u64::MAX);
static OPEN_ITEMS: AtomicU64 = AtomicU64::new(0);

pub fn note_open(ms: u64, items: usize) {
    OPEN_MS.store(ms, Ordering::Relaxed);
    OPEN_ITEMS.store(items as u64, Ordering::Relaxed);
}

fn display_text(cx: &mut App) -> Vec<String> {
    let scale = cx.active_window().and_then(|h| h.update(cx, |_, w, _| w.scale_factor()).ok()).unwrap_or(1.0);
    cx.displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            let (w, h) = (f32::from(b.size.width) * scale, f32::from(b.size.height) * scale);
            let mut s = format!("{}x{}", w.round() as i64, h.round() as i64);
            if (scale - 1.0).abs() > 0.01 {
                s.push_str(&format!(" ({}%)", (scale * 100.0).round() as i64));
            }
            s
        })
        .collect()
}

fn state(cx: &mut App) -> &mut State {
    if !cx.has_global::<State>() {
        cx.set_global(State { facts: None });
        let displays = display_text(cx);
        let dir = crate::settings::app_dir().unwrap_or_else(std::env::temp_dir);
        cx.spawn(async move |cx| {
            let facts = cx
                .background_spawn(async move {
                    let m = leaf_sys::bench::machine(&dir);
                    let drives = leaf_sys::list_drives().into_iter().filter(|d| matches!(d.kind, leaf_sys::DriveKind::Fixed | leaf_sys::DriveKind::Removable) && d.total > 0).map(|d| d.total).collect();
                    let cpu = if m.cores > 0 { format!("{} ({} {})", m.cpu, m.cores, t!("threads")) } else { m.cpu.clone() };
                    Facts { os: m.windows, cpu, ram_gb: m.ram_gb, gpus: leaf_sys::bench::gpu_names(), drives, displays }
                })
                .await;
            cx.update(|cx| {
                cx.global_mut::<State>().facts = Some(facts);
                cx.refresh_windows();
            });
        })
        .detach();
    }
    cx.global_mut::<State>()
}

/// Debug or release, as this build was made.
fn profile() -> &'static str {
    if cfg!(debug_assertions) { "debug" } else { "release" }
}

/// The lines on the right: (label, value). Facts still being read are left out; the page says so.
fn rows(f: &Facts) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, val: String| {
        if !val.is_empty() {
            v.push((k.to_string(), val));
        }
    };
    add(t!("OS"), f.os.clone());
    add(t!("CPU"), f.cpu.clone());
    if f.ram_gb > 0 {
        add(t!("Memory"), format!("{} GB", f.ram_gb));
    }
    for g in &f.gpus {
        add(t!("GPU"), g.clone());
    }
    add(t!("Drives"), f.drives.iter().map(|&b| format::size(b)).collect::<Vec<_>>().join(", "));
    for d in &f.displays {
        add(t!("Display"), d.clone());
    }
    add("leaf", format!("{} ({})", crate::installer::version(), env!("LEAF_COMMIT")));
    add(t!("Build"), profile().to_string());
    let ms = OPEN_MS.load(Ordering::Relaxed);
    add(
        t!("Folder open"),
        if ms == u64::MAX { t!("not yet").to_string() } else { format!("{ms} ms ({} {})", crate::bench_view::count(OPEN_ITEMS.load(Ordering::Relaxed)), t!("items")) },
    );
    let indexed: usize = leaf_sys::indexer::status().iter().filter(|s| s.1 == leaf_sys::indexer::State::Ready).map(|s| s.2).sum();
    if indexed > 0 {
        add(t!("Files indexed"), crate::bench_view::count(indexed as u64));
    }
    if let Some(m) = leaf_sys::bench::process_memory(std::process::id()) {
        add(t!("leaf memory"), format::size(m.0));
    }
    v
}

/// The lines beside the art: the title, a rule, the rows.
fn right_lines(rows: &[(String, String)]) -> Vec<(Option<String>, String)> {
    let w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0) + 2;
    let mut out = vec![(None, format!("leaf {}", crate::installer::version())), (None, "-".repeat(10 + w))];
    out.extend(rows.iter().map(|(k, val)| (Some(format!("{k:<w$}")), val.clone())));
    out
}

/// The summary as plain text, art and lines side by side.
fn as_text(rows: &[(String, String)]) -> String {
    let right = right_lines(rows);
    let art_w = ART[0].chars().count();
    let n = ART.len().max(right.len());
    let mut s = String::new();
    for i in 0..n {
        let left = ART.get(i).copied().unwrap_or("");
        let line = right.get(i).map(|(k, v)| format!("{}{v}", k.clone().unwrap_or_default())).unwrap_or_default();
        s.push_str(&format!("{left:<art_w$}  {line}").trim_end().to_string());
        s.push('\n');
    }
    s
}

/// The summary as a picture: drawn with the theme's colours. Blocks the thread (fonts).
fn as_png(rows: &[(String, String)], c: &[String; 4]) -> Option<Vec<u8>> {
    let [bg, text, dim, accent] = c;
    let right = right_lines(rows);
    let (size, cw, lh, pad) = (20.0_f32, 12.4_f32, 30.0_f32, 36.0_f32);
    let art_w = ART[0].chars().count() as f32 * cw;
    let key_w = right.iter().filter_map(|r| r.0.as_ref()).map(|k| k.chars().count()).max().unwrap_or(0) as f32 * cw;
    let right_w = right.iter().map(|(k, v)| (k.as_ref().map_or(0, |k| k.chars().count()) + v.chars().count()) as f32 * cw).fold(0.0, f32::max);
    let w = (pad * 2.0 + art_w + 2.0 * cw + right_w).ceil();
    let n = ART.len().max(right.len());
    let h = (pad * 2.0 + n as f32 * lh).ceil();
    let mut body = String::new();
    for (i, line) in ART.iter().enumerate() {
        body.push_str(&format!("<text x=\"{pad}\" y=\"{}\" fill=\"{accent}\" xml:space=\"preserve\">{}</text>", pad + (i as f32 + 0.8) * lh, img::esc(line)));
    }
    let rx = pad + art_w + 2.0 * cw;
    for (i, (k, v)) in right.iter().enumerate() {
        let y = pad + (i as f32 + 0.8) * lh;
        match k {
            Some(k) => body.push_str(&format!(
                "<text x=\"{rx}\" y=\"{y}\" fill=\"{accent}\" font-weight=\"700\" xml:space=\"preserve\">{}</text><text x=\"{}\" y=\"{y}\" fill=\"{text}\" xml:space=\"preserve\">{}</text>",
                img::esc(k.trim_end()),
                rx + key_w,
                img::esc(v)
            )),
            None if i == 0 => body.push_str(&format!("<text x=\"{rx}\" y=\"{y}\" fill=\"{accent}\" font-weight=\"700\">{}</text>", img::esc(v))),
            None => body.push_str(&format!("<text x=\"{rx}\" y=\"{y}\" fill=\"{dim}\">{}</text>", img::esc(v))),
        }
    }
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" font-family=\"{}\" font-size=\"{size}\"><rect width=\"{w}\" height=\"{h}\" fill=\"{bg}\"/>{body}</svg>",
        img::MONO
    );
    img::png(&img::draw_svg(&svg, w as u32, h as u32)?)
}

fn mono(p: &Palette) -> Option<&'static str> {
    (!p.font_mono.is_empty()).then_some(p.font_mono)
}

/// The page.
pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let facts = state(cx).facts.clone();
    let Some(facts) = facts else {
        return vec![div().text_sm().text_color(p.text_secondary).child(t!("Reading this PC…")).into_any_element()];
    };
    let rows = rows(&facts);
    let right = right_lines(&rows);
    let mut art = v_flex();
    for line in ART {
        art = art.child(div().whitespace_nowrap().text_color(p.accent).child(line.replace(' ', "\u{a0}")));
    }
    let mut lines = v_flex();
    for (k, v) in &right {
        lines = lines.child(match k {
            Some(k) => h_flex().gap_0().whitespace_nowrap().child(div().text_color(p.accent).font_weight(FontWeight::BOLD).child(k.replace(' ', "\u{a0}"))).child(div().text_color(p.text).child(v.clone())),
            None => h_flex().whitespace_nowrap().text_color(if v.starts_with('-') { p.text_tertiary } else { p.accent }).child(v.clone()),
        });
    }
    let mut body = h_flex().gap_6().items_start().text_sm();
    if let Some(f) = mono(p) {
        body = body.font_family(f);
    }
    let colors = [img::hex(p.card_bg), img::hex(p.text), img::hex(p.text_secondary), img::hex(p.accent)];
    let text = as_text(&rows);
    let pic_rows = rows.clone();
    vec![
        shell(p).p_4().child(body.child(art).child(lines)).into_any_element(),
        h_flex()
            .pt_2()
            .gap_2()
            .child(button("pc-copy-text", p, t!("Copy as text"), move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))))
            .child(button("pc-copy-pic", p, t!("Copy as picture"), move |_, cx| {
                let (rows, colors) = (pic_rows.clone(), colors.clone());
                cx.spawn(async move |cx| {
                    let png = cx.background_spawn(async move { as_png(&rows, &colors) }).await;
                    cx.update(|cx| match png {
                        Some(png) => img::copy_png(png, cx),
                        None => log::warn!("about this PC: the picture could not be drawn"),
                    });
                })
                .detach();
            }))
            .into_any_element(),
        div().pt_2().text_xs().text_color(p.text_tertiary).child(t!("Read on this PC only. No computer name, user name, serial number or address is shown or copied.")).into_any_element(),
    ]
}
