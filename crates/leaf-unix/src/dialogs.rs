//! Linux backend of rf_win::dialogs. Status: STUB

use crate::Owner;
use leaf_common::stub;
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/dialogs.rs:17)
pub enum Pick<'a> {
    File {
        filters: &'a [(&'a str, &'a str)],
    },
    Folder,
    Save {
        filters: &'a [(&'a str, &'a str)],
        default_name: &'a str,
    },
}

pub fn pick(_owner: Owner, _title: &str, _kind: Pick<'_>) -> Option<PathBuf> {
    stub!("dialogs::pick")
}

/// Several files to choose (rf-win's `pick_many`): no dialog on this system yet, so nothing is chosen.
pub fn pick_many(_owner: Owner, _title: &str) -> Vec<PathBuf> {
    Vec::new()
}

/// Autostart on Linux is a desktop entry in `$XDG_CONFIG_HOME/autostart/` (the XDG autostart rule: the session starts every entry
/// there that is not `Hidden`). `name` is the entry's file name stem. Nothing is written until the user flips the switch.
fn autostart_path(name: &str) -> PathBuf {
    crate::openwith::config_dirs().into_iter().next().unwrap_or_else(|| PathBuf::from(".config")).join("autostart").join(format!("{name}.desktop"))
}

pub fn starts_with_windows() -> bool {
    starts_with_windows_as(crate::APP_EXE_STEM)
}

/// [`starts_with_windows`] for another program of the bundle (entry `name`).
pub fn starts_with_windows_as(name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(autostart_path(name)) else { return false };
    !text.lines().any(|l| matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false"))
}

/// "Open leaf when you sign in": turns the autostart entry of the app on or off.
pub fn set_start_with_windows(exe: &Path, on: bool) -> anyhow::Result<()> {
    set_start_with_windows_as(crate::APP_EXE_STEM, exe, &[], on)
}

/// Entry `name` -> `Exec="exe" args...`. Off removes the entry (already absent is fine).
pub fn set_start_with_windows_as(name: &str, exe: &Path, args: &[&str], on: bool) -> anyhow::Result<()> {
    let path = autostart_path(name);
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        };
    }
    let mut exec = crate::default_fm::exec_quote(&exe.to_string_lossy())?;
    for a in args {
        exec.push(' ');
        exec.push_str(a);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("[Desktop Entry]
Type=Application
Name={name}
Exec={exec}
Terminal=false
X-GNOME-Autostart-enabled=true
"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_entry_is_written_read_and_removed_in_a_temp_config_folder() {
        let cfg = std::env::temp_dir().join(format!("leaf-autostart-{}", std::process::id()));
        // (only this test reads XDG_CONFIG_HOME among the tests that run with the default_fm one: both point it at their own temp folder)
        let _g = crate::default_fm::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &cfg) };
        assert!(!starts_with_windows_as("leaf-t"));
        set_start_with_windows_as("leaf-t", Path::new("/opt/my leaf/leaf"), &["--background"], true).unwrap();
        assert!(starts_with_windows_as("leaf-t"));
        let text = std::fs::read_to_string(cfg.join("autostart/leaf-t.desktop")).unwrap();
        assert!(text.contains("Exec=\"/opt/my leaf/leaf\" --background
"), "{text}");
        set_start_with_windows_as("leaf-t", Path::new("/x"), &[], false).unwrap();
        assert!(!starts_with_windows_as("leaf-t"));
        set_start_with_windows_as("leaf-t", Path::new("/x"), &[], false).unwrap();
        let _ = std::fs::remove_dir_all(&cfg);
    }
}
