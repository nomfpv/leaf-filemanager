//! The leaf side of `leaf-mcp` (docs/mcp.md): two requests that come over the hand-over channel and are answered with lines of data.
//!
//! - `show` + a path: the file's folder opens with the file selected (the same as `leaf reveal <file>`).
//! - `pick` + a prompt: leaf asks the user with the open dialog (several files can be chosen, the prompt is its title) and answers
//!   the chosen paths, none when the dialog is cancelled.
//!
//! Neither changes, moves or deletes anything. The requests are not shown to the user as launches: a launch message never starts
//! with [`PREFIX`].

use gpui_kit::App;

use crate::t;

pub const PREFIX: &str = "\u{1}mcp:";

/// Acts on a request (the text after [`PREFIX`]) and answers it.
pub fn handle(req: String, ack: leaf_sys::handover::Ack, cx: &mut App) {
    #[allow(unused_mut)]
    let mut ack = ack;
    if let Some(path) = req.strip_prefix("show:") {
        let path = std::path::PathBuf::from(path.trim());
        let shown = path.is_absolute() && path.exists() && crate::workspace::route_ipc_shown(format!("{}{}", crate::workspace::SELECT_PREFIX, path.display()), cx);
        if shown {
            cx.activate(true);
        }
        let held = if shown { ack.done() } else { ack.refuse() };
        if held {
            crate::last_window::settle(cx);
        }
    } else if let Some(prompt) = req.strip_prefix("pick:") {
        let title = if prompt.trim().is_empty() { t!("Choose files").to_string() } else { prompt.trim().chars().take(200).collect() };
        let (tx, rx) = futures::channel::oneshot::channel();
        leaf_sys::sta::run(move |owner| {
            let _ = tx.send(leaf_sys::dialogs::pick_many(owner, &title));
        });
        cx.spawn(async move |cx| {
            let chosen = rx.await.unwrap_or_default();
            for p in &chosen {
                ack.reply(&p.to_string_lossy());
            }
            if ack.done() {
                cx.update(|cx| crate::last_window::settle(cx));
            }
        })
        .detach();
    } else {
        ack.refuse();
    }
}
