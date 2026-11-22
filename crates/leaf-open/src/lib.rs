//! leaf-open: the program Windows starts for folders and Win+E while leaf is the default file manager (rf-win `default_fm`
//! registers it, copied to `%LOCALAPPDATA%\leaf-default-fm\`). It starts the leaf.exe named in [`PATH_FILE`] beside it, with the
//! same arguments (quoted so leaf reads back the same list). When that leaf is gone (a rebuild replacing it, a cleaned build folder, a portable copy
//! deleted) the folder opens in the real Explorer instead, through Explorer's own machine-wide handler, which our per-user verb
//! cannot send back here. So Windows always has a working folder handler.
//!
//! Kept tiny: std and a few Windows calls, no settings, no log.

/// The file beside leaf-open.exe that names the leaf.exe to start: one line, the full path. Written by rf-win `default_fm`.
pub const PATH_FILE: &str = "leaf-exe.txt";

/// The Win+E target (File Explorer's own window): what a launch with no argument opens when leaf is gone.
pub const FILE_EXPLORER_CLSID: &str = "{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";

/// What one launch does.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the named leaf.exe with the same command line.
    StartLeaf,
    /// Open this (a folder, a namespace) in the real Explorer.
    OpenInExplorer(String),
    /// No argument (Win+E) and no leaf: a plain Explorer window.
    PlainExplorer,
    /// The same fall-back ran a moment ago: do nothing rather than risk a loop.
    Nothing,
}

/// The decision, pure. `args`: the arguments after the program's name; `leaf_ok`: the named leaf.exe is there; `leaf_is_me`: the
/// name points at this launcher (or another leaf-open), which would start itself; `fell_back_just_now`: this same fall-back ran
/// within the last seconds.
pub fn decide(args: &[String], leaf_ok: bool, leaf_is_me: bool, fell_back_just_now: bool) -> Action {
    if leaf_ok && !leaf_is_me {
        return Action::StartLeaf;
    }
    if fell_back_just_now {
        return Action::Nothing;
    }
    let target = args.join(" ");
    let target = target.trim().trim_matches('"').trim();
    if target.is_empty() { Action::PlainExplorer } else { Action::OpenInExplorer(target.to_string()) }
}

/// The arguments as one command line that Windows' C runtime (and Rust's `std::env::args`) reads back into exactly the same
/// list: each one quoted when it has a space, a tab or a quote in it, or is empty; backslashes doubled only where they come before
/// a quote.
pub fn command_line(args: &[String]) -> String {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if !a.is_empty() && !a.contains([' ', '\t', '"']) {
            out.push_str(a);
            continue;
        }
        out.push('"');
        let mut slashes = 0usize;
        for c in a.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                c => {
                    out.push_str(&"\\".repeat(slashes));
                    out.push(c);
                    slashes = 0;
                }
            }
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
    }
    out
}

/// Whether the named program is a leaf-open (this one or a copy): starting it would only come back here.
pub fn is_launcher(path: &std::path::Path, me: Option<&std::path::Path>) -> bool {
    let named = path.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("leaf-open"));
    let same = me.is_some_and(|m| match (std::fs::canonicalize(m), std::fs::canonicalize(path)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    });
    named || same
}

/// The program: see the module comment. Does nothing off Windows.
pub fn main() {
    #[cfg(windows)]
    win::run();
}

#[cfg(windows)]
mod win {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW};
    use windows::Win32::UI::Shell::{SEE_MASK_CLASSKEY, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    use super::{Action, FILE_EXPLORER_CLSID, PATH_FILE, command_line, decide, is_launcher};

    pub fn run() {
        let me = std::env::current_exe().ok();
        let leaf = me.as_ref().and_then(|m| std::fs::read_to_string(m.with_file_name(PATH_FILE)).ok()).map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty());
        let leaf_ok = leaf.as_ref().is_some_and(|p| p.is_file());
        let leaf_is_me = leaf.as_ref().is_some_and(|l| is_launcher(l, me.as_deref()));
        let args: Vec<String> = std::env::args().skip(1).collect();

        let tail = command_line(&args);
        let mut action = decide(&args, leaf_ok, leaf_is_me, false);
        if action == Action::StartLeaf {
            let started = leaf.as_ref().is_some_and(|l| std::process::Command::new(l).raw_arg(&tail).spawn().is_ok());
            if started {
                return;
            }
            action = decide(&args, false, leaf_is_me, false);
        }
        // the fall-back, once per few seconds for the same command line
        if fell_back_recently(&tail) {
            action = Action::Nothing;
        }
        note_fallback(&tail);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        match action {
            Action::OpenInExplorer(target) => {
                // through Explorer's own handler only: `explorer.exe <folder>` would come back through our verb
                let _ = open_with_class(r"Software\Classes\Folder", &target);
            }
            Action::PlainExplorer => {
                if !open_with_class(&format!(r"Software\Classes\CLSID\{FILE_EXPLORER_CLSID}"), &format!("shell:::{FILE_EXPLORER_CLSID}")) {
                    let _ = std::process::Command::new(std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join("explorer.exe")).spawn();
                }
            }
            Action::StartLeaf | Action::Nothing => {}
        }
    }

    /// `ShellExecuteEx` "open" with an HKLM class key: that class's verbs (Explorer's own), never the per-user ones merged over
    /// them.
    fn open_with_class(class: &str, file: &str) -> bool {
        let mut key = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(class), None, KEY_READ, &mut key) } != ERROR_SUCCESS {
            return false;
        }
        let file = HSTRING::from(file);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_CLASSKEY | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            hkeyClass: key,
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let ok = unsafe { ShellExecuteExW(&mut info) }.is_ok();
        unsafe {
            let _ = RegCloseKey(key);
        }
        ok
    }

    fn guard_file() -> PathBuf {
        std::env::temp_dir().join("leaf-open-fallback.txt")
    }

    fn now_ms() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
    }

    fn fell_back_recently(tail: &str) -> bool {
        std::fs::read_to_string(guard_file())
            .ok()
            .and_then(|t| t.split_once('\t').map(|(ms, a)| (ms.parse::<u128>().unwrap_or(0), a.to_string())))
            .is_some_and(|(ms, a)| a == tail && now_ms().saturating_sub(ms) <= 3_000)
    }

    fn note_fallback(tail: &str) {
        let _ = std::fs::write(guard_file(), format!("{}\t{tail}", now_ms()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_and_the_command_line_passed_on() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let folder = a(&[r"C:\Users\user\My Files"]);
        // (args, leaf there, leaf is me, fell back just now) -> action
        let table: &[(&[String], bool, bool, bool, Action)] = &[
            (&folder, true, false, false, Action::StartLeaf),
            (&[], true, false, false, Action::StartLeaf),
            (&folder, false, false, false, Action::OpenInExplorer(r"C:\Users\user\My Files".into())),
            (&a(&["::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"]), false, false, false, Action::OpenInExplorer("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into())),
            (&[], false, false, false, Action::PlainExplorer),
            (&a(&["", " "]), false, false, false, Action::PlainExplorer),
            // the path file names this launcher: never start it again
            (&folder, true, true, false, Action::OpenInExplorer(r"C:\Users\user\My Files".into())),
            (&[], true, true, false, Action::PlainExplorer),
            // the same fall-back a moment ago: nothing
            (&folder, false, false, true, Action::Nothing),
            // leaf there wins over the guard (the guard is only for the fall-back)
            (&folder, true, false, true, Action::StartLeaf),
        ];
        for (args, ok, me, recent, want) in table {
            assert_eq!(&decide(args, *ok, *me, *recent), want, "{args:?} {ok} {me} {recent}");
        }

        // the command line handed on reads back as the same arguments
        let q = |v: &[&str]| command_line(&a(v));
        assert_eq!(q(&[r"C:\Users\user\My Files"]), r#""C:\Users\user\My Files""#);
        assert_eq!(q(&[r"C:\Users\user"]), r"C:\Users\user");
        assert_eq!(q(&[r"D:\"]), r"D:\", "a trailing backslash outside quotes stays one");
        assert_eq!(q(&[r"D:\My Files\"]), r#""D:\My Files\\""#, "doubled before the closing quote");
        assert_eq!(q(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(q(&[r#"a\"b"#]), r#""a\\\"b""#);
        assert_eq!(q(&[r"/select,C:\a b.txt", ""]), r#""/select,C:\a b.txt" """#);
        assert_eq!(q(&[]), "");

        assert!(is_launcher(std::path::Path::new(r"C:\Users\user\AppData\Local\leaf-default-fm\leaf-open.exe"), None));
        assert!(is_launcher(std::path::Path::new(r"C:\x\LEAF-OPEN.EXE"), None));
        assert!(!is_launcher(std::path::Path::new(r"C:\x\leaf.exe"), None));
    }
}
