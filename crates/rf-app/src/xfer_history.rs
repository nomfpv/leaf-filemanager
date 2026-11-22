//! The history of finished transfers (Home > "Transfers" and Settings > "Your numbers"): what, from, to, size, time, average speed,
//! how it ended. Written by the Status Center when a copy, move, download or upload ends (`status_center::set_transfer` names the
//! ones that count), kept in `transfers.json` in the config folder, the last 500. Paths and numbers only, never contents, and
//! never sent anywhere. Off (Settings > "Keep a history of transfers"): nothing is recorded; what is there stays until cleared.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

pub use rf_core::xfer_log::{Kind, Outcome, Record, Via};
use rf_core::xfer_log::{self, Log};

static ON: AtomicBool = AtomicBool::new(true);
static LOG: LazyLock<Mutex<Option<Log>>> = LazyLock::new(|| Mutex::new(None));

/// What a Status Center job says about itself so that its end is recorded.
#[derive(Clone)]
pub struct Note {
    pub kind: Kind,
    pub what: String,
    pub from: String,
    pub to: String,
    /// A download of the download manager: how it came.
    pub via: Option<Via>,
    /// The files it made, asked when it ends (a download's saved file; `to` is only the folder it was asked to go into).
    pub saved: Option<Saved>,
}

/// What a job calls its result files with once it is over.
pub type Saved = std::sync::Arc<dyn Fn() -> Vec<std::path::PathBuf> + Send + Sync>;

impl Note {
    pub fn new(kind: Kind, what: String, from: String, to: String) -> Self {
        Self { kind, what, from, to, via: None, saved: None }
    }
}

fn path() -> Option<std::path::PathBuf> {
    crate::settings::app_dir().map(|d| d.join("transfers.json"))
}

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn with<T>(f: impl FnOnce(&mut Log) -> T) -> T {
    let mut g = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let log = g.get_or_insert_with(|| path().map(|p| xfer_log::load(&p)).unwrap_or_default());
    f(log)
}

/// Records a finished transfer (nothing while the setting is off).
pub fn record(n: Note, bytes: u64, millis: u64, result: Outcome) {
    record_with(n, bytes, millis, result, None);
}

/// [`record`] with the words a failure ended with.
pub fn record_with(n: Note, bytes: u64, millis: u64, result: Outcome, reason: Option<String>) {
    if !enabled() {
        return;
    }
    let at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    // (a download: the file it made, apart from the folder it was asked to go into)
    let file = n.saved.as_ref().and_then(|f| f().into_iter().next()).map(|p| p.to_string_lossy().into_owned());
    with(|log| {
        log.push(Record { kind: n.kind, what: n.what, from: n.from, to: n.to, bytes, millis, result, at_ms, via: n.via, file, reason: reason.map(|r| r.chars().take(400).collect()) });
        if let Some(p) = path() {
            let _ = xfer_log::save(&p, log);
        }
    });
}

/// The records, newest first.
pub fn recent() -> Vec<Record> {
    with(|log| log.records.iter().rev().cloned().collect())
}

/// Forgets the records `gone` says (the download page's "Remove from the list" and "Clear finished"): the entries only, never a file.
pub fn forget(gone: impl Fn(&Record) -> bool) {
    with(|log| log.records.retain(|r| !gone(r)));
    with(|log| {
        if let Some(p) = path() {
            let _ = xfer_log::save(&p, log);
        }
    });
}

/// Forgets every record, in memory and in the file.
pub fn clear() {
    with(|log| log.records.clear());
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
}

/// A web address without what could be a secret: no sign-in, no query, no fragment.
pub fn plain_url(url: &str) -> String {
    let cut = url.split(['?', '#']).next().unwrap_or(url);
    match cut.split_once("://") {
        Some((scheme, rest)) => format!("{scheme}://{}", rest.rsplit_once('@').map_or(rest, |(_, h)| h)),
        None => cut.to_string(),
    }
}
