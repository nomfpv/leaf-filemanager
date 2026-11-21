//! Linux backend of rf_win::shell. Status: REAL for open (xdg-open) and the terminal; open-with, properties and the
//! admin terminal stay stubs.

use crate::Owner;
use leaf_common::{stub, stub_err};
use rf_core::terminal::Terminal;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn ensure_com() {
    stub!("shell::ensure_com")
}

/// Start a program detached from our streams; a thread waits for it so it never stays a zombie.
fn spawn_reaped(mut cmd: Command) -> anyhow::Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

/// A program on `PATH` (or an absolute path that is executable).
fn which(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let is_exe = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_exe(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| is_exe(p))
}

pub fn open(path: &Path) -> anyhow::Result<()> {
    let exe = which("xdg-open").ok_or_else(|| anyhow::anyhow!("xdg-open is not installed"))?;
    let mut c = Command::new(exe);
    c.arg(path);
    spawn_reaped(c)
}

pub fn open_with(_path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_with")
}

pub fn properties(_path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::properties")
}

pub fn open_terminal(dir: &Path) -> anyhow::Result<()> {
    open_terminal_with(Terminal::Auto, "", dir)
}

pub fn open_terminal_admin(_dir: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_terminal_admin")
}

pub fn resolve_shell_path(_s: &str) -> Option<std::path::PathBuf> {
    stub!("shell::resolve_shell_path")
}

pub fn suppress_critical_error_dialogs() {
    stub!("shell::suppress_critical_error_dialogs")
}

pub fn open_with_dialog(_owner: Owner, _path: &Path) -> anyhow::Result<()> {
    stub_err!("shell::open_with_dialog")
}

/// The program of a terminal kind, when installed (`$TERMINAL` for [`Terminal::EnvTerminal`]).
pub fn terminal_exe(kind: Terminal) -> Option<PathBuf> {
    match kind {
        Terminal::Konsole => which("konsole"),
        Terminal::Kitty => which("kitty"),
        Terminal::Alacritty => which("alacritty"),
        Terminal::Foot => which("foot"),
        Terminal::WezTerm => which("wezterm"),
        Terminal::Ghostty => which("ghostty"),
        Terminal::EnvTerminal => {
            let t = std::env::var("TERMINAL").ok()?;
            which(t.split_whitespace().next()?)
        }
        _ => None,
    }
}

/// The Linux terminals that can be started here, in the list's order.
pub fn installed_terminals() -> &'static [Terminal] {
    static CACHE: std::sync::OnceLock<Vec<Terminal>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        [Terminal::Konsole, Terminal::Kitty, Terminal::Alacritty, Terminal::Foot, Terminal::WezTerm, Terminal::Ghostty, Terminal::EnvTerminal]
            .into_iter()
            .filter(|k| terminal_exe(*k).is_some())
            .collect()
    })
}

/// The command that opens `exe` in `dir`.
fn terminal_command(kind: Terminal, exe: &Path, dir: &Path) -> Command {
    let mut c = Command::new(exe);
    match kind {
        Terminal::Konsole => {
            c.arg("--workdir").arg(dir);
        }
        Terminal::Kitty => {
            c.arg("--directory").arg(dir);
        }
        Terminal::Alacritty => {
            c.arg("--working-directory").arg(dir);
        }
        Terminal::Foot => {
            let mut a = OsString::from("--working-directory=");
            a.push(dir);
            c.arg(a);
        }
        Terminal::WezTerm => {
            c.arg("start").arg("--cwd").arg(dir);
        }
        Terminal::Ghostty => {
            let mut a = OsString::from("--working-directory=");
            a.push(dir);
            c.arg(a);
        }
        _ => {}
    }
    // every terminal also inherits the folder as its working directory
    c.current_dir(dir);
    c
}

pub fn open_terminal_with(kind: Terminal, custom: &str, dir: &Path) -> anyhow::Result<()> {
    if kind == Terminal::Custom {
        if custom.trim().is_empty() {
            anyhow::bail!("no custom terminal command is set");
        }
        let line = custom.replace("{path}", &dir.to_string_lossy());
        let mut c = Command::new("sh");
        c.arg("-c").arg(line).current_dir(dir);
        return spawn_reaped(c);
    }
    if kind == Terminal::EnvTerminal {
        let t = std::env::var("TERMINAL").map_err(|_| anyhow::anyhow!("$TERMINAL is not set"))?;
        let mut parts = t.split_whitespace();
        let exe = which(parts.next().unwrap_or("")).ok_or_else(|| anyhow::anyhow!("$TERMINAL names no installed program"))?;
        let mut c = Command::new(exe);
        c.args(parts).current_dir(dir);
        return spawn_reaped(c);
    }
    if let Some(exe) = terminal_exe(kind) {
        return spawn_reaped(terminal_command(kind, &exe, dir));
    }
    // automatic: $TERMINAL, then the installed ones, then gnome-terminal and xterm
    if std::env::var_os("TERMINAL").is_some() && terminal_exe(Terminal::EnvTerminal).is_some() {
        return open_terminal_with(Terminal::EnvTerminal, "", dir);
    }
    if let Some(k) = installed_terminals().iter().find(|k| **k != Terminal::EnvTerminal) {
        return open_terminal_with(*k, "", dir);
    }
    if let Some(exe) = which("gnome-terminal") {
        let mut c = Command::new(exe);
        c.arg("--working-directory").arg(dir).current_dir(dir);
        return spawn_reaped(c);
    }
    if let Some(exe) = which("xterm") {
        let mut c = Command::new(exe);
        c.current_dir(dir);
        return spawn_reaped(c);
    }
    anyhow::bail!("no terminal program found")
}


/// Runs `program` with `args` (and `env` set) inside the terminal `kind` in `dir`; the window stays open until Enter is pressed.
pub fn open_terminal_running(kind: Terminal, dir: &Path, program: &Path, args: &[OsString], env: &[(&str, OsString)]) -> anyhow::Result<()> {
    // sh runs the program, then waits so the output can be read
    let mut payload: Vec<OsString> = vec!["sh".into(), "-c".into(), "\"$0\" \"$@\"; echo; printf 'Press Enter to close '; read _".into(), program.into()];
    payload.extend(args.iter().cloned());
    let kind = if kind == Terminal::Custom { Terminal::Auto } else { kind };
    let mut candidates: Vec<Terminal> = Vec::new();
    if kind != Terminal::Auto {
        candidates.push(kind);
    }
    if std::env::var_os("TERMINAL").is_some() {
        candidates.push(Terminal::EnvTerminal);
    }
    candidates.extend(installed_terminals().iter().copied());
    for k in candidates {
        let Some(exe) = terminal_exe(k) else { continue };
        let mut c = terminal_command(k, &exe, dir);
        match k {
            Terminal::Kitty | Terminal::Foot => {}
            Terminal::WezTerm => {
                c.arg("--");
            }
            Terminal::EnvTerminal => {
                // $TERMINAL may carry its own words after the program
                let t = std::env::var("TERMINAL").unwrap_or_default();
                c.args(t.split_whitespace().skip(1)).arg("-e");
            }
            _ => {
                c.arg("-e");
            }
        }
        c.args(&payload).envs(env.iter().map(|(k, v)| (*k, v.clone())));
        return spawn_reaped(c);
    }
    if let Some(exe) = which("gnome-terminal") {
        let mut c = Command::new(exe);
        c.arg("--working-directory").arg(dir).arg("--").args(&payload).current_dir(dir).envs(env.iter().map(|(k, v)| (*k, v.clone())));
        return spawn_reaped(c);
    }
    if let Some(exe) = which("xterm") {
        let mut c = Command::new(exe);
        c.arg("-e").args(&payload).current_dir(dir).envs(env.iter().map(|(k, v)| (*k, v.clone())));
        return spawn_reaped(c);
    }
    anyhow::bail!("no terminal program found")
}
