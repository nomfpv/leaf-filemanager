//! Sleep and shut down this PC, for "when the downloads are done": systemd's `systemctl suspend` / `poweroff` (the logged-in user
//! may do both). Where there is no `systemctl` the call is not available.

use std::process::Command;

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
    // (the request reaches systemd at once; systemctl is a helper, so it also ends with leaf and is stopped after a minute)
    let mut c = Command::new(exe);
    c.arg(verb);
    let mut child = leaf_proc::spawn_helper(&mut c).map_err(|e| e.to_string())?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    }).map_err(|e| e.to_string())?;
    Ok(())
}
