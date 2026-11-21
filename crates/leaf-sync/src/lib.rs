//! leaf-sync: sync and backup jobs between two places of any kind (`leaf_vfs::Fs`: a local folder, sftp, ftp...), PLAN B3.
//!
//! A job is a name, a left and a right place, a mode and filters ([`Job`], saved in `sync.toml` in the config folder; no secrets:
//! a server is named by its address, its login stays with the saved host). Running it is always two steps:
//!
//! 1. [`plan`] reads both sides and says what a run would do, row by row (copy ->, copy <-, delete, conflict, skip, with counts and
//!    bytes). Nothing is written. The rows can be changed (skip one, turn its direction) before the run.
//! 2. [`run`] does exactly the rows of the plan, nothing else. Every row is checked again just before it is done: a file that changed
//!    since the plan, a link that appeared on the way, a root that went missing make that row fail instead of doing something the
//!    plan did not show.
//!
//! Safety, in the engine (not in the app):
//! - nothing is ever deleted for good: on a local side a removed or overwritten file goes to the Trash (through the caller's
//!   `trash`), on a server, and on a local side whose Trash will not take it (the caller's `bin_takes`), it is moved by rename
//!   into `.leaf-sync-trash/<run time>/` inside the job's root ([`TRASH_DIR`]), which [`purge`] empties when the user asks;
//! - links (symbolic links, junctions) are never followed and nothing is written through one: they are listed as skipped, and so is
//!   everything that would go through them;
//! - a run is refused when a root is missing, or empty while the last run saw files there ("the drive may not be connected"),
//!   instead of mirroring the emptiness onto the other side; a plan that deletes more than [`ASK_SHARE`] of a side's files says so
//!   ([`Plan::needs_second_ask`]) and the app asks again;
//! - in two-way mode a small state file per job remembers what both sides looked like after the last run, to tell "changed on both
//!   sides" (a conflict: never resolved by itself, both files stay, it is listed) from a deletion (propagated only when the other
//!   side did not change since).
//!
//! Every call blocks (it lists and copies): run it off the UI thread.

pub mod filter;
mod plan;
mod run;
mod state;
#[cfg(test)]
mod tests;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use leaf_vfs::{Fs, FsError, VPath};
pub use plan::{Action, Counts, Info, Plan, Row, forget_state, plan};
pub use run::{Control, Progress, Report, TRASH_DIR, purge, restore_from_trash, run};

/// The name of the jobs file in the config folder.
pub const FILE_NAME: &str = "sync.toml";

/// A plan that removes more than this share of the files of one side is asked about a second time.
pub const ASK_SHARE: f64 = 0.30;

/// How far two modification times may be apart and still be the same (FAT keeps 2 seconds, FTP often whole minutes are rounded to
/// the second).
pub const TIME_SLACK_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// The right side becomes like the left: copies left to right, removes on the right what the left does not have.
    #[default]
    Mirror,
    /// Copies left to right and never removes anything on the right.
    Backup,
    /// Changes go both ways; a file changed on both sides is a conflict.
    TwoWay,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompareBy {
    /// Size and modification time (2 seconds of slack).
    #[default]
    Time,
    /// Files of the same size are read on both sides and compared by their SHA-256.
    Hash,
}

fn yes() -> bool {
    true
}

/// One saved job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Stable, made once (names the state file).
    pub id: String,
    pub name: String,
    /// Addresses (`file:///C:/data`, `sftp://host/path`), never with a password.
    pub left: String,
    pub right: String,
    #[serde(default)]
    pub mode: Mode,
    /// Globs of the files taken (all when empty): `*.jpg`, `docs/**`. A pattern without `/` is matched against the name.
    #[serde(default)]
    pub include: Vec<String>,
    /// Globs of files and folders left out on both sides (never copied, never removed).
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub skip_hidden: bool,
    /// The `.gitignore` files found on either side leave out what they ignore (and `.git` itself).
    #[serde(default)]
    pub gitignore: bool,
    #[serde(default)]
    pub compare: CompareBy,
    /// An overwritten file goes to the Trash (or `.leaf-sync-trash`) first.
    #[serde(default = "yes")]
    pub keep_overwritten: bool,
    /// Every copied file is read back and compared by hash.
    #[serde(default)]
    pub verify: bool,
    /// When it last ran (Unix ms, 0 = never) and what came out, in a short line.
    #[serde(default)]
    pub last_run_ms: i64,
    #[serde(default)]
    pub last_result: String,
}

impl Job {
    pub fn new(name: &str, left: &str, right: &str) -> Job {
        Job {
            id: new_id(),
            name: name.to_string(),
            left: strip_secret(left),
            right: strip_secret(right),
            mode: Mode::Mirror,
            include: Vec::new(),
            exclude: Vec::new(),
            skip_hidden: false,
            gitignore: false,
            compare: CompareBy::Time,
            keep_overwritten: true,
            verify: false,
            last_run_ms: 0,
            last_result: String::new(),
        }
    }

    /// The two addresses, read. A wrong one is an error in leaf's words.
    pub fn sides(&self) -> Result<(VPath, VPath), Error> {
        let parse = |which: &str, s: &str| {
            VPath::parse(s.trim()).map_err(|e| Error::new(format!("The {which} side is not a valid address"), format!("{s}: {e}")))
        };
        let (l, r) = (parse("left", &self.left)?, parse("right", &self.right)?);
        if l.scheme() == r.scheme() && l.authority() == r.authority() && (l.starts_with(&r) || r.starts_with(&l)) {
            return Err(Error::new("The two sides are the same folder, or one is inside the other", format!("{l}  {r}")));
        }
        Ok((l, r))
    }

    /// The text of the mode, as the app shows it.
    pub fn mode_text(&self) -> &'static str {
        match self.mode {
            Mode::Mirror => "Mirror",
            Mode::Backup => "Backup",
            Mode::TwoWay => "Two-way",
        }
    }
}

/// An address without a password: `sftp://user:secret@host/x` -> `sftp://user@host/x` (the jobs file never holds one).
pub fn strip_secret(addr: &str) -> String {
    let addr = addr.trim();
    let Some((scheme, rest)) = addr.split_once("://") else { return addr.to_string() };
    let (auth, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    match auth.rsplit_once('@') {
        Some((user, host)) if user.contains(':') => format!("{scheme}://{}@{host}{path}", user.split(':').next().unwrap_or("")),
        _ => addr.to_string(),
    }
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:x}{n:x}", now_ms(), std::process::id())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// The saved jobs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Jobs {
    #[serde(default, rename = "job")]
    pub jobs: Vec<Job>,
}

impl Jobs {
    /// Reads `sync.toml` from `dir`; a missing file is no jobs, a broken one is kept aside and read as none (so a save cannot
    /// destroy it).
    pub fn load(dir: &Path) -> Jobs {
        let path = dir.join(FILE_NAME);
        match rf_core::read_config(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!("sync: {} is not readable ({e}); kept aside", path.display());
                rf_core::backup_corrupt(&path);
                Jobs::default()
            }),
            Err(_) => Jobs::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, dir.join(FILE_NAME))
    }

    pub fn get(&self, id: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    /// Replaces the job with the same id, or adds it.
    pub fn put(&mut self, job: Job) {
        match self.jobs.iter_mut().find(|j| j.id == job.id) {
            Some(j) => *j = job,
            None => self.jobs.push(job),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.jobs.retain(|j| j.id != id);
    }
}

/// A failure, in leaf's words; `details` holds what the file system or the server said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub details: String,
}

impl Error {
    pub fn new(message: impl Into<String>, details: impl Into<String>) -> Self {
        Self { message: message.into(), details: details.into() }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.details.is_empty() { write!(f, "{}", self.message) } else { write!(f, "{} ({})", self.message, self.details) }
    }
}

impl std::error::Error for Error {}
