//! Tokens the agent sessions used (PLAN F5, D1, D9, D12), read from the sessions' own files by `leaf_agents::usage`. Kept per session
//! until the session's file changes; read off the UI thread, one session after the other, only for sessions the agent list holds
//! (the last three days). Everything here is an estimate of "new tokens" (input, output, cache writes), never a bill.
//!
//! Behind the agent layer's switch: `agents::apply` calls [`clear`] when it goes off, and nothing is read before the list has sessions.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use leaf_agents::folder_key;
use leaf_agents::usage::{Tokens, Turn};

use crate::t;
use crate::theme::Palette;

const DAY_MS: i64 = 24 * 3600 * 1000;

#[derive(Default)]
struct UsageState {
    /// "agent:id" -> the session's last activity when it was read, and its turns.
    sessions: HashMap<String, (i64, Arc<Vec<Turn>>)>,
    /// "agent:id" -> the session's new tokens in all (so a folder row only adds numbers).
    totals: HashMap<String, u64>,
    pending: bool,
}

impl Global for UsageState {}

fn key(s: &leaf_agents::Session) -> String {
    format!("{}:{}", s.agent, s.id)
}

pub fn clear(cx: &mut App) {
    if cx.try_global::<UsageState>().is_some() {
        cx.set_global(UsageState::default());
    }
}

/// Reads the sessions whose file changed since the last read. Cheap when nothing did.
pub fn update(cx: &mut App) {
    let sessions = crate::agents::sessions(cx);
    let st = cx.default_global::<UsageState>();
    if st.pending || sessions.is_empty() {
        return;
    }
    let todo: Vec<leaf_agents::Session> = sessions.iter().filter(|s| st.sessions.get(&key(s)).is_none_or(|(at, _)| *at != s.last_activity_ms)).cloned().collect();
    if todo.is_empty() {
        return;
    }
    st.pending = true;
    cx.spawn(async move |cx| {
        let got = cx
            .background_executor()
            .spawn(async move {
                let adapters = leaf_agents::adapters();
                let cancel = AtomicBool::new(false);
                todo.into_iter()
                    .map(|s| {
                        let turns = adapters.iter().find(|a| a.id() == s.agent).and_then(|a| a.session_file(&s.id)).map(|f| leaf_agents::usage::read(&f, &cancel)).unwrap_or_default();
                        (key(&s), (s.last_activity_ms, Arc::new(turns)))
                    })
                    .collect::<Vec<_>>()
            })
            .await;
        let _ = cx.update(|cx| {
            if crate::agents::sessions(cx).is_empty() {
                return;
            }
            cx.update_global::<UsageState, _>(|s, _| {
                s.pending = false;
                for (k, (_, t)) in &got {
                    s.totals.insert(k.clone(), sum(t, 0).new_tokens());
                }
                s.sessions.extend(got);
            });
            cx.refresh_windows();
            // sessions that came in while this ran
            update(cx);
        });
    })
    .detach();
}

fn turns(cx: &App, s: &leaf_agents::Session) -> Option<Arc<Vec<Turn>>> {
    cx.try_global::<UsageState>()?.sessions.get(&key(s)).map(|(_, t)| t.clone())
}

fn sum(t: &[Turn], since_ms: i64) -> Tokens {
    let mut o = Tokens::default();
    for x in t.iter().filter(|x| x.at_ms >= since_ms) {
        o.add(&x.tokens);
    }
    o
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn inside(folder: &Path, dir: &Path) -> bool {
    let (f, d) = (folder_key(folder), folder_key(dir));
    f == d || f.strip_prefix(d.as_str()).is_some_and(|r| r.starts_with('/'))
}

/// New tokens of every session in the last 24 hours (the budget ring).
pub fn last_day(cx: &App) -> u64 {
    let since = now_ms() - DAY_MS;
    crate::agents::sessions(cx).iter().filter_map(|s| turns(cx, s)).map(|t| sum(&t, since).new_tokens()).sum()
}

/// New tokens of the sessions working in `dir` or inside it (all three days), for the folder row and the page. `None`: none known.
pub fn folder_tokens(cx: &App, dir: &Path) -> Option<u64> {
    let st = cx.try_global::<UsageState>().filter(|s| !s.totals.is_empty())?;
    let n: u64 = crate::agents::sessions(cx).iter().filter(|s| inside(&s.folder, dir)).filter_map(|s| st.totals.get(&key(s))).sum();
    (n > 0).then_some(n)
}

/// "1.2M", "340k", "820".
pub fn short(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1000..=999_999 => format!("{}k", (n + 500) / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.),
    }
}

/// The budget ring (D12): a ring of twelve dots, as many filled as the last 24 hours used of the budget (Settings > Developer). Quiet: no
/// colour change, no number it warns with; the tooltip gives the numbers. `None` while no budget is set or nothing was used.
pub fn ring(cx: &App, p: &Palette) -> Option<Stateful<gpui::Div>> {
    let budget = u64::from(cx.try_global::<crate::settings::Settings>()?.agent_budget_k) * 1000;
    if budget == 0 {
        return None;
    }
    let used = last_day(cx);
    if used == 0 {
        return None;
    }
    let filled = ((used as f64 / budget as f64) * 12.).round().clamp(0., 12.) as usize;
    let tip = format!("{} {} {} {} ({})", t!("Last 24 hours:"), short(used), t!("of"), short(budget), t!("new tokens, an estimate"));
    let mut d = div().id("budget-ring").relative().size(px(16.)).flex_shrink_0();
    for i in 0..12 {
        let a = (i as f32) * std::f32::consts::TAU / 12.;
        let (x, y) = (8. + 6. * a.sin() - 1.25, 8. - 6. * a.cos() - 1.25);
        d = d.child(div().absolute().left(px(x)).top(px(y)).size(px(2.5)).rounded_full().bg(if i < filled { p.accent } else { p.border_strong }));
    }
    Some(d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
}

/// The session ribbon (D9) of a project tab: one segment per session working in `dir` or inside it, the newest eight, left to right oldest
/// first; length is the session's duration, thickness its tokens, colour its state. `None` when no session of the folder has usage.
pub fn ribbon(cx: &App, dir: &Path, p: &Palette) -> Option<Div> {
    cx.try_global::<UsageState>().filter(|s| !s.sessions.is_empty())?;
    let mut segs: Vec<(i64, i64, u64, leaf_agents::State)> = Vec::new();
    for s in crate::agents::sessions(cx).iter().filter(|s| inside(&s.folder, dir)).take(8) {
        let Some(t) = turns(cx, s) else { continue };
        let (Some(a), Some(b)) = (t.first(), t.last()) else { continue };
        segs.push((a.at_ms, (b.at_ms - a.at_ms).max(60_000), sum(&t, 0).new_tokens(), s.state));
    }
    if segs.is_empty() {
        return None;
    }
    segs.sort_by_key(|s| s.0);
    let max = segs.iter().map(|s| s.2).max().unwrap_or(1).max(1);
    let mut d = h_flex().gap(px(1.)).items_end().h(px(3.)).w_full();
    for (_, len, tk, st) in segs {
        let thick = 1. + 2. * (tk as f32 / max as f32);
        d = d.child(div().flex_basis(px(0.)).flex_grow(((len / 60_000) as f32).clamp(1., 600.)).min_w(px(3.)).h(px(thick)).rounded_full().bg(crate::agents::state_color(st, p).opacity(0.8)));
    }
    Some(d)
}

/// One line of numbers for the Agents page: the last three days per day, then per project, each with a bar (F5, D1). Estimates.
pub fn page(cx: &App, p: &Palette) -> Option<gpui::Div> {
    cx.try_global::<UsageState>().filter(|s| !s.sessions.is_empty())?;
    let sessions = crate::agents::sessions(cx);
    let now = now_ms();
    let mut per_project: Vec<(String, std::path::PathBuf, u64)> = Vec::new();
    let mut per_lane: Vec<(crate::lanes::Lane, u64)> = Vec::new();
    for s in sessions.iter() {
        let Some(t) = turns(cx, s) else { continue };
        let n = sum(&t, 0).new_tokens();
        if n == 0 {
            continue;
        }
        let g = s.group().to_path_buf();
        match per_project.iter_mut().find(|(_, path, _)| folder_key(path) == folder_key(&g)) {
            Some(e) => e.2 += n,
            None => per_project.push((rf_core::history::display_name(&g), g, n)),
        }
        if let Some(l) = crate::lanes::of_dir(cx, &s.folder) {
            match per_lane.iter_mut().find(|(x, _)| x.path == l.path) {
                Some(e) => e.1 += n,
                None => per_lane.push((l, n)),
            }
        }
    }
    if per_project.is_empty() {
        return None;
    }
    per_project.sort_by(|a, b| b.2.cmp(&a.2));
    let days: Vec<u64> = (0..3).map(|d| sessions.iter().filter_map(|s| turns(cx, s)).map(|t| t.iter().filter(|x| x.at_ms >= now - (d + 1) * DAY_MS && x.at_ms < now - d * DAY_MS).map(|x| x.tokens.new_tokens()).sum::<u64>()).sum()).collect();
    let max = per_project.iter().map(|x| x.2).max().unwrap_or(1).max(1);
    let bar = |n: u64, color: Hsla| div().w(px(120.)).h(px(4.)).flex_shrink_0().rounded_full().bg(p.border).child(div().h_full().w(relative(n as f32 / max as f32)).rounded_full().bg(color));
    let line = |dot: Option<Hsla>, name: String, n: u64, indent: bool| {
        h_flex()
            .h(px(22.))
            .gap_2()
            .items_center()
            .pl(if indent { px(24.) } else { px(0.) })
            .children(dot.map(|c| div().size(px(6.)).flex_shrink_0().rounded_full().bg(c)))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text).child(name))
            .child(bar(n, p.accent))
            .child(div().w(px(56.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(short(n)))
    };
    let mut col = v_flex().px_3().py_2().gap(px(2.)).border_b_1().border_color(p.divider);
    col = col.child(
        h_flex().gap_3().items_center().child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Usage, new tokens (an estimate)")))
            .child(div().flex_1().text_xs().text_color(p.text_tertiary).child(format!("{} {} · {} {} · {} {}", t!("today"), short(days[0]), t!("yesterday"), short(days[1]), t!("day before"), short(days[2])))),
    );
    for (name, path, n) in per_project.iter().take(6) {
        col = col.child(line(None, name.clone(), *n, false));
        for (l, ln) in per_lane.iter().filter(|(l, _)| folder_key(&l.repo) == folder_key(path)) {
            col = col.child(line(Some(crate::lanes::color(l.color, p)), l.name.clone(), *ln, true));
        }
    }
    Some(col)
}
