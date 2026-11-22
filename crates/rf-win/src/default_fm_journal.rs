//! The default file manager's journal: before "Set as default file manager" writes anything, what was there is saved (each value's
//! type and bytes, or that it was missing, and the first key on the way that did not exist), so turning it off puts back exactly
//! that, also when another file manager had the keys before. A write that fails undoes the ones made before it.
//!
//! Kept in `%LOCALAPPDATA%\leaf-default-fm\journal.json`, outside leaf's own folders (a deleted portable leaf, a cleaned cache), with
//! an `undo.reg` next to it: if leaf is gone without its uninstaller, importing that file gives the folders back to Explorer.
//!
//! A registration from before the journal (on, no journal) stays on; turning it off then removes the keys as before.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows_registry::{CURRENT_USER, Type};

/// A value to write (under HKCU).
pub(super) struct Write {
    pub key: String,
    pub name: String,
    pub expand: bool,
    pub data: String,
}

impl Write {
    pub fn sz(key: impl Into<String>, name: &str, data: impl Into<String>) -> Self {
        Write { key: key.into(), name: name.to_string(), expand: false, data: data.into() }
    }
    pub fn expand_sz(key: impl Into<String>, name: &str, data: impl Into<String>) -> Self {
        Write { key: key.into(), name: name.to_string(), expand: true, data: data.into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Old {
    name: String,
    /// The registry type and the bytes; None when the value was not there.
    value: Option<(u32, Vec<u8>)>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Saved {
    /// The key under HKCU.
    key: String,
    /// The first key on the way to `key` that did not exist (it and what is below it go again when they are left empty).
    created: Option<String>,
    values: Vec<Old>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Journal {
    /// The program the commands start.
    exe: String,
    /// The state before it was turned on.
    keys: Vec<Saved>,
}

/// Where the journal and the launcher live for `classes_root`: `%LOCALAPPDATA%\leaf-default-fm` for HKCU's classes, a folder in
/// temp for the tests' scratch roots.
pub(super) fn state_dir(classes_root: &str) -> PathBuf {
    if classes_root == super::CLASSES {
        std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from).join("leaf-default-fm")
    } else {
        let tag: String = classes_root.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        std::env::temp_dir().join(format!("leaf-journal-{tag}"))
    }
}

fn journal_path(classes_root: &str) -> PathBuf {
    state_dir(classes_root).join("journal.json")
}

fn undo_reg_path(journal: &Path) -> PathBuf {
    journal.with_file_name(format!("{}-undo.reg", journal.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()))
}

fn read(journal: &Path) -> Option<Journal> {
    serde_json::from_str(&std::fs::read_to_string(journal).ok()?).ok()
}

/// The journal and its `undo.reg`, written whole (a temp file renamed over) before any registry change.
fn write(journal: &Path, j: &Journal) -> anyhow::Result<()> {
    if let Some(dir) = journal.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = journal.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(j)?)?;
    std::fs::rename(&tmp, journal)?;
    // the .reg is for the case leaf is gone: a failure here does not stop the switch
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(undo_reg_text(&j.keys).encode_utf16().flat_map(|c| c.to_le_bytes()));
    let _ = std::fs::write(undo_reg_path(journal), bytes);
    Ok(())
}

fn remove(journal: &Path) {
    let _ = std::fs::remove_file(journal);
    let _ = std::fs::remove_file(undo_reg_path(journal));
}

/// The state before, as a .reg file (UTF-16, what regedit writes): values back as they were, keys that did not exist removed.
fn undo_reg_text(saved: &[Saved]) -> String {
    let mut out = String::from("Windows Registry Editor Version 5.00\r\n");
    for s in saved.iter().rev() {
        if let Some(c) = &s.created {
            out.push_str(&format!("\r\n[-HKEY_CURRENT_USER\\{c}]\r\n"));
            continue;
        }
        out.push_str(&format!("\r\n[HKEY_CURRENT_USER\\{}]\r\n", s.key));
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

/// The keys the writes touch, each once, in the order first written.
fn keys_of(writes: &[Write]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for w in writes {
        match out.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(&w.key)) {
            Some((_, names)) if !names.iter().any(|n| n.eq_ignore_ascii_case(&w.name)) => names.push(w.name.clone()),
            Some(_) => {}
            None => out.push((w.key.clone(), vec![w.name.clone()])),
        }
    }
    out
}

/// What is there now for every value the writes touch.
fn snapshot(writes: &[Write]) -> Vec<Saved> {
    keys_of(writes)
        .into_iter()
        .map(|(key, names)| {
            let parts: Vec<&str> = key.split('\\').collect();
            let created = (1..=parts.len()).map(|n| parts[..n].join("\\")).find(|p| CURRENT_USER.open(p).is_err());
            let k = CURRENT_USER.open(&key).ok();
            let values = names.into_iter().map(|name| Old { value: k.as_ref().and_then(|k| k.get_value(&name).ok()).map(|v| (u32::from(v.ty()), v.to_vec())), name }).collect();
            Saved { key, created, values }
        })
        .collect()
}

/// A verb's `command` key that another program has taken since (its command no longer starts leaf). Other keys are leaf's own
/// by name (`leaf.Archive`, `Software\leaf`, values called `leaf`): no other program has them.
fn taken_by_another(s: &Saved) -> bool {
    s.key.to_ascii_lowercase().ends_with(r"\command") && !command_is_ours(&s.key)
}

fn command_is_ours(key: &str) -> bool {
    CURRENT_USER.open(key).and_then(|k| k.get_string("")).is_ok_and(|c| super::is_our_command(&c))
}

fn is_empty(path: &str) -> bool {
    CURRENT_USER.open(path).is_ok_and(|k| k.keys().is_ok_and(|mut i| i.next().is_none()) && k.values().is_ok_and(|mut i| i.next().is_none()))
}

/// Puts `saved` back. `only_ours`: a key another program has taken since is left to it. Every key is tried; the first error is
/// returned.
fn restore(saved: &[Saved], only_ours: bool) -> anyhow::Result<()> {
    let mut first: Option<anyhow::Error> = None;
    for s in saved.iter().rev() {
        if only_ours && taken_by_another(s) {
            log::warn!("default file manager: {} is another program's now, left as it is", s.key);
            continue;
        }
        let r = (|| -> anyhow::Result<()> {
            let k = CURRENT_USER.create(&s.key)?;
            for v in &s.values {
                match &v.value {
                    Some((ty, bytes)) => k.set_bytes(&v.name, Type::from(*ty), bytes)?,
                    None if k.get_value(&v.name).is_ok() => k.remove_value(&v.name)?,
                    None => {}
                }
            }
            drop(k);
            // the keys made for us go again, from the bottom up, as long as nothing else was put in them
            if let Some(created) = &s.created {
                let mut path = s.key.clone();
                while path.len() >= created.len() && is_empty(&path) {
                    CURRENT_USER.remove_tree(&path)?;
                    if path.eq_ignore_ascii_case(created) {
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

fn apply(writes: &[Write]) -> anyhow::Result<()> {
    for w in writes {
        let k = CURRENT_USER.create(&w.key)?;
        if w.expand { k.set_expand_string(&w.name, &w.data)? } else { k.set_string(&w.name, &w.data)? }
    }
    Ok(())
}

/// The journal is there and at least one of `commands` still starts leaf (else it is stale: see [`drop_if_stale`]).
fn live(journal: &Path, commands: &[String]) -> Option<Journal> {
    read(journal).filter(|_| commands.iter().any(|c| command_is_ours(c)))
}

/// Saves the state before (unless a live journal already holds it: then only values it does not know yet are added), runs `before`
/// (removals of leaf's own older keys), then writes. All or nothing: when a write fails, what this call changed is put back.
pub(super) fn record_and_apply(exe: &str, classes_root: &str, commands: &[String], writes: &[Write], before: impl FnOnce()) -> anyhow::Result<()> {
    let journal = journal_path(classes_root);
    let old = live(&journal, commands);
    let now = snapshot(writes);
    // on from before the journal (no record, the verbs already ours): what is there now is leaf's own, not the state to go back to.
    // Written as before, without a record, so turning it off removes the keys as it always did.
    if old.is_none() && commands.iter().any(|c| command_is_ours(c)) {
        before();
        if let Err(e) = apply(writes) {
            let _ = restore(&now, false);
            return Err(e);
        }
        return Ok(());
    }
    let mut keep = old.as_ref().map(|j| j.keys.clone()).unwrap_or_default();
    for s in &now {
        match keep.iter_mut().find(|k| k.key.eq_ignore_ascii_case(&s.key)) {
            Some(k) => {
                for v in &s.values {
                    if !k.values.iter().any(|o| o.name.eq_ignore_ascii_case(&v.name)) {
                        k.values.push(v.clone());
                    }
                }
            }
            None => keep.push(s.clone()),
        }
    }
    // the journal first: if it cannot be written nothing is changed
    write(&journal, &Journal { exe: exe.to_string(), keys: keep })?;
    before();
    if let Err(e) = apply(writes) {
        let undo = restore(&now, false);
        match &old {
            Some(j) => {
                let _ = write(&journal, j);
            }
            None => remove(&journal),
        }
        if let Err(u) = undo {
            return Err(e.context(format!("and putting back the earlier state failed too: {u:#}")));
        }
        return Err(e);
    }
    Ok(())
}

/// Puts back the state from before (a key another program took since stays that program's). None when there is no journal (a
/// registration from before it). The journal goes once that worked; when a key could not be put back it stays, to try again.
pub(super) fn restore_recorded(classes_root: &str) -> Option<anyhow::Result<()>> {
    let journal = journal_path(classes_root);
    let j = read(&journal)?;
    Some(restore(&j.keys, true).map(|()| remove(&journal)))
}

/// A journal whose verbs are no longer ours (another program took them, they were removed) is dropped without touching the keys.
/// True when it dropped one.
pub(super) fn drop_if_stale(classes_root: &str, commands: &[String]) -> bool {
    let journal = journal_path(classes_root);
    if journal.is_file() && live(&journal, commands).is_none() {
        remove(&journal);
        return true;
    }
    false
}
