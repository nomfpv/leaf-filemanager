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
        // (a helper: a player stuck for more than half a minute is ended by the reaper, and it ends with leaf)
        match leaf_proc::spawn_helper_tree(c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())) {
            Ok((mut child, tree)) => {
                let _ = std::thread::Builder::new().name("reaper".into()).spawn(move || {
                    let t0 = std::time::Instant::now();
                    while child.try_wait().is_ok_and(|s| s.is_none()) {
                        if t0.elapsed() > std::time::Duration::from_secs(30) {
                            leaf_proc::kill_tree(&mut child, &tree);
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
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
    let Ok(o) = leaf_proc::output_helper(Command::new(g).args(["get", "org.gnome.desktop.sound", "event-sounds"]).env("LC_ALL", "C"), Some(std::time::Duration::from_secs(5))) else { return true };
    !(o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "false")
}

/// `sounds/freedesktop/stereo/<name>.oga` (or `.ga`, `.wav`) in the data dirs.
fn theme_sound(name: &str) -> Option<PathBuf> {
    crate::openwith::data_dirs().into_iter().flat_map(|d| ["oga", "ogg", "wav"].map(|e| d.join(format!("sounds/freedesktop/stereo/{name}.{e}")))).find(|p| p.is_file())
}
