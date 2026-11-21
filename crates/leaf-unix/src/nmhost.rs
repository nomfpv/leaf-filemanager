//! Linux backend of the leaf-sys module `nmhost` (Windows side: rf-win nmhost.rs, HKCU keys). Status: REAL
//!
//! Browser native-messaging hosts on Linux are found by file: a `<name>.json` in each browser's own folder under the home folder. A
//! manifest is written only for a browser whose profile folder is there. Snap and Flatpak browsers cannot start a program outside
//! their sandbox: they are reported, not written for.

use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub connected: Vec<&'static str>,
    pub sandboxed: Vec<&'static str>,
}

/// (browser, Chromium family, its profile folder under the home folder, the host folder inside it)
const BROWSERS: &[(&str, bool, &str, &str)] = &[
    ("Chrome", true, ".config/google-chrome", "NativeMessagingHosts"),
    ("Chromium", true, ".config/chromium", "NativeMessagingHosts"),
    ("Edge", true, ".config/microsoft-edge", "NativeMessagingHosts"),
    ("Brave", true, ".config/BraveSoftware/Brave-Browser", "NativeMessagingHosts"),
    ("Vivaldi", true, ".config/vivaldi", "NativeMessagingHosts"),
    ("Firefox", false, ".mozilla", "native-messaging-hosts"),
];

/// (browser, the sandbox's folder under the home folder)
const SANDBOXED: &[(&str, &str)] = &[
    ("Firefox (Snap)", "snap/firefox"),
    ("Chromium (Snap)", "snap/chromium"),
    ("Firefox (Flatpak)", ".var/app/org.mozilla.firefox"),
    ("Chrome (Flatpak)", ".var/app/com.google.Chrome"),
    ("Chromium (Flatpak)", ".var/app/org.chromium.Chromium"),
    ("Brave (Flatpak)", ".var/app/com.brave.Browser"),
    ("Edge (Flatpak)", ".var/app/com.microsoft.Edge"),
];

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Writes `<name>.json` for every browser whose profile folder exists. `_dir` (leaf's own folder for the Windows manifests) is not
/// used here: the files sit in the browsers' folders.
pub fn register(name: &str, chromium: &str, firefox: &str, _dir: &Path) -> Result<Report, String> {
    let home = home().ok_or_else(|| "no home folder".to_string())?;
    let mut r = Report::default();
    let mut last_err = None;
    for (browser, chromium_family, profile, hosts) in BROWSERS {
        let base = home.join(profile);
        if !base.is_dir() {
            continue;
        }
        let dir = base.join(hosts);
        let res = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(dir.join(format!("{name}.json")), if *chromium_family { chromium } else { firefox }));
        match res {
            Ok(()) => r.connected.push(*browser),
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    r.sandboxed = SANDBOXED.iter().filter(|(_, d)| home.join(d).is_dir()).map(|(b, _)| *b).collect();
    if r.connected.is_empty() && r.sandboxed.is_empty() {
        return Err(last_err.unwrap_or_else(|| "no browser profile found".into()));
    }
    Ok(r)
}

pub fn unregister(name: &str, _dir: &Path) {
    let Some(home) = home() else { return };
    for (_, _, profile, hosts) in BROWSERS {
        let _ = std::fs::remove_file(home.join(profile).join(hosts).join(format!("{name}.json")));
    }
}

pub fn registered(name: &str) -> Vec<(&'static str, PathBuf)> {
    let Some(home) = home() else { return Vec::new() };
    BROWSERS
        .iter()
        .map(|(b, _, profile, hosts)| (*b, home.join(profile).join(hosts).join(format!("{name}.json"))))
        .filter(|(_, p)| p.is_file())
        .collect()
}
