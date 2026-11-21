//! Linux backend of rf_win::apps. Status: REAL. The programs of the desktop: every visible `.desktop` entry of type Application in the
//! `applications` folders of the XDG data dirs (the user's first), by name. Started from the Exec line by argument list, no shell.

use std::path::Path;

use crate::openwith::{data_dirs, exec_words_for, parse_entry, read_entry};

// TYPE-COPY(rf-win src/apps.rs:16)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub name: String,
    /// The desktop entry's id (`org.kde.kate.desktop`).
    pub id: String,
}

impl App {
    /// What names the app to the icon and launch code: its desktop entry id.
    pub fn parsing(&self) -> String {
        self.id.clone()
    }
}

/// Entries of one `applications` folder: the file's id is its path below the folder with `/` written `-`.
fn collect(root: &Path, dir: &Path, out: &mut Vec<App>, seen: &mut std::collections::HashSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect(root, &path, out, seen);
            continue;
        }
        if path.extension().is_none_or(|x| x != "desktop") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let id = rel.to_string_lossy().replace('/', "-");
        if !seen.insert(id.clone()) {
            continue; // an earlier (user) folder's entry of this id wins, hidden or not
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Some(a) = parse_entry(&id, &text) {
            out.push(App { name: a.name, id });
        }
    }
}

pub fn list() -> anyhow::Result<Vec<App>> {
    let (mut out, mut seen) = (Vec::new(), std::collections::HashSet::new());
    for d in data_dirs() {
        let root = d.join("applications");
        collect(&root, &root, &mut out, &mut seen);
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    Ok(out)
}

pub fn launch(app: &App) -> anyhow::Result<()> {
    let entry = read_entry(&app.id).ok_or_else(|| anyhow::anyhow!("{} is not an installed program", app.name))?;
    let words = exec_words_for(&entry.exec, None);
    let (prog, args) = words.split_first().ok_or_else(|| anyhow::anyhow!("{} has no command", app.name))?;
    let mut c = std::process::Command::new(prog);
    c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = c.spawn().map_err(|e| anyhow::anyhow!("{}: {e}", app.name))?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}
