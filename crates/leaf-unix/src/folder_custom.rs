//! Linux backend of rf_win::folder_custom. Status: REAL. A folder's own icon is the `Icon=` key of the `[Desktop Entry]` group of its
//! `.directory` file (an icon theme name or a path; the index half of the pair is always 0). Setting it changes only that key: every
//! other line stays, the file is written beside and renamed. Picking an icon has no system dialog here and answers none.

use crate::Owner;
use std::path::Path;

/// `Icon` from the `[Desktop Entry]` group of `.directory` text.
fn icon_from(text: &str) -> Option<String> {
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
        } else if in_group && let Some((k, v)) = line.split_once('=') && k.trim() == "Icon" {
            return Some(v.trim().to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

/// The text with its `[Desktop Entry]` group's `Icon` set to `icon` (`None`: the key removed); everything else as it was.
fn with_icon(text: &str, icon: Option<&str>) -> String {
    let new_line = icon.map(|i| format!("Icon={i}"));
    let (mut out, mut in_group, mut seen_group, mut done) = (Vec::<String>::new(), false, false, false);
    let flush = |out: &mut Vec<String>, done: &mut bool| {
        // the end of the group: the key goes in as its last line (before trailing blank lines)
        if !*done && let Some(l) = &new_line {
            let at = out.iter().rposition(|x| !x.trim().is_empty()).map_or(0, |i| i + 1);
            out.insert(at, l.clone());
        }
        *done = true;
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_group {
                flush(&mut out, &mut done);
            }
            in_group = t == "[Desktop Entry]";
            seen_group |= in_group;
            out.push(line.to_string());
            continue;
        }
        if in_group && t.split_once('=').is_some_and(|(k, _)| k.trim() == "Icon") {
            if !done && let Some(l) = &new_line {
                out.push(l.clone());
            }
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    if in_group {
        flush(&mut out, &mut done);
    }
    if !seen_group && let Some(l) = new_line {
        out.insert(0, "[Desktop Entry]".to_string());
        out.insert(1, l);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

pub fn folder_icon(dir: &Path) -> Option<(String, i32)> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(dir.join(".directory")).ok()?.take(16 << 10).read_to_end(&mut buf).ok()?;
    icon_from(&String::from_utf8_lossy(&buf)).map(|i| (i, 0))
}

pub fn set_folder_icon(dir: &Path, icon: Option<(&str, i32)>) -> anyhow::Result<()> {
    let file = dir.join(".directory");
    let old = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if icon.is_none() && old.is_empty() {
        return Ok(());
    }
    let text = with_icon(&old, icon.map(|(f, _)| f));
    let tmp = dir.join(format!(".directory.leaf-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Ok(md) = std::fs::metadata(&file) {
        let _ = std::fs::set_permissions(&tmp, md.permissions());
    }
    std::fs::rename(&tmp, &file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    crate::localized::forget(dir);
    Ok(())
}

pub fn pick_icon(_owner: Owner, _file: &str, _index: i32) -> Option<(String, i32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_the_icon_touches_only_that_key() {
        let t = "[Desktop Entry]\nName=Pics\nName[de]=Bilder\nIcon=old\nSortOrder=1\n\n[Other]\nIcon=keep\n";
        assert_eq!(icon_from(t).as_deref(), Some("old"));
        assert_eq!(with_icon(t, Some("folder-red")), "[Desktop Entry]\nName=Pics\nName[de]=Bilder\nIcon=folder-red\nSortOrder=1\n\n[Other]\nIcon=keep\n");
        assert_eq!(with_icon(t, None), "[Desktop Entry]\nName=Pics\nName[de]=Bilder\nSortOrder=1\n\n[Other]\nIcon=keep\n");
        assert_eq!(with_icon("[Desktop Entry]\nName=A\n[Other]\nX=1\n", Some("i")), "[Desktop Entry]\nName=A\nIcon=i\n[Other]\nX=1\n");
        assert_eq!(with_icon("", Some("i")), "[Desktop Entry]\nIcon=i\n");
    }
}
