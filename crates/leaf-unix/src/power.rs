//! Sleep and shut down this PC, for "when the downloads are done": systemd's `systemctl suspend` / `poweroff` (the logged-in user
//! may do both). Where there is no `systemctl` the call is not available.

use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Sleep,
    Shutdown,
}

fn systemctl() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("systemctl")).find(|f| f.is_file())).or_else(|| ["/usr/bin/systemctl", "/bin/systemctl"].into_iter().map(std::path::PathBuf::from).find(|f| f.is_file()))
}

/// Whether this system has a call for it.
pub fn available(_: Action) -> bool {
    systemctl().is_some()
}

pub fn run(action: Action) -> Result<(), String> {
    let exe = systemctl().ok_or_else(|| "systemctl is not installed".to_string())?;
    let verb = match action {
        Action::Sleep => "suspend",
        Action::Shutdown => "poweroff",
    };
    Command::new(exe).arg(verb).stdin(Stdio::null()).spawn().map(|_| ()).map_err(|e| e.to_string())
}
