//! The queue: at most `max` downloads run at once, the others wait in the order they were started.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

struct Q {
    running: usize,
    waiting: VecDeque<u64>,
    next: u64,
}

static QUEUE: Mutex<Q> = Mutex::new(Q { running: 0, waiting: VecDeque::new(), next: 0 });
static CHANGED: Condvar = Condvar::new();
/// Closed (by the queue's schedule): nobody gets a turn until it is opened again.
static CLOSED: AtomicBool = AtomicBool::new(false);

/// Opens or closes the queue: while it is closed the downloads that wait stay waiting (the ones running are the caller's to pause).
pub fn set_open(open: bool) {
    CLOSED.store(!open, Ordering::Relaxed);
    CHANGED.notify_all();
}

pub fn is_open() -> bool {
    !CLOSED.load(Ordering::Relaxed)
}

/// A running place; given back when dropped.
pub struct Turn(());

impl Drop for Turn {
    fn drop(&mut self) {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.running = q.running.saturating_sub(1);
        CHANGED.notify_all();
    }
}

/// Blocks until it is this download's turn (`max` at a time, first come first served). `None` when `cancel` was set meanwhile.
/// `waiting` is called with the place in the line (1 = next) when it changes, or when the queue is closed or opened.
pub fn wait_turn(max: usize, cancel: &AtomicBool, mut waiting: impl FnMut(usize)) -> Option<Turn> {
    let max = max.max(1);
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let me = q.next;
    q.next += 1;
    q.waiting.push_back(me);
    let mut last = (0, true);
    loop {
        let place = q.waiting.iter().position(|w| *w == me).unwrap_or(0) + 1;
        if place == 1 && q.running < max && is_open() {
            q.waiting.pop_front();
            q.running += 1;
            CHANGED.notify_all();
            return Some(Turn(()));
        }
        if cancel.load(Ordering::Relaxed) {
            q.waiting.retain(|w| *w != me);
            CHANGED.notify_all();
            return None;
        }
        if (place, is_open()) != last {
            last = (place, is_open());
            waiting(place);
        }
        q = CHANGED.wait_timeout(q, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
    }
}
