//! The download queue's schedule and what leaf does when the queue is done: a window of time (start, stop, weekdays) outside of which
//! the queue does not run (waiting downloads wait, running ones are paused and carry on when the window opens again), and then
//! nothing / close leaf / sleep / shut down. Sleep and shut down give a 60 s notice that can be cancelled.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::prompt::{self, Button, Kind};
use crate::settings::{Settings, WhenDone};
use crate::t;

/// Seconds of notice before the PC sleeps or shuts down.
const NOTICE_SECS: u64 = 60;

/// Whether `minute` (after midnight) of weekday `weekday` (0 = Monday) is inside the window that starts at `start` on the days in
/// the bit mask `days` and stops at `stop` (a stop before the start is after midnight, on the next day; equal: the whole day).
pub fn in_window(start: u16, stop: u16, days: u8, weekday: u32, minute: u16) -> bool {
    let on = |d: u32| days >> (d % 7) & 1 == 1;
    if start == stop {
        on(weekday)
    } else if start < stop {
        on(weekday) && minute >= start && minute < stop
    } else {
        (on(weekday) && minute >= start) || (on(weekday + 6) && minute < stop)
    }
}

fn open_now(s: &Settings) -> bool {
    if !s.download_schedule {
        return true;
    }
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    in_window(s.download_schedule_start, s.download_schedule_stop, s.download_schedule_days, now.weekday().num_days_from_monday(), (now.hour() * 60 + now.minute()) as u16)
}

static LAST_OPEN: Mutex<Option<bool>> = Mutex::new(None);
static HAD_WORK: AtomicBool = AtomicBool::new(false);
static FINISHED_ONE: AtomicBool = AtomicBool::new(false);

/// A download finished: the queue running dry afterwards counts as "done" (cancelling everything does not).
pub fn note_finished() {
    FINISHED_ONE.store(true, Ordering::Relaxed);
}

/// Looks at the schedule and the queue every 5 seconds, for as long as leaf runs.
pub fn start(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let _ = cx.update(tick);
            cx.background_executor().timer(Duration::from_secs(5)).await;
        }
    })
    .detach();
}

fn tick(cx: &mut App) {
    let s = cx.global::<Settings>();
    let (open, when) = (open_now(s), s.download_when_done);
    {
        let mut last = LAST_OPEN.lock().unwrap_or_else(|e| e.into_inner());
        if *last != Some(open) {
            *last = Some(open);
            crate::downloads::schedule_gate(open);
        }
    }
    if crate::downloads::live_count() > 0 {
        HAD_WORK.store(true, Ordering::Relaxed);
    } else if HAD_WORK.swap(false, Ordering::Relaxed) && FINISHED_ONE.swap(false, Ordering::Relaxed) {
        done(when, cx);
    }
}

fn done(when: WhenDone, cx: &mut App) {
    match when {
        WhenDone::Nothing => {}
        WhenDone::Close => cx.quit(),
        WhenDone::Sleep => countdown(leaf_sys::power::Action::Sleep, cx),
        WhenDone::Shutdown => countdown(leaf_sys::power::Action::Shutdown, cx),
    }
}

/// Which actions this system can do.
pub fn available(when: WhenDone) -> bool {
    match when {
        WhenDone::Sleep => leaf_sys::power::available(leaf_sys::power::Action::Sleep),
        WhenDone::Shutdown => leaf_sys::power::available(leaf_sys::power::Action::Shutdown),
        _ => true,
    }
}

/// The notice: a dialog that counts down and can be cancelled (or left with Esc, which cancels too); then the PC does it.
fn countdown(action: leaf_sys::power::Action, cx: &mut App) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        log::warn!("downloads done: no window to ask in, the PC is left as it is");
        return;
    };
    let sleep = action == leaf_sys::power::Action::Sleep;
    let deadline = Instant::now() + Duration::from_secs(NOTICE_SECS);
    // 0 = counting, 1 = now, 2 = cancelled
    let state = Rc::new(Cell::new(0u8));
    let shown = w.update(cx, {
        let state = state.clone();
        move |_, window, cx| {
            let live: Rc<dyn Fn() -> String> = Rc::new(move || {
                let left = deadline.saturating_duration_since(Instant::now()).as_secs() + 1;
                format!("{} {left} {}", if sleep { t!("The downloads are done. This PC goes to sleep in") } else { t!("The downloads are done. This PC shuts down in") }, t!("seconds."))
            });
            let (now, stop) = (state.clone(), state.clone());
            let buttons = vec![
                Button::new(if sleep { t!("Sleep now") } else { t!("Shut down now") }, Kind::Primary, move |_, _, _| {
                    now.set(1);
                    Ok(())
                }),
                Button::new(t!("Cancel"), Kind::Normal, move |_, _, _| {
                    stop.set(2);
                    Ok(())
                }),
            ];
            prompt::show_live(if sleep { t!("Sleep") } else { t!("Shut down") }, live, Vec::new(), buttons, window, cx);
        }
    });
    if shown.is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let open = w.update(cx, |_, window, cx| prompt::current(window, cx).is_some()).unwrap_or(false);
            let go = state.get() == 1 || (state.get() == 0 && open && Instant::now() >= deadline);
            if go {
                let _ = w.update(cx, |_, window, cx| prompt::close(window, cx));
                if let Err(e) = leaf_sys::power::run(action) {
                    log::warn!("downloads done: {e}");
                }
                return;
            }
            // cancelled with the button, or the dialog went away some other way (Esc)
            if state.get() == 2 || !open {
                return;
            }
            let _ = cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}
