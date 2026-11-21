//! Settings → Speed test: runs [`crate::bench`] on a background thread and shows the results:
//! a race played back at real speed, a table with time, CPU and memory per app, our engines
//! against Windows'. Nothing is uploaded or shared.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parking_lot::Mutex;
use leaf_sys::bench::{App as BenchApp, AppKind, Machine, OpenResult};

use crate::bench::{OpenTest, Progress, Results};
use crate::t;
use crate::theme::{self, Palette};

/// One dot of the race track stands for this long.
const DOT_MS: f64 = 20.0;

/// What the background run reports.
#[derive(Default)]
struct Shared {
    progress: Option<Progress>,
    done: Option<Result<Results, String>>,
}

struct State {
    detected: Vec<BenchApp>,
    machine: Option<Machine>,
    off: HashSet<AppKind>,
    running: Option<(Arc<Mutex<Shared>>, Arc<AtomicBool>)>,
    progress: Option<Progress>,
    results: Option<Results>,
    error: Option<String>,
    race: OpenTest,
    replay: Instant,
}
impl Global for State {}

fn state(cx: &mut App) -> &mut State {
    if !cx.has_global::<State>() {
        let detected = leaf_sys::bench::detect();
        cx.set_global(State {
            detected,
            machine: None,
            off: HashSet::new(),
            running: None,
            progress: None,
            results: crate::bench::last_results(),
            error: None,
            race: OpenTest::Files100k,
            replay: Instant::now() - Duration::from_secs(600),
        });
        // the machine line reads the registry and asks the disk: not on the UI thread
        cx.spawn(async move |cx| {
            let m = cx.background_spawn(async { leaf_sys::bench::machine(&crate::bench::bench_dir()) }).await;
            cx.update(|cx| {
                cx.global_mut::<State>().machine = Some(m);
                cx.refresh_windows();
            });
        })
        .detach();
    }
    cx.global_mut::<State>()
}

// ---------------------------------------------------------------- formatting

fn ms(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.1} s", v / 1000.0)
    } else if v >= 1000.0 {
        format!("{:.2} s", v / 1000.0)
    } else {
        format!("{v:.0} ms")
    }
}

fn mb(bytes: u64) -> String {
    format!("{} MB", (bytes as f64 / 1_048_576.0).round() as u64)
}

/// 2330750 → "2,330,750".
pub(crate) fn count(n: u64) -> String {
    let sep = ',';
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

fn times(slow: f64, fast: f64) -> String {
    let x = slow / fast.max(1.0);
    if x >= 10.0 { format!("{x:.0}×") } else { format!("{x:.1}×") }
}

fn test_label(t: OpenTest) -> &'static str {
    match t {
        OpenTest::Start => t!("Start (empty folder)"),
        OpenTest::Files1k => t!("1,000 files"),
        OpenTest::Files10k => t!("10,000 files"),
        OpenTest::Files100k => t!("100,000 files"),
        OpenTest::Images => t!("2,000 pictures"),
    }
}

fn app_label(k: AppKind) -> &'static str {
    match k {
        AppKind::Leaf => "leaf",
        AppKind::Explorer if !cfg!(windows) => t!("The system's file manager"),
        AppKind::Explorer => t!("File Explorer"),
        AppKind::Files => "Files",
        AppKind::FilePilot => "File Pilot",
    }
}

fn machine_line(m: &Machine) -> String {
    let mut parts = vec![m.cpu.clone()];
    if m.ram_gb > 0 {
        parts.push(format!("{} GB RAM", m.ram_gb));
    }
    if !m.disk.is_empty() {
        parts.push(m.disk.clone());
    }
    parts.push(m.windows.clone());
    parts.join(" · ")
}

// ---------------------------------------------------------------- running

fn start(cx: &mut App) {
    let st = state(cx);
    if st.running.is_some() {
        return;
    }
    let mut apps = vec![crate::bench::leaf_app()];
    apps.extend(st.detected.iter().filter(|a| !st.off.contains(&a.kind)).cloned());
    let plan = crate::bench::Plan { apps, open: OpenTest::ALL.to_vec(), copy: true, engines: true };
    let shared = Arc::new(Mutex::new(Shared::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    st.running = Some((shared.clone(), cancel.clone()));
    st.error = None;
    st.progress = None;
    {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name("rf-bench".into())
            .spawn(move || {
                let report = |p: Progress| shared.lock().progress = Some(p);
                let r = crate::bench::run(&plan, &report, &cancel).map_err(|e| format!("{e:#}"));
                shared.lock().done = Some(r);
                // the coffee strip quotes the newest results
            })
            .ok();
    }
    // mirror the thread's progress into the page until it finishes
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let (p, done) = {
                let mut s = shared.lock();
                (s.progress.clone(), s.done.take())
            };
            let finished = cx.update(|cx| {
                let st = cx.global_mut::<State>();
                st.progress = p;
                if let Some(d) = done {
                    st.running = None;
                    match d {
                        Ok(r) => {
                            st.results = Some(r);
                            st.replay = Instant::now();
                        }
                        Err(e) if e == "cancelled" => {}
                        Err(e) => st.error = Some(e),
                    }
                    cx.refresh_windows();
                    return true;
                }
                cx.refresh_windows();
                false
            });
            if finished {
                break;
            }
        }
        replay_frames(cx).await;
    })
    .detach();
}

/// Redraws while the race replays.
async fn replay_frames(cx: &mut AsyncApp) {
    loop {
        let more = cx.update(|cx| {
            let st = cx.global::<State>();
            let longest = st.results.as_ref().map_or(0.0, |r| race_longest(r, st.race));
            let going = st.replay.elapsed().as_secs_f64() * 1000.0 < longest + 300.0;
            cx.refresh_windows();
            going
        });
        if !more {
            break;
        }
        cx.background_executor().timer(Duration::from_millis(33)).await;
    }
}

fn race_longest(r: &Results, test: OpenTest) -> f64 {
    r.open
        .iter()
        .find(|o| o.test == test)
        .map_or(0.0, |o| o.apps.iter().filter_map(|a| a.median()).map(|m| m.settled_ms).fold(0.0, f64::max))
}

fn replay(test: OpenTest, cx: &mut App) {
    let st = state(cx);
    st.race = test;
    st.replay = Instant::now();
    cx.spawn(async move |cx| replay_frames(cx).await).detach();
}

// ---------------------------------------------------------------- page

fn button(id: impl Into<ElementId>, p: &Palette, icon: L, label: impl Into<SharedString>, primary: bool, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    h_flex()
        .id(id)
        .h(px(32.))
        .px_3()
        .gap_2()
        .items_center()
        .rounded(theme::RADIUS_SM)
        .text_sm()
        .when(primary, |d| d.bg(p.accent).text_color(p.accent_text).hover(|s| s.opacity(0.9)))
        .when(!primary, |d| d.bg(p.hover).border_1().border_color(p.border).hover(|s| s.bg(p.pressed)))
        .active(|s| s.opacity(0.8))
        .child(Icon::new(icon).size(px(14.)))
        .child(label.into())
        .on_click(move |_, window, cx| run(window, cx))
        .into_any_element()
}

fn shell(p: &Palette) -> Div {
    v_flex().w_full().rounded(theme::RADIUS).bg(p.hover).border_1().border_color(p.border)
}

pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let st = state(cx);
    let running = st.running.is_some();
    let mut out = Vec::new();

    // intro: what it does, on which machine, with which apps
    let machine = st.machine.as_ref().map(machine_line).unwrap_or_default();
    let mut chips = h_flex().gap_2().flex_wrap().child(chip(p, "bench-app-rf", "leaf", true, true, |_| {}));
    let installed: Vec<AppKind> = st.detected.iter().map(|a| a.kind).collect();
    for kind in [AppKind::Explorer, AppKind::Files, AppKind::FilePilot] {
        let here = installed.contains(&kind);
        let on = here && !st.off.contains(&kind);
        let label = if here { app_label(kind).to_string() } else { format!("{} · {}", app_label(kind), t!("not installed")) };
        chips = chips.child(chip(p, ("bench-app", kind as usize), &label, on, !here || running, move |cx| {
            let off = &mut state(cx).off;
            if !off.remove(&kind) {
                off.insert(kind);
            }
        }));
    }
    let action = if running {
        button("bench-stop", p, L::X, t!("Stop"), false, |_, cx| {
            if let Some((_, c)) = &state(cx).running {
                c.store(true, Ordering::Relaxed);
            }
        })
    } else {
        button("bench-start", p, L::Play, if st.results.is_some() { t!("Run again") } else { t!("Start the test") }, true, |_, cx| start(cx))
    };
    let mut intro = v_flex()
        .p_4()
        .gap_3()
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(Icon::new(L::Gauge).size(px(22.)).text_color(p.accent))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("Speed test on this PC")))
                        .child(div().text_xs().text_color(p.text_secondary).child(if machine.is_empty() { t!("Reading this PC…").to_string() } else { machine })),
                )
                .child(action),
        )
        .child(chips)
        .child(div().text_xs().text_color(p.text_secondary).child(t!(
            "Each app opens the same folders in turn; time, CPU and memory are measured. Other apps' windows open and close during the test: don't touch the mouse or keyboard. It takes about 5–10 minutes; the test files are made the first time (~1.3 GB, in local app data)."
        )));
    if running {
        let (frac, label) = match &st.progress {
            Some(Progress::Step { i, n, label }) => (*i as f32 / (*n).max(1) as f32, format!("{} {i}/{n} · {label}", t!("Step"))),
            Some(Progress::Preparing(what, f)) => (*f * 0.05, format!("{} {what}", t!("Preparing test files:"))),
            None => (0.0, t!("Starting…").to_string()),
        };
        intro = intro.child(
            v_flex()
                .gap_1p5()
                .child(div().h(px(4.)).w_full().rounded_full().bg(p.divider).child(div().h_full().w(relative(frac.clamp(0.01, 1.0))).rounded_full().bg(p.accent)))
                .child(div().text_xs().text_color(p.text_secondary).child(label)),
        );
    }
    if let Some(e) = &st.error {
        intro = intro.child(div().text_sm().text_color(p.danger).child(format!("{} {e}", t!("The test couldn't finish:"))));
    }
    out.push(shell(p).child(intro).into_any_element());

    let Some(r) = st.results.clone() else { return out };
    let race = st.race;
    let elapsed = st.replay.elapsed().as_secs_f64() * 1000.0;
    out.push(race_card(p, &r, race, elapsed));
    out.push(table_card(p, &r));
    if let Some(e) = engines_card(p, &r) {
        out.push(e);
    }
    out.push(
        h_flex()
            .pt_1()
            .gap_2()
            .text_xs()
            .text_color(p.text_tertiary)
            .child(div().flex_1().min_w_0().child(t!(
                "Medians of 3 runs (a warm-up run isn't counted). Time: from starting the app until its window stops changing for 500 ms. * The app was already running: CPU counts from when the window appeared, memory is the whole process. Memory is the figure Task Manager shows."
            )))
            .child(
                div()
                    .id("bench-delete")
                    .cursor_pointer()
                    .text_color(p.accent)
                    .hover(|s| s.underline())
                    .child(t!("Delete test files"))
                    .on_click(|_, _, cx| {
                        std::thread::spawn(|| {
                            let _ = crate::bench::delete_test_files();
                        });
                        state(cx).results = None;
                        cx.refresh_windows();
                    }),
            )
            .into_any_element(),
    );
    out
}

fn chip(p: &Palette, id: impl Into<ElementId>, label: &str, on: bool, locked: bool, toggle: impl Fn(&mut App) + 'static) -> AnyElement {
    h_flex()
        .id(id)
        .h(px(28.))
        .px_3()
        .gap_1p5()
        .items_center()
        .rounded_full()
        .border_1()
        .text_sm()
        .when(on, |d| d.bg(p.selection).border_color(p.accent))
        .when(!on, |d| d.border_color(p.border).text_color(p.text_secondary))
        .when(!locked, |d| d.hover(|s| s.bg(p.pressed)).on_click(move |_, _, cx| {
            toggle(cx);
            cx.refresh_windows();
        }))
        .when(on, |d| d.child(Icon::new(L::Check).size(px(12.)).text_color(p.accent)))
        .child(label.to_string())
        .into_any_element()
}

fn race_card(p: &Palette, r: &Results, race: OpenTest, elapsed: f64) -> AnyElement {
    // (they go on to a second line where the page is narrow)
    let mut tabs = h_flex().gap_1().flex_wrap();
    for (i, o) in r.open.iter().enumerate() {
        let t = o.test;
        let on = t == race;
        tabs = tabs.child(
            div()
                .id(("bench-tab", i))
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .text_sm()
                .when(on, |d| d.bg(p.selection).text_color(p.text))
                .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.pressed)))
                .child(test_label(t))
                .on_click(move |_, _, cx| replay(t, cx)),
        );
    }
    let o = r.open.iter().find(|o| o.test == race);
    let rows: Vec<(AppKind, Option<OpenResult>, Option<String>)> =
        o.map(|o| o.apps.iter().map(|a| (a.app, a.median().cloned(), a.error.clone())).collect()).unwrap_or_default();
    let longest = rows.iter().filter_map(|r| r.1.as_ref().map(|m| m.settled_ms)).fold(1.0, f64::max);
    let best = rows.iter().filter_map(|r| r.1.as_ref().map(|m| m.settled_ms)).fold(f64::MAX, f64::min);
    let mut lanes = v_flex().gap_3();
    for (app, m, err) in rows {
        let (track, value): (AnyElement, String) = match (&m, err) {
            (Some(m), _) => {
                let shown = elapsed.min(m.settled_ms);
                let done = elapsed >= m.settled_ms;
                let total_dots = (longest / DOT_MS).ceil().max(1.0);
                let lit = (shown / DOT_MS).ceil();
                let frac = (lit / total_dots).clamp(0.0, 1.0) as f32;
                let color = if app == AppKind::Leaf { p.accent } else { p.text_secondary };
                (
                    div().h(px(8.)).w_full().rounded_full().bg(p.divider).child(div().h_full().w(relative(frac.max(0.005))).rounded_full().bg(color)).into_any_element(),
                    if done { format!("{}{}", ms(m.settled_ms), if m.shared { " *" } else { "" }) } else { t!("Listing…").to_string() },
                )
            }
            (None, Some(_)) => (div().text_xs().text_color(p.text_tertiary).child(t!("couldn't be measured")).into_any_element(), "—".into()),
            _ => (div().into_any_element(), "—".into()),
        };
        let winner = m.as_ref().is_some_and(|m| m.settled_ms == best && elapsed >= m.settled_ms);
        lanes = lanes.child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().w(px(120.)).flex_shrink_0().text_sm().child(app_label(app)))
                .child(div().flex_1().min_w_0().child(track))
                .child(
                    div()
                        .w(px(96.))
                        .flex_shrink_0()
                        .text_sm()
                        .text_right()
                        .font_weight(if winner { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                        .text_color(if winner { p.accent } else { p.text })
                        .child(value),
                ),
        );
    }
    shell(p)
        .p_4()
        .gap_4()
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t!("Opening a folder, at real speed")))
                .child(button("bench-replay", p, L::RotateCcw, t!("Replay"), false, move |_, cx| replay(race, cx))),
        )
        .child(tabs)
        .child(lanes)
        .into_any_element()
}

/// The details of each opening, one block per test: a row per app with its time and, beside it, what the machine paid (processor time,
/// memory). Stacked, not a table with a column per app: the settings page is narrow, and four columns of two lines did not fit in it
/// (the Files column was cut off and the cells broke mid-word).
fn table_card(p: &Palette, r: &Results) -> AnyElement {
    let mut list = v_flex();
    for (i, o) in r.open.iter().enumerate() {
        let best = o.apps.iter().filter_map(|a| a.median().map(|m| m.settled_ms)).fold(f64::MAX, f64::min);
        let mut block = v_flex().px_4().py_3().gap_1p5().when(i > 0, |d| d.border_t_1().border_color(p.divider)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(test_label(o.test)));
        for a in &o.apps {
            let (time, cost, winner) = match (a.median(), &a.error) {
                (Some(m), _) => {
                    // Task Manager's figure; older results: private bytes, else the working set
                    let mem = if m.private_ws > 0 { mb(m.private_ws) } else if m.private > 0 { mb(m.private) } else { mb(m.ram) };
                    (
                        format!("{}{}{}", ms(m.settled_ms), if m.shared { " *" } else { "" }, if m.timed_out { " ⏱" } else { "" }),
                        format!("{} {} · {} {}", t!("CPU"), ms(m.cpu_ms), t!("memory"), mem),
                        m.settled_ms == best,
                    )
                }
                (None, Some(e)) => ("—".to_string(), format!("{} ({})", t!("couldn't be measured"), e.chars().take(60).collect::<String>()), false),
                _ => ("—".to_string(), String::new(), false),
            };
            block = block.child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(112.)).flex_shrink_0().text_sm().text_color(p.text_secondary).truncate().child(app_label(a.app)))
                    .child(
                        div()
                            .w(px(84.))
                            .flex_shrink_0()
                            .text_sm()
                            .font_weight(if winner { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                            .text_color(if winner { p.accent } else { p.text })
                            .child(time),
                    )
                    .child(div().flex_1().min_w_0().text_xs().text_color(p.text_tertiary).child(cost)),
            );
        }
        list = list.child(block);
    }
    shell(p)
        .child(h_flex().px_4().pt_4().pb_1().child(div().font_weight(FontWeight::SEMIBOLD).child(t!("Details: time, CPU and memory"))))
        .child(list)
        .into_any_element()
}

fn engines_card(p: &Palette, r: &Results) -> Option<AnyElement> {
    let e = &r.engines;
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    // "işlemci süresi: leaf 1.20 s · Windows 3.40 s · bellek: +12 MB · +40 MB"
    let cost = |ours: (Option<f64>, Option<f64>), theirs: Option<(Option<f64>, Option<f64>, &str)>| -> String {
        let mb = |m: f64| format!("+{m:.0} MB");
        let mut parts = Vec::new();
        if let Some(c) = ours.0 {
            let mut s = format!("{}: leaf {}", t!("CPU time"), ms(c));
            if let Some((Some(tc), _, name)) = theirs {
                s.push_str(&format!(" · {name} {}", ms(tc)));
            }
            parts.push(s);
        }
        if let Some(m) = ours.1 {
            let mut s = format!("{}: {}", t!("memory"), mb(m));
            if let Some((_, Some(tm), _)) = theirs {
                s.push_str(&format!(" · {}", mb(tm)));
            }
            parts.push(s);
        }
        parts.join(" · ")
    };
    let mut versus = |label: &str, v: &Option<crate::bench::Versus>, windows_name: &str| {
        let Some(v) = v else { return };
        let usage = cost((v.ours_cpu_ms, v.ours_mb), Some((v.windows_cpu_ms, v.windows_mb, windows_name)));
        let (Some(a), Some(b)) = (v.ours_ms, v.windows_ms) else {
            if let Some(a) = v.ours_ms {
                rows.push((label.to_string(), format!("leaf {}", ms(a)), String::new(), usage));
            }
            return;
        };
        // within 10 %: the disk sets the pace for both
        let verdict = if (a - b).abs() <= a.max(b) * 0.1 {
            t!("the same speed").to_string()
        } else if b > a {
            t!("{x} faster").replace("{x}", &times(b, a))
        } else {
            t!("{x} slower").replace("{x}", &times(a, b))
        };
        rows.push((label.to_string(), format!("leaf {} · {windows_name} {}", ms(a), ms(b)), verdict, usage));
    };
    let theirs = if cfg!(windows) { "Windows" } else { t!("the system") };
    versus(t!("Copying: 10,000 small files"), &e.copy_small, theirs);
    versus(t!("Copying: one 1 GB file"), &e.copy_big, theirs);
    versus(t!("Extracting a ZIP: 10,000 files"), &e.unzip, theirs);
    if let Some(s) = e.search_ms {
        let rate = e.search_entries as f64 / (s / 1000.0).max(0.001);
        let usage = cost((e.search_cpu_ms, e.search_mb), None);
        rows.push((t!("Search in subfolders").to_string(), format!("{} {} · {}", count(e.search_entries), t!("items"), ms(s)), t!("{n} items a second").replace("{n}", &count(rate as u64)), usage));
    }
    if rows.is_empty() {
        return None;
    }
    // each row stacked: the name with the verdict at its right, then the times, then what they cost (a fixed name column squeezed the
    // times into a column a few letters wide on the narrow settings page)
    let mut list = v_flex();
    for (i, (label, detail, verdict, usage)) in rows.into_iter().enumerate() {
        list = list.child(
            v_flex()
                .px_4()
                .py_3()
                .gap_1()
                .when(i > 0, |d| d.border_t_1().border_color(p.divider))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().flex_1().min_w_0().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                        .when(!verdict.is_empty(), |d| d.child(div().flex_shrink_0().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.accent).child(verdict))),
                )
                .child(div().text_sm().text_color(p.text_secondary).child(detail))
                // what each side cost the machine, under the times
                .when(!usage.is_empty(), |d| d.child(div().text_xs().text_color(p.text_tertiary).child(usage))),
        );
    }
    Some(
        shell(p)
            .child(h_flex().px_4().pt_4().pb_1().child(div().font_weight(FontWeight::SEMIBOLD).child(if cfg!(windows) { t!("leaf's engines and Windows'") } else { t!("leaf's engines and the system's") })))
            .child(list)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_get_separators() {
        let c = super::count(2_330_750);
        assert!(c == "2.330.750" || c == "2,330,750", "{c}");
        assert_eq!(super::count(999), "999");
    }
}
