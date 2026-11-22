//! A Usenet server as a job uses it: where, how, how many connections, and in which order servers are asked.

use serde::{Deserialize, Serialize};

/// The order servers are asked in: every article goes to the primary servers first; one they do not have goes to the
/// backup servers, and only then to the fill servers (a block account, used for what the others miss).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    #[default]
    Primary,
    Backup,
    Fill,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::Primary, Priority::Backup, Priority::Fill];
}

pub const DEFAULT_CONNECTIONS: u32 = 8;
pub const MAX_CONNECTIONS: u32 = 50;

/// A server's password, in memory only: it is read from the keyring when a job starts and is never in a file, a log or
/// a `Debug` text (`***`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        // (not a guarantee against copies the allocator made, but the buffer that held it does not keep it)
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Server {
    /// Shown in the job's card and the log (the host name, or the user's own name for it).
    pub name: String,
    pub host: String,
    pub port: u16,
    /// NNTPS (563 by default). Off only when the user ticked it.
    pub tls: bool,
    pub user: String,
    pub password: Option<Password>,
    /// Connections this server takes (1..=50).
    pub connections: u32,
    pub priority: Priority,
    /// Days the server keeps articles (0: unknown, every article is asked for).
    pub retention_days: u32,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("connections", &self.connections)
            .field("priority", &self.priority)
            .field("retention_days", &self.retention_days)
            .finish()
    }
}

impl Server {
    pub fn default_port(tls: bool) -> u16 {
        if tls { 563 } else { 119 }
    }

    /// Whether this server can still have an article posted at `date` (Unix seconds) `now`.
    pub fn keeps(&self, date: u64, now: u64) -> bool {
        self.retention_days == 0 || date == 0 || now.saturating_sub(date) <= u64::from(self.retention_days) * 86_400
    }
}
