//! The download list that survives a restart: what was running, paused or waiting (to be started again, each resuming from its
//! own `.leaf-part.state`) and the last finished or failed ones. A small JSON file; addresses with a password never get here, nor
//! a browser's cookies or sign-in headers (see [`Saved::browser`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Finished and failed downloads that are remembered.
pub const HISTORY: usize = 50;

/// A download that had not finished.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub key: u64,
    pub url: String,
    pub dest: PathBuf,
    /// The SHA-256 the file must have, when one was given.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Read the checksum from `url.sha256` when the download starts.
    #[serde(default)]
    pub verify: bool,
    /// It was paused (it comes back paused; the others come back waiting for their turn).
    #[serde(default)]
    pub paused: bool,
    /// It came from the browser with the browser's sign-in (Cookie / Authorization). Those are never written here: after a
    /// restart it has to be started from the browser again.
    #[serde(default)]
    pub browser: bool,
    /// Sorted into a category subfolder by the settings (the folder is the download folder).
    #[serde(default)]
    pub sort: bool,
}

/// A download that ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Past {
    pub url: String,
    pub dest: PathBuf,
    pub ok: bool,
    /// Seconds since 1970.
    pub when: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub active: Vec<Saved>,
    #[serde(default)]
    pub history: Vec<Past>,
}

impl Store {
    /// The file's content; an empty list when there is none or it cannot be read.
    pub fn load(path: &Path) -> Store {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        if let Ok(bytes) = serde_json::to_vec_pretty(self)
            && std::fs::write(&tmp, bytes).is_ok()
            && std::fs::rename(&tmp, path).is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Remembers a download that ended, newest first, the oldest beyond [`HISTORY`] forgotten.
    pub fn remember(&mut self, past: Past) {
        self.history.insert(0, past);
        self.history.truncate(HISTORY);
    }

    pub fn forget(&mut self, key: u64) {
        self.active.retain(|s| s.key != key);
    }
}
