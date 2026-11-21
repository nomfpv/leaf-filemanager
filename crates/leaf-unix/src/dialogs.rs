//! Linux backend of rf_win::dialogs. Status: STUB

use crate::Owner;
use leaf_common::stub;
use std::ffi::OsString;
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

/// Several files to choose (rf-win's `pick_many`): kdialog, else zenity (argument lists, no shell); with neither, nothing is chosen.
pub fn pick_many(_owner: Owner, title: &str) -> Vec<PathBuf> {
    let (exe, args) = if let Some(k) = crate::shell::which("kdialog") {
        (k, pick_many_args_kdialog(title))
    } else if let Some(z) = crate::shell::which("zenity") {
        (z, pick_many_args_zenity(title))
    } else {
        log::info!("dialogs::pick_many: neither kdialog nor zenity is installed, nothing chosen");
        return Vec::new();
    };
    let out = match std::process::Command::new(exe).args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output() {
        Ok(o) => o,
        Err(e) => {
            log::info!("dialogs::pick_many: the dialog did not start: {e}");
            return Vec::new();
        }
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_picked(&out.stdout)
}

fn pick_many_args_kdialog(title: &str) -> Vec<OsString> {
    ["--title", title, "--getopenfilename", ".", "--multiple", "--separate-output"].iter().map(OsString::from).collect()
}

fn pick_many_args_zenity(title: &str) -> Vec<OsString> {
    ["--file-selection", "--multiple", "--separator=\n", "--title", title].iter().map(OsString::from).collect()
}

/// One path per line (kdialog `--separate-output`, zenity with a newline separator).
fn parse_picked(stdout: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    stdout.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| PathBuf::from(std::ffi::OsStr::from_bytes(l))).collect()
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
    fn picked_paths_split_on_newlines() {
        assert_eq!(parse_picked(b"/a/b\n/c d\n"), vec![PathBuf::from("/a/b"), PathBuf::from("/c d")]);
        assert!(parse_picked(b"\n").is_empty());
    }

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
