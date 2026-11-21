//! "Open in terminal": which folder (the guards) and which terminal (the setting). The OS call is leaf_sys's.

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
    if leaf_sys::ftp::is_ftp(&p) || leaf_sys::network::server_root(&p).is_some() {
        return None;
    }
    p.ancestors().filter(|a| !a.as_os_str().is_empty()).find(|a| a.is_dir()).map(Path::to_path_buf)
}

/// The `ssh` arguments that open a login shell in a server folder, as a list (no shell string is built from the server's names
/// on this side): `-l user`, `-p port`, a terminal (`-t`), `--` before the host (a host cannot pass for an option), and one remote
/// command, `cd` to the folder (quoted for the remote shell) and then the user's login shell. `None` for anything but an
/// `sftp://` place with a host and user name that can be passed on as they are.
pub fn ssh_args(v: &rf_core::vpath::VPath) -> Option<Vec<std::ffi::OsString>> {
    if v.scheme() != "sftp" || !leaf_remote::known_hosts::valid_host(v.host()) {
        return None;
    }
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if let Some(user) = v.user() {
        if user.starts_with('-') || user.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '\'' | '"')) {
            return None;
        }
        args.extend(["-l".into(), user.into()]);
    }
    if let Some(port) = v.port() {
        args.extend(["-p".into(), port.to_string().into()]);
    }
    args.extend(["-t".into(), "--".into(), v.host().into(), format!("cd -- {} && exec \"${{SHELL:-/bin/sh}}\" -l", sh_quote(v.path())).into()]);
    Some(args)
}

/// `s` as one word for a POSIX shell: in single quotes, a quote inside as `'''`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Whether a terminal button has anything to open for this location (from the location alone: it runs every frame).
pub fn offered(loc: &Location) -> bool {
    if let Location::Remote(v) = loc {
        return ssh_args(v).is_some();
    }
    location_dir(loc).is_some_and(|p| !leaf_sys::ftp::is_ftp(&p) && leaf_sys::network::server_root(&p).is_none())
}

/// Opens the terminal of the settings in the folder for `p` (see [`target`]); nothing happens, quietly, when there is none.
pub fn open(p: PathBuf, cx: &App) {
    let s = cx.global::<crate::settings::Settings>();
    let (kind, custom) = (s.terminal, s.terminal_custom.clone());
    std::thread::spawn(move || {
        if let Some(dir) = target(&p) {
            if let Err(e) = leaf_sys::shell::open_terminal_with(kind, &custom, &dir) {
                log::warn!("terminal: {e:#}");
            }
        }
    });
}

/// [`open`] for the folder a location shows.
pub fn open_location(loc: &Location, cx: &App) {
    if let Location::Remote(v) = loc {
        open_ssh(v, cx);
        return;
    }
    if let Some(p) = location_dir(loc) {
        open(p, cx);
    }
}

/// The elevated terminal (Windows asks for permission), in the folder for `p`.
pub fn open_admin(p: PathBuf) {
    std::thread::spawn(move || {
        if let Some(dir) = target(&p) {
            if let Err(e) = leaf_sys::shell::open_terminal_admin(&dir) {
                log::warn!("terminal (admin): {e:#}");
            }
        }
    });
}

/// The terminal of the settings, running the system `ssh` into the server folder (it asks for the password itself, in its window).
fn open_ssh(v: &rf_core::vpath::VPath, cx: &App) {
    let Some(args) = ssh_args(v) else { return };
    let kind = cx.global::<crate::settings::Settings>().terminal;
    std::thread::spawn(move || {
        let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
        if let Err(e) = leaf_sys::shell::open_terminal_running(kind, &home, std::path::Path::new("ssh"), &args, &[]) {
            log::warn!("terminal (ssh): {e:#}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_path_is_quoted_for_the_remote_shell() {
        assert_eq!(sh_quote("/srv/it's $HOME;rm -rf"), r"'/srv/it'\''s $HOME;rm -rf'");
        let v = rf_core::vpath::VPath::parse("sftp://me@nas.local:2222/srv/it's here").unwrap();
        let a: Vec<String> = ssh_args(&v).unwrap().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&a[..6], ["-l", "me", "-p", "2222", "-t", "--"]);
        assert_eq!(a[6], "nas.local");
        assert!(a[7].starts_with(r"cd -- '/srv/it'\''s here' && exec "));
        assert!(ssh_args(&rf_core::vpath::VPath::parse("sftp://-oProxyCommand=x/").unwrap()).is_none());
    }
}
