//! Connections to one server, over all downloads: a cap so several downloads from the same host do not open dozens of connections
//! together (servers answer that with 429 or a ban).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

static CAP: AtomicUsize = AtomicUsize::new(32);
static USED: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);

/// Connections per host over all running downloads (0 = no cap). Applies to downloads that start after the change.
pub fn set_host_cap(n: usize) {
    CAP.store(if n == 0 { 0 } else { n.min(64) }, Ordering::Relaxed);
}

/// The server part of an address, in lower case (without a login).
pub(crate) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or("").to_ascii_lowercase()
}

/// Connections a download holds on its host; given back when dropped.
pub(crate) struct Lease {
    host: String,
    n: AtomicUsize,
}

impl Lease {
    /// Up to `want` connections to `host`: what is left under the cap, and one at least (a download always gets to run).
    pub fn take(host: String, want: usize) -> Lease {
        let cap = CAP.load(Ordering::Relaxed);
        let mut used = USED.lock().unwrap_or_else(|e| e.into_inner());
        let slot = used.entry(host.clone()).or_default();
        let free = if cap == 0 { want } else { cap.saturating_sub(*slot) };
        let n = want.min(free).max(1);
        *slot += n;
        Lease { host, n: AtomicUsize::new(n) }
    }

    pub fn count(&self) -> usize {
        self.n.load(Ordering::Relaxed)
    }

    /// Up to `n` more connections, as far as the cap leaves room; returns how many were added.
    pub fn grow(&self, n: usize) -> usize {
        let cap = CAP.load(Ordering::Relaxed);
        let mut used = USED.lock().unwrap_or_else(|e| e.into_inner());
        let slot = used.entry(self.host.clone()).or_default();
        let add = if cap == 0 { n } else { n.min(cap.saturating_sub(*slot)) };
        *slot += add;
        self.n.fetch_add(add, Ordering::Relaxed);
        add
    }

    /// Gives connections back down to `n` (the server asked for fewer).
    pub fn shrink_to(&self, n: usize) {
        let n = n.max(1);
        let cur = self.n.load(Ordering::Relaxed);
        if n < cur {
            self.n.store(n, Ordering::Relaxed);
            release(&self.host, cur - n);
        }
    }
}

fn release(host: &str, k: usize) {
    let mut used = USED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = used.get_mut(host) {
        *slot = slot.saturating_sub(k);
        if *slot == 0 {
            used.remove(host);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        release(&self.host, self.n.load(Ordering::Relaxed));
    }
}
