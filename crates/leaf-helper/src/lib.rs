//! leaf-helper: the administrator helper (PLAN W3).
//!
//! Leaf starts it only after the user clicked "Retry as administrator" on an operation that was refused with "access denied":
//! through the normal UAC prompt on Windows (`ShellExecute` verb `runas`), through `pkexec` on Linux. Nothing is installed or
//! remembered: no service, no scheduled task, no registry entry. It lives for one leaf process and ends when leaf's end of the
//! conversation closes, when leaf's process ends, or after [`leaf_common::admin::IDLE_LIMIT`] without a request.
//!
//! What it accepts is the fixed list of typed requests in [`leaf_common::admin::Request`]; [`exec`] runs them. Delete, copy and move
//! are the helper's own walk relative to open handles ([`tree`], [`node`]), not leaf's path-based engines. Who may talk to it, and how a path is opened so that a link
//! swapped in after the check cannot redirect the operation, is per system: [`os`] (`win.rs`, `unix.rs`). Every place where the
//! check and the action are not one step says so in a comment starting with "CHECK-THEN-ACT".

use leaf_common::admin::{Refusal, Reply};

pub mod exec;
pub mod serve;
pub mod tree;

#[cfg(windows)]
pub mod node_win;
#[cfg(windows)]
pub use node_win as node;
#[cfg(unix)]
pub mod node_unix;
#[cfg(unix)]
pub use node_unix as node;

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub use win as os;

#[cfg(unix)]
pub mod unix;
#[cfg(unix)]
pub use unix as os;

/// Why an operation did not happen: refused before anything was done, or tried and failed.
#[derive(Debug)]
pub enum Fail {
    Refused(Refusal),
    Failed { denied: bool, detail: String },
}

impl Fail {
    pub fn reply(self) -> Reply {
        match self {
            Self::Refused(why) => Reply::Refused { why },
            Self::Failed { denied, detail } => Reply::Failed { denied, detail },
        }
    }
}

impl From<Refusal> for Fail {
    fn from(r: Refusal) -> Self {
        Self::Refused(r)
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Self::Failed { denied: e.kind() == std::io::ErrorKind::PermissionDenied, detail: e.to_string() }
    }
}

/// The command line leaf starts the helper with: `--channel <name> --token <64 hex> --user <SID or uid> --pid <leaf's pid>`, in
/// this order, nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The base name of the pipes (Windows) or the socket (Linux): letters, digits and `-`.
    pub channel: String,
    pub token: String,
    /// The user leaf runs as: a SID on Windows (`S-1-5-21-…`), a uid on Linux.
    pub user: String,
    pub leaf_pid: u32,
}

/// Reads [`Args`]; anything else (another order, an extra word, a malformed value) is `None` and the helper ends.
pub fn parse_args(v: &[String]) -> Option<Args> {
    let [a, channel, b, token, c, user, d, pid] = v else { return None };
    if a != "--channel" || b != "--token" || c != "--user" || d != "--pid" {
        return None;
    }
    let channel_ok = !channel.is_empty() && channel.len() <= 80 && channel.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !channel_ok || !leaf_common::admin::token_ok(token) || !user_ok(user) {
        return None;
    }
    let leaf_pid: u32 = pid.parse().ok().filter(|&p| p > 0)?;
    Some(Args { channel: channel.clone(), token: token.clone(), user: user.clone(), leaf_pid })
}

/// A SID's text (`S-1-` then digits and dashes) on Windows, a uid's digits on Linux: only these characters, so the SID can go into
/// the pipe's security descriptor text without changing what it says.
fn user_ok(u: &str) -> bool {
    if cfg!(windows) {
        u.len() <= 184 && u.starts_with("S-1-") && u[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-') && !u.ends_with('-')
    } else {
        !u.is_empty() && u.len() <= 10 && u.bytes().all(|b| b.is_ascii_digit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_helper_command_line_is_exact() {
        let user = if cfg!(windows) { "S-1-5-21-1-2-3-1001" } else { "1000" };
        let t = "ab".repeat(32);
        let ok: Vec<String> = ["--channel", "leaf-helper-1", "--token", &t, "--user", user, "--pid", "42"].iter().map(|s| s.to_string()).collect();
        assert!(parse_args(&ok).is_some());
        let mut bad = ok.clone();
        bad[5] = "S-1-5-21)(A;;GA;;;WD".into();
        assert!(parse_args(&bad).is_none(), "a SID that would change the security descriptor");
        let mut bad = ok.clone();
        bad[1] = r"..\x".into();
        assert!(parse_args(&bad).is_none());
        let mut bad = ok.clone();
        bad.push("--more".into());
        assert!(parse_args(&bad).is_none());
    }
}
