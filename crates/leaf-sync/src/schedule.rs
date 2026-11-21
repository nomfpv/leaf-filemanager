//! When a job starts by itself: manually, every N hours, daily at a time, or when leaf starts. leaf does this only while it runs
//! (there is no system task); a time that passed while leaf was closed is made up once, at the next start.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    #[default]
    Manual,
    /// Every `hours` hours after the last run.
    Hours,
    /// Every day at `minute` (minutes after local midnight).
    Daily,
    /// Once each time leaf starts.
    Start,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default)]
    pub kind: RunKind,
    #[serde(default = "one")]
    pub hours: u32,
    #[serde(default = "night")]
    pub minute: u32,
}

fn one() -> u32 {
    1
}

fn night() -> u32 {
    3 * 60
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule { kind: RunKind::Manual, hours: one(), minute: night() }
    }
}

const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

impl Schedule {
    /// Is a run due? `last_ms`: when the job last ran or was last tried (0 = never: a job that never ran is not started by itself,
    /// the first run is made by hand). `offset_s`: local time minus UTC. `at_start`: leaf has just started.
    pub fn due(&self, last_ms: i64, now_ms: i64, offset_s: i64, at_start: bool) -> bool {
        match self.kind {
            RunKind::Manual => false,
            _ if last_ms <= 0 => false,
            RunKind::Start => at_start,
            RunKind::Hours => now_ms - last_ms >= i64::from(self.hours.max(1)) * HOUR_MS,
            RunKind::Daily => {
                let off = offset_s * 1000;
                let local = now_ms + off;
                let mut at = local.div_euclid(DAY_MS) * DAY_MS + i64::from(self.minute.min(24 * 60 - 1)) * MIN_MS;
                if at > local {
                    at -= DAY_MS;
                }
                last_ms < at - off
            }
        }
    }

    /// The line the app shows: "every 6 hours", "daily at 03:00", ...
    pub fn text(&self) -> String {
        match self.kind {
            RunKind::Manual => "manually".into(),
            RunKind::Start => "when leaf starts".into(),
            RunKind::Hours if self.hours <= 1 => "every hour".into(),
            RunKind::Hours => format!("every {} hours", self.hours),
            RunKind::Daily => format!("daily at {:02}:{:02}", self.minute / 60, self.minute % 60),
        }
    }
}
