//! "Your numbers" (Settings): what is done with Leaf, counted. `bump` is called where an operation
//! ends; the tally is one small file, `usage.json` in the config folder, written at most once a
//! minute and when Leaf quits. Nothing is ever sent anywhere. Off (Settings > "Count my usage"):
//! nothing is counted and nothing is written.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

pub use rf_core::usage::Counter;
use rf_core::usage::Usage;

static ON: AtomicBool = AtomicBool::new(true);

struct State {
    usage: Usage,
    loaded: bool,
    dirty: bool,
    last_write: Instant,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { usage: Usage::default(), loaded: false, dirty: false, last_write: Instant::now() }));

fn path() -> Option<std::path::PathBuf> {
    crate::settings::app_dir().map(|d| d.join("usage.json"))
}

fn today() -> i32 {
    use chrono::Datelike;
    chrono::Local::now().date_naive().num_days_from_ce()
}

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn load(st: &mut State) {
    if !st.loaded {
        st.loaded = true;
        if let Some(text) = path().and_then(|p| rf_core::read_config(&p).ok()) {
            st.usage = Usage::from_text(&text);
        }
    }
}

fn write(st: &mut State) {
    st.dirty = false;
    st.last_write = Instant::now();
    if let Some(p) = path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, st.usage.to_text()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// Counts `n` more of `c` (nothing while the setting is off).
pub fn bump(c: Counter, n: u64) {
    if n == 0 || !enabled() {
        return;
    }
    let Ok(mut st) = STATE.lock() else { return };
    load(&mut st);
    st.usage.add(c, n, today());
    st.dirty = true;
    if st.last_write.elapsed() >= Duration::from_secs(60) {
        write(&mut st);
    }
}

/// Writes what is waiting (at quit).
pub fn flush() {
    if let Ok(mut st) = STATE.lock()
        && st.dirty
    {
        write(&mut st);
    }
}

/// The numbers as they are now, and today's day number.
pub fn snapshot() -> (Usage, i32) {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    load(&mut st);
    (st.usage.clone(), today())
}

/// Forgets everything, in memory and in the file.
pub fn reset() {
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    st.loaded = true;
    st.usage = Usage::default();
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
    st.dirty = false;
}

/// Starts the count of the time a window of Leaf was in front: every ten seconds, while one is.
pub fn start_focus_clock(cx: &mut gpui_kit::App) {
    const STEP: u64 = 10;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(STEP)).await;
            let active = cx.update(|cx| cx.active_window().is_some());
            if active {
                bump(Counter::FocusSecs, STEP);
            }
        }
    })
    .detach();
}
