//! Linux backend of rf_win::openrules. Status: REAL. Opening a file the way the user set it up: with their own program for its type
//! when there is one (`rf_core::openrules`), else with the desktop's default (`xdg-open`, which hands it to KDE's or GNOME's own
//! opener). Was a stub: a double click on a file (a photo, a PDF, a text file) opened nothing on Linux (first Linux tester).

use std::path::Path;
use std::sync::{Arc, RwLock};

use rf_core::openrules::{self, OpenRule};

static RULES: RwLock<Option<Arc<Vec<OpenRule>>>> = RwLock::new(None);

/// The user's lists, as the settings have them now.
pub fn set(rules: Vec<OpenRule>) {
    if let Ok(mut r) = RULES.write() {
        *r = Some(Arc::new(rules));
    }
}

/// The lists the process follows.
pub fn rules() -> Arc<Vec<OpenRule>> {
    RULES.read().ok().and_then(|r| r.clone()).unwrap_or_default()
}

/// The programs offered for this file under "Open with": the user's own for its type first, in their order, then the desktop's
/// (`mimeapps.list` and the `.desktop` files that list the media type) as their desktop-entry ids (`org.kde.kate.desktop`), each once.
pub fn programs_for(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = match openrules::ext_of(path) {
        Some(ext) => openrules::programs_for(&rules(), &ext).to_vec(),
        None => Vec::new(),
    };
    // the extension's type, not `xdg-mime` (this runs when a menu opens, on the interface thread)
    if let Some(mime) = crate::fileinfo::mime_guess(path) {
        for a in crate::openwith::apps_for(mime).into_iter().take(12) {
            if !out.contains(&a.id) {
                out.push(a.id);
            }
        }
    }
    out
}

// TYPE-COPY(rf-win src/openrules.rs:34)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Default,
    With(String),
    Fallback { program: String, error: String },
}

/// What a double click on this file opens it with: the user's program, or `None` for the desktop's default. A folder is never a rule's.
pub fn default_for(path: &Path, rules: &[OpenRule], is_dir: impl Fn(&Path) -> bool) -> Option<String> {
    let ext = openrules::ext_of(path)?;
    let program = openrules::default_program(rules, &ext)?;
    (!is_dir(path)).then(|| program.to_string())
}

/// Opens a file as a double click does: the user's program for its type, else the desktop's default; a program that cannot be
/// started falls back to the default and the answer says so.
pub fn open(path: &Path) -> anyhow::Result<Opened> {
    let Some(program) = default_for(path, &rules(), |p| p.is_dir()) else {
        crate::shell::open(path)?;
        return Ok(Opened::Default);
    };
    match open_in(&program, path) {
        Ok(()) => Ok(Opened::With(program)),
        Err(e) => {
            log::warn!("open rules: {program:?} could not be started for {path:?}: {e:#}");
            crate::shell::open(path)?;
            Ok(Opened::Fallback { program, error: format!("{e:#}") })
        }
    }
}

/// Starts `program` with the file as its one argument (no shell), in the file's folder, detached.
pub fn open_in(program: &str, file: &Path) -> anyhow::Result<()> {
    // a desktop entry's id (from `programs_for`): its Exec line, field codes filled in
    if program.ends_with(".desktop") && !program.contains('/') {
        return crate::openwith::launch_id(program, file);
    }
    let mut c = std::process::Command::new(program);
    c.arg(file).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Some(dir) = file.parent().filter(|d| d.is_dir()) {
        c.current_dir(dir);
    }
    let mut child = c.spawn()?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_double_click_takes_the_users_program_only_for_a_listed_type_and_a_missing_one_is_an_error() {
        let rules = [OpenRule { exts: vec!["jpg".into()], programs: vec!["/usr/bin/gwenview".into()], double_click: true }];
        assert_eq!(default_for(Path::new("/a/b.JPG"), &rules, |_| false), Some("/usr/bin/gwenview".into()));
        assert_eq!(default_for(Path::new("/a/b.txt"), &rules, |_| false), None);
        assert_eq!(default_for(Path::new("/a/b.jpg"), &rules, |_| true), None);
        assert!(open_in("/nonexistent/leaf-no-such-program", Path::new("/tmp")).is_err());
    }
}
