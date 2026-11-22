//! Linux backend of rf_win::syncroots. Status: REAL (read only; never run on a desktop)
//!
//! The folders of cloud clients, only where the client's own configuration says so (no guessing at `~/Nextcloud`):
//! Nextcloud and ownCloud (`Nextcloud/nextcloud.cfg`, `ownCloud/owncloud.cfg` in the config folder: the `localPath` of every `Folders` entry)
//! and Dropbox (`~/.dropbox/info.json`: the `path` of the personal and business accounts). A folder that does not exist is left out.

use std::path::PathBuf;

// TYPE-COPY(rf-win src/syncroots.rs:18)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRoot {
    pub name: String,
    pub path: PathBuf,
    pub provider: String,
}

pub fn list() -> Vec<SyncRoot> {
    let mut out: Vec<SyncRoot> = Vec::new();
    if let Some(cfg) = dirs::config_dir() {
        for (dir, file, name) in [("Nextcloud", "nextcloud.cfg", "Nextcloud"), ("ownCloud", "owncloud.cfg", "ownCloud")] {
            if let Ok(t) = std::fs::read_to_string(cfg.join(dir).join(file)) {
                out.extend(parse_owncloud_cfg(&t, name));
            }
        }
    }
    if let Some(home) = dirs::home_dir()
        && let Ok(t) = std::fs::read_to_string(home.join(".dropbox/info.json"))
    {
        out.extend(parse_dropbox(&t));
    }
    out.retain(|r| r.path.is_dir());
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// The `localPath` values of an ownCloud-family `*.cfg` (an INI file; its keys read `0\Folders\1\localPath=/home/u/Nextcloud/`). Several
/// accounts of one client are told apart by their folder's name.
fn parse_owncloud_cfg(text: &str, provider: &str) -> Vec<SyncRoot> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            if !(k.contains("\\Folders\\") || k.contains("/Folders/")) || !(k.ends_with("\\localPath") || k.ends_with("/localPath")) {
                return None;
            }
            let v = v.trim();
            if !v.starts_with('/') {
                return None;
            }
            let path = PathBuf::from(v.trim_end_matches('/'));
            let folder = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let name = if folder.is_empty() || folder.eq_ignore_ascii_case(provider) { provider.to_string() } else { format!("{provider} ({folder})") };
            Some(SyncRoot { name, path, provider: provider.to_string() })
        })
        .collect()
}

/// `{"personal": {"path": "/home/u/Dropbox", ...}, "business": {"path": ...}}`.
fn parse_dropbox(text: &str) -> Vec<SyncRoot> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    for (key, name) in [("personal", "Dropbox"), ("business", "Dropbox (business)")] {
        if let Some(p) = v.get(key).and_then(|a| a.get("path")).and_then(|p| p.as_str()).filter(|p| p.starts_with('/')) {
            out.push(SyncRoot { name: name.to_string(), path: PathBuf::from(p), provider: "Dropbox".to_string() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_configs_name_their_folders() {
        let c = "[General]\nx=1\n[Accounts]\n0\\Folders\\1\\localPath=/home/u/Nextcloud/\n0\\Folders\\1\\targetPath=/\n1\\Folders\\1\\localPath=/home/u/Work\n";
        let r = parse_owncloud_cfg(c, "Nextcloud");
        assert_eq!(r.iter().map(|s| (s.name.as_str(), s.path.to_str().unwrap())).collect::<Vec<_>>(), vec![("Nextcloud", "/home/u/Nextcloud"), ("Nextcloud (Work)", "/home/u/Work")]);
        let d = parse_dropbox("{\"personal\":{\"path\":\"/home/u/Dropbox\",\"host\":1},\"business\":{\"path\":\"relative\"}}");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, PathBuf::from("/home/u/Dropbox"));
    }
}
