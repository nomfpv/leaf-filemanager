//! Linux backend of rf_win::default_fm: "make leaf the default file manager".
//!
//! On Windows this takes over the `Folder\shell\open` verbs in the registry. The Linux twin is the default handler of the media type
//! `inode/directory`: the line `inode/directory=leaf.desktop;...` in the `[Default Applications]` section of
//! `$XDG_CONFIG_HOME/mimeapps.list`, which is what `xdg-mime default leaf.desktop inode/directory` writes (written here directly: no
//! process, and the folders come from the environment, so tests use temp ones). The programs that were the default before stay in the
//! list behind leaf, and [`disable`] takes only leaf out, so the old default is back.
//!
//! The entry `leaf.desktop` is the packaged one when a data folder has it. A copy run from a build folder has none, so a per-user one
//! is written to `$XDG_DATA_HOME/applications/leaf.desktop` (marked `X-Leaf-Generated=true`; only such a file is ever rewritten or
//! removed). `classes_root` (a registry root on Windows) means nothing here and is ignored.

use std::path::{Path, PathBuf};

use crate::openwith::{config_dirs, data_dirs, mimeapps_ids};

/// The registry root on Windows; unused here (kept so the app's calls stay the same).
pub const CLASSES: &str = "";

pub const APP_NAME: &str = "leaf";

const ID: &str = "leaf.desktop";
const MIME: &str = "inode/directory";
const SECTION: &str = "Default Applications";
const MARK: &str = "X-Leaf-Generated=true";

fn config_home() -> PathBuf {
    config_dirs().into_iter().next().unwrap_or_else(|| PathBuf::from(".config"))
}

fn data_home() -> PathBuf {
    data_dirs().into_iter().next().unwrap_or_else(|| PathBuf::from(".local/share"))
}

fn mimeapps_path() -> PathBuf {
    config_home().join("mimeapps.list")
}

fn own_entry_path() -> PathBuf {
    data_home().join("applications").join(ID)
}

/// The text of `mimeapps.list` with the `[Default Applications]` line of `mime` changed by `f` (the ids in it in, the ids out; no ids
/// out removes the line). Everything else stays as it was.
fn edit_default(text: &str, mime: &str, f: impl Fn(Vec<String>) -> Vec<String>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut in_section = false;
    let (mut at, mut section_end) = (None, None);
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            if in_section {
                section_end = Some(i);
            }
            in_section = t == format!("[{SECTION}]");
            if in_section {
                section_end = Some(lines.len());
            }
        } else if in_section && t.split_once('=').is_some_and(|(k, _)| k.trim() == mime) {
            at = Some(i);
        }
    }
    let old: Vec<String> = at
        .and_then(|i| lines[i].split_once('=').map(|(_, v)| v.split(';').filter(|s| !s.is_empty()).map(str::to_string).collect()))
        .unwrap_or_default();
    let new = f(old);
    let line = (!new.is_empty()).then(|| format!("{mime}={};", new.join(";")));
    match (at, line) {
        (Some(i), Some(l)) => lines[i] = l,
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, Some(l)) => match section_end {
            Some(mut e) => {
                // (above the blank lines that close the section)
                while e > 0 && lines[e - 1].trim().is_empty() {
                    e -= 1;
                }
                lines.insert(e, l)
            }
            None => {
                lines.push(format!("[{SECTION}]"));
                lines.push(l);
            }
        },
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Writes `text` to `path` through a temporary file beside it, so that a crash leaves the old file whole.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // (a link to the file, as dotfile managers make, is written through: the rename would replace the link itself)
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tmp = path.with_extension(format!("leaf-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// The `Exec` value for `exe`: double-quoted, as the desktop entry rules ask. A path with a character that needs the double escape
/// of the spec (a backslash) is refused.
pub(crate) fn exec_quote(exe: &str) -> anyhow::Result<String> {
    if exe.contains(['\\', '\n']) {
        anyhow::bail!("the program's path has a character a desktop entry cannot hold: {exe}");
    }
    let mut s = String::from("\"");
    for c in exe.chars() {
        if matches!(c, '"' | '`' | '$') {
            s.push('\\');
        }
        // (a percent sign is a field code to the launcher: a literal one is written twice)
        if c == '%' {
            s.push('%');
        }
        s.push(c);
    }
    s.push('"');
    Ok(s)
}

fn own_entry_text(exe: &str) -> anyhow::Result<String> {
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=leaf\nGenericName=File Manager\nExec={} %U\nIcon=leaf\nTerminal=false\n\
         Categories=System;FileTools;FileManager;\nMimeType=inode/directory;\nStartupWMClass=leaf\n{MARK}\n",
        exec_quote(exe)?
    ))
}

/// A `leaf.desktop` that someone else made (the package) is in a data folder.
fn packaged_entry_exists() -> bool {
    data_dirs().iter().any(|d| {
        let p = d.join("applications").join(ID);
        std::fs::read_to_string(&p).is_ok_and(|t| !t.contains(MARK))
    })
}

/// Makes leaf the default handler of folders for this user.
pub fn enable(exe: &str, _classes_root: &str) -> anyhow::Result<()> {
    if !packaged_entry_exists() {
        write_atomic(&own_entry_path(), &own_entry_text(exe)?)?;
    }
    let path = mimeapps_path();
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let new = edit_default(&old, MIME, |ids| std::iter::once(ID.to_string()).chain(ids.into_iter().filter(|i| i != ID)).collect());
    if new != old {
        write_atomic(&path, &new)?;
    }
    Ok(())
}

/// Not applicable: Windows registers the archive types for the viewer; here the media types are the entry's own.
pub fn add_archives(_exe: &str, _classes_root: &str) -> bool {
    false
}

/// Gives the folders back to what was the default before: leaf is taken out of the list, and the entry written by [`enable`] goes.
pub fn disable(_classes_root: &str) -> anyhow::Result<()> {
    let path = mimeapps_path();
    if let Ok(old) = std::fs::read_to_string(&path) {
        let new = edit_default(&old, MIME, |ids| ids.into_iter().filter(|i| i != ID).collect());
        if new != old {
            write_atomic(&path, &new)?;
        }
    }
    let own = own_entry_path();
    if std::fs::read_to_string(&own).is_ok_and(|t| t.contains(MARK)) {
        let _ = std::fs::remove_file(own);
    }
    Ok(())
}

/// Is leaf the default handler of folders now (first in the effective list)?
pub fn is_enabled(_classes_root: &str) -> bool {
    mimeapps_ids(SECTION, MIME).first().is_some_and(|i| i == ID)
}

/// The program the registration ends in (here the same as [`registered_exe`]: there is no launcher on Linux).
pub fn registered_leaf(classes_root: &str) -> Option<PathBuf> {
    registered_exe(classes_root)
}

/// The program the generated entry starts (None when there is none of ours).
pub fn registered_exe(_classes_root: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(own_entry_path()).ok().filter(|t| t.contains(MARK))?;
    let exec = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
    let words = crate::openwith::exec_words(exec, Path::new(""));
    words.first().map(PathBuf::from)
}

/// The entry made by [`enable`] points at a program that is gone (moved, rebuilt elsewhere): points it here. True when it changed.
pub fn heal(current: &str, classes_root: &str) -> bool {
    match registered_exe(classes_root) {
        Some(old) if !old.exists() && is_enabled(classes_root) => own_entry_text(current).and_then(|t| write_atomic(&own_entry_path(), &t)).is_ok(),
        _ => false,
    }
}

/// rf-win opens these in the real Explorer past its own folder verb; there is no Explorer here (the callers then start the
/// system's opener themselves).
pub fn open_in_explorer(_target: &str) -> bool {
    false
}

pub fn reveal_in_explorer(_file: &Path) -> bool {
    false
}

/// rf-win's folder-handler launcher (leaf-open.exe); none here, so a copy in a build folder is still refused.
pub fn launcher_beside(_exe: &Path) -> bool {
    false
}

/// True when the argument should be handed back to the system's file manager (shell namespaces, non-folders).
pub fn should_forward_to_explorer(arg: &str) -> bool {
    let a = arg.trim().trim_matches('"');
    !a.is_empty() && (a.starts_with("::") || a.starts_with("shell:") || !Path::new(a).is_dir())
}

/// Forwards are remembered for a few seconds to break a loop (what we hand to the system's folder handler can come straight back).
fn forward_log() -> PathBuf {
    // (the user's own runtime folder when there is one: a name in a shared /tmp can be a link someone else planted)
    std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!("leaf-forward-{}.txt", unsafe { libc::getuid() }))
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// True when `arg` was handed to the system within the last few seconds.
pub fn forwarded_recently(arg: &str) -> bool {
    forwarded_within(&forward_log(), arg, now_ms(), 5_000)
}

/// Remembers that `arg` is being handed to the system.
pub fn note_forward(arg: &str) {
    note_in(&forward_log(), arg, now_ms());
}

// COPY(rf-win default_fm.rs:236)
fn forwarded_within(log: &Path, arg: &str, now: u128, window_ms: u128) -> bool {
    let Ok(text) = std::fs::read_to_string(log) else { return false };
    text.lines().filter_map(|l| l.split_once('\t')).any(|(t, a)| a == arg && t.parse::<u128>().is_ok_and(|t| now.saturating_sub(t) <= window_ms))
}

fn note_in(log: &Path, arg: &str, now: u128) {
    let old = std::fs::read_to_string(log).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().filter(|l| !l.is_empty()).collect();
    let keep = lines.len().saturating_sub(7);
    lines.drain(..keep);
    let mut out: String = lines.iter().map(|l| format!("{l}\n")).collect();
    out.push_str(&format!("{now}\t{arg}\n"));
    let _ = std::fs::write(log, out);
}

/// Tests of this crate that point the XDG folders at their own temp folders take this lock first.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mimeapps_line_is_edited_and_the_old_default_stays_behind_leaf() {
        let put = |t: &str| edit_default(t, MIME, |ids| std::iter::once(ID.to_string()).chain(ids.into_iter().filter(|i| i != ID)).collect());
        let take = |t: &str| edit_default(t, MIME, |ids| ids.into_iter().filter(|i| i != ID).collect());
        assert_eq!(put(""), "[Default Applications]\ninode/directory=leaf.desktop;\n");
        let t = "[Added Associations]\na=b.desktop;\n\n[Default Applications]\ntext/plain=ed.desktop;\ninode/directory=dolphin.desktop;\n\n[Removed Associations]\nx=y;\n";
        let on = put(t);
        assert!(on.contains("inode/directory=leaf.desktop;dolphin.desktop;\n"));
        assert!(on.contains("text/plain=ed.desktop;\n") && on.contains("[Removed Associations]\nx=y;"));
        assert_eq!(take(&on), t, "turning it off restores the text as it was");
        // no line of the type yet: it is added at the end of the section, not of the file
        let t2 = "[Default Applications]\ntext/plain=ed.desktop;\n\n[Removed Associations]\nx=y;\n";
        assert_eq!(put(t2), "[Default Applications]\ntext/plain=ed.desktop;\ninode/directory=leaf.desktop;\n\n[Removed Associations]\nx=y;\n");
    }

    #[test]
    fn enable_and_disable_work_on_temp_folders_only() {
        let root = std::env::temp_dir().join(format!("leaf-dfm-{}", std::process::id()));
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (cfg, data) = (root.join("config"), root.join("data"));
        std::fs::create_dir_all(&cfg).unwrap();
        // the environment of the process is the test's own for these two names (no other test of the crate reads them)
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &cfg);
            std::env::set_var("XDG_DATA_HOME", &data);
            std::env::set_var("XDG_DATA_DIRS", root.join("none"));
            std::env::set_var("XDG_CONFIG_DIRS", root.join("none"));
        }
        std::fs::write(cfg.join("mimeapps.list"), "[Default Applications]\ninode/directory=thunar.desktop;\n").unwrap();
        assert!(!is_enabled(CLASSES));
        enable("/opt/my leaf/leaf", CLASSES).unwrap();
        assert!(is_enabled(CLASSES));
        assert_eq!(registered_exe(CLASSES), Some(PathBuf::from("/opt/my leaf/leaf")));
        // the registered program is not there (never was): healed to the current one, which is there now
        assert!(heal("/bin/sh", CLASSES) && registered_exe(CLASSES) == Some(PathBuf::from("/bin/sh")));
        assert!(!heal("/x/leaf", CLASSES), "a program that is there is left alone");
        assert!(std::fs::read_to_string(cfg.join("mimeapps.list")).unwrap().contains("leaf.desktop;thunar.desktop;"));
        disable(CLASSES).unwrap();
        assert!(!is_enabled(CLASSES));
        assert_eq!(std::fs::read_to_string(cfg.join("mimeapps.list")).unwrap(), "[Default Applications]\ninode/directory=thunar.desktop;\n");
        assert!(!data.join("applications/leaf.desktop").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
