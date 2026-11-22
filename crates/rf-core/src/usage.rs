//! "Your numbers": counters that grow by one step when an operation that happens anyway ends (files copied,
//! folders opened, ...), kept per day for the last 90 days and as lifetime totals. Pure data and its text form;
//! the file, the once-a-minute write and the switch are in rf-app (`usage.rs`). The numbers stay on this PC.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Days of history kept.
pub const DAYS_KEPT: usize = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Counter {
    FilesCopied,
    FilesMoved,
    FilesDeleted,
    FilesRenamed,
    BytesCopied,
    FoldersOpened,
    Searches,
    BulkRenames,
    ArchivesMade,
    ArchivesExtracted,
    Launches,
    TabsOpened,
    /// Seconds the window was in front.
    FocusSecs,
}

impl Counter {
    pub const ALL: [Counter; 13] = [
        Counter::FilesCopied,
        Counter::FilesMoved,
        Counter::FilesDeleted,
        Counter::FilesRenamed,
        Counter::BytesCopied,
        Counter::FoldersOpened,
        Counter::Searches,
        Counter::BulkRenames,
        Counter::ArchivesMade,
        Counter::ArchivesExtracted,
        Counter::Launches,
        Counter::TabsOpened,
        Counter::FocusSecs,
    ];

    /// The name in the file (never changed once released).
    pub fn key(self) -> &'static str {
        match self {
            Counter::FilesCopied => "files_copied",
            Counter::FilesMoved => "files_moved",
            Counter::FilesDeleted => "files_deleted",
            Counter::FilesRenamed => "files_renamed",
            Counter::BytesCopied => "bytes_copied",
            Counter::FoldersOpened => "folders_opened",
            Counter::Searches => "searches",
            Counter::BulkRenames => "bulk_renames",
            Counter::ArchivesMade => "archives_made",
            Counter::ArchivesExtracted => "archives_extracted",
            Counter::Launches => "launches",
            Counter::TabsOpened => "tabs_opened",
            Counter::FocusSecs => "focus_secs",
        }
    }
}

/// One day's counters; `day` is the number of days since 0001-01-01 (chrono's `num_days_from_ce`) in local time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Day {
    pub day: i32,
    #[serde(default)]
    pub n: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Since when the totals count (a day number; 0 = nothing counted yet).
    #[serde(default)]
    pub since: i32,
    #[serde(default)]
    pub totals: BTreeMap<String, u64>,
    /// Oldest first, at most [`DAYS_KEPT`].
    #[serde(default)]
    pub days: Vec<Day>,
    /// How many different days something was counted (the 90 kept days let go of the old ones; this does not).
    #[serde(default)]
    pub days_used: u32,
}

impl Usage {
    /// Adds `n` to the counter for day `today`: a new day starts a new record, and the oldest ones beyond 90 go.
    pub fn add(&mut self, c: Counter, n: u64, today: i32) {
        if n == 0 {
            return;
        }
        if self.since == 0 {
            self.since = today;
        }
        *self.totals.entry(c.key().to_string()).or_default() += n;
        if self.days.last().is_none_or(|d| d.day < today) {
            self.days.push(Day { day: today, n: BTreeMap::new() });
            self.days_used += 1;
            let extra = self.days.len().saturating_sub(DAYS_KEPT);
            self.days.drain(..extra);
        }
        // (a clock that went back: the newest record takes it rather than a record out of order)
        let d = self.days.last_mut().expect("a day was just pushed");
        *d.n.entry(c.key().to_string()).or_default() += n;
    }

    pub fn total(&self, c: Counter) -> u64 {
        self.totals.get(c.key()).copied().unwrap_or(0)
    }

    /// The counter for each of the `n` days ending on `today`, oldest first.
    pub fn per_day(&self, c: Counter, today: i32, n: usize) -> Vec<u64> {
        (0..n as i32)
            .rev()
            .map(|back| self.days.iter().find(|d| d.day == today - back).and_then(|d| d.n.get(c.key())).copied().unwrap_or(0))
            .collect()
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An unreadable text is no numbers (the file is only a tally).
    pub fn from_text(s: &str) -> Usage {
        let mut u: Usage = serde_json::from_str(s).unwrap_or_default();
        u.days.sort_by_key(|d| d.day);
        let extra = u.days.len().saturating_sub(DAYS_KEPT);
        u.days.drain(..extra);
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_roll_over_and_stay_at_ninety_days() {
        let mut u = Usage::default();
        u.add(Counter::FilesCopied, 3, 1000);
        u.add(Counter::FilesCopied, 2, 1000);
        u.add(Counter::Searches, 1, 1001);
        assert_eq!(u.days.len(), 2);
        assert_eq!(u.total(Counter::FilesCopied), 5);
        assert_eq!(u.per_day(Counter::FilesCopied, 1001, 3), vec![0, 5, 0]);
        for d in 0..120 {
            u.add(Counter::Launches, 1, 1002 + d);
        }
        assert_eq!(u.days.len(), DAYS_KEPT);
        assert_eq!(u.days.last().unwrap().day, 1121);
        // the totals keep what the days let go of
        assert_eq!(u.total(Counter::Launches), 120);
        assert_eq!(u.total(Counter::FilesCopied), 5);
        let back = Usage::from_text(&u.to_text());
        assert_eq!(back, u);
        assert_eq!(Usage::from_text("not json"), Usage::default());
    }
}
