//! Linux backend of rf_win::localized. Status: REAL (a folder's own display name from its `.directory` file)
//!
//! A folder named by the user's language is already what `user-dirs.dirs` says (`~/Dokumente`), so those show their real names. What
//! a folder can carry on top of its name is the desktop's `.directory` file (`[Desktop Entry]` with `Name=` and `Name[de]=`): that is
//! the label here, the one for the user's language first. Answers are cached by path like on Windows; drawing asks the cache only
//! ([`label_if_known`]) and [`warm`] fills it from a worker thread. A network mount is never touched.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn by_path() -> &'static Mutex<HashMap<PathBuf, Option<Arc<str>>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<str>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The language keys of the session, most specific first: `de_DE@euro.UTF-8` gives `de_DE@euro`, `de_DE`, `de@euro`, `de`.
fn lang_keys() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|k| std::env::var(k).ok()).find(|v| !v.is_empty()).unwrap_or_default();
    let raw = raw.split('.').next().unwrap_or("").to_string();
    if raw.is_empty() || raw == "C" || raw == "POSIX" {
        return Vec::new();
    }
    let (base, modifier) = match raw.split_once('@') {
        Some((b, m)) => (b.to_string(), Some(m.to_string())),
        None => (raw, None),
    };
    let lang = base.split('_').next().unwrap_or("").to_string();
    let mut v = Vec::new();
    if let Some(m) = &modifier {
        v.push(format!("{base}@{m}"));
    }
    v.push(base.clone());
    if let Some(m) = &modifier {
        v.push(format!("{lang}@{m}"));
    }
    if lang != base {
        v.push(lang);
    }
    v
}

/// `Name` from the `[Desktop Entry]` group of `.directory` text, for the first of `keys` that has a `Name[key]`, else the plain one.
fn name_from(text: &str, keys: &[String]) -> Option<String> {
    let mut in_group = false;
    let mut plain = None;
    let mut found: Vec<Option<String>> = vec![None; keys.len()];
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "Name" {
            plain = Some(v.to_string());
        } else if let Some(l) = k.strip_prefix("Name[").and_then(|r| r.strip_suffix(']'))
            && let Some(i) = keys.iter().position(|x| x == l)
        {
            found[i] = Some(v.to_string());
        }
    }
    found.into_iter().flatten().next().or(plain).filter(|n| !n.is_empty())
}

fn read_label(dir: &Path) -> Option<Arc<str>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(dir.join(".directory")).ok()?.take(16 << 10).read_to_end(&mut buf).ok()?;
    let name = name_from(&String::from_utf8_lossy(&buf), &lang_keys())?;
    // the folder's own name is no label
    (dir.file_name().map(|n| n.to_string_lossy() != name.as_str()).unwrap_or(true)).then(|| Arc::from(name.as_str()))
}

/// The label of folder `dir`, if it has one. `attrs` (from a listing) is not needed here.
pub fn folder_label(dir: &Path, _attrs: Option<u32>) -> Option<Arc<str>> {
    if let Some(hit) = by_path().lock().ok()?.get(dir) {
        return hit.clone();
    }
    // breadcrumbs and tab titles ask while drawing: never touch a network mount from here
    if is_network(dir) {
        return None;
    }
    let label = read_label(dir);
    if let Ok(mut c) = by_path().lock() {
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(dir.to_path_buf(), label.clone());
    }
    label
}

/// Drops the cached label of `dir` (its `.directory` file changed).
pub fn forget(dir: &Path) {
    if let Ok(mut c) = by_path().lock() {
        c.remove(dir);
    }
}

/// The label of `dir` if it was already asked for, from the cache alone (never touches the disk).
pub fn label_if_known(dir: &Path) -> Option<Arc<str>> {
    by_path().lock().ok()?.get(dir)?.clone()
}

/// Whether [`folder_label`] has already answered for `dir` (with a name or with "none").
pub fn is_known(dir: &Path) -> bool {
    by_path().lock().is_ok_and(|c| c.contains_key(dir))
}

/// Asks [`folder_label`] for every folder not asked for yet; returns how many have a label. Touches the disk: a worker thread only.
pub fn warm<I: IntoIterator<Item = PathBuf>>(dirs: I) -> usize {
    dirs.into_iter().filter(|d| !is_known(d)).filter(|d| folder_label(d, None).is_some()).count()
}

/// The network mount points (nfs, cifs, sshfs, ...), read from the mount table at most every 30 seconds.
fn network_mounts() -> Vec<String> {
    static C: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);
    let mut g = C.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = g.as_ref()
        && t.elapsed() < Duration::from_secs(30)
    {
        return v.clone();
    }
    let v: Vec<String> = std::fs::read_to_string("/proc/self/mountinfo")
        .map(|t| t.lines().filter_map(crate::drives::parse_mount_line).filter(|(_, fs, _)| crate::drives::is_network_fs(fs)).map(|(m, _, _)| m).collect())
        .unwrap_or_default();
    *g = Some((Instant::now(), v.clone()));
    v
}

/// Whether `p` is on a network mount (reading there costs a round trip).
pub fn is_network(p: &Path) -> bool {
    network_mounts().iter().any(|m| p.starts_with(m))
}

/// `%Name%`, `${Name}` and `$Name` become the environment variable's value (unknown names stay as written); a leading `~` is the
/// home folder.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let mut rest = s;
    if let Some(r) = s.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with('/'))
        && let Some(h) = dirs::home_dir()
    {
        out.push_str(&h.to_string_lossy());
        rest = r;
    }
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while let Some(i) = rest.find(['%', '$']) {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, used) = if rest.as_bytes()[i] == b'%' {
            match after.find('%') {
                Some(j) => (&after[..j], j + 1),
                None => ("", 0),
            }
        } else if let Some(r) = after.strip_prefix('{') {
            match r.find('}') {
                Some(j) => (&r[..j], j + 2),
                None => ("", 0),
            }
        } else {
            let n = after.find(|c: char| !is_name(c)).unwrap_or(after.len());
            (&after[..n], n)
        };
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => {
                out.push_str(&v);
                rest = &after[used..];
            }
            _ => {
                out.push(rest.as_bytes()[i] as char);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_file_gives_the_name_of_the_users_language() {
        let t = "[Desktop Entry]\nName=Pictures\nName[de]=Bilder\nIcon=folder\n[Other]\nName=No\n";
        assert_eq!(name_from(t, &["de_DE".into(), "de".into()]).as_deref(), Some("Bilder"));
        assert_eq!(name_from(t, &["fr".into()]).as_deref(), Some("Pictures"));
        assert_eq!(name_from("[Other]\nName=x\n", &[]), None);
    }
}
