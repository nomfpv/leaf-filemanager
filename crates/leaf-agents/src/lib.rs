//! leaf-agents: what coding agents are doing on this computer (PLAN F2), read from the agents' own data folders.
//!
//! One [`Adapter`] per agent. It lists the agent's sessions ([`Session`]: id, working folder, git branch, state, last activity, a
//! title) from files the agent keeps for itself. Those formats are not documented and change: every line or file that does not parse
//! is skipped without a word, and nothing here ever writes to the agent's folders.
//!
//! The state of a session is a best guess from the file's last records and age, plus the agent programs running right now
//! ([`procs`]) matched by working folder, where the system tells it.
//!
//! Privacy: the title (the first prompt line) only travels to the Agents page; nothing in this crate logs prompt text.
//! Every call blocks (it reads files and lists processes): run it off the UI thread.

pub mod claude;
pub mod procs;
mod watch;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use watch::Watch;

/// How far a session is in its work, as far as the files say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    /// The agent is working now.
    Running,
    /// The agent stopped for the user: its turn is done, or it waits for an answer to a question or a permission.
    Waiting,
    /// A session that is open (its program runs) but quiet for a while.
    Idle,
    /// No program runs it any more.
    Ended,
}

impl State {
    /// The agent program is alive (running, waiting or idle).
    pub fn is_live(self) -> bool {
        self != State::Ended
    }
    /// Shown on the presence dot and in the status line: working or waiting for the user.
    pub fn is_active(self) -> bool {
        matches!(self, State::Running | State::Waiting)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Which agent ([`Adapter::id`]).
    pub agent: &'static str,
    pub id: String,
    /// The folder the session works in.
    pub folder: PathBuf,
    /// The git branch it was on at its last record.
    pub branch: Option<String>,
    /// When `folder` is a linked git worktree: the repository it belongs to.
    pub repo: Option<PathBuf>,
    pub state: State,
    /// Milliseconds since the Unix epoch of the session file's last change.
    pub last_activity_ms: i64,
    /// The agent's own title for the session, else the first prompt line, cut to [`TITLE_MAX`] characters. May be empty.
    pub title: String,
}

impl Session {
    /// The place sessions are grouped under: the repository when `folder` is one of its worktrees, else the folder.
    pub fn group(&self) -> &Path {
        self.repo.as_deref().unwrap_or(&self.folder)
    }
}

pub const TITLE_MAX: usize = 80;

/// One agent program's way to list its sessions.
pub trait Adapter: Send + Sync {
    /// Short stable name ("claude").
    fn id(&self) -> &'static str;
    /// Folders to watch for changes (they may not exist).
    fn watch_roots(&self) -> Vec<PathBuf>;
    /// Whether a running program (its file name without extension, lower case, and its first arguments) is this agent.
    fn is_agent_process(&self, name: &str, args: &[String]) -> bool;
    /// The sessions touched within `max_age`, newest first, at most `limit`. `live` says which folders have a running agent program.
    fn sessions(&self, live: &procs::Live, max_age: std::time::Duration, limit: usize) -> Vec<Session>;
}

/// The adapters this build has.
pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(claude::Claude::new())]
}

/// Everything now: the running programs, then every adapter's sessions (newest first).
pub fn snapshot(adapters: &[Box<dyn Adapter>], max_age: std::time::Duration, limit: usize) -> Vec<Session> {
    let live = procs::scan(adapters);
    let mut all: Vec<Session> = adapters.iter().flat_map(|a| a.sessions(&live, max_age, limit)).collect();
    all.sort_by(|a, b| b.last_activity_ms.cmp(&a.last_activity_ms));
    all.truncate(limit);
    all
}

/// Cuts a prompt to its first non-empty line, at most [`TITLE_MAX`] characters ("..." marks a cut).
pub fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out: String = line.chars().take(TITLE_MAX).collect();
    if line.chars().count() > TITLE_MAX {
        out.pop();
        out.push('\u{2026}');
    }
    out
}

pub(crate) fn ms_of(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A folder as a comparison key: no trailing separator, forward slashes, lower case on Windows.
pub fn folder_key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let s = s.trim_end_matches('/');
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// If `folder` is a linked git worktree (its `.git` is a file naming `<repo>/.git/worktrees/<name>`), the repository's folder.
pub fn worktree_repo(folder: &Path) -> Option<PathBuf> {
    let git = folder.join(".git");
    if !git.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&git).ok()?;
    let dir = PathBuf::from(text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim());
    let dir = if dir.is_absolute() { dir } else { folder.join(dir) };
    // <repo>/.git/worktrees/<name>
    let common = dir.parent()?;
    if common.file_name()? != "worktrees" {
        return None;
    }
    let dot_git = common.parent()?;
    dot_git.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_the_first_line_cut_to_eighty() {
        assert_eq!(title_of("\n  fix the thing\nmore"), "fix the thing");
        let long = "x".repeat(200);
        let t = title_of(&long);
        assert_eq!(t.chars().count(), TITLE_MAX);
        assert!(t.ends_with('\u{2026}'));
        assert_eq!(title_of(""), "");
    }
}
