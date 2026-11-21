//! Shell actions (open, properties). Each call initializes COM on the calling thread,
//! so run them on a worker thread — shell extensions can block for seconds.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rf_core::terminal::Terminal;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::Shell::{
    SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::wide::wide;

/// Initializes an STA on the current thread (idempotent; errors ignored when already initialized).
pub fn ensure_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
}

fn exec(path: &Path, verb: Option<&str>, args: Option<&str>) -> anyhow::Result<()> {
    ensure_com();
    let file = wide(path);
    let verb_w = verb.map(wide);
    let args_w = args.map(wide);
    let dir = path.parent().map(wide);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: args_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info)? };
    Ok(())
}

/// Starts a program (an `.exe`, a script, a shortcut) with a command line of its own and a working directory: what "Open with" does
/// for a program the user named. No error box of the shell's: the caller says what went wrong.
pub fn run_program(program: &Path, args: &str, dir: Option<&Path>) -> anyhow::Result<()> {
    ensure_com();
    let (file, args_w, dir_w) = (wide(program), wide(args), dir.map(wide));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        lpDirectory: dir_w.as_ref().map_or(PCWSTR::null(), |v| PCWSTR(v.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info)? };
    Ok(())
}

/// Opens a file with its default handler (same as double-click in Explorer).
pub fn open(path: &Path) -> anyhow::Result<()> {
    exec(path, None, None)
}

/// Shows the shell "Open with" dialog.
pub fn open_with(path: &Path) -> anyhow::Result<()> {
    exec(path, Some("openas"), None)
}

/// Shows the Windows properties dialog for the item.
pub fn properties(path: &Path) -> anyhow::Result<()> {
    exec(path, Some("properties"), None)
}

/// Opens a terminal (Windows Terminal if present, else PowerShell) in `dir`.
pub fn open_terminal(dir: &Path) -> anyhow::Result<()> {
    open_terminal_with(Terminal::Auto, "", dir)
}

/// A terminal start, ready to run: the program, its command line as written (no further quoting), the working directory (None: the
/// program's own default) and whether it needs a console of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub program: PathBuf,
    pub args: String,
    pub cwd: Option<PathBuf>,
    pub console: bool,
}

fn is_unc(dir: &Path) -> bool {
    let s = dir.to_string_lossy();
    s.starts_with(r"\\") && !s.starts_with(r"\\?\") && !s.starts_with(r"\\.\")
}

/// `"dir"` for a command line: a trailing backslash would escape the closing quote, so it is doubled.
fn quoted(dir: &Path) -> String {
    let d = dir.to_string_lossy();
    format!("\"{d}{}\"", if d.ends_with('\\') { "\\" } else { "" })
}

/// Splits a command line into its program and the rest: `"C:\a b\x.exe" -d "{path}"` or `wt -d {path}`.
fn split_command(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some((rest[..end].to_string(), rest[end + 1..].trim().to_string()));
    }
    let mut it = line.splitn(2, char::is_whitespace);
    let prog = it.next().filter(|p| !p.is_empty())?;
    Some((prog.to_string(), it.next().unwrap_or("").trim().to_string()))
}

/// How to start `kind` in `dir`, given the program's path. `None` for Custom without a command, and for the Linux terminals.
pub(crate) fn launch_for(kind: Terminal, custom: &str, dir: &Path, exe: &Path) -> Option<Launch> {
    let unc = is_unc(dir);
    // (cmd cannot have a share as its working directory, nor can the programs that start from the process's)
    let cwd = (!unc).then(|| dir.to_path_buf());
    let ps_goto = || format!("-NoExit -Command \"Set-Location -LiteralPath '{}'\"", dir.to_string_lossy().replace('\'', "''"));
    let launch = |args: String, cwd: Option<PathBuf>, console: bool| Launch { program: exe.to_path_buf(), args, cwd, console };
    Some(match kind {
        Terminal::Auto | Terminal::WindowsTerminal => launch(format!("-d {}", quoted(dir)), None, false),
        Terminal::Pwsh | Terminal::WindowsPowerShell => launch(if unc { ps_goto() } else { String::new() }, cwd, true),
        // pushd maps the share to a drive letter for the session
        Terminal::Cmd => launch(if unc { format!("/K pushd {}", quoted(dir)) } else { String::new() }, cwd, true),
        Terminal::Wsl => launch(format!("--cd {}", quoted(dir)), None, true),
        Terminal::GitBash => launch(format!("--cd={}", quoted(dir)), cwd, false),
        Terminal::Custom => {
            let (program, args) = split_command(custom)?;
            let d = dir.to_string_lossy();
            let path = format!("{d}{}", if d.ends_with('\\') { "\\" } else { "" });
            Launch { program: PathBuf::from(program), args: args.replace("{path}", &path), cwd, console: false }
        }
        _ => return None,
    })
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_string_lossy().split(';').map(|d| Path::new(d.trim_matches('"')).join(name)).find(|p| p.is_file())
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty())
}

/// The program that starts `kind`, if it is installed.
pub fn terminal_exe(kind: Terminal) -> Option<PathBuf> {
    let under = |var: &str, rest: &str| env_dir(var).map(|d| d.join(rest)).filter(|p| p.is_file());
    let system = |rest: &str| under("SystemRoot", rest);
    match kind {
        Terminal::Auto => terminal_exe(Terminal::WindowsTerminal).or_else(|| terminal_exe(Terminal::WindowsPowerShell)),
        Terminal::WindowsTerminal => on_path("wt.exe").or_else(|| under("LOCALAPPDATA", r"Microsoft\WindowsApps\wt.exe")),
        Terminal::Pwsh => on_path("pwsh.exe")
            .or_else(|| under("ProgramFiles", r"PowerShell\7\pwsh.exe"))
            .or_else(|| under("LOCALAPPDATA", r"Microsoft\WindowsApps\pwsh.exe")),
        Terminal::WindowsPowerShell => system(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        Terminal::Cmd => env_dir("ComSpec").filter(|p| p.is_file()).or_else(|| system(r"System32\cmd.exe")),
        Terminal::Wsl => system(r"System32\wsl.exe"),
        Terminal::GitBash => under("ProgramFiles", r"Git\git-bash.exe")
            .or_else(|| under("ProgramFiles(x86)", r"Git\git-bash.exe"))
            .or_else(|| under("LOCALAPPDATA", r"Programs\Git\git-bash.exe"))
            .or_else(|| on_path("git.exe").and_then(|g| g.parent()?.parent().map(|d| d.join("git-bash.exe"))).filter(|p| p.is_file())),
        _ => None,
    }
}

/// The terminals this PC can start (Automatic first, the custom command last). Looked up once.
pub fn installed_terminals() -> &'static [Terminal] {
    static LIST: OnceLock<Vec<Terminal>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut v = vec![Terminal::Auto];
        v.extend(
            [Terminal::WindowsTerminal, Terminal::Pwsh, Terminal::WindowsPowerShell, Terminal::Cmd, Terminal::Wsl, Terminal::GitBash]
                .into_iter()
                .filter(|t| terminal_exe(*t).is_some()),
        );
        v.push(Terminal::Custom);
        v
    })
}

fn spawn(l: &Launch) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x10;
    let mut c = std::process::Command::new(&l.program);
    c.raw_arg(&l.args);
    if let Some(d) = &l.cwd {
        c.current_dir(d);
    }
    if l.console {
        c.creation_flags(CREATE_NEW_CONSOLE);
    }
    c.spawn().map(|_| ())
}

/// Opens the chosen terminal in `dir`. A terminal that is not installed (or a custom command that is empty or fails) falls back to
/// Automatic.
pub fn open_terminal_with(kind: Terminal, custom: &str, dir: &Path) -> anyhow::Result<()> {
    let chosen = match kind {
        Terminal::Custom => launch_for(kind, custom, dir, Path::new("")),
        k => terminal_exe(k).and_then(|exe| launch_for(k, custom, dir, &exe)),
    };
    if let Some(l) = chosen {
        if spawn(&l).is_ok() {
            return Ok(());
        }
    }
    if kind != Terminal::Auto {
        return open_terminal_with(Terminal::Auto, "", dir);
    }
    // nothing found by path: let the system look the names up
    let wt = Launch { program: "wt.exe".into(), args: format!("-d {}", quoted(dir)), cwd: None, console: false };
    if spawn(&wt).is_err() {
        spawn(&Launch { program: "powershell.exe".into(), args: String::new(), cwd: (!is_unc(dir)).then(|| dir.to_path_buf()), console: true })?;
    }
    Ok(())
}

/// Base64 of `text` as UTF-16LE: what PowerShell's `-EncodedCommand` takes (no quoting layer can touch it).
fn encoded_command(text: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A PowerShell single-quoted string (every kind of single quote doubled).
fn ps_quote(s: &str) -> String {
    let mut o = String::from("'");
    for c in s.chars() {
        o.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            o.push(c);
        }
    }
    o.push('\'');
    o
}

/// Runs `program` with `args` (and `env` set) inside the terminal `kind` in `dir`, and leaves the window open when it ends. The
/// command goes to PowerShell as an encoded script, so no quoting of the terminal's own command line can change it. Windows
/// Terminal, PowerShell 7, Windows PowerShell and cmd run it themselves; Git Bash, WSL and a custom command cannot take a Windows
/// program line, so those use the automatic terminal.
pub fn open_terminal_running(kind: Terminal, dir: &Path, program: &Path, args: &[std::ffi::OsString], env: &[(&str, std::ffi::OsString)]) -> anyhow::Result<()> {
    let mut script = format!("Set-Location -LiteralPath {}; ", ps_quote(&dir.to_string_lossy()));
    for (k, v) in env {
        script.push_str(&format!("$env:{k} = {}; ", ps_quote(&v.to_string_lossy())));
    }
    script.push_str(&format!("& {}", ps_quote(&program.to_string_lossy())));
    for a in args {
        script.push(' ');
        script.push_str(&ps_quote(&a.to_string_lossy()));
    }
    let b64 = encoded_command(&script);
    let ps = terminal_exe(Terminal::WindowsPowerShell).unwrap_or_else(|| PathBuf::from("powershell.exe"));
    let ps_args = format!("-NoExit -NoProfile -EncodedCommand {b64}");
    let kind = match kind {
        Terminal::Wsl | Terminal::GitBash | Terminal::Custom => Terminal::Auto,
        k => k,
    };
    let own = |exe: PathBuf, args: String, console: bool| Launch { program: exe, args, cwd: (!is_unc(dir)).then(|| dir.to_path_buf()), console };
    let chosen = match kind {
        Terminal::Auto | Terminal::WindowsTerminal => terminal_exe(kind)
            .map(|wt| Launch { program: wt, args: format!("-d {} \"{}\" {ps_args}", quoted(dir), ps.to_string_lossy()), cwd: None, console: false }),
        Terminal::Pwsh | Terminal::WindowsPowerShell => terminal_exe(kind).map(|exe| own(exe, ps_args.clone(), true)),
        Terminal::Cmd => terminal_exe(kind).map(|exe| own(exe, format!("/K {} -NoProfile -EncodedCommand {b64}", ps.to_string_lossy()), true)),
        _ => None,
    };
    if let Some(l) = chosen {
        if spawn(&l).is_ok() {
            return Ok(());
        }
    }
    // no such terminal here: Windows Terminal by name, else a PowerShell console
    let wt = Launch { program: "wt.exe".into(), args: format!("-d {} powershell.exe {ps_args}", quoted(dir)), cwd: None, console: false };
    if spawn(&wt).is_err() {
        spawn(&own(ps, ps_args, true))?;
    }
    Ok(())
}

/// Opens an elevated terminal in `dir` (Windows asks for permission: UAC). Windows Terminal if
/// present, else PowerShell. Declining the permission prompt is not an error.
pub fn open_terminal_admin(dir: &Path) -> anyhow::Result<()> {
    let run = |file: &str, args: &str| -> windows::core::Result<()> {
        ensure_com();
        let (file, args, verb, cwd) = (wide(file), wide(args), wide("runas"), wide(dir));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()),
            lpDirectory: PCWSTR(cwd.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info) }
    };
    // ERROR_CANCELLED: the user said no at the prompt (asking again with PowerShell would nag)
    let declined = |e: &windows::core::Error| e.code().0 as u32 == 0x8007_04C7;
    match run("wt.exe", &terminal_args(dir)) {
        Ok(()) => Ok(()),
        Err(e) if declined(&e) => Ok(()),
        Err(_) => match run("powershell.exe", &powershell_args(dir)) {
            Ok(()) => Ok(()),
            Err(e) if declined(&e) => Ok(()),
            Err(e) => Err(e.into()),
        },
    }
}

/// `-d "C:\some dir"` for Windows Terminal. A trailing backslash would escape the closing
/// quote (`"C:\"`), so it is doubled.
fn terminal_args(dir: &Path) -> String {
    let d = dir.to_string_lossy();
    let tail = if d.ends_with('\\') { "\\" } else { "" };
    format!("-d \"{d}{tail}\"")
}

/// An elevated process starts in System32 whatever directory is asked for: PowerShell is told
/// to go to the folder itself (single quotes doubled, as PowerShell escapes them).
fn powershell_args(dir: &Path) -> String {
    format!("-NoExit -Command \"Set-Location -LiteralPath '{}'\"", dir.to_string_lossy().replace('\'', "''"))
}

/// `shell:Downloads`, `shell:startup`, `shell:sendto`… → file-system path (None for virtual
/// folders like `shell:RecycleBinFolder`). Blocking (shell namespace): off the UI thread.
pub fn resolve_shell_path(s: &str) -> Option<std::path::PathBuf> {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
    ensure_com();
    let w = crate::wide::wide(s.trim());
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(windows::core::PCWSTR(w.as_ptr()), None).ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let out = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        out.map(std::path::PathBuf::from)
    }
}

#[cfg(test)]
mod shell_path_tests {
    #[test]
    fn resolves_known_shell_paths() {
        let p = super::resolve_shell_path("shell:Windows").expect("shell:Windows");
        assert!(p.to_string_lossy().to_lowercase().ends_with("windows"), "{p:?}");
        assert!(super::resolve_shell_path("shell:RecycleBinFolder").is_none());
    }
}

/// No "There is no disk in the drive" / "Insert a disk" system boxes when touching an empty
/// card reader or an ejected disc: such calls just fail (and the app shows "drive not ready").
/// Process-wide; call once at startup.
pub fn suppress_critical_error_dialogs() {
    use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode};
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

/// Tells the shell (Explorer windows, desktop, file dialogs) that `dir`'s contents changed,
/// the way the shell's own copy engine does when it finishes.
pub fn notify_updated(dir: &std::path::Path) {
    use windows::Win32::UI::Shell::{SHCNE_UPDATEDIR, SHCNF_PATHW, SHChangeNotify};
    let w = crate::wide::wide(dir);
    // SAFETY: a NUL-terminated wide path, read during the call
    unsafe { SHChangeNotify(SHCNE_UPDATEDIR, SHCNF_PATHW, Some(w.as_ptr() as *const _), None) };
}

/// Explorer's "Change..." next to "Opens with": the shell's app chooser for `path`, which on OK makes the
/// chosen app the default for the extension (OAIF_REGISTER_EXT + OAIF_FORCE_REGISTRATION) without opening the
/// file (no OAIF_EXEC). Modal: run it on the STA shell thread.
pub fn open_with_dialog(owner: windows::Win32::Foundation::HWND, path: &Path) -> anyhow::Result<()> {
    use windows::Win32::UI::Shell::{OAIF_ALLOW_REGISTRATION, OAIF_FORCE_REGISTRATION, OAIF_REGISTER_EXT, OPENASINFO, SHOpenWithDialog};
    ensure_com();
    let file = wide(path);
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_FORCE_REGISTRATION };
    // SAFETY: info and the path buffer outlive the modal call
    unsafe { SHOpenWithDialog(Some(owner), &info)? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn elevated_terminal_arguments() {
        assert_eq!(super::terminal_args(Path::new(r"C:\Users\user\a b")), r#"-d "C:\Users\user\a b""#);
        // a drive root: the backslash must not escape the closing quote
        assert_eq!(super::terminal_args(Path::new(r"D:\")), r#"-d "D:\\""#);
        assert_eq!(super::powershell_args(Path::new(r"C:\it's")), r#"-NoExit -Command "Set-Location -LiteralPath 'C:\it''s'""#);
    }

    #[test]
    fn terminal_command_lines() {
        use super::{Terminal as T, launch_for};
        let exe = Path::new(r"C:\x\t.exe");
        let l = |k, c: &str, d: &str| launch_for(k, c, Path::new(d), exe).unwrap();
        assert_eq!(l(T::WindowsTerminal, "", r"C:\a b").args, r#"-d "C:\a b""#);
        assert_eq!(l(T::WindowsTerminal, "", r"D:\").args, r#"-d "D:\\""#);
        let p = l(T::Pwsh, "", r"C:\a");
        assert_eq!((p.args.as_str(), p.cwd.as_deref()), ("", Some(Path::new(r"C:\a"))));
        // a share: PowerShell goes there itself, cmd uses pushd, neither gets it as the working directory
        let p = l(T::Pwsh, "", r"\\srv\it's");
        assert_eq!((p.args.as_str(), p.cwd), (r#"-NoExit -Command "Set-Location -LiteralPath '\\srv\it''s'""#, None));
        let c = l(T::Cmd, "", r"\\srv\share");
        assert_eq!((c.args.as_str(), c.cwd), (r#"/K pushd "\\srv\share""#, None));
        assert_eq!(l(T::Cmd, "", r"C:\a").args, "");
        assert_eq!(l(T::Wsl, "", r"C:\a").args, r#"--cd "C:\a""#);
        let c = l(T::Custom, r#""C:\my term\t.exe" --dir "{path}" -x"#, r"C:\a b");
        assert_eq!((c.program.as_path(), c.args.as_str()), (Path::new(r"C:\my term\t.exe"), r#"--dir "C:\a b" -x"#));
        assert!(launch_for(T::Custom, "  ", Path::new(r"C:\a"), exe).is_none());
        assert!(launch_for(T::Kitty, "", Path::new(r"C:\a"), exe).is_none());
    }
}
