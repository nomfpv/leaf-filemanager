//! Test driver (tools/TESTING.md): `LEAF_CHECK_SCRIPT=<file>` runs the file's lines, in order, on the UI thread once the first
//! folder is listed. Off (and free) unless the variable is set. Lines: `select a|b|c`, `run <CommandId>` (the action is dispatched
//! like a key press), `type <text>`, `key <name>` (enter, escape, tab, ...), `wait <ms>`, `wait-idle`, `goto <path|home>`, `usage <path>` (storage usage view), `bin restore`,
//! `pane left|right`, `shot <name>` (prints `LEAF_SHOT <name>` on stdout and waits for the wrapper's picture), `quit`.
//! Blank lines and lines starting with `#` are ignored; an unknown line is logged and skipped.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;

use super::Workspace;

/// Starts the script of this window if the variable is set (the first window only).
pub(super) fn start(window: &mut Window, cx: &mut Context<Workspace>) {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    let Some(path) = std::env::var_os("LEAF_CHECK_SCRIPT") else { return };
    if TAKEN.swap(true, Ordering::Relaxed) {
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            log::error!("check script {path:?}: {e}");
            return;
        }
    };
    cx.spawn_in(window, async move |this, cx| {
        let ex = cx.background_executor().clone();
        let sleep = |ms: u64| ex.timer(Duration::from_millis(ms));
        // the first listing: a view that has listed its folder
        let t0 = Instant::now();
        loop {
            sleep(200).await;
            let Ok(ready) = this.update_in(cx, |ws, _, cx| ws.tabs.get(ws.active).is_some_and(|t| t.view().read(cx).load_ms.is_some() && !t.view().read(cx).loading)) else { return };
            if ready || t0.elapsed() > Duration::from_secs(30) {
                break;
            }
        }
        sleep(500).await;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (cmd, arg) = line.split_once(' ').map_or((line, ""), |(c, a)| (c, a.trim_start()));
            log::info!("check script: {line}");
            match cmd {
                "wait" => sleep(arg.parse().unwrap_or(500)).await,
                "wait-idle" => {
                    sleep(300).await;
                    let t = Instant::now();
                    loop {
                        let Ok(busy) = this.update_in(cx, |ws, _, cx| ws.ops.read(cx).running() > 0 || ws.tabs.iter().any(|t| t.view().read(cx).loading)) else { return };
                        if !busy || t.elapsed() > Duration::from_secs(60) {
                            break;
                        }
                        sleep(150).await;
                    }
                    sleep(300).await;
                }
                "shot" => {
                    println!("LEAF_SHOT {arg}");
                    use std::io::Write as _;
                    let _ = std::io::stdout().flush();
                    // the wrapper writes <LEAF_CHECK_SHOTDIR>/<name>.png; without one, a fixed wait
                    match std::env::var_os("LEAF_CHECK_SHOTDIR") {
                        Some(dir) => {
                            let png = std::path::Path::new(&dir).join(format!("{arg}.png"));
                            let t = Instant::now();
                            while !png.exists() && t.elapsed() < Duration::from_secs(15) {
                                sleep(200).await;
                            }
                            sleep(400).await;
                        }
                        None => sleep(1500).await,
                    }
                }
                "quit" => {
                    let _ = this.update_in(cx, |_, _, cx| crate::quit_when_idle(cx));
                    return;
                }
                _ => {
                    let (cmd, arg) = (cmd.to_string(), arg.to_string());
                    let Ok(known) = this.update_in(cx, |ws, window, cx| ws.script_step(&cmd, &arg, window, cx)) else { return };
                    if !known {
                        log::warn!("check script: unknown line: {line}");
                    }
                    sleep(350).await;
                }
            }
        }
    })
    .detach();
}

impl Workspace {
    /// One line of the script that acts on the window; false when the line is not known.
    fn script_step(&mut self, cmd: &str, arg: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match cmd {
            "select" => {
                let names: Vec<String> = arg.split('|').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
                self.active_tab().clone().update(cx, |v, cx| v.select_when_listed(names, cx));
            }
            "run" => match crate::registry::by_id(arg) {
                Some(spec) => window.dispatch_action((spec.action)(), cx),
                None => return false,
            },
            "type" => {
                for c in arg.chars() {
                    let s = c.to_string();
                    let key = if c == ' ' { "space".to_string() } else { s.to_lowercase() };
                    let k = Keystroke { modifiers: if c.is_uppercase() { Modifiers::shift() } else { Modifiers::default() }, key, key_char: Some(s) };
                    window.dispatch_keystroke(k, cx);
                }
            }
            "key" => {
                // `enter`, `escape`, `tab`, `ctrl-a`, `shift-delete`
                let mut modifiers = Modifiers::default();
                let mut key = String::new();
                for part in arg.split('-').filter(|p| !p.is_empty()) {
                    match part {
                        "ctrl" => modifiers.control = true,
                        "shift" => modifiers.shift = true,
                        "alt" => modifiers.alt = true,
                        k => key = k.to_string(),
                    }
                }
                if key.is_empty() {
                    return false;
                }
                window.dispatch_keystroke(Keystroke { modifiers, key, key_char: None }, cx);
            }
            "goto" => {
                let loc = if arg.eq_ignore_ascii_case("home") { rf_core::Location::Home } else { super::parse_launch(arg).0 };
                self.navigate(loc, window, cx);
            }
            "usage" => self.navigate(rf_core::Location::DiskUsage(std::path::PathBuf::from(arg)), window, cx),
            // the Recycle Bin page: `bin restore` restores what is selected there (select with `run SelectAll`)
            "bin" if arg == "restore" => {
                if let Some(rv) = self.active_tab().read(cx).recycle_view() {
                    rv.update(cx, |rv, cx| rv.restore_selected(cx));
                }
            }
            "pane" => {
                let ix = self.active;
                if let Some(t) = self.tabs.get_mut(ix) {
                    t.right_active = arg == "right" && t.right.is_some();
                }
                self.focus_active(window, cx);
                cx.notify();
            }
            _ => return false,
        }
        true
    }
}
