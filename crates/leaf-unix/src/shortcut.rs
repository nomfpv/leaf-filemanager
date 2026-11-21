//! Linux backend of rf_win::shortcut: the twin of `.lnk` files is the symbolic link, and the `.desktop` link file for what a
//! desktop shows (Type=Link with a `URL=file://...`, or an application entry that starts a program).
//!
//! [`resolve`] reads either kind, [`create`] makes a symbolic link (a name that ends in `.desktop` makes a link file; a name that ends
//! in `.lnk`, which is what the app asks for on Windows, loses that ending: a link here has the name of the thing it points at).
//! Nothing is ever overwritten.

use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/shortcut.rs:15)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutInfo {
    pub target: PathBuf,
    pub args: String,
    pub working_dir: PathBuf,
    pub icon: String,
    pub icon_index: i32,
}

/// What a link points at, read from a symlink (its target made absolute against the link's folder) or a `.desktop` file.
pub fn resolve(path: &Path) -> anyhow::Result<ShortcutInfo> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        let raw = std::fs::read_link(path)?;
        let target = if raw.is_absolute() { raw } else { path.parent().unwrap_or(Path::new("/")).join(raw) };
        let working_dir = if target.is_dir() { target.clone() } else { target.parent().map(Path::to_path_buf).unwrap_or_default() };
        return Ok(ShortcutInfo { target, working_dir, ..Default::default() });
    }
    if is_desktop(path) {
        let d = details(path)?;
        return Ok(ShortcutInfo { target: d.target.into(), args: d.args, working_dir: d.working_dir.into(), icon: d.icon, icon_index: 0 });
    }
    anyhow::bail!("{} is not a link", path.display())
}

fn is_desktop(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("desktop"))
}

/// The name a link is made under: a Windows `.lnk` ending is dropped.
fn link_name(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) { p.with_extension("") } else { p.to_path_buf() }
}

/// Makes a link to `target` at `link_path`: a `.desktop` link file, or else a symbolic link. Fails when the name is taken.
pub fn create(link_path: &Path, target: &Path) -> anyhow::Result<()> {
    if is_desktop(link_path) {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "link".into());
        let text = format!("[Desktop Entry]\nType=Link\nName={name}\nURL=file://{}\n", url_encode(&target.to_string_lossy()));
        // (create_new: never replaces)
        use std::io::Write;
        std::fs::OpenOptions::new().write(true).create_new(true).open(link_path)?.write_all(text.as_bytes())?;
        return Ok(());
    }
    let link = link_name(link_path);
    std::os::unix::fs::symlink(target, &link).map_err(|e| anyhow::anyhow!("{}: {e}", link.display()))
}

fn url_encode(p: &str) -> String {
    let mut s = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn url_decode(u: &str) -> String {
    let b = u.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && let Ok(v) = u8::from_str_radix(&u[i + 1..i + 3], 16) {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// TYPE-COPY(rf-win src/shortcut.rs:76)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub target: String,
    pub args: String,
    pub working_dir: String,
    pub comment: String,
    pub show: i32,
    pub hotkey: u16,
    pub icon: String,
    pub icon_index: i32,
    pub run_as_admin: bool,
}

/// The `[Desktop Entry]` keys of a link file.
fn entry_keys(text: &str) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
        } else if in_main && let Some((k, v)) = line.split_once('=') {
            m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    m
}

/// What the Shortcut tab shows: a symlink has only its target; a `.desktop` file its link or program, arguments, folder, comment
/// and icon.
pub fn details(path: &Path) -> anyhow::Result<Details> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        let r = resolve(path)?;
        return Ok(Details { target: r.target.to_string_lossy().into_owned(), working_dir: r.working_dir.to_string_lossy().into_owned(), ..Default::default() });
    }
    if !is_desktop(path) {
        anyhow::bail!("{} is not a link", path.display());
    }
    let keys = entry_keys(&std::fs::read_to_string(path)?);
    let get = |k: &str| keys.get(k).cloned().unwrap_or_default();
    let (target, args) = if keys.get("Type").is_some_and(|t| t == "Link") {
        (url_decode(get("URL").strip_prefix("file://").unwrap_or(&get("URL"))), String::new())
    } else {
        let words = crate::openwith::exec_words(&get("Exec"), Path::new(""));
        let mut it = words.into_iter().map(|w| w.to_string_lossy().into_owned()).filter(|w| !w.is_empty());
        let exe = it.next().unwrap_or_default();
        (exe, it.collect::<Vec<_>>().join(" "))
    };
    Ok(Details { target, args, working_dir: get("Path"), comment: get("Comment"), icon: get("Icon"), ..Default::default() })
}

/// `text` with `key` set to `value` in the `[Desktop Entry]` section (an empty value removes the key).
fn set_key(text: &str, key: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let (mut in_main, mut done) = (false, false);
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_main && !done && !value.is_empty() {
                out.push(format!("{key}={value}"));
                done = true;
            }
            in_main = t == "[Desktop Entry]";
        } else if in_main && t.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            if !done && !value.is_empty() {
                out.push(format!("{key}={value}"));
            }
            done = true;
            continue;
        }
        out.push(line.to_string());
    }
    if in_main && !done && !value.is_empty() {
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Writes the Shortcut tab back: a symlink is made to point at the new target (a new link beside it, then renamed over it); a
/// `.desktop` file gets its keys changed, the others stay as they were.
pub fn save(path: &Path, d: &Details) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        if d.target.is_empty() {
            anyhow::bail!("a link needs a target");
        }
        let tmp = path.with_extension(format!("leaf-tmp-{}", std::process::id()));
        std::os::unix::fs::symlink(&d.target, &tmp)?;
        return std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .map_err(Into::into);
    }
    if !is_desktop(path) {
        anyhow::bail!("{} is not a link", path.display());
    }
    let mut text = std::fs::read_to_string(path)?;
    if entry_keys(&text).get("Type").is_some_and(|t| t == "Link") {
        text = set_key(&text, "URL", &format!("file://{}", url_encode(&d.target)));
    } else {
        let mut exec = crate::default_fm::exec_quote(&d.target)?;
        if !d.args.is_empty() {
            exec.push(' ');
            exec.push_str(&d.args);
        }
        text = set_key(&text, "Exec", &exec);
        text = set_key(&text, "Path", &d.working_dir);
    }
    text = set_key(&text, "Comment", &d.comment);
    text = set_key(&text, "Icon", &d.icon);
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlinks_and_link_files_round_trip_in_a_temp_folder() {
        let root = std::env::temp_dir().join(format!("leaf-shortcut-{}", std::process::id()));
        let docs = root.join("my docs");
        std::fs::create_dir_all(&docs).unwrap();
        // a Windows-style name makes a plain symlink; the name is never taken over
        create(&root.join("docs - Shortcut.lnk"), &docs).unwrap();
        let link = root.join("docs - Shortcut");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(resolve(&link).unwrap().target, docs);
        assert!(create(&root.join("docs - Shortcut.lnk"), &docs).is_err(), "the name is taken");
        // a relative link is read against its own folder
        std::os::unix::fs::symlink("my docs", root.join("rel")).unwrap();
        assert_eq!(resolve(&root.join("rel")).unwrap().target, root.join("my docs"));
        // a .desktop link file keeps a path with a space and a percent sign
        let odd = root.join("a b%c");
        std::fs::write(&odd, b"").unwrap();
        let f = root.join("x.desktop");
        create(&f, &odd).unwrap();
        assert_eq!(resolve(&f).unwrap().target, odd);
        // saving changes only what the tab shows
        let mut d = details(&f).unwrap();
        d.comment = "hello".into();
        save(&f, &d).unwrap();
        assert_eq!(details(&f).unwrap().comment, "hello");
        assert_eq!(resolve(&f).unwrap().target, odd);
        let mut dl = details(&link).unwrap();
        dl.target = root.join("rel").to_string_lossy().into_owned();
        save(&link, &dl).unwrap();
        assert_eq!(resolve(&link).unwrap().target, root.join("rel"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
