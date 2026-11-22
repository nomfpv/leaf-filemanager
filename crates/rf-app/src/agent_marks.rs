//! Two quiet marks on file rows, only inside a project that has agent sessions and only while the agent layer is on (PLAN D4, D7):
//! the author stripe (a thin edge mark on a file changed since the last commit: the accent when an agent most likely wrote it, grey
//! when you did) and the context weight bar (a file's size as a share of a context window). Both read what the row already has (the
//! entry and the shared git status); nothing is read from disk and nothing runs per row beyond a few comparisons.
//!
//! Attribution is a best guess: a file counts as an agent's when a session of its project was last active within 90 seconds of the
//! file's change. The tooltip says so.

use std::path::Path;

use gpui_kit::*;
use leaf_sys::git::FileState;
use rf_core::Entry;

use crate::icons::{Category, category};
use crate::t;
use crate::theme::Palette;

/// A session's last record this close to a file's change makes the file an agent's (best guess).
const NEAR_MS: i64 = 90_000;
/// The context window the weight bar is measured against, in tokens.
const WINDOW_TOKENS: u64 = 200_000;
/// Below this share of the window a file gets no bar (a bar on every file would be noise).
const MIN_SHARE: f32 = 0.01;

pub struct RowMarks {
    pub stripe: Option<Stateful<Div>>,
    pub weight: Option<Stateful<Div>>,
    /// A folder's tokens spent by agent sessions working in it (D1), small grey text after the name.
    pub tokens: Option<Stateful<Div>>,
}

/// The marks of the file `e` in `dir`; empty unless a session works in `dir` or above it. `id` is the row's index (element ids).
pub fn row(cx: &App, id: usize, dir: Option<&Path>, e: &Entry, git: Option<FileState>, p: &Palette) -> RowMarks {
    let none = RowMarks { stripe: None, weight: None, tokens: None };
    let Some(dir) = dir else { return none };
    if e.is_dir() {
        // (D1) the tokens the sessions inside this folder used: an estimate (a session is counted where it started, not per subfolder)
        let tokens = crate::agent_usage::folder_tokens(cx, &dir.join(&*e.name)).map(|n| {
            let tip = format!("{} {} ({})", crate::agent_usage::short(n), t!("new tokens"), t!("sessions working in this folder or inside it, an estimate"));
            div()
                .id(("folder-tokens", id))
                .flex_shrink_0()
                .text_xs()
                .text_color(p.text_tertiary)
                .child(format!("{} {}", crate::agent_usage::short(n), t!("tok")))
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        });
        return RowMarks { tokens, ..none };
    }
    let sessions = crate::agents::sessions_over(cx, dir);
    if sessions.is_empty() {
        return none;
    }
    let changed = matches!(git, Some(FileState::Modified | FileState::Added | FileState::Untracked | FileState::Renamed));
    let stripe = changed.then(|| {
        let agent = e.modified_ms > 0 && sessions.iter().any(|(ms, _)| (ms - e.modified_ms).abs() <= NEAR_MS);
        let (color, tip) = if agent {
            (p.accent, t!("Changed since the last commit, most likely by an agent (a best guess)"))
        } else {
            (p.text_tertiary, t!("Changed since the last commit, most likely by you (a best guess)"))
        };
        div().id(("author-stripe", id)).absolute().left(px(0.)).top(px(5.)).bottom(px(5.)).w(px(2.)).rounded_full().bg(color).tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
    });
    let texty = !matches!(category(e), Category::Image | Category::Video | Category::Audio | Category::Archive | Category::Executable | Category::Other | Category::Pdf);
    let weight = texty.then(|| leaf_dev::context::estimate_tokens(e.size)).filter(|&tk| tk as f32 / WINDOW_TOKENS as f32 >= MIN_SHARE).map(|tk| {
        let share = (tk as f32 / WINDOW_TOKENS as f32).min(1.);
        let tip = format!("{} ({})", leaf_dev::context::tokens_phrase(tk), t!("share of a 200,000 token window"));
        let color = if share >= 0.25 { p.warning } else { p.text_tertiary };
        div()
            .id(("context-weight", id))
            .flex_shrink_0()
            .w(px(28.))
            .h(px(3.))
            .rounded_full()
            .bg(p.border)
            .child(div().h_full().w(relative(share)).rounded_full().bg(color))
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
    });
    RowMarks { stripe, weight, tokens: None }
}
