//! "Open leaf for Win+E and for 'Show in folder' from other programs" (Settings > Advanced), per user (HKCU only, no admin).
//!
//! The same three verbs the default file manager takes over (see the module above), written with a journal: what was there before
//! (each value's type and bytes, or that it was missing, and the first key that did not exist) is saved before anything is written,
//! so turning it off puts back exactly that, also when another file manager had the keys before. A write that fails undoes the
//! ones made before it.
//!
//! - `Folder\shell\open` and `Folder\shell\explore`: every folder the shell opens, also for `SHOpenFolderAndSelectItems` ("Show in
//!   folder" in browsers, editors, Task Manager: Windows opens the folder through this verb and names the file to a window it finds
//!   in its window list, see `shell_window`) and for `explorer.exe <folder>` started by another program (seen here: what leaf hands
//!   to Explorer comes back through this verb, hence the loop guard in `default_fm`).
//! - `CLSID\{52205fd8-…}\shell\opennewwindow`: Win+E. Empty `DelegateExecute` so the command wins over HKLM's handler.
//!
//! Next to the journal an `undo.reg` is kept: if leaf is deleted without its uninstaller, double-clicking that file (or
//! `reg import`) gives the folders back to Explorer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE, Type};

/// The values each verb's `command` key holds; only these are written and put back.
const VALUES: [&str; 2] = ["", "DelegateExecute"];

/// The `command` keys taken over (under the classes root) and whether the command gets the folder (`"%1"`).
fn keys() -> [(String, bool); 3] {
    [
        (r"Folder\shell\open\command".to_string(), true),
        (r"Folder\shell\explore\command".to_string(), true),
        (format!(r"{}\shell\opennewwindow\command", super::WIN_E_CLSID), false),
    ]
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Old {
    name: String,
    /// The registry type and the bytes; None when the value was not there.
    value: Option<(u32, Vec<u8>)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Saved {
    key: String,
    /// The first key on the way to `key` that did not exist (it and what is below it go again when they are left empty).
    created: Option<String>,
    values: Vec<Old>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Journal {
    /// The program the commands start.
    exe: String,
    /// The state before the switch was turned on.
    keys: Vec<Saved>,
}

/// Where the journal is kept: `%LOCALAPPDATA%\leaf-explorer\journal.json`. Outside leaf's own folders, so it outlives a portable
/// leaf that is deleted and a cache that is cleaned.
pub fn journal_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from).join("leaf-explorer").join("journal.json")
}

fn undo_reg_path(journal: &Path) -> PathBuf {
    journal.with_file_name("undo.reg")
}

/// Shown on Windows only (the Linux backend says false).
pub fn supported() -> bool {
    true
}

fn read_journal(journal: &Path) -> Option<Journal> {
    serde_json::from_str(&std::fs::read_to_string(journal).ok()?).ok()
}

/// The journal and its `undo.reg`, written whole (a temp file renamed over) before any registry change.
fn write_journal(journal: &Path, j: &Journal, classes_root: &str) -> anyhow::Result<()> {
    if let Some(dir) = journal.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = journal.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(j)?)?;
    std::fs::rename(&tmp, journal)?;
    // the .reg is a convenience for the case leaf is gone: a failure here does not stop the switch
    let reg: Vec<u16> = undo_reg_text(&j.keys, classes_root).encode_utf16().collect();
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(reg.iter().flat_map(|c| c.to_le_bytes()));
    let _ = std::fs::write(undo_reg_path(journal), bytes);
    Ok(())
}

fn remove_journal(journal: &Path) {
    let _ = std::fs::remove_file(journal);
    let _ = std::fs::remove_file(undo_reg_path(journal));
}

/// The state before, as a .reg file (UTF-16, what regedit writes): values back as they were, keys that did not exist removed.
fn undo_reg_text(saved: &[Saved], classes_root: &str) -> String {
    let mut out = String::from("Windows Registry Editor Version 5.00\r\n");
    for s in saved {
        if let Some(c) = &s.created {
            out.push_str(&format!("\r\n[-HKEY_CURRENT_USER\\{classes_root}\\{c}]\r\n"));
            continue;
        }
        out.push_str(&format!("\r\n[HKEY_CURRENT_USER\\{classes_root}\\{}]\r\n", s.key));
        for v in &s.values {
            let name = if v.name.is_empty() { "@".to_string() } else { format!("\"{}\"", v.name) };
            match &v.value {
                None => out.push_str(&format!("{name}=-\r\n")),
                Some((ty, bytes)) => {
                    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    out.push_str(&format!("{name}=hex({ty:x}):{}\r\n", hex.join(",")));
                }
            }
        }
    }
    out
}

/// What is there now, for each key, before anything is written.
fn snapshot(classes_root: &str) -> Vec<Saved> {
    keys()
        .into_iter()
        .map(|(key, _)| {
            let parts: Vec<&str> = key.split('\\').collect();
            let created = (1..=parts.len()).map(|n| parts[..n].join("\\")).find(|p| CURRENT_USER.open(format!(r"{classes_root}\{p}")).is_err());
            let k = CURRENT_USER.open(format!(r"{classes_root}\{key}")).ok();
            let values = VALUES
                .iter()
                .map(|name| Old { name: name.to_string(), value: k.as_ref().and_then(|k| k.get_value(name).ok()).map(|v| (u32::from(v.ty()), v.to_vec())) })
                .collect();
            Saved { key, created, values }
        })
        .collect()
}

/// Whether the command under `key` starts leaf.
fn key_is_ours(classes_root: &str, key: &str) -> bool {
    CURRENT_USER.open(format!(r"{classes_root}\{key}")).and_then(|k| k.get_string("")).is_ok_and(|c| super::is_our_command(&c))
}

fn write_all(exe: &str, classes_root: &str) -> anyhow::Result<()> {
    let base = CURRENT_USER.create(classes_root)?;
    for (key, with_arg) in keys() {
        let cmd = if with_arg { format!("\"{exe}\" \"%1\"") } else { format!("\"{exe}\"") };
        super::set_command(&base, &key, &cmd)?;
    }
    Ok(())
}

fn is_empty(classes_root: &str, path: &str) -> bool {
    CURRENT_USER.open(format!(r"{classes_root}\{path}")).is_ok_and(|k| k.keys().is_ok_and(|mut i| i.next().is_none()) && k.values().is_ok_and(|mut i| i.next().is_none()))
}

/// Puts `saved` back. `only_ours`: a key another program has taken since is left to it. Every key is tried; the first error is
/// returned.
fn restore(classes_root: &str, saved: &[Saved], only_ours: bool) -> anyhow::Result<()> {
    let mut first: Option<anyhow::Error> = None;
    for s in saved.iter().rev() {
        if only_ours && !key_is_ours(classes_root, &s.key) {
            log::warn!("explorer hook: {} is another program's now, left as it is", s.key);
            continue;
        }
        let r = (|| -> anyhow::Result<()> {
            let k = CURRENT_USER.create(format!(r"{classes_root}\{}", s.key))?;
            for v in &s.values {
                match &v.value {
                    Some((ty, bytes)) => k.set_bytes(&v.name, Type::from(*ty), bytes)?,
                    None => {
                        if k.get_value(&v.name).is_ok() {
                            k.remove_value(&v.name)?;
                        }
                    }
                }
            }
            drop(k);
            // the keys made for us go again, from the bottom up, as long as nothing else was put in them
            if let Some(created) = &s.created {
                let mut path = s.key.clone();
                while path.len() >= created.len() && is_empty(classes_root, &path) {
                    CURRENT_USER.remove_tree(format!(r"{classes_root}\{path}"))?;
                    if path == *created {
                        break;
                    }
                    match path.rfind('\\') {
                        Some(i) => path.truncate(i),
                        None => break,
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Turns it on: the three verbs start `exe`. Already on: only the program is brought up to date and the state from before stays
/// in the journal. All or nothing: when a write fails, what this call changed is put back and the error returned.
pub fn enable(exe: &str, classes_root: &str, journal: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(Path::new(exe).is_file(), "{exe} is not there");
    let old = read_journal(journal).filter(|_| key_is_ours(classes_root, &keys()[0].0));
    let now = snapshot(classes_root);
    let before = old.as_ref().map_or_else(|| now.clone(), |j| j.keys.clone());
    // the journal first: if it cannot be written nothing is changed
    write_journal(journal, &Journal { exe: exe.to_string(), keys: before }, classes_root)?;
    if let Err(e) = write_all(exe, classes_root) {
        let undo = restore(classes_root, &now, false);
        match &old {
            Some(j) => {
                let _ = write_journal(journal, j, classes_root);
            }
            None => remove_journal(journal),
        }
        if let Err(u) = undo {
            return Err(e.context(format!("and putting back the earlier state failed too: {u:#}")));
        }
        return Err(e);
    }
    Ok(())
}

/// Turns it off: the state from before it was turned on comes back (a key another program took since stays that program's). The
/// journal goes once that worked; a key that could not be put back keeps it, to try again.
pub fn disable(classes_root: &str, journal: &Path) -> anyhow::Result<()> {
    let Some(j) = read_journal(journal) else { return Ok(()) };
    restore(classes_root, &j.keys, true)?;
    remove_journal(journal);
    Ok(())
}

/// On: the journal is there and the folder verb starts leaf.
pub fn is_enabled(classes_root: &str, journal: &Path) -> bool {
    journal.is_file() && key_is_ours(classes_root, &keys()[0].0)
}

/// The start-up check. While on: a registered leaf that is gone (moved, rebuilt elsewhere) is replaced by `current`, so Windows can
/// still open folders; a journal whose keys are no longer ours (the default file manager turned off, another program took them)
/// is dropped without touching the keys. Returns what it did, for the log.
pub fn check(current: &str, classes_root: &str, journal: &Path) -> Option<&'static str> {
    let j = read_journal(journal)?;
    if !keys().iter().any(|(k, _)| key_is_ours(classes_root, k)) {
        remove_journal(journal);
        return Some("the keys are not ours any more: journal dropped");
    }
    if !Path::new(&j.exe).is_file() && Path::new(current).is_file() {
        return Some(if enable(current, classes_root, journal).is_ok() { "re-pointed to this leaf" } else { "re-pointing failed" });
    }
    None
}

/// Opens `target` (a folder, a server root, a `::{CLSID}` or `shell:` namespace) in the real Explorer, through HKLM's `Folder`
/// class (Explorer's own handler) and not the merged one, so our HKCU verb cannot send it back to leaf. False when the shell
/// refused; the caller then starts explorer.exe.
pub fn open_in_explorer(target: &str) -> bool {
    use windows::Win32::System::Registry::HKEY;
    use windows::Win32::UI::Shell::{SEE_MASK_CLASSKEY, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};
    let Ok(class) = LOCAL_MACHINE.open(r"Software\Classes\Folder") else { return false };
    crate::shell::ensure_com();
    let file = crate::wide::wide(std::ffi::OsStr::new(target));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_CLASSKEY | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        hkeyClass: HKEY(class.as_raw()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.is_ok()
}

