//! "Open in terminal": which folder (the guards) and which terminal (the setting). The OS call is rf_win's.

use std::path::{Path, PathBuf};

use gpui_kit::App;
use rf_core::Location;

/// The folder a location's terminal opens in, from the location alone (no disk): a search or the disk usage view stands for
/// the folder under it; Home, the Recycle Bin, a library, a phone and the like have none. Not yet checked against the disk:
/// that is [`target`].
pub fn location_dir(loc: &Location) -> Option<PathBuf> {
    match loc {
        Location::DiskUsage(p) => Some(p.clone()),
        other => other.path().map(Path::to_path_buf),
    }
}

/// The real folder a terminal opens in for `p`: inside an archive the archive's folder, a folder that is gone its nearest
/// existing parent; none for FTP and a network root (`\server`, whose folders are shares, not places). Asks the disk:
/// run it off the UI thread.
pub fn target(p: &Path) -> Option<PathBuf> {
    let p = match crate::archive_view::split(p) {
        Some((archive, _)) => archive.parent()?.to_path_buf(),
        None => p.to_path_buf(),
    };
    if rf_win::ftp::is_ftp(&p) || rf_win::network::server_root(&p).is_some() {
        return None;
    }
    p.ancestors().filter(|a| !a.as_os_str().is_empty()).find(|a| a.is_dir()).map(Path::to_path_buf)
}

/// Whether a terminal button has anything to open for this location (from the location alone: it runs every frame).
pub fn offered(loc: &Location) -> bool {
    location_dir(loc).is_some_and(|p| !rf_win::ftp::is_ftp(&p) && rf_win::network::server_root(&p).is_none())
}

/// Opens the terminal of the settings in the folder for `p` (see [`target`]); nothing happens, quietly, when there is none.
pub fn open(p: PathBuf, cx: &App) {
    let s = cx.global::<crate::settings::Settings>();
    let (kind, custom) = (s.terminal, s.terminal_custom.clone());
    std::thread::spawn(move || {
        if let Some(dir) = target(&p) {
            if let Err(e) = rf_win::shell::open_terminal_with(kind, &custom, &dir) {
                log::warn!("terminal: {e:#}");
            }
        }
    });
}

/// [`open`] for the folder a location shows.
pub fn open_location(loc: &Location, cx: &App) {
    if let Some(p) = location_dir(loc) {
        open(p, cx);
    }
}

/// The elevated terminal (Windows asks for permission), in the folder for `p`.
pub fn open_admin(p: PathBuf) {
    std::thread::spawn(move || {
        if let Some(dir) = target(&p) {
            if let Err(e) = rf_win::shell::open_terminal_admin(&dir) {
                log::warn!("terminal (admin): {e:#}");
            }
        }
    });
}
