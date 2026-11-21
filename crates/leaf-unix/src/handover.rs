//! Linux backend of rf_win::handover: the hand-over of a launch to the leaf that already runs, over the Unix socket of
//! `single_instance` instead of the pipe. The conversation is rf-win's (see its handover.rs for why): the second launch writes its
//! request line and waits for the first instance's words, `q` (taken), `ok` (a tab or window is open), `leaving` (on its way out)
//! or `failed`; it ends on `ok`, and becomes the first instance itself when the first one is leaving or gone. The gate is
//! rf-win's as it is (plain state, no system call).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::single_instance::{InstanceGuard, Role, connect, instance_name, read_request, serve_clients, take_lock, write_message};

struct GateState {
    leaving: bool,
    inflight: u32,
    deferred: bool,
}

/// Requests in flight against the quit.
// TYPE-COPY(rf-win handover.rs:219), with its methods: PORTABLE? (std only)
pub struct Gate(Mutex<GateState>);

impl Gate {
    pub const fn new() -> Gate {
        Gate(Mutex::new(GateState { leaving: false, inflight: 0, deferred: false }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A request arrives: true when it is taken, false when the process is leaving.
    pub fn admit(&self) -> bool {
        let mut s = self.lock();
        if s.leaving {
            return false;
        }
        s.inflight += 1;
        true
    }

    /// A taken request has been acted on. True when a refused quit has to be looked at again.
    pub fn finished(&self) -> bool {
        let mut s = self.lock();
        s.inflight = s.inflight.saturating_sub(1);
        let again = s.inflight == 0 && s.deferred;
        if again {
            s.deferred = false;
        }
        again
    }

    /// The process wants to quit. True: go ahead. False: a request is in flight.
    pub fn try_leave(&self) -> bool {
        let mut s = self.lock();
        if s.leaving {
            return true;
        }
        if s.inflight > 0 {
            s.deferred = true;
            return false;
        }
        s.leaving = true;
        true
    }

    /// The process ends whatever is in flight: requests are refused from now on.
    pub fn leave_now(&self) {
        self.lock().leaving = true;
    }

    pub fn is_leaving(&self) -> bool {
        self.lock().leaving
    }

    /// A quit that was decided is called off.
    pub fn stay(&self) {
        self.lock().leaving = false;
    }
}

impl Default for Gate {
    fn default() -> Self {
        Gate::new()
    }
}

/// This process's gate.
pub static GATE: Gate = Gate::new();

/// One step of the hand-over with its time, when `LEAF_TRACE_HANDOVER` asks for it (`1`: `leaf-handover.log` in the temp folder, or
/// the file the variable names). The steps carry no paths.
pub fn trace(step: &str) {
    static SINK: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    let Some(path) = SINK
        .get_or_init(|| {
            let v = std::env::var_os("LEAF_TRACE_HANDOVER").filter(|v| !v.is_empty())?;
            Some(if v == "1" { std::env::temp_dir().join("leaf-handover.log") } else { v.into() })
        })
        .as_ref()
    else {
        return;
    };
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let day = ms % 86_400_000;
    let line = format!(
        "{:02}:{:02}:{:02}.{:03} pid {:>6} age {:>6} ms  {step}\n",
        day / 3_600_000,
        day / 60_000 % 60,
        day / 1000 % 60,
        day % 1000,
        std::process::id(),
        crate::process_age_ms()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// A taken request. Whoever acts on it calls [`done`](Ack::done); one that is dropped instead closes the socket without the last word
/// (the second launch then shows the folder itself).
// TYPE-COPY(rf-win handover.rs:336): the socket in place of the pipe
pub struct Ack {
    stream: Option<UnixStream>,
    counted: bool,
    gate: &'static Gate,
}

impl Ack {
    /// The request has been acted on: tells the second launch, which then ends. True when a quit that was refused because of this
    /// request has to be looked at again.
    pub fn done(mut self) -> bool {
        trace("first: request acted on, telling the second launch");
        self.say("ok\n");
        self.release()
    }

    /// The request could not be shown: the second launch shows the folder itself. Same return as [`done`](Ack::done).
    pub fn refuse(mut self) -> bool {
        trace("first: request could not be shown, telling the second launch");
        self.say("failed\n");
        self.release()
    }

    fn say(&mut self, text: &str) {
        if let Some(s) = self.stream.as_mut() {
            let _ = s.write_all(text.as_bytes());
        }
    }

    fn release(&mut self) -> bool {
        self.stream = None;
        std::mem::take(&mut self.counted) && self.gate.finished()
    }
}

impl Drop for Ack {
    fn drop(&mut self) {
        if self.counted {
            trace("first: request dropped without being acted on");
            self.release();
        }
    }
}

/// Serves the socket of this instance group on a background thread. `on_message` gets each request and its [`Ack`]. Requests are
/// refused with `leaving` once [`GATE`] says the process is leaving.
pub fn serve(on_message: impl Fn(String, Ack) + Send + Sync + 'static) {
    serve_with(&instance_name(), &GATE, on_message)
}

/// [`serve`] for a named group with a gate of its own (the tests use their own).
pub fn serve_with(app: &str, gate: &'static Gate, on_message: impl Fn(String, Ack) + Send + Sync + 'static) {
    let cb = Arc::new(on_message);
    serve_clients(
        app,
        "leaf-handover-socket",
        Arc::new(move |c: UnixStream| {
            let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
            let Ok(mut out) = c.try_clone() else { return };
            let Some(line) = read_request(&mut BufReader::new(c)) else { return };
            trace("first: request read from the socket");
            if !gate.admit() {
                trace("first: leaving, refusing the request");
                let _ = out.write_all(b"leaving\n");
                return;
            }
            let _ = out.write_all(b"q\n");
            cb(line, Ack { stream: Some(out), counted: true, gate });
        }),
    );
}

/// The time limits of a hand-over (rf-win's, handover.rs `LIMITS`).
const CONNECT: Duration = Duration::from_millis(3_000);
const STALL: Duration = Duration::from_millis(30_000);
const EXIT_WAIT: Duration = Duration::from_millis(6_000);

/// What one try to hand a request over came to.
#[derive(Debug, PartialEq, Eq)]
enum Sent {
    Handed,
    /// It is leaving, did not say a word before closing, or could not show the request (rf-win: Leaving / Dropped / Refused).
    Leaving,
    Refused,
    /// It took the request and did not act on it within the stall time: it still holds it and will show it.
    Stalled,
    /// No socket to talk to (not up yet, gone, stale).
    NoSocket,
}

/// Writes `message` and waits for the first instance's last word (rf-win handover.rs `request` + `judge`).
fn request(app: &str, message: &str, stall: Duration) -> Sent {
    let Some(mut s) = connect(app) else { return Sent::NoSocket };
    if !write_message(&mut s, message) {
        return Sent::Leaving;
    }
    let begun = Instant::now();
    // wake up to look at the clock, so that a silent first instance is given up on at the stall time
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let mut r = BufReader::new(s);
    let mut line = String::new();
    loop {
        match r.read_line(&mut line) {
            // closed without the last word
            Ok(0) => return Sent::Leaving,
            Ok(_) => {
                match line.trim() {
                    "ok" => return Sent::Handed,
                    "leaving" => return Sent::Leaving,
                    "failed" => return Sent::Refused,
                    // "q" (taken) and anything unknown: keep waiting
                    _ => {}
                }
                line.clear();
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return Sent::Leaving,
        }
        if begun.elapsed() >= stall {
            return Sent::Stalled;
        }
    }
}

/// Decides the role of this launch and, when a first instance runs, hands it `message` and waits until its window or tab is there
/// ("" = a new tab or window at Home). [`Role::Forwarded`]: the first instance has it. [`Role::Primary`]: this process shows the
/// folder itself, because nobody ran, or the one that ran was leaving or did not answer.
pub fn acquire(message: &str) -> Role {
    acquire_in(&instance_name(), message, STALL)
}

fn acquire_in(app: &str, message: &str, stall: Duration) -> Role {
    let begun = Instant::now();
    let mut no_socket_since: Option<Instant> = None;
    let mut leaving_since: Option<Instant> = None;
    trace("second: launch, looking for a first instance");
    loop {
        match take_lock(app) {
            Ok(Some(f)) => {
                trace(&format!("second: no first instance, this process is it (after {} ms)", begun.elapsed().as_millis()));
                return Role::Primary(InstanceGuard::locked(f));
            }
            Err(()) => return Role::Primary(InstanceGuard::inert()),
            Ok(None) => {}
        }
        match request(app, message, stall) {
            Sent::Handed => {
                trace(&format!("second: first instance acted on it after {} ms, this process ends", begun.elapsed().as_millis()));
                return Role::Forwarded;
            }
            // it holds the request and is slow: it is the only one that has it, a second window would be a double
            Sent::Stalled => return Role::Forwarded,
            Sent::NoSocket => {
                // (it may be starting and has no socket yet, or ending and has none any more: the lock is looked at again)
                if no_socket_since.get_or_insert_with(Instant::now).elapsed() < CONNECT {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                trace("second: the first instance has no socket, showing the folder beside it");
                return Role::Primary(InstanceGuard::inert());
            }
            Sent::Refused => return Role::Primary(InstanceGuard::inert()),
            Sent::Leaving => {
                // Ask again until it is gone: the lock is free then and this process is the first instance (or the one that won it
                // is up, and gets the request).
                if leaving_since.get_or_insert_with(Instant::now).elapsed() < EXIT_WAIT {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                trace("second: the first instance is still there, showing the folder beside it");
                return Role::Primary(InstanceGuard::inert());
            }
        }
    }
}

/// A second message to the running instance after [`acquire`] returned [`Role::Forwarded`] (the file to select in the folder just
/// handed over). False when the first instance did not take it.
pub fn send_followup(message: &str) -> bool {
    let app = instance_name();
    let begun = Instant::now();
    loop {
        match request(&app, message, STALL) {
            Sent::Handed | Sent::Stalled => return true,
            Sent::NoSocket if begun.elapsed() < Duration::from_millis(1_000) => std::thread::sleep(Duration::from_millis(20)),
            other => {
                trace(&format!("second: the follow-up was not taken ({other:?})"));
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_lets_the_quit_go_when_nothing_is_in_flight() {
        let g = super::Gate::new();
        assert!(g.admit());
        assert!(!g.try_leave(), "a request in flight holds the quit back");
        assert!(g.finished(), "the last one says to look at the quit again");
        assert!(g.try_leave());
        assert!(!g.admit(), "leaving: requests are refused");
        assert!(super::GATE.try_leave());
    }

    #[test]
    fn a_second_launch_waits_for_the_ok_and_a_leaving_first_is_told() {
        crate::single_instance::test_runtime_dir();
        static GATE_A: Gate = Gate::new();
        let app = "leaf-ho-test";
        let Role::Primary(_guard) = acquire_in(app, "one", Duration::from_secs(5)) else { panic!("nobody runs") };
        // Each acted-on request says so on this channel once its Ack is released (the gate counted it off). The second launch hears
        // "ok"/"failed" a moment before that release, so the quit below waits for these and not for a sleep.
        let (released_tx, released) = std::sync::mpsc::channel::<()>();
        serve_with(app, &GATE_A, move |m, ack| {
            let released_tx = released_tx.clone();
            std::thread::spawn(move || {
                // (a first instance that takes a while to open the window: the second launch must wait for its word)
                std::thread::sleep(Duration::from_millis(100));
                if m == "refuse" {
                    ack.refuse();
                } else {
                    ack.done();
                }
                let _ = released_tx.send(());
            });
        });
        assert!(matches!(acquire_in(app, "/tmp/x", Duration::from_secs(30)), Role::Forwarded));
        assert!(matches!(acquire_in(app, "refuse", Duration::from_secs(30)), Role::Primary(_)));
        for _ in 0..2 {
            released.recv_timeout(Duration::from_secs(30)).expect("both requests are released");
        }
        assert!(GATE_A.try_leave());
        assert_eq!(request(app, "late", Duration::from_secs(5)), Sent::Leaving);
    }
}
