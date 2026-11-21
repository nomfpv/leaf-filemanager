//! The portable copy keeps its settings next to itself.
//!
//! WHY (issue 71: "The portable version should save the settings in the same folder as the executable and not inside the system's
//! AppData/Roaming folder"). Velopack's portable zip is a folder with `Update.exe`, a `.portable` file, and the program in `current\`;
//! an update replaces `current\` and nothing else. So the data goes into `data\` beside `current\`: it moves with the folder (a USB
//! stick, another PC) and survives updates.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The data folder of a portable copy started from `exe`: `<root>\data` when `exe` is in `<root>\current` and `<root>\.portable`
/// exists (`is_file` says whether a path is a file).
pub fn data_dir_for(exe: &Path, is_file: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let current = exe.parent()?;
    if !current.file_name()?.to_string_lossy().eq_ignore_ascii_case("current") {
        return None;
    }
    let root = current.parent()?;
    is_file(&root.join(".portable")).then(|| root.join("data"))
}

/// This program's data folder when it runs as the portable copy (made the first time), else `None`.
pub fn data_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let dir = data_dir_for(&exe, &|p| p.is_file())?;
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        Some(dir)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_copy_in_a_portable_folder_has_a_data_folder_of_its_own() {
        let marker = |root: &'static str| move |p: &Path| p == Path::new(root).join(".portable");
        let exe = Path::new(r"E:\Tools\leaf\current\leaf.exe");
        assert_eq!(data_dir_for(exe, &marker(r"E:\Tools\leaf")), Some(PathBuf::from(r"E:\Tools\leaf\data")));
        assert_eq!(data_dir_for(exe, &|_| false), None, "an installed copy has no .portable beside current");
        assert_eq!(data_dir_for(Path::new(r"E:\build\leaf.exe"), &|_| true), None, "a build is not in current");
    }
}
