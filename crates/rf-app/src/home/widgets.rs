//! The bodies of the Home widgets (the six sections the page had before it was a dashboard, unchanged inside). Each reads its data
//! when it is drawn, and only then.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::{Location, format};

use super::{Size, WidgetCx};
use crate::folder_view::{FolderEvent, FolderView, open_in_background, tile_cols};
use crate::icons;
use crate::places;
use crate::t;
use crate::theme::{self, Palette};

fn card(id: ElementId, p: &Palette) -> Stateful<Div> {
    v_flex()
        .id(id)
        .p_3()
        .gap_2()
        .min_w_0()
        .rounded(px(p.tile_radius))
        .bg(p.tile_bg)
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .hover(|s| s.bg(p.tile_hover))
}

pub fn pinned(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let pinned = v.places.read(cx).pinned.clone();
    if pinned.is_empty() {
        return None;
    }
    let p = &w.p;
    let mut grid = div().grid().grid_cols(tile_cols(w.width, 140., 220., 12.)).gap_3();
    for (i, f) in pinned.into_iter().enumerate() {
        let path = f.path.clone();
        grid = grid.child(
            card(("pin", i).into(), p)
                .child(Icon::new(f.icon).size(px(28.)).text_color(p.accent))
                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::i18n::td(&f.title)))
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(Location::Path(path.clone()), cx))),
        );
    }
    Some(grid.into_any_element())
}

/// Spinning or not, per drive root, asked once off the UI thread (`None` = not asked yet or the system does not say).
static MEDIA: std::sync::Mutex<Option<std::collections::HashMap<std::path::PathBuf, Option<bool>>>> = std::sync::Mutex::new(None);

fn media_of(root: &std::path::Path, local: bool, cx: &mut Context<FolderView>) -> Option<&'static str> {
    if !local {
        return None;
    }
    let known = {
        let mut g = MEDIA.lock().ok()?;
        let m = g.get_or_insert_with(Default::default);
        match m.get(root) {
            Some(r) => Some(*r),
            None => {
                m.insert(root.to_path_buf(), None);
                None
            }
        }
    };
    match known {
        Some(r) => r.map(|hdd| if hdd { "HDD" } else { "SSD" }),
        None => {
            let root = root.to_path_buf();
            cx.spawn(async move |this, cx| {
                let r2 = root.clone();
                let r = cx.background_executor().spawn(async move { leaf_sys::drives::seek_penalty(&r2) }).await;
                if let Ok(mut g) = MEDIA.lock() {
                    g.get_or_insert_with(Default::default).insert(root, r);
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
            None
        }
    }
}

/// Read and write throughput per local drive root, from two readings of the disk counters (`leaf_sys::drives::disk_counters`).
#[derive(Default)]
struct Io {
    roots: Vec<std::path::PathBuf>,
    last: std::collections::HashMap<std::path::PathBuf, (std::time::Instant, u64, u64)>,
    rate: std::collections::HashMap<std::path::PathBuf, (u64, u64)>,
    running: bool,
    /// When the drive tiles were last drawn: the sampling stops a few seconds after the Home page is no longer on screen.
    seen: Option<std::time::Instant>,
}
static IO: std::sync::Mutex<Option<Io>> = std::sync::Mutex::new(None);

fn io_with<R>(f: impl FnOnce(&mut Io) -> R) -> Option<R> {
    IO.lock().ok().map(|mut g| f(g.get_or_insert_with(Default::default)))
}

/// Called while the drive tiles are drawn: keeps the sampler running (one reading every 2 seconds, off the UI thread) and lets it
/// stop by itself when the tiles have not been drawn for 5 seconds.
fn io_touch(roots: Vec<std::path::PathBuf>, cx: &mut Context<FolderView>) {
    let start = io_with(|io| {
        io.seen = Some(std::time::Instant::now());
        io.roots = roots;
        !std::mem::replace(&mut io.running, true)
    });
    if start != Some(true) {
        return;
    }
    cx.spawn(async move |this, cx| {
        loop {
            let roots = io_with(|io| {
                if io.seen.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
                    io.running = false;
                    // (a reading taken long ago is no base for the next one)
                    io.last.clear();
                    io.rate.clear();
                    return None;
                }
                Some(io.roots.clone())
            })
            .flatten();
            let Some(roots) = roots else { break };
            let got = cx
                .background_executor()
                .spawn(async move { roots.into_iter().filter_map(|r| leaf_sys::drives::disk_counters(&r).map(|(rd, wr)| (r, rd, wr))).collect::<Vec<_>>() })
                .await;
            let now = std::time::Instant::now();
            io_with(|io| {
                for (root, rd, wr) in got {
                    if let Some((t0, r0, w0)) = io.last.get(&root) {
                        let secs = now.duration_since(*t0).as_secs_f64().max(0.001);
                        io.rate.insert(root.clone(), ((rd.saturating_sub(*r0) as f64 / secs) as u64, (wr.saturating_sub(*w0) as f64 / secs) as u64));
                    }
                    io.last.insert(root, (now, rd, wr));
                }
            });
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                io_with(|io| io.running = false);
                break;
            }
            cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
        }
    })
    .detach();
}

pub fn drives(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let drives = v.places.read(cx).drives.clone();
    io_touch(drives.iter().filter(|d| matches!(d.kind, leaf_sys::DriveKind::Fixed | leaf_sys::DriveKind::Removable)).map(|d| d.root.clone()).collect(), cx);
    let p = &w.p;
    let mut grid = div().grid().grid_cols(tile_cols(w.width, 240., 340., 12.)).gap_3();
    for (i, d) in drives.into_iter().enumerate() {
        let root = d.root.clone();
        let usage_root = d.root.clone();
        let frac = d.used_fraction();
        let bar_color = if frac > 0.9 { p.danger } else { p.accent };
        let local = matches!(d.kind, leaf_sys::DriveKind::Fixed | leaf_sys::DriveKind::Removable);
        let kind_line = [Some(d.fs.clone()).filter(|f| !f.is_empty()), media_of(&d.root, local, cx).map(str::to_string)].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        grid = grid.child(
            card(("drive", i).into(), p)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(Icon::new(places::drive_icon(&d)).size(px(28.)).text_color(p.text_secondary))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::places::drive_name(&d)))
                                .child(
                                    div()
                                        .h(px(6.))
                                        .w_full()
                                        .rounded_full()
                                        .bg(p.pressed)
                                        .child(div().h_full().rounded_full().bg(bar_color).w(relative(frac))),
                                )
                                .child(div().text_xs().text_color(p.text_secondary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(if d.total > 0 {
                                    format!("{} {} / {}", format::size(d.free), t!("free"), format::size(d.total))
                                } else {
                                    String::new()
                                }))
                                .when(!kind_line.is_empty(), |c| c.child(div().text_xs().text_color(p.text_tertiary).child(kind_line)))
                                .children(io_with(|io| io.rate.get(&d.root).copied()).flatten().map(|(rd, wr)| {
                                    div().text_xs().text_color(p.text_tertiary).child(format!("{} {}/s · {} {}/s", t!("Read"), format::size(rd), t!("Write"), format::size(wr)))
                                })),
                        )
                        // where the space goes, as its own tab (the card itself opens the drive)
                        .child(
                            div()
                                .id(("drive-usage", i))
                                .size(px(28.))
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .justify_center()
                                .rounded(theme::RADIUS_SM)
                                .hover(|s| s.bg(p.hover))
                                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Show disk usage")).build(w, cx))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(FolderEvent::OpenInNewTab(Location::DiskUsage(usage_root.clone())))
                                }))
                                .child(Icon::new(L::ChartPie).size(px(16.)).text_color(p.text_secondary)),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(Location::Path(root.clone()), cx))),
        );
    }
    Some(grid.into_any_element())
}

/// Phones and cameras (not drives): none connected = nothing to show.
pub fn devices(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let devices = v.places.read(cx).devices.clone();
    if devices.is_empty() {
        return None;
    }
    let p = &w.p;
    let mut grid = div().grid().grid_cols(tile_cols(w.width, 240., 340., 12.)).gap_3();
    for (i, d) in devices.into_iter().enumerate() {
        let loc = Location::Shell { parsing: d.parsing.clone(), name: d.name.clone() };
        grid = grid.child(
            card(("device", i).into(), p)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(Icon::new(L::Usb).size(px(28.)).text_color(p.text_secondary))
                        .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(d.name)),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(loc.clone(), cx))),
        );
    }
    Some(grid.into_any_element())
}

pub fn network(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let network = v.places.read(cx).network.clone();
    if network.is_empty() {
        return None;
    }
    let p = &w.p;
    let mut grid = div().grid().grid_cols(tile_cols(w.width, 240., 340., 12.)).gap_3();
    for (i, (name, target)) in network.into_iter().enumerate() {
        let shown_target = target.to_string_lossy().into_owned();
        grid = grid.child(
            card(("home-net", i).into(), p)
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(Icon::new(L::Network).size(px(28.)).text_color(p.text_secondary))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::rtl::display_text(&name)))
                                .child(div().text_xs().text_color(p.text_secondary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(shown_target)),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(crate::remote::network_place(target.clone()), cx))),
        );
    }
    Some(grid.into_any_element())
}

/// Every tag with how many items carry it.
pub fn tags(_: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let tags: Vec<(u32, String, u32, usize)> = {
        let store = &cx.global::<crate::tags::Tags>().0;
        store.defs.iter().map(|d| (d.id, d.name.clone(), d.color, store.paths_with(d.id).len())).collect()
    };
    if tags.is_empty() {
        return None;
    }
    let p = &w.p;
    let mut grid = div().grid().grid_cols(tile_cols(w.width, 140., 220., 12.)).gap_3();
    for (id, name, color, count) in tags {
        grid = grid.child(
            card(("home-tag", id as usize).into(), p)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().size(px(10.)).flex_shrink_0().when(!p.square_marks, |d| d.rounded_full()).bg(crate::tags::color(color)))
                        .child(div().flex_1().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::i18n::td(&name))),
                )
                .child(div().text_xs().text_color(p.text_secondary).child(crate::i18n::items(count)))
                .on_click(cx.listener(move |this, _, _, cx| this.navigate(Location::Tag(id), cx))),
        );
    }
    Some(grid.into_any_element())
}

pub fn recent(v: &mut FolderView, w: &WidgetCx, cx: &mut Context<FolderView>) -> Option<AnyElement> {
    let cleared = cx.global::<crate::settings::Settings>().recent_cleared_ms;
    let recent: Vec<places::Recent> = v.places.read(cx).recent.iter().filter(|r| r.used_ms > cleared).cloned().collect();
    if recent.is_empty() {
        return None;
    }
    let p = &w.p;
    // narrow: the name takes the room and the folder it is in is left out
    let roomy = matches!(w.size, Size::Wide | Size::Large) && w.width >= 560.;
    let mut list = v_flex().gap_0p5();
    for (i, r) in recent.into_iter().enumerate() {
        let e = rf_core::Entry::new(&r.name, if r.is_dir { rf_core::EntryKind::Dir } else { rf_core::EntryKind::File }, 0, 0, 0, 0);
        let cat = icons::category(&e);
        let parent = r.target.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let target = r.target.clone();
        let is_dir = r.is_dir;
        // folders under their Explorer name (Downloads → İndirilenler)
        let shown: String = if is_dir { leaf_sys::localized::label_if_known(&target).map_or_else(|| r.name.clone(), |l| l.to_string()) } else { r.name.clone() };
        let name = div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(shown);
        list = list.child(
            h_flex()
                .id(("recent", i))
                .h(theme::ROW_H)
                .px_2()
                .gap_3()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .text_sm()
                .hover(|s| s.bg(p.hover))
                .cursor_pointer()
                .child(Icon::new(icons::icon_for(cat)).size(px(16.)).text_color(theme::kind_color(cat, p.dark)))
                .map(|d| {
                    if roomy {
                        d.child(name.w(px(260.)).flex_shrink_0())
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text_tertiary).child(parent))
                    } else {
                        d.child(name.flex_1().min_w_0())
                    }
                })
                .child(div().w(px(130.)).flex_shrink_0().text_color(p.text_secondary).child(format::datetime(r.used_ms)))
                // dragged out like a row of the list (the owner: "son kullanılanlarda ... drag drop ta olmuyor")
                .on_mouse_down(MouseButton::Left, {
                    let target = target.clone();
                    cx.listener(move |this, ev: &MouseDownEvent, _, _| this.home_press = Some((target.clone(), ev.position)))
                })
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, _| {
                    let Some((path, at)) = this.home_press.as_ref() else { return };
                    if ev.pressed_button != Some(MouseButton::Left) {
                        this.home_press = None;
                        return;
                    }
                    let (dx, dy) = (f32::from(ev.position.x - at.x), f32::from(ev.position.y - at.y));
                    if dx * dx + dy * dy > 36.0 {
                        let path = path.clone();
                        this.home_press = None;
                        crate::ops::drag_paths_out(vec![path]);
                    }
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.home_press = None;
                    if is_dir {
                        this.navigate(Location::Path(target.clone()), cx);
                    } else {
                        open_in_background(target.clone(), cx);
                    }
                })),
        );
    }
    Some(list.into_any_element())
}

/// Recent's "Clear".
pub fn recent_clear(p: &Palette, cx: &mut Context<FolderView>) -> AnyElement {
    div()
        .id("recent-clear")
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .text_color(p.text_secondary)
        .cursor_pointer()
        .hover(|s| s.bg(p.hover).text_color(p.text))
        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Empties this list; Windows' own list of recent items is left alone")).build(w, cx))
        .child(t!("Clear"))
        .on_click(cx.listener(|_, _, _, cx| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
            crate::settings::update(cx, |s| s.recent_cleared_ms = now);
            cx.notify();
        }))
        .into_any_element()
}
