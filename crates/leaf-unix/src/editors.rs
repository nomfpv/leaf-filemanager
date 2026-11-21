//! Linux backend of rf_win::editors. Status: REAL. The code editors found on `PATH` from a short list of known ones (the Settings'
//! "open in editor" choice and the workspace's default). The list is kept like the Windows one: `peek` never blocks, `refresh` looks.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// TYPE-COPY(rf-win src/editors.rs:18)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub name: &'static str,
    pub exe: PathBuf,
}

/// The name we show and the program looked for on `PATH`, in the order they are offered.
const KNOWN: [(&str, &str); 12] = [
    ("Visual Studio Code", "code"),
    ("VSCodium", "codium"),
    ("Cursor", "cursor"),
    ("Zed", "zed"),
    ("Sublime Text", "subl"),
    ("Kate", "kate"),
    ("KWrite", "kwrite"),
    ("Text Editor", "gnome-text-editor"),
    ("gedit", "gedit"),
    ("IntelliJ IDEA", "idea"),
    ("RustRover", "rustrover"),
    ("Geany", "geany"),
];

const FRESH: Duration = Duration::from_secs(30);

struct State {
    list: Vec<Editor>,
    at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { list: Vec::new(), at: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

fn detect() -> Vec<Editor> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    KNOWN.iter().filter_map(|(name, exe)| dirs.iter().map(|d| d.join(exe)).find(|p| p.is_file()).map(|exe| Editor { name, exe })).collect()
}

pub fn peek() -> (Vec<Editor>, bool) {
    let s = state();
    (s.list.clone(), s.at.is_some_and(|t| t.elapsed() < FRESH))
}

pub fn refresh() -> bool {
    let list = detect();
    let mut s = state();
    let changed = s.list != list;
    *s = State { list, at: Some(Instant::now()) };
    changed
}

pub fn installed() -> Vec<Editor> {
    let (list, fresh) = peek();
    if fresh {
        return list;
    }
    refresh();
    peek().0
}

pub fn name_of(list: &[Editor], exe: &Path) -> Option<&'static str> {
    list.iter().find(|e| e.exe == exe).map(|e| e.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_the_editor_it_is() {
        let list = vec![Editor { name: "Kate", exe: PathBuf::from("/usr/bin/kate") }];
        assert_eq!(name_of(&list, Path::new("/usr/bin/kate")), Some("Kate"));
        assert_eq!(name_of(&list, Path::new("/usr/bin/vim")), None);
    }
}
