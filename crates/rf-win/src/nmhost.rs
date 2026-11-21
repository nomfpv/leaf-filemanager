//! Browser native-messaging host registration for the current user, no admin (Linux twin: leaf-unix nmhost.rs). The manifests are
//! two JSON files leaf writes into `dir` (one for the Chromium family, one for Firefox); each browser finds its file through a key
//! under HKCU whose default value is the file's path.

use std::path::{Path, PathBuf};

/// What a registration reached.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// The browsers that now find the host.
    pub connected: Vec<&'static str>,
    /// Browsers that are there but sandboxed so they cannot start it (Linux Snap / Flatpak; none on Windows).
    pub sandboxed: Vec<&'static str>,
}

/// (browser, Chromium family, key under HKCU without the host name)
const BROWSERS: &[(&str, bool, &str)] = &[
    ("Chrome", true, r"Software\Google\Chrome\NativeMessagingHosts"),
    ("Edge", true, r"Software\Microsoft\Edge\NativeMessagingHosts"),
    ("Brave", true, r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts"),
    ("Firefox", false, r"Software\Mozilla\NativeMessagingHosts"),
];

fn files(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    (dir.join(format!("{name}.chromium.json")), dir.join(format!("{name}.firefox.json")))
}

/// Writes the two manifests into `dir` and points every browser's key at its file.
pub fn register(name: &str, chromium: &str, firefox: &str, dir: &Path) -> Result<Report, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (cf, ff) = files(dir, name);
    std::fs::write(&cf, chromium).map_err(|e| e.to_string())?;
    std::fs::write(&ff, firefox).map_err(|e| e.to_string())?;
    let mut r = Report::default();
    let mut last_err = None;
    for (browser, chromium_family, key) in BROWSERS {
        let file = if *chromium_family { &cf } else { &ff };
        match windows_registry::CURRENT_USER.create(format!(r"{key}\{name}")).and_then(|k| k.set_string("", file.to_string_lossy())) {
            Ok(()) => r.connected.push(*browser),
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    if r.connected.is_empty() {
        return Err(last_err.unwrap_or_default());
    }
    Ok(r)
}

/// Removes the keys and the manifests.
pub fn unregister(name: &str, dir: &Path) {
    for (_, _, key) in BROWSERS {
        let _ = windows_registry::CURRENT_USER.remove_tree(format!(r"{key}\{name}"));
    }
    let (cf, ff) = files(dir, name);
    let _ = std::fs::remove_file(cf);
    let _ = std::fs::remove_file(ff);
}

/// The browsers whose key points at a manifest file that is there, with that file.
pub fn registered(name: &str) -> Vec<(&'static str, PathBuf)> {
    BROWSERS
        .iter()
        .filter_map(|(browser, _, key)| {
            let path = PathBuf::from(windows_registry::CURRENT_USER.open(format!(r"{key}\{name}")).ok()?.get_string("").ok()?);
            path.is_file().then_some((*browser, path))
        })
        .collect()
}
