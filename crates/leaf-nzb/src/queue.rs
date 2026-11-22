//! The Usenet queue: one job downloads at a time (all connections on it, as Usenet clients do); the others wait by priority,
//! then in their order, which the user can change. "Pause all" holds every job.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JobPriority {
    High,
    #[default]
    Normal,
    Low,
}

struct Entry {
    id: u64,
    priority: JobPriority,
}

struct Q {
    running: Option<u64>,
    /// In queue order (the user's order within a priority).
    waiting: Vec<Entry>,
}

static QUEUE: Mutex<Q> = Mutex::new(Q { running: None, waiting: Vec::new() });
static CHANGED: Condvar = Condvar::new();
static PAUSED_ALL: AtomicBool = AtomicBool::new(false);

fn q() -> std::sync::MutexGuard<'static, Q> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The order the waiting jobs get their turn in.
fn next(q: &Q) -> Option<u64> {
    q.waiting.iter().min_by_key(|e| e.priority).map(|e| e.id)
}

pub fn pause_all(on: bool) {
    PAUSED_ALL.store(on, Ordering::Relaxed);
    CHANGED.notify_all();
}

pub fn all_paused() -> bool {
    PAUSED_ALL.load(Ordering::Relaxed)
}

/// Adds a job to the line (before it waits for its turn).
pub fn add(id: u64, priority: JobPriority) {
    let mut q = q();
    if !q.waiting.iter().any(|e| e.id == id) && q.running != Some(id) {
        q.waiting.push(Entry { id, priority });
    }
    CHANGED.notify_all();
}

pub fn set_priority(id: u64, priority: JobPriority) {
    if let Some(e) = q().waiting.iter_mut().find(|e| e.id == id) {
        e.priority = priority;
    }
    CHANGED.notify_all();
}

/// Moves a waiting job one place earlier (`up`) or later in the line.
pub fn move_by(id: u64, up: bool) {
    let mut q = q();
    if let Some(i) = q.waiting.iter().position(|e| e.id == id) {
        let j = if up { i.checked_sub(1) } else { (i + 1 < q.waiting.len()).then_some(i + 1) };
        if let Some(j) = j {
            q.waiting.swap(i, j);
        }
    }
    CHANGED.notify_all();
}

/// The place in the line (1 = next), `None` when running or not in it.
pub fn place(id: u64) -> Option<usize> {
    let q = q();
    let mut order: Vec<&Entry> = q.waiting.iter().collect();
    order.sort_by_key(|e| e.priority);
    order.iter().position(|e| e.id == id).map(|p| p + 1)
}

pub struct Turn(u64);

impl Drop for Turn {
    fn drop(&mut self) {
        let mut q = q();
        if q.running == Some(self.0) {
            q.running = None;
        }
        CHANGED.notify_all();
    }
}

/// Blocks until it is job `id`'s turn (`open`: the download schedule lets jobs start). `None` when `cancel` was set.
pub fn wait_turn(id: u64, cancel: &AtomicBool, open: &dyn Fn() -> bool, mut waiting: impl FnMut(Option<usize>)) -> Option<Turn> {
    let mut q = q();
    let mut last = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            q.waiting.retain(|e| e.id != id);
            CHANGED.notify_all();
            return None;
        }
        if q.running.is_none() && next(&q) == Some(id) && open() && !all_paused() {
            q.waiting.retain(|e| e.id != id);
            q.running = Some(id);
            return Some(Turn(id));
        }
        let mut order: Vec<&Entry> = q.waiting.iter().collect();
        order.sort_by_key(|e| e.priority);
        let place = if all_paused() || !open() { None } else { order.iter().position(|e| e.id == id).map(|p| p + 1) };
        if last != Some(place) {
            last = Some(place);
            waiting(place);
        }
        q = CHANGED.wait_timeout(q, Duration::from_millis(300)).unwrap_or_else(|e| e.into_inner()).0;
    }
}
