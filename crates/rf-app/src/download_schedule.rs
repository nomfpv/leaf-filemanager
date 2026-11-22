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

/// A download was started (the queue has work, whether or not a tick sees it while it runs).
pub fn note_started() {
    HAD_WORK.store(true, Ordering::Relaxed);
}

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
            crate::usenet::schedule_gate(open);
        }
    }
    // (empty: it is "done" when something was started and one of them finished; otherwise the mark is dropped, so a download
    // cancelled later is never taken for the end of a queue)
    if crate::downloads::live_count() + crate::usenet::live_count() == 0 {
        let started = HAD_WORK.swap(false, Ordering::Relaxed);
        if FINISHED_ONE.swap(false, Ordering::Relaxed) && started {
            done(when, cx);
        }
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

/// What the countdown does next. `state`: 0 = counting, 1 = "now" was pressed, 2 = cancelled; `own_open`: the countdown's own dialog
/// is the one on screen (any other dialog replaced it, or Esc closed it: not open); `due`: the 60 s are over.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Wait,
    Go,
    Cancel,
}

fn step(state: u8, own_open: bool, due: bool) -> Step {
    match state {
        1 => Step::Go,
        2 => Step::Cancel,
        _ if !own_open => Step::Cancel,
        _ if due => Step::Go,
        _ => Step::Wait,
    }
}

static COUNTING: AtomicBool = AtomicBool::new(false);

/// The notice: a dialog that counts down and can be cancelled (or left with Esc, which cancels too); then the PC does it. It is shown
/// when no other dialog is open (it waits its turn), and only its own dialog counts: if something else takes the place on screen, the
/// PC is left as it is.
fn countdown(action: leaf_sys::power::Action, cx: &mut App) {
    if COUNTING.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        log::warn!("downloads done: no window to ask in, the PC is left as it is");
        COUNTING.store(false, Ordering::Relaxed);
        return;
    };
    let sleep = action == leaf_sys::power::Action::Sleep;
    let state = Rc::new(Cell::new(0u8));
    cx.spawn(async move |cx| {
        let mut shown: Option<(Entity<prompt::DialogView>, Instant)> = None;
        loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let Some((own, deadline)) = shown.clone() else {
                // waits until no dialog is open in the window, then shows its own
                let r = w.update(cx, |_, window, cx| {
                    if prompt::current(window, cx).is_some() {
                        return None;
                    }
                    let deadline = Instant::now() + Duration::from_secs(NOTICE_SECS);
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
                    prompt::current(window, cx).map(|e| (e, deadline))
                });
                match r {
                    Ok(Some(d)) => shown = Some(d),
                    Ok(None) => {}
                    Err(_) => break,
                }
                continue;
            };
            let Ok(own_open) = w.update(cx, |_, window, cx| prompt::current(window, cx).is_some_and(|c| c == own)) else { break };
            match step(state.get(), own_open, Instant::now() >= deadline) {
                Step::Go => {
                    let _ = w.update(cx, |_, window, cx| {
                        if prompt::current(window, cx).is_some_and(|c| c == own) {
                            prompt::close(window, cx);
                        }
                    });
                    if let Err(e) = leaf_sys::power::run(action) {
                        log::warn!("downloads done: {e}");
                    }
                    break;
                }
                Step::Cancel => break,
                Step::Wait => {
                    let _ = cx.update(|cx| cx.refresh_windows());
                }
            }
        }
        COUNTING.store(false, Ordering::Relaxed);
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{Step, step};

    #[test]
    fn only_the_countdowns_own_dialog_counts() {
        // another dialog took its place: no shutdown, even when the time is up
        assert_eq!(step(0, false, true), Step::Cancel);
        assert_eq!(step(0, false, false), Step::Cancel);
        // its own dialog, time up: go; before: wait
        assert_eq!(step(0, true, true), Step::Go);
        assert_eq!(step(0, true, false), Step::Wait);
        assert_eq!(step(2, true, true), Step::Cancel);
    }
}
