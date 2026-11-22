//! Linux backend of rf_win::dialogs. Status: PARTIAL (pick, pick_many and the autostart switch are real; never run on a desktop)

use crate::Owner;
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

/// The system's file, folder or save dialog. Order: the desktop's FileChooser portal (KDE then shows its own dialog), else `kdialog`, else
/// `zenity`; with none of them, nothing is chosen (the log says why). `None` also when the user closes the dialog.
pub fn pick(_owner: Owner, title: &str, kind: Pick<'_>) -> Option<PathBuf> {
    match portal_pick(title, &kind) {
        PortalAnswer::Chosen(p) => return Some(p),
        PortalAnswer::Closed => return None,
        PortalAnswer::Unavailable => {}
    }
    let (exe, args) = if let Some(k) = crate::shell::which("kdialog") {
        (k, pick_args_kdialog(title, &kind))
    } else if let Some(z) = crate::shell::which("zenity") {
        (z, pick_args_zenity(title, &kind))
    } else {
        log::info!("dialogs::pick: no FileChooser portal, kdialog or zenity: nothing chosen");
        return None;
    };
    // (the dialog is on screen and the user takes their time: a helper without a time limit)
    let out = leaf_proc::output_helper(std::process::Command::new(exe).args(args).env("LC_ALL", "C"), None).ok()?;
    if !out.status.success() {
        return None;
    }
    parse_picked(&out.stdout).into_iter().next()
}

enum PortalAnswer {
    Chosen(PathBuf),
    Closed,
    Unavailable,
}

/// `"*.png;*.jpg"` (the Windows-side pattern list) as the portal's `(kind, glob)` pairs; `*.*` is every file.
fn globs(pattern: &str) -> Vec<(u32, String)> {
    pattern.split(';').map(str::trim).filter(|g| !g.is_empty()).map(|g| (0u32, if g == "*.*" { "*".to_string() } else { g.to_string() })).collect()
}

type PortalFilter = (String, Vec<(u32, String)>);

fn portal_filters(filters: &[(&str, &str)]) -> Vec<PortalFilter> {
    filters.iter().map(|(name, pat)| (name.to_string(), globs(pat))).filter(|(_, g)| !g.is_empty()).collect()
}

/// `file:///a%20b` -> `/a b`. Only `file:` URIs on this machine are paths.
pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

fn portal_pick(title: &str, kind: &Pick<'_>) -> PortalAnswer {
    use std::collections::HashMap;
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
    const DEST: &str = "org.freedesktop.portal.Desktop";
    let Ok(conn) = Connection::session() else { return PortalAnswer::Unavailable };
    let Ok(chooser) = Proxy::new(&conn, DEST, "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser") else { return PortalAnswer::Unavailable };
    // (no such interface on this desktop: the version property is not there)
    if chooser.get_property::<u32>("version").is_err() {
        return PortalAnswer::Unavailable;
    }
    let token = format!("leaf_pick_{}_{}", std::process::id(), PICK_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let Some(sender) = conn.unique_name().map(|n| n.as_str().trim_start_matches(':').replace('.', "_")) else { return PortalAnswer::Unavailable };
    // the answer comes as a signal on the request object, whose path is known from our token: listen before asking
    let Ok(req) = Proxy::new(&conn, DEST, format!("/org/freedesktop/portal/desktop/request/{sender}/{token}"), "org.freedesktop.portal.Request") else {
        return PortalAnswer::Unavailable;
    };
    let Ok(mut responses) = req.receive_signal("Response") else { return PortalAnswer::Unavailable };
    let mut o: HashMap<&str, Value> = HashMap::new();
    o.insert("handle_token", Value::from(token.as_str()));
    o.insert("modal", Value::from(true));
    let (method, filters): (&str, Vec<PortalFilter>) = match kind {
        Pick::File { filters } => ("OpenFile", portal_filters(filters)),
        Pick::Folder => {
            o.insert("directory", Value::from(true));
            ("OpenFile", Vec::new())
        }
        Pick::Save { filters, default_name } => {
            if !default_name.is_empty() {
                o.insert("current_name", Value::from(*default_name));
            }
            ("SaveFile", portal_filters(filters))
        }
    };
    if !filters.is_empty() {
        o.insert("filters", Value::from(filters));
    }
    let called: zbus::Result<OwnedObjectPath> = chooser.call(method, &("", title, o));
    if let Err(e) = called {
        log::info!("dialogs::pick: the portal's {method} failed: {e}");
        return PortalAnswer::Unavailable;
    }
    let Some(msg) = responses.next() else { return PortalAnswer::Unavailable };
    let Ok((code, results)) = msg.body().deserialize::<(u32, HashMap<String, OwnedValue>)>() else { return PortalAnswer::Unavailable };
    if code != 0 {
        // 1: the user closed it, 2: it ended some other way; neither is a reason to open a second dialog
        return PortalAnswer::Closed;
    }
    let uris: Vec<String> = results.get("uris").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::<String>::try_from(v).ok()).unwrap_or_default();
    match uris.iter().find_map(|u| uri_to_path(u)) {
        Some(p) => PortalAnswer::Chosen(p),
        None => PortalAnswer::Closed,
    }
}

static PICK_N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// kdialog's filter text: `*.png *.jpg|Pictures`, filters separated by a line break.
fn kdialog_filter(filters: &[(&str, &str)]) -> String {
    let lines: Vec<String> = filters
        .iter()
        .map(|(name, pat)| {
            let g: Vec<String> = globs(pat).into_iter().map(|(_, g)| g).collect();
            format!("{}|{}", g.join(" "), name)
        })
        .collect();
    lines.join("\n")
}

fn pick_args_kdialog(title: &str, kind: &Pick<'_>) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["--title".into(), title.into()];
    match kind {
        Pick::File { filters } => {
            a.push("--getopenfilename".into());
            a.push(".".into());
            if !filters.is_empty() {
                a.push(kdialog_filter(filters).into());
            }
        }
        Pick::Folder => {
            a.push("--getexistingdirectory".into());
            a.push(".".into());
        }
        Pick::Save { filters, default_name } => {
            a.push("--getsavefilename".into());
            a.push(if default_name.is_empty() { ".".to_string() } else { format!("./{default_name}") }.into());
            if !filters.is_empty() {
                a.push(kdialog_filter(filters).into());
            }
        }
    }
    a
}

fn pick_args_zenity(title: &str, kind: &Pick<'_>) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["--file-selection".into(), "--title".into(), title.into()];
    let filter_args = |filters: &[(&str, &str)]| -> Vec<OsString> {
        filters
            .iter()
            .map(|(name, pat)| {
                let g: Vec<String> = globs(pat).into_iter().map(|(_, g)| g).collect();
                OsString::from(format!("--file-filter={name} | {}", g.join(" ")))
            })
            .collect()
    };
    match kind {
        Pick::File { filters } => a.extend(filter_args(filters)),
        Pick::Folder => a.push("--directory".into()),
        Pick::Save { filters, default_name } => {
            a.extend(filter_args(filters));
            a.push("--save".into());
            a.push("--confirm-overwrite".into());
            if !default_name.is_empty() {
                a.push(format!("--filename={default_name}").into());
            }
        }
    }
    a
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
    let out = match leaf_proc::output_helper(std::process::Command::new(exe).args(args), None) {
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
    fn portal_uris_and_filters() {
        assert_eq!(uri_to_path("file:///a%20b/c%C3%A9.txt"), Some(PathBuf::from("/a b/cé.txt")));
        assert_eq!(uri_to_path("file://localhost/x"), Some(PathBuf::from("/x")));
        assert_eq!(uri_to_path("smb://h/s"), None);
        assert_eq!(globs("*.png; *.jpg"), vec![(0, "*.png".to_string()), (0, "*.jpg".to_string())]);
        assert_eq!(globs("*.*"), vec![(0, "*".to_string())]);
        assert_eq!(kdialog_filter(&[("Pictures", "*.png;*.jpg")]), "*.png *.jpg|Pictures");
    }

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
