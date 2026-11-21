//! "Open with" on Linux: the programs that list the file's media type in their desktop entries, a chooser (kdialog or zenity,
//! when installed) and the start of the chosen one. The default program comes from `xdg-mime` and the `mimeapps.list` files.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One desktop entry that can open the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// `org.kde.kate.desktop`
    pub id: String,
    pub name: String,
    pub exec: String,
}

pub(crate) fn data_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        Some(h) => v.push(PathBuf::from(h)),
        None => {
            if let Some(h) = dirs::home_dir() {
                v.push(h.join(".local/share"));
            }
        }
    }
    let rest = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    v.extend(rest.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v
}

pub(crate) fn config_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    match std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(h) => v.push(PathBuf::from(h)),
        None => {
            if let Some(h) = dirs::home_dir() {
                v.push(h.join(".config"));
            }
        }
    }
    let rest = std::env::var("XDG_CONFIG_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    v.extend(rest.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v
}

/// A program on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// The media type of a file: `xdg-mime` when installed (it looks at the content too), else the extension guess.
pub fn mime_of(path: &Path) -> Option<String> {
    if let Some(x) = which("xdg-mime")
        && let Ok(out) = Command::new(x).args(["query", "filetype"]).arg(path).stdin(Stdio::null()).stderr(Stdio::null()).output()
    {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && t.contains('/') {
            return Some(t);
        }
    }
    crate::fileinfo::mime_guess(path).map(str::to_string)
}

/// Desktop-entry ids named for `mime` in the `[section]` of the `mimeapps.list` files (config and data dirs, user first).
pub(crate) fn mimeapps_ids(section: &str, mime: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = Vec::new();
    for c in config_dirs() {
        files.push(c.join("mimeapps.list"));
    }
    for d in data_dirs() {
        files.push(d.join("applications/mimeapps.list"));
    }
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let mut in_section = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_section = line == format!("[{section}]");
            } else if in_section && let Some((k, v)) = line.split_once('=') && k.trim() == mime {
                out.extend(v.split(';').filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
    }
    out
}

/// Desktop-entry ids from the `mimeinfo.cache` files for `mime`.
fn cache_ids(mime: &str) -> Vec<String> {
    let mut out = Vec::new();
    for d in data_dirs() {
        let Ok(text) = std::fs::read_to_string(d.join("applications/mimeinfo.cache")) else { continue };
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') && k == mime {
                out.extend(v.split(';').filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
    }
    out
}

/// The entry for `id` (`a.desktop`; `a-b.desktop` may be the file `a/b.desktop`), when it is one to show.
pub(crate) fn read_entry(id: &str) -> Option<DesktopApp> {
    let text = data_dirs().into_iter().find_map(|d| {
        let p = d.join("applications").join(id);
        std::fs::read_to_string(&p).ok().or_else(|| std::fs::read_to_string(d.join("applications").join(id.replacen('-', "/", 1))).ok())
    })?;
    parse_entry(id, &text)
}

pub(crate) fn parse_entry(id: &str, text: &str) -> Option<DesktopApp> {
    let (mut name, mut exec, mut hidden, mut kind_ok) = (None, None, false, true);
    let mut in_main = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "NoDisplay" | "Hidden" => hidden |= v.trim() == "true",
            "Type" => kind_ok = v.trim() == "Application",
            _ => {}
        }
    }
    if hidden || !kind_ok {
        return None;
    }
    Some(DesktopApp { id: id.to_string(), name: name?, exec: exec? })
}

/// The program that opens this media type by default, when there is one.
pub fn default_app(mime: &str) -> Option<DesktopApp> {
    let mut ids = mimeapps_ids("Default Applications", mime);
    if ids.is_empty()
        && let Some(x) = which("xdg-mime")
        && let Ok(out) = Command::new(x).args(["query", "default", mime]).stdin(Stdio::null()).stderr(Stdio::null()).output()
    {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if t.ends_with(".desktop") {
            ids.push(t);
        }
    }
    ids.iter().find_map(|i| read_entry(i)).or_else(|| cache_ids(mime).iter().find_map(|i| read_entry(i)))
}

/// The programs that can open `mime`: the default first, then the added associations, then the rest, each once, by name inside the groups.
pub fn apps_for(mime: &str) -> Vec<DesktopApp> {
    let removed = mimeapps_ids("Removed Associations", mime);
    let mut ids: Vec<String> = mimeapps_ids("Default Applications", mime);
    ids.extend(mimeapps_ids("Added Associations", mime));
    let mut rest = cache_ids(mime);
    rest.sort();
    ids.extend(rest);
    let mut out: Vec<DesktopApp> = Vec::new();
    for id in ids {
        if removed.contains(&id) || out.iter().any(|a| a.id == id) {
            continue;
        }
        if let Some(a) = read_entry(&id) {
            out.push(a);
        }
    }
    out
}

/// The words of an `Exec` line (double quotes group; a backslash escapes inside them), with the field codes filled in: `%f %F %u %U` the
/// file, `%%` a percent sign, the other codes dropped. Without a file code the file is appended.
pub(crate) fn exec_words(exec: &str, file: &Path) -> Vec<std::ffi::OsString> {
    exec_words_for(exec, Some(file))
}

/// `exec_words` for a start without a file (the apps list): the file codes are dropped and nothing is appended.
pub(crate) fn exec_words_for(exec: &str, file: Option<&Path>) -> Vec<std::ffi::OsString> {
    let mut words: Vec<String> = Vec::new();
    let (mut cur, mut have, mut quoted) = (String::new(), false, false);
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if have {
                    words.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        words.push(cur);
    }
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    let mut gave_file = false;
    for w in words {
        match w.as_str() {
            "%f" | "%F" | "%u" | "%U" => {
                if let Some(f) = file {
                    out.push(f.as_os_str().to_owned());
                }
                gave_file = true;
            }
            "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            w if w.contains('%') => {
                // a code inside a word (--file=%f): the file's path in place, the rest of the codes dropped
                let mut s = String::new();
                let mut it = w.chars();
                while let Some(c) = it.next() {
                    if c != '%' {
                        s.push(c);
                        continue;
                    }
                    match it.next() {
                        Some('%') => s.push('%'),
                        Some('f' | 'F' | 'u' | 'U') => {
                            if let Some(f) = file {
                                s.push_str(&f.to_string_lossy());
                            }
                            gave_file = true;
                        }
                        _ => {}
                    }
                }
                out.push(s.into());
            }
            w => out.push(w.into()),
        }
    }
    if !gave_file && let Some(f) = file {
        out.push(f.as_os_str().to_owned());
    }
    out
}

/// Starts the program of the desktop entry `id` (`org.kde.kate.desktop`) on `path`, detached.
pub fn launch_id(id: &str, path: &Path) -> anyhow::Result<()> {
    let app = read_entry(id).ok_or_else(|| anyhow::anyhow!("{id} is not an installed program"))?;
    launch(&app, path)
}

/// Starts `app` on `path`, detached.
pub fn launch(app: &DesktopApp, path: &Path) -> anyhow::Result<()> {
    let words = exec_words(&app.exec, path);
    let (prog, args) = words.split_first().ok_or_else(|| anyhow::anyhow!("{} has no command", app.id))?;
    let mut c = Command::new(prog);
    c.args(args);
    if let Some(dir) = path.parent() {
        c.current_dir(dir);
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().map_err(|e| anyhow::anyhow!("{}: {e}", app.name))?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

/// Asks which of `apps` to use: kdialog's or zenity's list, whichever is installed. `Ok(None)` = the user closed it.
fn choose(apps: &[DesktopApp], file: &Path) -> anyhow::Result<Option<usize>> {
    let title = "Open with";
    let text = format!("Open {} with", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    if let Some(k) = which("kdialog") {
        let mut c = Command::new(k);
        c.args(["--title", title, "--menu", &text]);
        for (i, a) in apps.iter().enumerate() {
            c.arg(i.to_string()).arg(&a.name);
        }
        let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output()?;
        return Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten());
    }
    if let Some(z) = which("zenity") {
        let mut c = Command::new(z);
        c.args(["--list", "--title", title, "--text", &text, "--column", "#", "--column", "Program", "--hide-column", "1", "--print-column", "1"]);
        for (i, a) in apps.iter().enumerate() {
            c.arg(i.to_string()).arg(&a.name);
        }
        let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output()?;
        return Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten());
    }
    anyhow::bail!("neither kdialog nor zenity is installed to choose a program")
}

/// The chooser for `path`'s media type, then the chosen program is started (`launch`) or, with `make_default`, made the type's default
/// (`xdg-mime default`) instead. `Ok(false)` = the user chose nothing.
pub fn open_with(path: &Path, make_default: bool) -> anyhow::Result<bool> {
    let mime = mime_of(path).ok_or_else(|| anyhow::anyhow!("the file's type is not known"))?;
    let apps = apps_for(&mime);
    anyhow::ensure!(!apps.is_empty(), "no program lists {mime}");
    let Some(i) = choose(&apps, path)? else { return Ok(false) };
    let app = apps.get(i).ok_or_else(|| anyhow::anyhow!("no such choice"))?;
    if make_default {
        let x = which("xdg-mime").ok_or_else(|| anyhow::anyhow!("xdg-mime is not installed"))?;
        let st = Command::new(x).args(["default", &app.id, &mime]).stdin(Stdio::null()).status()?;
        anyhow::ensure!(st.success(), "xdg-mime could not set the default");
    } else {
        launch(app, path)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_get_the_file_in_place_of_their_codes() {
        let f = Path::new("/t/a b.txt");
        assert_eq!(exec_words("kate -b %U", f), vec!["kate", "-b", "/t/a b.txt"]);
        assert_eq!(exec_words("\"/opt/x y/app\" --name=%c %i", f), vec!["/opt/x y/app", "--name=", "/t/a b.txt"]);
        assert_eq!(exec_words("ed --file=%f", f), vec!["ed", "--file=/t/a b.txt"]);
        let e = parse_entry("a.desktop", "[Desktop Entry]\nType=Application\nName=A\nExec=a %f\n[Desktop Action x]\nName=No\n").unwrap();
        assert_eq!((e.name.as_str(), e.exec.as_str()), ("A", "a %f"));
        assert!(parse_entry("h.desktop", "[Desktop Entry]\nName=H\nExec=h\nNoDisplay=true\n").is_none());
    }
}
