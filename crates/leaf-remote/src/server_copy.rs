//! Copies within one SFTP server done by the server itself (H6): `rsync -a` (when it has `--info=progress2`) or `cp -a`, run
//! over the open SSH connection, so the bytes never cross the line twice. For a reviewer:
//!
//! - the server's tools are asked about once per server and remembered for this run (`PROBE`); a server without a POSIX shell
//!   (Windows OpenSSH, an sftp-only account, a restricted shell) answers something else, and the caller copies as before;
//! - the command only ever writes a new part name (`.name.<n>.leaf-part`, the generic copy's own) and refuses to start when
//!   anything is there under that name; the part is then renamed to the real name by SFTP, without replacing unless the
//!   conflict dialog said "Replace" for a file. A folder is never merged by the server (the caller copies those as before);
//! - every path in a command is one shell word made by `checksum::shell_path` (single quotes; `/~` is the home folder), after
//!   `--`;
//! - the command prints its process id first; Cancel signals that process group (`kill -TERM -- -pid`), then the part is
//!   removed.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use leaf_vfs::copy::part_name;
use leaf_vfs::{Fs, FsError, Kind, VPath};

use crate::checksum::shell_path;

/// What the server copies with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `rsync -a --info=progress2`: progress from its output.
    Rsync,
    /// `cp -a` (GNU or BusyBox): progress from the part's size (`du -sk`).
    Cp,
}

impl Tool {
    pub fn program(self) -> &'static str {
        match self {
            Tool::Rsync => "rsync",
            Tool::Cp => "cp",
        }
    }
}

/// Asks the server's shell which tool it has. Any other shell (cmd.exe, PowerShell, an sftp-only account) prints no
/// `leaf-tool` line.
pub const PROBE: &str = "if command -v rsync >/dev/null 2>&1 && rsync --help 2>&1 | grep -q -e --info=; then echo leaf-tool rsync; \
elif command -v cp >/dev/null 2>&1; then echo leaf-tool cp; else echo leaf-tool none; fi";

/// The tool a `PROBE` run named (none: the server copies nothing itself).
pub fn parse_probe(out: &str) -> Option<Tool> {
    match out.lines().map(str::trim).rfind(|l| l.starts_with("leaf-tool "))? {
        "leaf-tool rsync" => Some(Tool::Rsync),
        "leaf-tool cp" => Some(Tool::Cp),
        _ => None,
    }
}

/// The exit code of a copy command that found something under the part name and did not start.
pub const PART_TAKEN: u32 = 17;

/// The command that copies `src` (a file, or a folder with all it holds) to the new name `part`: refuses when anything is at
/// `part` (a file, a folder, a link), prints `leaf-pid <pid>`, then becomes the copy program (same pid, same process group).
pub fn copy_command(tool: Tool, src: &str, part: &str, is_dir: bool) -> String {
    let (src, part) = (shell_path(src), shell_path(part));
    let run = match tool {
        // (a folder's trailing `/`: its contents go into the new folder `part`, not a folder inside it)
        Tool::Rsync if is_dir => format!("rsync -a --info=progress2 -- {src}/ \"$P\""),
        Tool::Rsync => format!("rsync -a --info=progress2 -- {src} \"$P\""),
        Tool::Cp => format!("cp -a -- {src} \"$P\""),
    };
    format!("P={part}; if [ -e \"$P\" ] || [ -L \"$P\" ]; then exit {PART_TAKEN}; fi; echo leaf-pid $$; exec {run} 2>&1")
}

/// The command that prints how much is at `part` so far (KiB), for `cp`'s progress.
pub fn size_command(part: &str) -> String {
    format!("du -sk -- {} 2>/dev/null", shell_path(part))
}

/// The bytes a `du -sk` run printed.
pub fn parse_size(out: &str) -> Option<u64> {
    out.split_whitespace().next()?.parse::<u64>().ok().map(|k| k.saturating_mul(1024))
}

/// The command that stops a copy whose command printed `leaf-pid <pid>` (its whole process group: rsync's helpers too).
pub fn kill_command(pid: u32) -> String {
    format!("kill -TERM -- -{pid} 2>/dev/null || kill -TERM {pid}")
}

/// The process id a copy command printed (`leaf-pid <pid>`), read from whole lines only (a line still without its `\n` may be
/// cut: `leaf-pid 4` of `leaf-pid 4242`). A pid of 1 or less is refused: `kill -- -1` would signal every process of the user.
pub fn parse_pid(head: &str) -> Option<u32> {
    let whole = &head[..head.rfind('\n')?];
    whole.lines().find_map(|l| l.trim().strip_prefix("leaf-pid ")?.trim().parse::<u32>().ok()).filter(|&p| p > 1)
}

/// The bytes done in a piece of `rsync --info=progress2` output (`  1,234,567  45%  12.34MB/s  0:00:12`): the last such line
/// in it (lines end in `\r` while it runs).
pub fn parse_rsync_progress(text: &str) -> Option<u64> {
    text.split(['\r', '\n']).filter_map(|l| {
        let mut w = l.split_whitespace();
        let first = w.next()?;
        w.next().filter(|p| p.ends_with('%'))?;
        let digits: String = first.chars().filter(|c| !matches!(c, ',' | '.' | '\'')).collect();
        digits.parse::<u64>().ok()
    }).last()
}

/// The start of the error of a copy that could not be stopped (its part is left alone: it is still being written).
pub const UNSTOPPED: &str = "leaf could not stop the copy on the server";

/// How a copy command ended.
#[derive(Debug, Clone, Default)]
pub struct Ended {
    /// The exit code (none: the connection ended first).
    pub code: Option<u32>,
    /// It printed its process id (it had started).
    pub started: bool,
    /// The last line it printed that was not progress (the details of an error).
    pub last_line: String,
    /// Cancel came before the command said its pid, and none came: the copy may still be running on the server.
    pub unstopped: bool,
}

/// What a copy command's ending means: `Unsupported` when the command never started (the caller copies as before).
pub fn outcome(tool: Tool, e: &Ended, part: &VPath) -> Result<(), FsError> {
    if e.unstopped {
        return Err(FsError::Other(format!("{UNSTOPPED}; it may still be writing {} (remove it when it is done)", part.path())));
    }
    match e.code {
        Some(0) => Ok(()),
        Some(PART_TAKEN) => Err(FsError::Exists),
        _ if !e.started => Err(FsError::Unsupported),
        Some(n) => Err(FsError::Other(format!("The server could not copy it ({} ended with code {n}: {})", tool.program(), e.last_line.trim()))),
        None => Err(FsError::Offline("the connection ended during the copy on the server".into())),
    }
}

/// The tools of each server (by address authority) this run asked about.
fn tools() -> &'static Mutex<HashMap<String, Option<Tool>>> {
    static T: OnceLock<Mutex<HashMap<String, Option<Tool>>>> = OnceLock::new();
    T.get_or_init(Mutex::default)
}

/// The server's tool, asked once per server (`ask` runs `PROBE`). A server that does not answer (exec refused, the sftp-only
/// account that waits) counts as having none for this run.
pub fn tool_of(server: &str, ask: impl FnOnce() -> Result<(String, Option<u32>), FsError>) -> Result<Option<Tool>, FsError> {
    if let Some(t) = tools().lock().unwrap_or_else(|e| e.into_inner()).get(server) {
        return Ok(*t);
    }
    let t = match ask() {
        Err(FsError::Cancelled) => return Err(FsError::Cancelled),
        Ok((out, Some(0))) => parse_probe(&out),
        Ok(_) => None,
        Err(e) => {
            log::info!("remote: {server} runs no commands for copies ({e}); copies go through this PC");
            None
        }
    };
    if let Some(t) = t {
        log::info!("remote: {server} copies with {} on the server", t.program());
    }
    tools().lock().unwrap_or_else(|e| e.into_inner()).insert(server.to_string(), t);
    Ok(t)
}

/// Copies `from` to `to` on one server with `tool`: `run(command, part)` runs the command, which writes `part` only; then
/// `part` gets the name `to` (replacing a FILE there only with `replace`: the conflict dialog's answer). The caller has asked
/// about a name that is there; when one is there without `replace` (it appeared meanwhile) nothing runs. On any failure the
/// part is removed and `to` is as it was. `Unsupported` (nothing was done): copy another way.
pub fn copy_with(
    fs: &dyn Fs,
    tool: Tool,
    from: &VPath,
    to: &VPath,
    replace: bool,
    run: &mut dyn FnMut(&str, &VPath) -> Result<(), FsError>,
) -> Result<(), FsError> {
    if from.scheme() != to.scheme() || from.authority() != to.authority() || from.is_root() || to.is_root() || to.starts_with(from) {
        return Err(FsError::Unsupported);
    }
    let src = fs.stat(from)?;
    // links: the generic copy leaves them out and says so; leaf does the same here
    let is_dir = match src.kind {
        Kind::File => false,
        Kind::Dir => true,
        Kind::Link => return Err(FsError::Unsupported),
    };
    match fs.stat(to) {
        Err(FsError::NotFound) => {}
        // a folder is merged, never replaced: that is the generic copy's work
        Ok(t) if replace && !is_dir && t.kind == Kind::File => {}
        // (and a folder where "Replace" was said for a file: the generic copy leaves it out and says so)
        Ok(_) if is_dir || replace => return Err(FsError::Unsupported),
        Ok(_) => return Err(FsError::Exists),
        Err(e) => return Err(e),
    }
    let parent = to.parent().ok_or(FsError::Denied)?;
    let part = parent.join(&part_name(to.name()));
    if let Err(e) = run(&copy_command(tool, from.path(), part.path(), is_dir), &part) {
        // (`Exists`: something was under the part name before the command; it is not leaf's to remove. A copy that could
        // not be stopped is left alone too: it is still writing there, and the error names the part)
        if e != FsError::Exists && !matches!(&e, FsError::Other(m) if m.starts_with(UNSTOPPED)) {
            let _ = fs.remove(&part, true);
            // once more: a copy that was signalled may have written a last piece after the first removal
            if fs.stat(&part).is_ok() {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let _ = fs.remove(&part, true);
            }
        }
        return Err(e);
    }
    if let Err(e) = fs.rename(&part, to, replace) {
        let _ = fs.remove(&part, true);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_vfs::MemFs;

    #[test]
    fn server_copy_quoting() {
        use crate::checksum::quote;
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote("$HOME `x` \"y\""), "'$HOME `x` \"y\"'");
        assert_eq!(quote("a\nb"), "'a\nb'");
        assert_eq!(quote("-rf"), "'-rf'");
        assert_eq!(quote(""), "''");
        assert_eq!(shell_path("/~/my dir"), "\"$HOME\"'/my dir'");
        let c = copy_command(Tool::Cp, "/srv/-x 'q' $y\nz", "/srv/.p.leaf-part", false);
        assert_eq!(c, "P='/srv/.p.leaf-part'; if [ -e \"$P\" ] || [ -L \"$P\" ]; then exit 17; fi; echo leaf-pid $$; exec cp -a -- '/srv/-x '\\''q'\\'' $y\nz' \"$P\" 2>&1");
        assert!(copy_command(Tool::Rsync, "/a b", "/p", true).contains("-- '/a b'/ \"$P\""));
        // a real POSIX shell reads each quoted word back as the very same text
        #[cfg(unix)]
        for s in ["a b", "it's", "$HOME `x` \"y\" \\", "a\nb", "-rf", "--", "*?[x]", "~"] {
            let out = std::process::Command::new("sh").arg("-c").arg(format!("printf %s {}", quote(s))).output().unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
        }
    }

    #[test]
    fn server_copy_parsing() {
        assert_eq!(parse_probe("motd\nleaf-tool rsync\n"), Some(Tool::Rsync));
        assert_eq!(parse_probe("leaf-tool cp"), Some(Tool::Cp));
        assert_eq!(parse_probe("leaf-tool none"), None);
        assert_eq!(parse_probe("'command' is not recognized as an internal or external command"), None);
        assert_eq!(parse_pid("leaf-pid 4242\n"), Some(4242));
        // a line that arrived in two pieces is read only once it is whole; pid 1 (and 0) is never taken
        assert_eq!(parse_pid("welcome\nleaf-pid 4"), None);
        assert_eq!(parse_pid("welcome\nleaf-pid 4242\n"), Some(4242));
        assert_eq!(parse_pid("leaf-pid 1\n"), None);
        assert_eq!(parse_pid("leaf-pid 0\n"), None);
        assert_eq!(parse_rsync_progress("      1,234,567  45%   12.34MB/s    0:00:12\r  2,000,000  70%  1MB/s  0:00:01 (xfr#1)\r"), Some(2_000_000));
        assert_eq!(parse_rsync_progress("rsync: error\n"), None);
        assert_eq!(parse_size("12\t/srv/x\n"), Some(12 * 1024));
    }

    /// A name that is there is never the copy command's target: without "Replace" nothing runs; with it the command writes
    /// the part name only and the rename puts it over the file; a failed command leaves the file as it was.
    #[test]
    fn server_copy_conflict_never_targets_final_name() {
        let fs = MemFs::new();
        let root = VPath::parse("mem://test/").unwrap();
        let (src, dst) = (root.join("src.txt"), root.join("dst.txt"));
        fs.put_file(&src, b"new".to_vec(), 1);
        fs.put_file(&dst, b"old".to_vec(), 1);
        let read = |p: &VPath| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut fs.open_read(p, 0).unwrap(), &mut s).unwrap();
            s
        };
        let commands = std::cell::RefCell::new(Vec::<String>::new());
        let mut run = |cmd: &str, part: &VPath| {
            commands.borrow_mut().push(cmd.to_string());
            assert!(part.name().ends_with(".leaf-part") && part.name() != dst.name());
            fs.put_file(part, b"new".to_vec(), 1);
            Ok(())
        };
        // the name appeared after the question (or no "Replace"): nothing runs, nothing changes
        assert_eq!(copy_with(&fs, Tool::Cp, &src, &dst, false, &mut run), Err(FsError::Exists));
        assert!(commands.borrow().is_empty());
        assert_eq!(read(&dst), "old");
        // "Replace": the command names the part, never the file that is there
        copy_with(&fs, Tool::Rsync, &src, &dst, true, &mut run).unwrap();
        assert_eq!(commands.borrow().len(), 1);
        assert!(!commands.borrow()[0].contains(&crate::checksum::quote(dst.path())));
        assert_eq!(read(&dst), "new");
        // a command that fails: the file stays, the part is gone
        fs.put_file(&dst, b"old".to_vec(), 1);
        let mut fail = |_: &str, part: &VPath| {
            fs.put_file(part, b"half".to_vec(), 1);
            Err(FsError::Other("cp ended with code 1".into()))
        };
        assert!(copy_with(&fs, Tool::Cp, &src, &dst, true, &mut fail).is_err());
        assert_eq!(read(&dst), "old");
        let mut left = Vec::new();
        fs.list(&root, 100, &|| false, &mut |b| left.extend(b)).unwrap();
        assert!(left.iter().all(|e| !e.name.ends_with(".leaf-part")));
        // a folder that is there is merged by the generic copy, never by the server
        fs.put_dir(&root.join("d"), 1);
        fs.put_dir(&root.join("e"), 1);
        assert_eq!(copy_with(&fs, Tool::Cp, &root.join("d"), &root.join("e"), true, &mut run), Err(FsError::Unsupported));
        assert_eq!(commands.borrow().len(), 1);
    }
}
