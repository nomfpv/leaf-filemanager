//! Linux backend of rf_win::handover. Status: STUB
//!
//! No pipe between launches yet: [`serve`] never calls back, [`acquire`] says this process is the first, a follow-up is never
//! delivered. The gate is rf-win's as it is (plain state, no system call): the quit asks it, and an inert gate that said "do not
//! leave" would keep the app from ever quitting.

use std::sync::Mutex;

use leaf_common::stub;

use crate::single_instance::{InstanceGuard, Role};

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

pub fn trace(_step: &str) {
    stub!("handover::trace")
}

/// A taken request. None is ever made here ([`serve`] takes none).
// TYPE-COPY(rf-win handover.rs:336): the pipe and the gate dropped
pub struct Ack {
    _p: (),
}

impl Ack {
    pub fn done(self) -> bool {
        stub!("handover::Ack::done")
    }

    pub fn refuse(self) -> bool {
        stub!("handover::Ack::refuse")
    }
}

/// Never calls `on_message` (serve style): no second launch reaches this process yet.
pub fn serve(_on_message: impl Fn(String, Ack) + Send + Sync + 'static) {
    stub!("handover::serve")
}

/// Always the first instance: "forwarded" would make the app quit at start, with nobody to forward to.
pub fn acquire(_message: &str) -> Role {
    // STUB(linux): Role has no Default
    stub::hit("handover::acquire");
    Role::Primary(InstanceGuard::inert())
}

pub fn send_followup(_message: &str) -> bool {
    stub!("handover::send_followup")
}

#[cfg(test)]
mod tests {
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
}
