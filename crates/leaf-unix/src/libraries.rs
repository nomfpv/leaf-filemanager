//! Linux backend of rf_win::libraries. Status: REAL (read only; never run on a desktop)
//!
//! A Windows library is a named set of folders. The Linux twin is a place the desktop already lists for the user: the XDG folders the
//! sidebar does not pin by itself (`Templates`, `Public` in `user-dirs.dirs`), the GTK bookmarks (`~/.config/gtk-3.0/bookmarks`, what
//! GNOME and Xfce file managers keep) and the folders of KDE's Places panel (`~/.local/share/user-places.xbel`). Each is a library of one
//! folder, and its `file` is that folder (the key `load` is asked with). Only folders that exist; the home folder and the six pinned
//! defaults (Desktop, Downloads, Documents, Pictures, Music, Videos) are left out, they are already in the sidebar. Nothing is written.

use std::path::{Path, PathBuf};

// TYPE-COPY(rf-win src/libraries.rs:18)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub file: PathBuf,
    pub name: String,
    pub folders: Vec<PathBuf>,
    pub default_save: Option<PathBuf>,
}

/// The library whose `file` is `file` (one of those [`list`] gives).
pub fn load(file: &Path) -> Option<Library> {
    list().into_iter().find(|l| l.file == file)
}

pub fn list() -> Vec<Library> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let mut raw: Vec<(String, PathBuf)> = Vec::new();
    for (key, name) in [("XDG_TEMPLATES_DIR", "Templates"), ("XDG_PUBLICSHARE_DIR", "Public")] {
        if let Some(p) = xdg_user_dir(key, &home) {
            raw.push((name.to_string(), p));
        }
    }
    if let Some(cfg) = dirs::config_dir()
        && let Ok(t) = std::fs::read_to_string(cfg.join("gtk-3.0/bookmarks"))
    {
        raw.extend(parse_gtk_bookmarks(&t));
    }
    if let Some(data) = dirs::data_dir()
        && let Ok(t) = std::fs::read_to_string(data.join("user-places.xbel"))
    {
        raw.extend(parse_xbel(&t));
    }
    let hidden: Vec<PathBuf> = crate::known::KnownFolder::PINNED_DEFAULTS.iter().filter_map(|k| k.path()).chain(std::iter::once(home)).collect();
    let mut out: Vec<Library> = Vec::new();
    for (name, p) in raw {
        if hidden.contains(&p) || out.iter().any(|l| l.file == p) || !p.is_dir() {
            continue;
        }
        let name = if name.trim().is_empty() { p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { name };
        out.push(Library { file: p.clone(), name, folders: vec![p.clone()], default_save: Some(p) });
    }
    out
}

/// `XDG_TEMPLATES_DIR="$HOME/Templates"` in `~/.config/user-dirs.dirs`. A folder equal to the home folder means "not set".
fn xdg_user_dir(key: &str, home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dirs::config_dir()?.join("user-dirs.dirs")).ok()?;
    let p = parse_user_dir(&text, key, home)?;
    (p != home).then_some(p)
}

fn parse_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let line = text.lines().map(str::trim).find_map(|l| l.strip_prefix(key)?.strip_prefix('='))?;
    let v = line.trim().trim_matches('"');
    let rest = v.strip_prefix("$HOME").unwrap_or(v);
    if v.starts_with("$HOME") { Some(home.join(rest.trim_start_matches('/'))) } else if v.starts_with('/') { Some(PathBuf::from(v)) } else { None }
}

/// `file:///home/u/Pics Pictures` lines: the address, then the label (optional). Only local folders.
fn parse_gtk_bookmarks(text: &str) -> Vec<(String, PathBuf)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (uri, label) = l.split_once(' ').unwrap_or((l, ""));
            let p = crate::dialogs::uri_to_path(uri)?;
            Some((label.trim().to_string(), p))
        })
        .collect()
}

/// The local folders of `user-places.xbel`: each `<bookmark href="file:///..."> ... <title>Name</title> ... </bookmark>`, unless the entry is
/// hidden (`<IsHidden>true</IsHidden>`). A line scan, not a full XML parser: the file is machine-written, one tag per line.
fn parse_xbel(text: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for block in text.split("<bookmark ").skip(1) {
        let block = block.split("</bookmark>").next().unwrap_or(block);
        let Some(href) = attr(block, "href") else { continue };
        if block.contains("<IsHidden>true</IsHidden>") {
            continue;
        }
        let Some(p) = crate::dialogs::uri_to_path(&xml_unescape(&href)) else { continue };
        let title = block.split_once("<title>").and_then(|(_, r)| r.split_once("</title>")).map(|(t, _)| xml_unescape(t)).unwrap_or_default();
        out.push((title, p));
    }
    out
}

fn attr(tag_text: &str, name: &str) -> Option<String> {
    let head = tag_text.split('>').next()?;
    let i = head.find(&format!("{name}=\""))? + name.len() + 2;
    Some(head[i..].split('"').next()?.to_string())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_and_places_are_read() {
        let g = "file:///home/u/Projects Projects\nfile:///home/u/My%20Stuff\nsmb://h/s Share\n\n";
        assert_eq!(
            parse_gtk_bookmarks(g),
            vec![("Projects".to_string(), PathBuf::from("/home/u/Projects")), (String::new(), PathBuf::from("/home/u/My Stuff"))]
        );
        let x = "<xbel>\n <bookmark href=\"file:///home/u/Code\">\n  <title>Code &amp; Co</title>\n </bookmark>\n <bookmark href=\"file:///home/u/Old\">\n  <title>Old</title>\n  <info><metadata><IsHidden>true</IsHidden></metadata></info>\n </bookmark>\n <bookmark href=\"trash:/\">\n  <title>Trash</title>\n </bookmark>\n</xbel>";
        assert_eq!(parse_xbel(x), vec![("Code & Co".to_string(), PathBuf::from("/home/u/Code"))]);
        let d = "XDG_TEMPLATES_DIR=\"$HOME/Vorlagen\"\nXDG_PUBLICSHARE_DIR=\"/srv/pub\"\n";
        assert_eq!(parse_user_dir(d, "XDG_TEMPLATES_DIR", Path::new("/home/u")), Some(PathBuf::from("/home/u/Vorlagen")));
        assert_eq!(parse_user_dir(d, "XDG_PUBLICSHARE_DIR", Path::new("/home/u")), Some(PathBuf::from("/srv/pub")));
    }
}
