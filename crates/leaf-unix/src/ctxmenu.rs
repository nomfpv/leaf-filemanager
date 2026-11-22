//! Linux backend of rf_win::ctxmenu. Status: PARTIAL (build / invoke are real; the native popups do not apply; never run on a desktop)
//!
//! leaf draws its own menu; this file supplies what other programs add to it, the way a KDE file manager does:
//! - "Open with": the programs that list the item's media type (`openwith::apps_for`: mimeapps.list, mimeinfo.cache, `.desktop` files),
//!   as children with the verb `openas`, plus a last "Other application..." that asks through kdialog / zenity.
//! - KDE service menus: `.desktop` files under `kio/servicemenus`, `kservices5/ServiceMenus` and `kservices6/ServiceMenus` of the XDG data
//!   dirs. `MimeType=` (exact, `image/*`, `all/all`, `all/allfiles`, `inode/directory`) decides whether it applies to every item,
//!   `Actions=` lists the `[Desktop Action x]` groups, `X-KDE-Submenu` groups them under a submenu, `X-KDE-Priority=TopLevel` sorts them first,
//!   `X-KDE-MinNumberOfUrls` / `MaxNumberOfUrls` are honoured. Actions that need a terminal are not offered.
//! - For the empty area of a folder: a "New" submenu holding the `NewFolder` entry the app expects.
//!
//! A chosen entry runs its `Exec=` line as an argument vector (never through a shell) with the field codes expanded per the Desktop Entry
//! spec (`%f %F %u %U %c %i %k %%`; `%f`/`%u` start the program once per item). `show_for_*` / `show_simple` are the native popups of the
//! Windows shell: not applicable, they answer an error / nothing and the app shows its own menu. No icons are given (`icon: None`).

use crate::Owner;
use leaf_common::stub_err;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub use leaf_common::ctxmenu::{ExtraItem, MenuOutcome, ShellIcon, ShellItem, ShellTarget};

pub fn show_for_items(_owner: Owner, _items: &[PathBuf], _screen: (i32, i32), _extended: bool, _extra: &[ExtraItem]) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_items")
}

pub fn show_for_one(_owner: Owner, _item: &Path, _screen: (i32, i32), _extended: bool, _extra: &[ExtraItem]) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_one")
}

pub fn show_for_background(_owner: Owner, _dir: &Path, _screen: (i32, i32), _extended: bool, _extra: &[ExtraItem]) -> anyhow::Result<MenuOutcome> {
    stub_err!("ctxmenu::show_for_background")
}

/// What a chosen entry does.
#[derive(Debug, Clone)]
enum Act {
    /// Start this desktop entry's program on the items.
    OpenWith(crate::openwith::DesktopApp),
    /// Ask which program (kdialog / zenity) for the first item.
    Choose,
    /// A service-menu action: its `Exec=`, the entry's name and icon (for `%c` / `%i`) and the file it came from (`%k`).
    Service { exec: String, name: String, icon: String, file: PathBuf },
    /// Nothing to run (the `NewFolder` entry is the app's own).
    Nothing,
}

struct Session {
    id: u64,
    items: Vec<PathBuf>,
    acts: Vec<Act>,
}

fn sessions() -> &'static Mutex<Vec<Session>> {
    static S: Mutex<Vec<Session>> = Mutex::new(Vec::new());
    &S
}

/// The newest few menus stay invokable (an old menu's ids go with it).
const KEEP: usize = 6;

/// Builds the entries for `target`: `(session, items)`; `invoke(session, id)` runs one. Blocks (it reads `.desktop` files and may ask
/// `xdg-mime`): worker threads only.
pub fn build(_owner: Owner, target: &ShellTarget, _extended: bool) -> anyhow::Result<(u64, Vec<ShellItem>)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let (items, background): (Vec<PathBuf>, bool) = match target {
        ShellTarget::Items(v) => (v.clone(), false),
        ShellTarget::One(p) => (vec![p.clone()], false),
        ShellTarget::Background(d) => (vec![d.clone()], true),
    };
    anyhow::ensure!(!items.is_empty(), "nothing to build a menu for");
    let mut acts: Vec<Act> = Vec::new();
    let mut out: Vec<ShellItem> = Vec::new();
    let add = |acts: &mut Vec<Act>, label: String, verb: &str, act: Act| -> ShellItem {
        acts.push(act);
        ShellItem { id: acts.len() as u32, label, hint: None, verb: verb.to_string(), icon: None, enabled: true, separator: false, children: Vec::new() }
    };
    let mimes: Vec<String> = items.iter().enumerate().map(|(i, p)| mime_for(p, i == 0)).collect();

    if !background {
        // "Open with": one list when every item has the same type
        if mimes.iter().all(|m| *m == mimes[0]) && mimes[0] != "inode/directory" {
            let mut kids: Vec<ShellItem> = crate::openwith::apps_for(&mimes[0]).into_iter().take(12).map(|a| add(&mut acts, a.name.clone(), "openas", Act::OpenWith(a))).collect();
            if !kids.is_empty() {
                kids.push(ShellItem { separator: true, ..empty_item() });
            }
            kids.push(add(&mut acts, "Other application...".to_string(), "openas", Act::Choose));
            out.push(ShellItem { label: "Open with".to_string(), children: kids, ..empty_item() });
        }
    } else {
        // the app puts its own Folder and Text entries in, and shows what else comes after them
        let new = add(&mut acts, "Folder".to_string(), "NewFolder", Act::Nothing);
        out.push(ShellItem { label: "New".to_string(), children: vec![new], ..empty_item() });
    }

    // KDE service menus
    let menus = service_menus();
    let mut top: Vec<ShellItem> = Vec::new();
    let mut plain: Vec<ShellItem> = Vec::new();
    let mut groups: Vec<(String, Vec<ShellItem>)> = Vec::new();
    for m in menus.iter().filter(|m| m.applies(&items, &mimes)) {
        for a in &m.actions {
            let it = add(&mut acts, a.name.clone(), "", Act::Service { exec: a.exec.clone(), name: a.name.clone(), icon: a.icon.clone(), file: m.file.clone() });
            match &m.submenu {
                Some(s) => match groups.iter_mut().find(|(n, _)| n == s) {
                    Some((_, v)) => v.push(it),
                    None => groups.push((s.clone(), vec![it])),
                },
                None if m.top_level => top.push(it),
                None => plain.push(it),
            }
        }
    }
    let service: Vec<ShellItem> =
        top.into_iter().chain(groups.into_iter().map(|(n, kids)| ShellItem { label: n, children: kids, ..empty_item() })).chain(plain).collect();
    if !service.is_empty() && !out.is_empty() {
        out.push(ShellItem { separator: true, ..empty_item() });
    }
    out.extend(service);

    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut s = sessions().lock().unwrap_or_else(|e| e.into_inner());
    s.push(Session { id, items, acts });
    if s.len() > KEEP {
        s.remove(0);
    }
    Ok((id, out))
}

fn empty_item() -> ShellItem {
    ShellItem { id: 0, label: String::new(), hint: None, verb: String::new(), icon: None, enabled: true, separator: false, children: Vec::new() }
}

/// The media type of an item: folders are `inode/directory`; the first file asks `xdg-mime` (it looks inside the file), the rest go by name.
fn mime_for(p: &Path, first: bool) -> String {
    if p.is_dir() {
        return "inode/directory".to_string();
    }
    let m = if first { crate::openwith::mime_of(p) } else { crate::fileinfo::mime_guess(p).map(str::to_string) };
    m.unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Runs the entry `id` of a menu built earlier. `Err` when the menu is gone, the entry is not one that runs, or the program did not start.
pub fn invoke(_owner: Owner, session: u64, id: u32, _screen: (i32, i32)) -> anyhow::Result<MenuOutcome> {
    let (items, act) = {
        let s = sessions().lock().unwrap_or_else(|e| e.into_inner());
        let sess = s.iter().find(|x| x.id == session).ok_or_else(|| anyhow::anyhow!("this menu is gone"))?;
        let act = sess.acts.get((id as usize).wrapping_sub(1)).cloned().ok_or_else(|| anyhow::anyhow!("no such entry"))?;
        (sess.items.clone(), act)
    };
    match act {
        Act::OpenWith(app) => {
            spawn_all(&app.exec, &app.name, "", None, &items)?;
            Ok(MenuOutcome::Invoked("openas".into()))
        }
        Act::Choose => {
            crate::openwith::open_with(&items[0], false)?;
            Ok(MenuOutcome::Invoked("openas".into()))
        }
        Act::Service { exec, name, icon, file } => {
            spawn_all(&exec, &name, &icon, Some(&file), &items)?;
            Ok(MenuOutcome::Invoked(name))
        }
        Act::Nothing => Ok(MenuOutcome::Dismissed),
    }
}

/// A verb of the Windows shell (`runas`...): there is no such thing here.
pub fn invoke_verb(_owner: Owner, _target: &ShellTarget, _verb: &str, _screen: (i32, i32)) -> anyhow::Result<()> {
    stub_err!("ctxmenu::invoke_verb")
}

/// The native popup of simple items: not applicable (the app's own menu is drawn).
pub fn show_simple(_owner: Owner, _screen: (i32, i32), _items: &[ExtraItem]) -> Option<u32> {
    leaf_common::stub!("ctxmenu::show_simple")
}

// ---- running an Exec line

/// Starts `exec` for `items` (once for all of them with `%F`/`%U`, once per item with `%f`/`%u`), detached, in the first item's folder.
fn spawn_all(exec: &str, name: &str, icon: &str, file: Option<&Path>, items: &[PathBuf]) -> anyhow::Result<()> {
    let cmds = expand_exec(exec, items, name, icon, file);
    anyhow::ensure!(!cmds.is_empty(), "{name} has no command");
    for words in cmds {
        let (prog, args) = words.split_first().ok_or_else(|| anyhow::anyhow!("{name} has no command"))?;
        let mut c = Command::new(prog);
        c.args(args);
        if let Some(dir) = items.first().and_then(|p| if p.is_dir() { Some(p.as_path()) } else { p.parent() }) {
            c.current_dir(dir);
        }
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = c.spawn().map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        std::thread::Builder::new().name("reaper".into()).spawn(move || {
            let _ = child.wait();
        })?;
    }
    Ok(())
}

/// The words of an `Exec` value (Desktop Entry spec: double quotes group, a backslash inside them escapes `"`, `` ` ``, `$` and itself;
/// white space outside quotes separates). Field codes are left in the words.
pub(crate) fn split_exec(exec: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let (mut cur, mut have, mut quoted) = (String::new(), false, false);
    let mut chars = exec.chars();
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
    words
}

/// `/a b/c` -> `file:///a%20b/c`.
fn file_url(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// The commands to run for `exec`: each word's field codes are filled in. `%F`/`%U` alone as a word become every item; `%f`/`%u` make one
/// command per item (the whole line repeated); inside a longer word (`--file=%f`) the code is that item's. No file code: one command
/// without files (the spec appends none). `%i` is `--icon <Icon>` (nothing without an icon), `%c` the name, `%k` the entry's file,
/// `%%` a percent sign; the deprecated codes are dropped.
pub(crate) fn expand_exec(exec: &str, items: &[PathBuf], name: &str, icon: &str, file: Option<&Path>) -> Vec<Vec<OsString>> {
    let words = split_exec(exec);
    let per_item = words.iter().any(|w| w.contains("%f") || w.contains("%u")) && !words.iter().any(|w| w == "%F" || w == "%U");
    let runs: Vec<Option<&PathBuf>> = if per_item { items.iter().map(Some).collect() } else { vec![None] };
    let mut out = Vec::new();
    for one in runs {
        let mut cmd: Vec<OsString> = Vec::new();
        for w in &words {
            match w.as_str() {
                "%F" => cmd.extend(items.iter().map(|p| p.as_os_str().to_owned())),
                "%U" => cmd.extend(items.iter().map(|p| OsString::from(file_url(p)))),
                "%i" => {
                    if !icon.is_empty() {
                        cmd.push("--icon".into());
                        cmd.push(icon.into());
                    }
                }
                w if w.contains('%') => {
                    let mut s = String::new();
                    let mut it = w.chars();
                    while let Some(c) = it.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match it.next() {
                            Some('%') => s.push('%'),
                            Some('f') | Some('F') => {
                                if let Some(p) = one.or(items.first()) {
                                    s.push_str(&p.to_string_lossy());
                                }
                            }
                            Some('u') | Some('U') => {
                                if let Some(p) = one.or(items.first()) {
                                    s.push_str(&file_url(p));
                                }
                            }
                            Some('c') => s.push_str(name),
                            Some('k') => {
                                if let Some(f) = file {
                                    s.push_str(&f.to_string_lossy());
                                }
                            }
                            _ => {}
                        }
                    }
                    // a word that was only a single-file code in a per-item run is that item's path
                    if w == "%f" || w == "%u" {
                        if let Some(p) = one.or(items.first()) {
                            cmd.push(if w == "%f" { p.as_os_str().to_owned() } else { OsString::from(file_url(p)) });
                        }
                    } else if !s.is_empty() {
                        cmd.push(s.into());
                    }
                }
                w => cmd.push(w.into()),
            }
        }
        if !cmd.is_empty() {
            out.push(cmd);
        }
    }
    out
}

// ---- KDE service menus

struct ServiceAction {
    name: String,
    exec: String,
    icon: String,
}

struct ServiceMenu {
    file: PathBuf,
    mime: Vec<String>,
    submenu: Option<String>,
    top_level: bool,
    min: usize,
    max: usize,
    actions: Vec<ServiceAction>,
}

impl ServiceMenu {
    fn applies(&self, items: &[PathBuf], mimes: &[String]) -> bool {
        let n = items.len();
        if n < self.min || (self.max > 0 && n > self.max) {
            return false;
        }
        mimes.iter().all(|m| self.mime.iter().any(|pat| mime_matches(pat, m)))
    }
}

/// Whether a service menu's `MimeType` pattern covers `mime`. `all/all`: everything; `all/allfiles`: everything but folders; `x/*`: a
/// family. (Parent types such as `text/plain` for `text/x-python` are not followed.)
fn mime_matches(pattern: &str, mime: &str) -> bool {
    let dir = mime == "inode/directory";
    match pattern {
        "all/all" | "all/*" => true,
        "all/allfiles" | "allfiles" => !dir,
        p if p.ends_with("/*") => mime.starts_with(&p[..p.len() - 1]),
        p => p.eq_ignore_ascii_case(mime),
    }
}

/// `[section] -> key -> value` of a `.desktop` file (keys keep their `[lang]` suffix; the first value of a key wins).
fn parse_desktop(text: &str) -> Vec<(String, HashMap<String, String>)> {
    let mut out: Vec<(String, HashMap<String, String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(h) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push((h.to_string(), HashMap::new()));
        } else if let Some((k, v)) = line.split_once('=')
            && let Some((_, m)) = out.last_mut()
        {
            m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    out
}

/// The locale suffixes to look for, most specific first: `de_DE.UTF-8@x` -> `de_DE`, `de`.
fn lang_keys() -> Vec<String> {
    let l = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "C" && v != "POSIX")).unwrap_or_default();
    let base = l.split(['.', '@']).next().unwrap_or("").to_string();
    let mut v = Vec::new();
    if !base.is_empty() {
        v.push(base.clone());
        if let Some((lang, _)) = base.split_once('_') {
            v.push(lang.to_string());
        }
    }
    v
}

/// `Name` in the user's language when the file has it.
fn localized(m: &HashMap<String, String>, key: &str, langs: &[String]) -> Option<String> {
    langs.iter().find_map(|l| m.get(&format!("{key}[{l}]"))).or_else(|| m.get(key)).cloned().filter(|s| !s.is_empty())
}

fn list_field(m: &HashMap<String, String>, key: &str) -> Vec<String> {
    m.get(key).map(|v| v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

/// The service menus on this machine, the user's own first; a file name found in an earlier folder hides the same name in a later one.
fn service_menus() -> Vec<ServiceMenu> {
    let langs = lang_keys();
    let mut seen: Vec<std::ffi::OsString> = Vec::new();
    let mut out = Vec::new();
    for d in crate::openwith::data_dirs() {
        for sub in ["kio/servicemenus", "kservices5/ServiceMenus", "kservices6/ServiceMenus"] {
            let Ok(rd) = std::fs::read_dir(d.join(sub)) else { continue };
            let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "desktop")).collect();
            files.sort();
            for f in files {
                let Some(n) = f.file_name().map(|n| n.to_owned()) else { continue };
                if seen.contains(&n) {
                    continue;
                }
                seen.push(n);
                if let Ok(text) = std::fs::read_to_string(&f)
                    && let Some(m) = parse_service_menu(&f, &text, &langs)
                {
                    out.push(m);
                }
            }
        }
    }
    out
}

fn parse_service_menu(file: &Path, text: &str, langs: &[String]) -> Option<ServiceMenu> {
    let sections = parse_desktop(text);
    let (_, main) = sections.iter().find(|(n, _)| n == "Desktop Entry")?;
    if main.get("Hidden").is_some_and(|v| v == "true") || main.get("NoDisplay").is_some_and(|v| v == "true") {
        return None;
    }
    let only = list_field(main, "OnlyShowIn");
    if (!only.is_empty() && !only.iter().any(|d| d == "KDE")) || list_field(main, "NotShowIn").iter().any(|d| d == "KDE") {
        return None;
    }
    if let Some(t) = main.get("TryExec").filter(|t| !t.is_empty())
        && crate::shell::which(t).is_none()
    {
        return None;
    }
    let mut mime = list_field(main, "MimeType");
    // (the old spelling: media types among the service types)
    for key in ["X-KDE-ServiceTypes", "ServiceTypes"] {
        mime.extend(m_list_commas(main, key).into_iter().filter(|t| t.contains('/') && !t.starts_with("KonqPopupMenu/")));
    }
    if mime.is_empty() {
        return None;
    }
    let mut actions = Vec::new();
    for id in list_field(main, "Actions") {
        let Some((_, a)) = sections.iter().find(|(n, _)| *n == format!("Desktop Action {id}")) else { continue };
        let (Some(name), Some(exec)) = (localized(a, "Name", langs), a.get("Exec").filter(|e| !e.is_empty())) else { continue };
        if a.get("Terminal").is_some_and(|v| v == "true") || main.get("Terminal").is_some_and(|v| v == "true") {
            continue;
        }
        actions.push(ServiceAction { name, exec: exec.clone(), icon: a.get("Icon").cloned().unwrap_or_default() });
    }
    if actions.is_empty() {
        return None;
    }
    Some(ServiceMenu {
        file: file.to_path_buf(),
        mime,
        submenu: localized(main, "X-KDE-Submenu", langs),
        top_level: main.get("X-KDE-Priority").is_some_and(|v| v.eq_ignore_ascii_case("TopLevel")),
        min: main.get("X-KDE-MinNumberOfUrls").and_then(|v| v.parse().ok()).unwrap_or(0),
        max: main.get("X-KDE-MaxNumberOfUrls").and_then(|v| v.parse().ok()).unwrap_or(0),
        actions,
    })
}

fn m_list_commas(m: &HashMap<String, String>, key: &str) -> Vec<String> {
    m.get(key).map(|v| v.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn exec_lines_split_and_expand_without_a_shell() {
        assert_eq!(split_exec("\"/opt/my app/run\" --x \"a \\\"b\\\"\" %F"), vec!["/opt/my app/run", "--x", "a \"b\"", "%F"]);
        let items = vec![PathBuf::from("/t/a b.txt"), PathBuf::from("/t/c.txt")];
        // %F: one run, every file
        assert_eq!(expand_exec("kate %F", &items, "Kate", "", None), vec![os(&["kate", "/t/a b.txt", "/t/c.txt"])]);
        // %U: the addresses
        assert_eq!(expand_exec("x %U", &items[..1], "X", "", None), vec![os(&["x", "file:///t/a%20b.txt"])]);
        // %f: once per item
        assert_eq!(expand_exec("ed --file=%f", &items, "Ed", "", None), vec![os(&["ed", "--file=/t/a b.txt"]), os(&["ed", "--file=/t/c.txt"])]);
        assert_eq!(expand_exec("v %f", &items, "V", "", None), vec![os(&["v", "/t/a b.txt"]), os(&["v", "/t/c.txt"])]);
        // %i, %c, %k, %%
        assert_eq!(expand_exec("t %i --name %c --k=%k 100%%", &items[..1], "My Tool", "tool-icon", Some(Path::new("/s/t.desktop"))), vec![os(&["t", "--icon", "tool-icon", "--name", "My Tool", "--k=/s/t.desktop", "100%"])]);
        // no file code: no file
        assert_eq!(expand_exec("hello", &items, "H", "", None), vec![os(&["hello"])]);
    }

    #[test]
    fn a_service_menu_file_is_read_and_filtered() {
        let text = "[Desktop Entry]\nType=Service\nMimeType=image/*;inode/directory;\nActions=a;b;t;\nX-KDE-Submenu=Tools\nX-KDE-Submenu[de]=Werkzeuge\nX-KDE-Priority=TopLevel\n\n[Desktop Action a]\nName=Do A\nName[de]=Tu A\nExec=doa %F\nIcon=ia\n\n[Desktop Action b]\nName=No exec\n\n[Desktop Action t]\nName=In terminal\nExec=top\nTerminal=true\n";
        let m = parse_service_menu(Path::new("/x/m.desktop"), text, &["de_DE".to_string(), "de".to_string()]).unwrap();
        assert_eq!((m.submenu.as_deref(), m.top_level, m.actions.len()), (Some("Werkzeuge"), true, 1));
        assert_eq!((m.actions[0].name.as_str(), m.actions[0].exec.as_str(), m.actions[0].icon.as_str()), ("Tu A", "doa %F", "ia"));
        let imgs = vec!["image/png".to_string(), "image/jpeg".to_string()];
        let two = vec![PathBuf::from("/a.png"), PathBuf::from("/b.jpg")];
        assert!(m.applies(&two, &imgs));
        assert!(!m.applies(&two, &["image/png".to_string(), "text/plain".to_string()]));
        assert!(mime_matches("all/allfiles", "text/plain") && !mime_matches("all/allfiles", "inode/directory"));
        assert!(mime_matches("all/all", "inode/directory") && mime_matches("text/plain", "TEXT/plain"));
    }
}
