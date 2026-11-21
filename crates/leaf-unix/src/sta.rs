//! Linux backend of rf_win::sta. Status: REAL
//!
//! The "shell thread": one long-lived worker thread that runs the jobs handed to it, one after another, in the order given. On Windows
//! it is a single-threaded COM apartment with a hidden owner window; here there is no apartment and no window, and the job gets an inert
//! [`Owner`]. The thread and the order are kept, so work the app sends here (clipboard, paste, menus) is never dropped or run twice at
//! once.

use std::sync::OnceLock;
use std::sync::mpsc::Sender;

use crate::Owner;

type Job = Box<dyn FnOnce(Owner) + Send>;

thread_local! {
    /// Set on the shell thread itself, so a [`call`] made from a job running there runs in place instead of waiting for itself.
    static ON_SHELL_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static THREAD: OnceLock<Sender<Job>> = OnceLock::new();

fn start() -> Sender<Job> {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("rf-shell-sta".into())
        .spawn(move || {
            ON_SHELL_THREAD.with(|c| c.set(true));
            while let Ok(job) = rx.recv() {
                // (a job that panics must not take the thread, and every later job, with it)
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(Owner))).is_err() {
                    log::warn!("shell thread: a job panicked");
                }
            }
        })
        .expect("spawn shell thread");
    tx
}

/// Runs `f` on the shell thread. `f` receives the owner (inert on Linux).
pub fn run(f: impl FnOnce(Owner) + Send + 'static) {
    let tx = THREAD.get_or_init(start);
    let _ = tx.send(Box::new(f));
}

/// Runs `f` on the shell thread and waits for its answer, at most `limit`. `None` when the thread did not get to it or did not
/// finish in that time (the job still runs to its end there; its answer is dropped).
pub fn call<R: Send + 'static>(limit: std::time::Duration, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    if ON_SHELL_THREAD.with(|c| c.get()) {
        return Some(f());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    run(move |_| {
        let _ = tx.send(f());
    });
    rx.recv_timeout(limit).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn a_call_gives_its_answer_and_a_call_from_inside_a_call_does_not_wait_for_itself() {
        assert_eq!(super::call(Duration::from_secs(10), || 6 * 7), Some(42));
        let nested = super::call(Duration::from_secs(10), || super::call(Duration::from_secs(10), || "inner"));
        assert_eq!(nested, Some(Some("inner")));
    }

    #[test]
    fn a_call_that_takes_too_long_gives_none_and_the_thread_lives_on() {
        let slow = super::call(Duration::from_millis(30), || std::thread::sleep(Duration::from_millis(300)));
        assert_eq!(slow, None);
        assert_eq!(super::call(Duration::from_secs(10), || 1), Some(1));
    }

    #[test]
    fn jobs_run_in_order_on_one_thread_and_a_panic_does_not_end_it() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..50 {
            let s = seen.clone();
            super::run(move |_| s.lock().unwrap().push((i, std::thread::current().id())));
        }
        super::run(|_| panic!("a job that fails"));
        assert_eq!(super::call(Duration::from_secs(10), || 7), Some(7), "the thread lives on");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), (0..50).collect::<Vec<_>>());
        assert!(seen.iter().all(|s| s.1 == seen[0].1));
        assert_ne!(seen[0].1, std::thread::current().id());
    }
}
