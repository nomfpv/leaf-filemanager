//! The record of finished transfers (copy, move, download, upload): what, from, to, how big, how long, how fast, how it ended and
//! when. Paths and numbers only, never file contents; the last [`KEEP`] are kept. Pure data and its text form; the file is written
//! here too ([`save`], atomically) so the bound can be tested; rf-app decides when (`xfer_history.rs`) and keeps the switch.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Records kept; the oldest go first.
pub const KEEP: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Copy,
    Move,
    Download,
    Upload,
}

/// How a download came to leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Web,
    Video,
    Usenet,
    Browser,
    Torrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub kind: Kind,
    /// What was transferred: its name, or "12 items".
    pub what: String,
    pub from: String,
    pub to: String,
    pub bytes: u64,
    pub millis: u64,
    pub result: Outcome,
    /// When it ended, ms since 1970.
    pub at_ms: i64,
    /// For a download of the download manager: the way it came. `None` on older records and on transfers of other kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// For a download: the file (or, for Usenet, the folder) it made, when it made one. `to` is the folder it was asked to go into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// For a failed download: why, in leaf's words (first lines only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Record {
    /// The way a download of the download manager came; older records are told by their address (a web address is a web download;
    /// a download from a server of the Hosts list is not the manager's).
    pub fn download_via(&self) -> Option<Via> {
        if self.kind != Kind::Download {
            return None;
        }
        self.via.or_else(|| (self.from.starts_with("http://") || self.from.starts_with("https://")).then_some(Via::Web))
    }

    /// Bytes a second over the whole run (None when nothing was counted or it took no time).
    pub fn speed(&self) -> Option<f64> {
        (self.bytes > 0 && self.millis > 0).then(|| self.bytes as f64 / (self.millis as f64 / 1000.0))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Log {
    /// Oldest first.
    #[serde(default)]
    pub records: Vec<Record>,
}

impl Log {
    pub fn push(&mut self, r: Record) {
        self.records.push(r);
        let extra = self.records.len().saturating_sub(KEEP);
        self.records.drain(..extra);
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An unreadable text is an empty history (the file is only a record).
    pub fn from_text(s: &str) -> Log {
        let mut l: Log = serde_json::from_str(s).unwrap_or_default();
        let extra = l.records.len().saturating_sub(KEEP);
        l.records.drain(..extra);
        l
    }
}

/// Writes the log next to itself first and renames it over the old one, so a crash leaves the old file whole.
pub fn save(path: &Path, log: &Log) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, log.to_text())?;
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> Log {
    crate::read_config(path).map(|t| Log::from_text(&t)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(i: u64) -> Record {
        Record { kind: Kind::Copy, what: format!("f{i}"), from: "C:/a".into(), to: "D:/b".into(), bytes: 1000 * i, millis: 500, result: Outcome::Done, at_ms: i as i64, via: None, file: None, reason: None }
    }

    #[test]
    fn the_file_keeps_the_last_five_hundred() {
        let dir = std::env::temp_dir().join(format!("leaf-xfer-log-{}", std::process::id()));
        let path = dir.join("transfers.json");
        let mut log = Log::default();
        for i in 0..(KEEP as u64 + 20) {
            log.push(rec(i));
        }
        save(&path, &log).unwrap();
        let back = load(&path);
        assert_eq!(back.records.len(), KEEP);
        assert_eq!(back.records.first().unwrap().what, "f20");
        assert_eq!(back.records.last().unwrap().what, format!("f{}", KEEP + 19));
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(rec(2).speed(), Some(4000.0));
        // a broken file is an empty history
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).records.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
