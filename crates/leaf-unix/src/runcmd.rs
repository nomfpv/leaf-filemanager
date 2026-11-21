//! Linux backend of rf_win::runcmd: looking for and starting the program of a command typed into the address field.
//!
//! The rules that decide whether the text is a command at all are in `rf_core::addrcmd`. This file finds the program the way a shell
//! would, and the way a launcher would:
//!
//! 1. a program typed with a folder part (`/opt/x/run`, `./tool`, `~/bin/x`) is looked for there only (a relative one in the folder
//!    that is open), and must have the executable bit;
//! 2. a bare name (`firefox`, `htop`) is looked for in the folders of `PATH` (the folder that is open is NOT searched: a file planted
//!    in a downloaded folder must not be started by typing a well-known name; relative `PATH` entries are skipped for the same reason);
//! 3. then the `Name` of an application's `.desktop` entry (`Visual Studio Code`, case-insensitive; the words after the longest name
//!    that matches are its arguments), started through its `Exec` line.
//!
//! Looking is plain file-system reading: call it off the UI thread.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rf_core::addrcmd::CommandLine;

/// A program that was found, and the arguments to give it (a string as typed; shell-style quoting groups words).
// TYPE-COPY(rf-win src/runcmd.rs:24)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub exe: PathBuf,
    pub args: String,
}

/// Is `p` a file a program can be started from: a file (a link is followed) with an executable bit.
pub fn is_program_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// The folders a bare program name is searched in, in order: those of `PATH` (absolute ones only).
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            if d.is_absolute() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs
}

/// `~` and `~/x` with the home folder in place.
fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(p));
    }
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// The application entries: (name, id) and the `Exec` line.
fn desktop_entries() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for d in crate::openwith::data_dirs() {
        let Ok(rd) = std::fs::read_dir(d.join("applications")) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let id = e.file_name().to_string_lossy().into_owned();
            if let Some(app) = crate::openwith::parse_entry(&id, &text) {
                out.push((app.name, app.exec));
            }
        }
    }
    out
}

/// Single-quotes a word for [`split_args`].
fn quote(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:+,@".contains(c)) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', r"'\''"))
    }
}

/// The words of an argument string: whitespace separates, single quotes keep everything, double quotes keep spaces (a backslash
/// escapes there), a backslash outside quotes escapes the next character. Nothing is expanded.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut have) = (String::new(), false);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                have = true;
                for q in it.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                have = true;
                while let Some(q) = it.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(it.peek(), Some('"' | '\\' | '$' | '`')) => cur.push(it.next().unwrap_or('\\')),
                        q => cur.push(q),
                    }
                }
            }
            '\\' => {
                have = true;
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// Looks for the program of the first reading of `readings` that names one. `work_dir` is the folder a relative program is taken from.
pub fn find(readings: &[CommandLine], work_dir: &Path) -> Option<Found> {
    find_with(readings, work_dir, &search_dirs(), &|| desktop_entries(), &is_program_file)
}

/// [`find`] with the world passed in (tests use folders they made themselves).
pub fn find_with(
    readings: &[CommandLine],
    work_dir: &Path,
    dirs: &[PathBuf],
    entries: &dyn Fn() -> Vec<(String, String)>,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<Found> {
    for r in readings {
        let program = r.program.trim();
        if program.is_empty() {
            continue;
        }
        let hit = if program.contains('/') || program == "~" {
            let p = expand_tilde(program);
            let p = if p.is_absolute() { p } else { work_dir.join(p) };
            Some(p).filter(|p| is_file(p))
        } else {
            dirs.iter().map(|d| d.join(program)).find(|p| is_file(p))
        };
        if let Some(exe) = hit {
            return Some(Found { exe, args: r.args.clone() });
        }
    }
    // an application's name, typed as it is shown (it may be several words: the longest name that matches wins)
    let first = readings.first()?;
    let words: Vec<&str> = std::iter::once(first.program.as_str()).chain(first.args.split_whitespace()).collect();
    let apps = entries();
    for n in (1..=words.len()).rev() {
        let name = words[..n].join(" ");
        let Some((_, exec)) = apps.iter().find(|(an, _)| an.eq_ignore_ascii_case(&name)) else { continue };
        // (an empty file in place of the field codes: dropped again below)
        let mut parts: Vec<String> =
            crate::openwith::exec_words(exec, Path::new("")).into_iter().map(|w| w.to_string_lossy().into_owned()).filter(|w| !w.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let prog = parts.remove(0);
        let exe = if prog.contains('/') { PathBuf::from(&prog) } else { dirs.iter().map(|d| d.join(&prog)).find(|p| is_file(p))? };
        let mut args: Vec<String> = parts.iter().map(|p| quote(p)).collect();
        // what was typed after the name follows as the user's own arguments (words, as typed)
        let typed: Vec<&str> = words[n..].to_vec();
        args.extend(typed.iter().map(|w| quote(w)));
        return Some(Found { exe, args: args.join(" ") });
    }
    None
}

/// What starting a program came to.
// TYPE-COPY(rf-win src/runcmd.rs:117)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    Yes,
    Declined,
    Pending,
    Failed(String),
}

/// Starts `found` with the arguments as typed and `work_dir` as the working directory, detached (its own process group, no
/// terminal input, output dropped). A thread waits for the child so that it leaves no zombie.
pub fn start(found: &Found, work_dir: &Path) -> Started {
    let mut c = Command::new(&found.exe);
    c.args(split_args(&found.args)).current_dir(work_dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
    match c.spawn() {
        Ok(mut child) => {
            let _ = std::thread::Builder::new().name("rf-runcmd".into()).spawn(move || {
                let _ = child.wait();
            });
            Started::Yes
        }
        Err(e) => Started::Failed(format!("{}: {e}", found.exe.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(program: &str, args: &str) -> CommandLine {
        CommandLine { program: program.into(), args: args.into() }
    }

    #[test]
    fn finds_programs_on_the_path_by_path_and_by_desktop_name() {
        let root = std::env::temp_dir().join(format!("leaf-runcmd-{}", std::process::id()));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (n, mode) in [("tool", 0o755), ("data", 0o644)] {
            std::fs::write(bin.join(n), b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(bin.join(n), std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let dirs = [bin.clone()];
        let none = || Vec::new();
        let apps = || vec![("My Editor".to_string(), "tool --new %U".to_string())];
        let f = |l: Vec<CommandLine>, w: &Path| find_with(&l, w, &dirs, &none, &is_program_file);
        assert_eq!(f(vec![line("tool", "-x")], &root), Some(Found { exe: bin.join("tool"), args: "-x".into() }));
        assert_eq!(f(vec![line("data", "")], &root), None, "no executable bit, no program");
        assert_eq!(f(vec![line("./bin/tool", "")], &root).map(|x| x.exe), Some(root.join("./bin/tool")));
        assert_eq!(f(vec![line("bin/tool", "")], &bin), None, "a relative path is the open folder's, not the PATH's");
        let by_name = find_with(&[line("my", "editor a.txt")], &root, &dirs, &apps, &is_program_file).unwrap();
        assert_eq!(by_name, Found { exe: bin.join("tool"), args: "--new a.txt".into() });
        assert_eq!(split_args(r#"a "b c" 'd e' f\ g"#), ["a", "b c", "d e", "f g"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
