//! Time marks for the preview pane's picture, from "the focused item changed" to "the picture is in hand",
//! written only when the environment variable `LEAF_PREVIEW_TRACE` is set (a test copy; nothing is
//! read, written or timed otherwise: `on()` is one cached bool).
//!
//! The lines go to `preview-trace.log` in `LEAF_CONFIG_DIR` (the temp folder when it is not set):
//!
//! ```text
//! +   1234.5 ms [thread] text
//! ```
//!
//! The time is milliseconds since the first mark of the process. Files are named by a running number and
//! their extension and size only, never by name or path.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

fn state() -> &'static Option<(Instant, Mutex<std::fs::File>)> {
    static S: OnceLock<Option<(Instant, Mutex<std::fs::File>)>> = OnceLock::new();
    S.get_or_init(|| {
        std::env::var_os("LEAF_PREVIEW_TRACE")?;
        let dir = std::env::var_os("LEAF_CONFIG_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&dir);
        let f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("preview-trace.log")).ok()?;
        Some((Instant::now(), Mutex::new(f)))
    })
}

pub fn on() -> bool {
    state().is_some()
}

/// One line. Cheap to call when off (returns before the text is built if the caller checks `on()` first).
pub fn mark(text: &str) {
    let Some((t0, f)) = state() else { return };
    if let Ok(mut f) = f.lock() {
        let _ = writeln!(f, "+{:>10.1} ms [{}] {text}", t0.elapsed().as_secs_f64() * 1000.0, std::thread::current().name().unwrap_or("ui"));
    }
}

/// (idle, kernel + user) CPU time of the whole machine, in 100 ns units.
fn cpu_times() -> Option<(u64, u64)> {
    leaf_sys::process::cpu_times()
}

/// How busy the whole machine was between two readings of `cpu_times`, in percent.
fn busy_percent(from: Option<(u64, u64)>) -> String {
    match (from, cpu_times()) {
        (Some((i0, t0)), Some((i1, t1))) if t1 > t0 => format!("{:.0} %", 100.0 * (1.0 - (i1 - i0) as f64 / (t1 - t0) as f64)),
        _ => "unknown".to_string(),
    }
}

/// What the pane has done for the current target.
#[derive(Default)]
pub struct PaneTrace {
    seq: u32,
    selected_at: Option<Instant>,
    cpu_at_select: Option<(u64, u64)>,
    asked: bool,
    shown: bool,
}

impl PaneTrace {
    /// The focused item changed (`Preview::set_target`).
    pub fn selected(&mut self, ext: &str, size: u64) {
        if !on() {
            return;
        }
        self.seq += 1;
        self.selected_at = Some(Instant::now());
        self.cpu_at_select = cpu_times();
        self.asked = false;
        self.shown = false;
        mark(&format!("pane #{} selected: .{ext}, {} KB", self.seq, size / 1024));
    }

    /// The pane asked the cache for the picture (the first render that wanted one).
    pub fn asked(&mut self, px: u32, urgent: bool, held: bool) {
        if !on() || self.asked || self.selected_at.is_none() {
            return;
        }
        self.asked = true;
        let since = self.selected_at.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        mark(&format!("pane #{} asks for the picture at {px} px (urgent lane: {urgent}), {since:.1} ms after selecting; previous picture held meanwhile: {held}", self.seq));
    }

    /// A render had the picture (it is drawn in this frame).
    pub fn shown(&mut self) {
        if !on() || self.shown || self.selected_at.is_none() {
            return;
        }
        self.shown = true;
        let since = self.selected_at.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        mark(&format!("pane #{} PICTURE IN HAND {since:.1} ms after selecting; the machine was {} busy meanwhile", self.seq, busy_percent(self.cpu_at_select)));
    }
}

/// An answer of the thumbnail pool: every urgent one, and the list's own when it was slow.
pub fn pool_answer(urgent: bool, dropped: bool, ok: bool, waited: Duration, took: Duration) {
    if !on() {
        return;
    }
    let slow = waited > Duration::from_millis(500) || took > Duration::from_millis(250);
    if urgent || slow {
        mark(&format!(
            "pool answer: {} lane, dropped {dropped}, picture {ok}, waited {:.1} ms in the queue, made in {:.1} ms",
            if urgent { "urgent" } else { "worker" },
            waited.as_secs_f64() * 1000.0,
            took.as_secs_f64() * 1000.0
        ));
    }
}
