//! Linux backend of rf_win::sound: the operation-finished sound. Status: REAL (never heard on a desktop)
//!
//! `canberra-gtk-play -i complete` first (it plays the sound theme's event sound and honours the desktop's "event sounds" switch itself),
//! else `pw-play` / `paplay` on the freedesktop theme's `complete.oga` (found in the XDG data dirs) unless GNOME's
//! `org.freedesktop`-style switch `gsettings org.gnome.desktop.sound event-sounds` says false. Without any of them: silent. The player starts
//! detached; nothing waits for it and a failure is not an error.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn notify() {
    let quiet = |mut c: Command| -> bool {
        match c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(mut child) => {
                let _ = std::thread::Builder::new().name("reaper".into()).spawn(move || {
                    let _ = child.wait();
                });
                true
            }
            Err(_) => false,
        }
    };
    if let Some(p) = crate::shell::which("canberra-gtk-play") {
        let mut c = Command::new(p);
        c.args(["-i", "complete", "-d", "leaf"]);
        if quiet(c) {
            return;
        }
    }
    if !event_sounds_on() {
        return;
    }
    let Some(file) = theme_sound("complete") else { return };
    for player in ["pw-play", "paplay"] {
        if let Some(p) = crate::shell::which(player) {
            let mut c = Command::new(p);
            c.arg(&file);
            if quiet(c) {
                return;
            }
        }
    }
}

/// `gsettings get org.gnome.desktop.sound event-sounds` (GNOME and others that use its schema); true when it cannot be asked.
fn event_sounds_on() -> bool {
    let Some(g) = crate::shell::which("gsettings") else { return true };
    let Ok(o) = Command::new(g).args(["get", "org.gnome.desktop.sound", "event-sounds"]).env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null()).output() else { return true };
    !(o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "false")
}

/// `sounds/freedesktop/stereo/<name>.oga` (or `.ga`, `.wav`) in the data dirs.
fn theme_sound(name: &str) -> Option<PathBuf> {
    crate::openwith::data_dirs().into_iter().flat_map(|d| ["oga", "ogg", "wav"].map(|e| d.join(format!("sounds/freedesktop/stereo/{name}.{e}")))).find(|p| p.is_file())
}
