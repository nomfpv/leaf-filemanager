//! `ShellFs` (H1): the files of an SSH server that has no SFTP (a small router, a BusyBox box, an appliance), reached through
//! shell commands on exec channels of the same connection. `SftpFs` hands every file operation of such a connection here
//! (`Conn::shell` is set), so the host looks and behaves like any other SFTP host in the app: same address, same dialogs,
//! same trash, same conflict handling.
//!
//! For a reviewer:
//! - Every value that goes into a command is one shell word made by [`sh_quote`] (single quotes, a quote inside as `'\''`,
//!   NUL refused), which is `checksum::quote`, the function the other exec features use. Paths are always absolute (they
//!   start with `/`), so a name starting with `-` never reaches a tool as an option; `--` is written before path arguments
//!   all the same wherever the tool takes it (`find` takes none: its path is absolute). No command is built with a glob from
//!   user data: the one glob (`* .[!.]* ..?*` in a listing) is leaf's own text, its matches go to the tools as arguments.
//! - Exec channels get no PTY: the bytes of `cat` and of `cat > file` pass unchanged.
//! - What the server has is asked ONCE per connection, when it is made ([`PROBE`]); nothing else runs on the host unless the
//!   user did something there. A login that lands in a menu (OPNsense's root) is told as such, not parsed.
//! - Names: `find -printf` (GNU) or `stat -c` / `stat -f` (BusyBox, BSD) print NUL-separated fields, so any byte but NUL
//!   and `/` passes. Only where neither exists is `ls -la` parsed; names with a newline or ` -> ` can then be read wrong,
//!   which the host's tile says ([`ShellState::names_reliable`]), and an item such a listing may have shown wrong is never
//!   deleted, moved, overwritten or changed ([`Doubt`]). A name that is not UTF-8 is left out (counted, never guessed: a
//!   lossy name would point at another file). A cut listing is an error, never shown as the whole folder.
//! - A write goes to a temporary name in the same folder (`cat > tmp`, under `umask 077`), its size is checked, and only then
//!   is it given its mode (and the replaced file's owner where the tools allow) and moved over the real name (`mv -f`, one
//!   rename): a cut transfer never leaves half a file under the real name. A replace through a link writes the file it leads
//!   to. Resuming (`At`) cuts the file at the offset (`dd seek=`, the size checked) and appends (`cat >>`).
//! - Every command reaches `sh` encoded ([`crate::hostkind::posix`]), so a csh, tcsh or fish login shell parses none of it.
//! - A delete or a move runs with its pid known and is stopped there when it runs past [`LONG`].
//! - `mv` into an existing folder would move the file INTO it: every move first refuses a folder at the target and removes
//!   a link there (only when replacing), so a move never lands somewhere else.
//! - A recursive delete is `rm -rf -- path` built only by [`rm_command`], which refuses an empty path, `/`, a path that is
//!   not normalised, the home folder (by every name the server gave for it, and where the folder really is with links
//!   resolved) and every folder above it, and a path whose quoting does not read back as itself.
//! - Left out (H1 items 5 and 6): bulk `tar` transfers and the legacy SCP protocol; files go one by one.

use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leaf_vfs::{Caps, Details, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, Space, VPath, WriteMode, WriteOpts, WriteSink};

use crate::conn::{self, Conn, RawOut};
use crate::hostkind::HostFamily;

// ------------------------------------------------------------------------------------------------------------ quoting

/// `s` as one word for a POSIX shell (single quotes; a quote inside as `'\''`). A NUL cannot be carried by a shell word and
/// is refused. The one quoting of this module: every path and value in a command goes through it.
pub fn sh_quote(s: &str) -> Result<String, FsError> {
    if s.contains('\0') {
        return Err(FsError::Other("The name holds a NUL character, which a server command cannot carry".into()));
    }
    Ok(crate::checksum::quote(s))
}

/// A server path as a shell word: it must be absolute (so it can never be read as an option) and hold no `.` or `..` part
/// (`VPath` normalises them away; the login folder the server named is joined in after that, so every command checks again).
fn qp(p: &str) -> Result<String, FsError> {
    if !p.starts_with('/') {
        return Err(FsError::Other("leaf only sends absolute paths to the server".into()));
    }
    if p.split('/').any(|s| s == "." || s == "..") {
        return Err(FsError::Other("leaf does not send a path with . or .. in it to the server".into()));
    }
    sh_quote(p)
}

/// What a POSIX shell makes of a word [`sh_quote`] wrote (single-quoted parts and `\'`), for the check that the quoting
/// did not change the path. `None`: not such a word.
fn unquote(w: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = w;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('\'') {
            let end = r.find('\'')?;
            out.push_str(&r[..end]);
            rest = &r[end + 1..];
        } else if let Some(r) = rest.strip_prefix("\\'") {
            out.push('\'');
            rest = r;
        } else {
            return None;
        }
    }
    Some(out)
}

/// Exit codes leaf's commands use for what the shell found before running a tool.
const E_NOT_FOUND: u32 = 40;
const E_EXISTS: u32 = 41;
const E_DENIED: u32 = 43;
const E_NOT_DIR: u32 = 44;
const E_LINK: u32 = 45;
const E_BACKUP_DIR: u32 = 47;
const E_CUT: u32 = 46;

/// The command that removes `path`: `rm -rf` with `recursive`, otherwise a file or link (`rm -f`) or an empty folder
/// (`rmdir`). Refused: an empty path, `/`, a path that is not absolute and normalised (`//`, `.`, `..`, a trailing `/`), each
/// of `homes` (the login folder as `pwd`, `pwd -P` and `$HOME` named it) and every folder above one, and a path whose quoted
/// form does not read back as itself. A recursive remove also asks the server where the folder really is (`cd -P`, links
/// resolved) and refuses `/`, a home folder and every folder above one there too: FreeBSD's `/home` is a link to `/usr/home`.
pub fn rm_command(path: &str, homes: &[String], recursive: bool) -> Result<String, FsError> {
    let bad = || FsError::Other("leaf does not remove this path on the server".into());
    if path.is_empty() || path == "/" || !path.starts_with('/') || path.ends_with('/') {
        return Err(FsError::Denied);
    }
    if path[1..].split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return Err(bad());
    }
    if homes.iter().any(|h| home_at_or_under(path, h)) {
        return Err(FsError::Denied);
    }
    let q = qp(path)?;
    if unquote(&q).as_deref() != Some(path) {
        return Err(bad());
    }
    let head = format!("P={q}; if [ ! -e \"$P\" ] && [ ! -L \"$P\" ]; then exit {E_NOT_FOUND}; fi; ");
    Ok(if recursive {
        let mut list = String::new();
        for h in homes.iter().map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty()) {
            list.push(' ');
            list.push_str(&sh_quote(h)?);
        }
        format!(
            "{head}R=$(cd -P \"$P\" 2>/dev/null && pwd -P) || exit {E_DENIED}; case \"$R\" in ''|/) exit {E_DENIED};; esac; \
             for H in{list}; do case \"$H/\" in \"$R\"/*) exit {E_DENIED};; esac; done; exec rm -rf -- \"$P\""
        )
    } else {
        format!("{head}if [ -d \"$P\" ] && [ ! -L \"$P\" ]; then exec rmdir -- \"$P\"; else exec rm -f -- \"$P\"; fi")
    })
}

/// `path` is the home folder `home` or a folder above it.
fn home_at_or_under(path: &str, home: &str) -> bool {
    let (path, home) = (path.trim_end_matches('/'), home.trim_end_matches('/'));
    path.is_empty() || path == home || home.starts_with(&format!("{path}/"))
}

/// How a replaced file's owner and group are given to the new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeepOwner {
    /// GNU `chown --reference`.
    Reference,
    /// The numbers from `stat` (`-c %u:%g` or `-f %u:%g`), then `chown`.
    Stat(&'static str),
    /// No tool to read them (`ls` only): the new version gets the uploading user's.
    No,
}

/// The command that moves `from` to `to` (both server paths): a folder at `to` is never replaced and never moved into; a
/// file or link at `to` only with `replace` (a link is removed first: `mv` would follow a link to a folder). With `backup`,
/// the old file becomes that name first (moved back when the new one cannot take the name). `mode`: set on `from` first, and
/// the move fails when it cannot be set (nothing at `to` is touched then). `keep`: when a regular file is replaced, its owner
/// and group are given to `from` first (best effort: a user who is not root can only give away what is theirs). `mv_t`:
/// GNU's `mv -T`, so a folder that appears at `to` after the check is never moved into.
///
/// Not kept on a replace: hard links (the name gets a new file; other names of the old one keep the old contents), ACLs,
/// extended attributes and SELinux labels.
pub(crate) fn move_command(from: &str, to: &str, replace: bool, backup: Option<&str>, mode: Option<u32>, keep: KeepOwner, mv_t: bool) -> Result<String, FsError> {
    let mut c = format!("S={}; T={}; ", qp(from)?, qp(to)?);
    let chown = match keep {
        KeepOwner::Reference => "chown --reference=\"$T\" -- \"$S\" 2>/dev/null".to_string(),
        KeepOwner::Stat(f) => format!("o=$(stat {f} -- \"$T\" 2>/dev/null) && chown -- \"$o\" \"$S\" 2>/dev/null"),
        KeepOwner::No => String::new(),
    };
    if replace && !chown.is_empty() {
        // (before chmod: a chown clears the set-user-ID bits)
        c.push_str(&format!("if [ -f \"$T\" ] && [ ! -L \"$T\" ]; then {chown}; fi; "));
    }
    if let Some(m) = mode {
        c.push_str(&format!("chmod {:o} -- \"$S\" || exit 1; ", m & 0o7777));
    }
    let taken = format!("exit {E_EXISTS}");
    let (on_link, on_file) = match (replace, backup) {
        (false, _) => (taken.clone(), taken.clone()),
        (true, None) => ("rm -f -- \"$T\" || exit 1".to_string(), ":".to_string()),
        (true, Some(b)) => (
            taken.clone(),
            format!(
                "B={}; if [ -L \"$B\" ]; then rm -f -- \"$B\" || exit 1; elif [ -d \"$B\" ]; then exit {E_BACKUP_DIR}; fi; mv -f -- \"$T\" \"$B\" || exit 1; moved=1",
                qp(b)?
            ),
        ),
    };
    c.push_str(&format!("moved=0; if [ -L \"$T\" ]; then {on_link}; elif [ -d \"$T\" ]; then {taken}; elif [ -e \"$T\" ]; then {on_file}; fi; "));
    let t = if mv_t { " -T" } else { "" };
    c.push_str(&format!("mv -f{t} -- \"$S\" \"$T\" || {{ if [ \"$moved\" = 1 ]; then mv -f{t} -- \"$B\" \"$T\"; fi; exit 1; }}"));
    Ok(c)
}

// ------------------------------------------------------------------------------------------------- the one check

/// How a folder is listed on this host, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lister {
    /// GNU `find -printf` with NUL between the fields: one process, every name exact.
    FindPrintf,
    /// `stat -c` (GNU, BusyBox) per name, the names printed by the shell with NUL after them.
    StatC,
    /// `stat -f` (FreeBSD, the other BSDs, macOS), the same way.
    StatF,
    /// `ls -la` parsed (nothing better there); `epoch`: GNU `ls` with `--time-style=+%s`.
    Ls { epoch: bool },
}

/// What one look at the server found (asked once per connection).
#[derive(Debug)]
pub struct ShellState {
    pub home: String,
    /// The login folder as `pwd`, `pwd -P` (links resolved) and `$HOME` named it: none of them, and no folder above one, is
    /// ever removed or moved.
    pub homes: Vec<String>,
    pub family: HostFamily,
    pub lister: Lister,
    readlink: bool,
    dd: bool,
    df: bool,
    /// `ls -q` (a character it cannot show becomes `?`).
    ls_q: bool,
    /// GNU `mv -T` (never moves into a folder at the target).
    mv_t: bool,
    /// What finds the file a link leads to: `readlink -f` or `realpath`.
    resolve: Option<&'static str>,
    /// GNU `chown --reference`.
    chown_ref: bool,
    /// The free space `df` said the last time the user did something here (the host tile shows it; it never runs `df`).
    last_free: Mutex<Option<Space>>,
}

impl ShellState {
    /// Every name of a listing is exact (`find`/`stat`); `false`: `ls` is parsed and unusual names may be read wrong.
    pub fn names_reliable(&self) -> bool {
        !matches!(self.lister, Lister::Ls { .. })
    }

    pub(crate) fn last_free(&self) -> Option<Space> {
        *self.last_free.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn keep_owner(&self) -> KeepOwner {
        match self.lister {
            _ if self.chown_ref => KeepOwner::Reference,
            Lister::FindPrintf | Lister::StatC => KeepOwner::Stat("-c %u:%g"),
            Lister::StatF => KeepOwner::Stat("-f %u:%g"),
            Lister::Ls { .. } => KeepOwner::No,
        }
    }
}

/// The question asked once per connection: the system (the same lines as `hostkind::PROBE`), the login folder and the
/// tools. Between `leaf-shell-begin` and `leaf-shell-end`; a menu shell prints something else.
pub const PROBE: &str = "echo leaf-shell-begin; echo \"leaf-uname $(uname -s 2>/dev/null)\"; \
if ls --help 2>&1 | head -n 3 | grep -qi busybox; then echo leaf-busybox; fi; \
echo \"leaf-pwd $(pwd)\"; echo \"leaf-pwdp $(pwd -P 2>/dev/null)\"; echo \"leaf-home $HOME\"; \
if find / -maxdepth 0 -printf '' >/dev/null 2>&1; then echo leaf-find-printf; fi; \
if stat -c %s / >/dev/null 2>&1; then echo leaf-stat-c; elif stat -f %z / >/dev/null 2>&1; then echo leaf-stat-f; fi; \
if LC_ALL=C ls -la --time-style=+%s / >/dev/null 2>&1; then echo leaf-ls-epoch; fi; \
if LC_ALL=C ls -laq / >/dev/null 2>&1; then echo leaf-ls-q; fi; \
if mv --help 2>&1 | grep -q -- --no-target-directory; then echo leaf-mv-t; fi; \
if chown --help 2>&1 | grep -q -- --reference; then echo leaf-chown-ref; fi; \
if readlink -f / >/dev/null 2>&1; then echo leaf-readlink-f; elif command -v realpath >/dev/null 2>&1; then echo leaf-realpath; fi; \
for t in readlink dd df; do if command -v $t >/dev/null 2>&1; then echo \"leaf-has $t\"; fi; done; \
echo leaf-shell-end";

/// Why the shell cannot stand in for SFTP.
#[derive(Debug, PartialEq, Eq)]
pub enum NoShell {
    /// The login printed a menu (OPNsense, pfSense and some appliances do that for root).
    Menu,
    /// The check did not run (a restricted shell, no shell at all, or no answer).
    NoAnswer,
}

/// What a [`PROBE`] run printed. A menu, an error or nothing is refused, never guessed at.
pub fn parse_probe(out: &str) -> Result<ShellState, NoShell> {
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let (Some(b), Some(e)) = (lines.iter().position(|l| *l == "leaf-shell-begin"), lines.iter().rposition(|l| *l == "leaf-shell-end")) else {
        return Err(if looks_like_menu(out) { NoShell::Menu } else { NoShell::NoAnswer });
    };
    if e < b {
        return Err(NoShell::NoAnswer);
    }
    let l = &lines[b..e];
    let has = |w: &str| l.contains(&w);
    // (an absolute path with no `.` or `..` part; a trailing `/` dropped)
    let value = |k: &str| {
        l.iter()
            .find_map(|x| x.strip_prefix(k))
            .map(str::trim)
            .filter(|v| v.starts_with('/') && !v.split('/').any(|s| s == "." || s == ".."))
            .map(|v| if v == "/" { v } else { v.trim_end_matches('/') })
    };
    let home = value("leaf-pwd ").or_else(|| value("leaf-home ")).unwrap_or("/").to_string();
    let mut homes: Vec<String> = Vec::new();
    for h in ["leaf-pwd ", "leaf-pwdp ", "leaf-home "].iter().filter_map(|k| value(k)).chain(std::iter::once(home.as_str())) {
        if !homes.iter().any(|x| x == h) {
            homes.push(h.to_string());
        }
    }
    let lister = if has("leaf-find-printf") {
        Lister::FindPrintf
    } else if has("leaf-stat-c") {
        Lister::StatC
    } else if has("leaf-stat-f") {
        Lister::StatF
    } else {
        Lister::Ls { epoch: has("leaf-ls-epoch") }
    };
    Ok(ShellState {
        home,
        homes,
        family: crate::hostkind::parse_probe(&l.join("\n")),
        lister,
        readlink: has("leaf-has readlink"),
        dd: has("leaf-has dd"),
        df: has("leaf-has df"),
        ls_q: has("leaf-ls-q"),
        mv_t: has("leaf-mv-t"),
        resolve: if has("leaf-readlink-f") {
            Some("readlink -f")
        } else if has("leaf-realpath") {
            Some("realpath")
        } else {
            None
        },
        chown_ref: has("leaf-chown-ref"),
        last_free: Mutex::new(None),
    })
}

/// A numbered menu with a prompt (OPNsense: ` 0) Logout ... 8) Shell ... Enter an option:`).
fn looks_like_menu(out: &str) -> bool {
    let l = out.to_ascii_lowercase();
    let numbered = out.lines().filter(|x| x.trim_start().split_once(')').is_some_and(|(n, _)| !n.is_empty() && n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))).count();
    l.contains("enter an option") || l.contains("select an option") || numbered >= 3
}

/// The look at a new connection that has no SFTP: runs [`PROBE`] (8 seconds at most; a menu that waits for a key is cut off
/// there) and tells a menu or a refusal in plain words.
pub(crate) async fn start(c: &Conn) -> Result<ShellState, FsError> {
    let mut ch = c.open_exec(PROBE).await?;
    let mut text = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(m) = ch.wait().await {
            match m {
                russh::ChannelMsg::Data { data } if text.len() < 64 * 1024 => text.extend_from_slice(&data),
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
    })
    .await;
    let _ = ch.close().await;
    match parse_probe(&String::from_utf8_lossy(&text)) {
        Ok(st) => {
            log::info!("remote: shell access ({:?}, {}, home {})", st.lister, st.family.name(), st.home);
            Ok(st)
        }
        Err(NoShell::Menu) => Err(FsError::Other(
            "This server has no SFTP, and its login opens a menu instead of a shell (as root on OPNsense does), so leaf cannot \
             reach the files. Turn on SFTP on the server, or log in as a user whose shell is sh or csh."
                .into(),
        )),
        Err(NoShell::NoAnswer) => Err(FsError::Other(
            "This server has no SFTP, and it did not run leaf's check command (no shell for this login, or a restricted one), \
             so leaf cannot reach the files."
                .into(),
        )),
    }
}

// ---------------------------------------------------------------------------------------------------------- parsing

/// One item as a listing or a look at one path printed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub size: u64,
    /// Unix seconds (0: unknown).
    pub mtime: i64,
    /// The permission bits (`0o7777` at most).
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub target: Option<Vec<u8>>,
    /// `ls` printed it in a way that may not be the real name (a link whose line holds ` -> ` more than once).
    pub unsure: bool,
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

fn kind_of_mode(raw: u32) -> Kind {
    match raw & S_IFMT {
        S_IFDIR => Kind::Dir,
        S_IFLNK => Kind::Link,
        _ => Kind::File,
    }
}

fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("").trim()
}

/// The fields of NUL-separated output (the last NUL ends the last field).
fn fields(out: &[u8]) -> Vec<&[u8]> {
    let mut f: Vec<&[u8]> = out.split(|&b| b == 0).collect();
    if f.last().is_some_and(|l| l.is_empty()) {
        f.pop();
    }
    f
}

/// The `-printf` format of a listing: type, mode, size, mtime, uid, gid, name, link target; `with_names` adds the owner's and
/// the group's names.
fn find_format(with_names: bool) -> &'static str {
    if with_names { "%y\\0%m\\0%s\\0%T@\\0%U\\0%G\\0%f\\0%l\\0%u\\0%g\\0" } else { "%y\\0%m\\0%s\\0%T@\\0%U\\0%G\\0%f\\0%l\\0" }
}

/// What `find -printf` with [`find_format`] printed.
pub fn parse_find(out: &[u8], with_names: bool) -> Vec<Rec> {
    let n = if with_names { 10 } else { 8 };
    fields(out)
        .chunks_exact(n)
        .filter_map(|f| {
            let kind = match text(f[0]) {
                "d" => Kind::Dir,
                "l" => Kind::Link,
                "" => return None,
                _ => Kind::File,
            };
            Some(Rec {
                name: f[6].to_vec(),
                kind,
                size: text(f[2]).parse().unwrap_or(0),
                mtime: text(f[3]).split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0),
                mode: u32::from_str_radix(text(f[1]), 8).ok().map(|m| m & 0o7777),
                uid: text(f[4]).parse().ok(),
                gid: text(f[5]).parse().ok(),
                owner: with_names.then(|| text(f[8]).to_string()).filter(|s| !s.is_empty()),
                group: with_names.then(|| text(f[9]).to_string()).filter(|s| !s.is_empty()),
                target: (kind == Kind::Link).then(|| f[7].to_vec()),
                unsure: false,
            })
        })
        .collect()
}

/// The `stat` format of one item: raw mode, size, mtime, uid, gid (numbers only: nothing in it comes from a name), and the
/// owner's and group's names on their own.
fn stat_format(l: Lister) -> (&'static str, &'static str, &'static str) {
    match l {
        Lister::StatF => ("-f", "%p %z %m %u %g", "%Su %Sg"),
        _ => ("-c", "%f %s %Y %u %g", "%U %G"),
    }
}

/// What the shell loop of a `stat` listing printed: per item its name, the `stat` line and the link's target, each ended by
/// NUL; `with_names` adds the owner line. `hex`: `stat -c %f` (GNU, BusyBox) prints the mode in hex, `stat -f %p` (BSD) in
/// octal.
pub fn parse_stat(out: &[u8], hex: bool, with_names: bool) -> Vec<Rec> {
    let n = if with_names { 4 } else { 3 };
    fields(out)
        .chunks_exact(n)
        .filter_map(|f| {
            let w: Vec<&str> = text(f[1]).split_whitespace().collect();
            if w.len() < 5 {
                return None;
            }
            let raw = u32::from_str_radix(w[0], if hex { 16 } else { 8 }).ok()?;
            let kind = kind_of_mode(raw);
            let names: Vec<&str> = if with_names { text(f[3]).split_whitespace().collect() } else { Vec::new() };
            Some(Rec {
                name: f[0].to_vec(),
                kind,
                size: w[1].parse().unwrap_or(0),
                mtime: w[2].parse().unwrap_or(0),
                mode: Some(raw & 0o7777),
                uid: w[3].parse().ok(),
                gid: w[4].parse().ok(),
                owner: names.first().map(|s| s.to_string()),
                group: names.get(1).map(|s| s.to_string()),
                target: (kind == Kind::Link).then(|| f[2].to_vec()),
                unsure: false,
            })
        })
        .collect()
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Days since 1970-01-01 of a date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The date of a day number (the inverse of [`days_from_civil`]).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `touch -t` form (`CCYYMMDDhhmm.SS`, read in UTC with `TZ=UTC0`) of a Unix time.
fn touch_stamp(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}{:02}{:02}.{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The permission bits of an `ls` mode string (`-rwsr-x--T`).
fn ls_mode(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let mut m = 0u32;
    for (i, (bit, sbit)) in [(0o400, 0), (0o200, 0), (0o100, 0o4000), (0o040, 0), (0o020, 0), (0o010, 0o2000), (0o004, 0), (0o002, 0), (0o001, 0o1000)].iter().enumerate() {
        match b[i + 1] {
            b'r' | b'w' | b'x' => m |= bit,
            b's' | b't' => m |= bit | sbit,
            b'S' | b'T' => m |= sbit,
            b'-' => {}
            _ => return None,
        }
    }
    Some(m)
}

/// Splits off the next run of non-blank bytes: (the word, the rest after it).
fn word(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let start = s.iter().position(|&b| b != b' ' && b != b'\t')?;
    let s = &s[start..];
    let end = s.iter().position(|&b| b == b' ' || b == b'\t').unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// The lines of `LC_ALL=C ls -la` (GNU, BusyBox, BSD; `--time-style=+%s` too). `now`: Unix seconds, for the year a recent
/// date leaves out. The time is read as UTC (`ls` prints the server's local time). `.` and `..` and the `total` line are left
/// out. A name with a newline, or a link whose name holds ` -> `, cannot be told apart here: that is why `ls` is the last
/// choice, and why [`read_ls`] says which names to doubt.
pub fn parse_ls(out: &[u8], now: i64) -> Vec<Rec> {
    parse_ls_counted(out, now).0
}

/// [`parse_ls`] and how many lines could not be read (skipped, never guessed). Every byte of the text comes from the server
/// and the names in it: nothing here indexes into it by position without `get`, and dates out of range are refused.
pub fn parse_ls_counted(out: &[u8], now: i64) -> (Vec<Rec>, usize) {
    let mut skipped = 0;
    let mut recs = Vec::new();
    for l in out.split(|&b| b == b'\n') {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        if l.is_empty() || l.starts_with(b"total ") || l == b"leaf-ctl" {
            continue;
        }
        match parse_ls_line(l, now) {
            Some(r) if r.name == b"." || r.name == b".." => {}
            Some(r) => recs.push(r),
            None => skipped += 1,
        }
    }
    (recs, skipped)
}

/// The earliest and latest second a listing's date may have (the years 1 to 9999).
const T_MIN: i64 = -62_135_596_800;
const T_MAX: i64 = 253_402_300_799;

/// The time of a date and clock time read from a listing; `None` when a part is out of range. The year is clamped to 1..=9999.
fn date_time(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(0..=23).contains(&hh) || !(0..=59).contains(&mm) || !(0..=60).contains(&ss) {
        return None;
    }
    Some(days_from_civil(y.clamp(1, 9999), m, d) * 86400 + hh * 3600 + mm * 60 + ss)
}

fn num_of(s: Option<&str>) -> Option<i64> {
    let s = s?;
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_ls_line(line: &[u8], now: i64) -> Option<Rec> {
    let (mode_s, rest) = word(line)?;
    let mode_s = std::str::from_utf8(mode_s).ok()?;
    let kind = match mode_s.as_bytes().first()? {
        b'd' => Kind::Dir,
        b'l' => Kind::Link,
        b'-' | b'c' | b'b' | b'p' | b's' => Kind::File,
        _ => return None,
    };
    let mode = ls_mode(mode_s)?;
    let (_links, rest) = word(rest)?;
    let (owner, rest) = word(rest)?;
    let (group, rest) = word(rest)?;
    let (size_w, mut rest) = word(rest)?;
    let mut size = text(size_w).parse::<u64>().ok();
    if size_w.ends_with(b",") {
        // a device: `major, minor`
        rest = word(rest)?.1;
        size = Some(0);
    } else if size.is_none() && size_w.contains(&b',') {
        size = Some(0);
    }
    let size = size?;
    let (d1, r1) = word(rest)?;
    let d1s = text(d1);
    let (mtime, rest) = if d1s.len() >= 9 && d1s.bytes().all(|b| b.is_ascii_digit()) {
        // `--time-style=+%s`
        (num_of(Some(d1s))?.clamp(T_MIN, T_MAX), r1)
    } else if d1s.len() == 10 && d1s.as_bytes().get(4) == Some(&b'-') {
        // `2024-01-02 03:04[:05.000000000] [+0000]` (GNU full-iso / long-iso)
        let (y, m, d) = (num_of(d1s.get(..4))?, num_of(d1s.get(5..7))?, num_of(d1s.get(8..))?);
        let (t, mut r2) = word(r1)?;
        let t = text(t);
        let (hh, mm) = (num_of(t.get(..2))?, num_of(t.get(3..5))?);
        let ss = num_of(t.get(6..8)).unwrap_or(0);
        if let Some((z, r3)) = word(r2)
            && (z.starts_with(b"+") || z.starts_with(b"-"))
            && z.len() == 5
        {
            r2 = r3;
        }
        (date_time(y, m, d, hh, mm, ss)?, r2)
    } else {
        // `Jan  2 03:04` (this half year) or `Jan  2  2024`
        let mon = MONTHS.iter().position(|m| *m == d1s)? as i64 + 1;
        let (day, r2) = word(r1)?;
        let day = num_of(Some(text(day)))?;
        let (yt, r3) = word(r2)?;
        let yt = text(yt);
        let t = if let Some((h, m)) = yt.split_once(':') {
            let (h, m) = (num_of(Some(h))?, num_of(m.get(..2))?);
            let (y_now, _, _) = civil_from_days(now.clamp(T_MIN, T_MAX).div_euclid(86400));
            let this = date_time(y_now, mon, day, h, m, 0)?;
            // (a date with no year is within the last half year: one that would lie ahead is last year's)
            if this > now.saturating_add(86400) { date_time(y_now - 1, mon, day, h, m, 0)? } else { this }
        } else {
            date_time(num_of(Some(yt))?, mon, day, 0, 0, 0)?
        };
        (t, r3)
    };
    // the name is what follows the one blank after the date
    let name = rest.strip_prefix(b" ").unwrap_or(rest);
    if name.is_empty() {
        return None;
    }
    let mut unsure = false;
    let (name, target) = match kind {
        Kind::Link => match name.windows(4).position(|w| w == b" -> ") {
            Some(i) => {
                let (n, t) = (name.get(..i)?, name.get(i + 4..)?);
                // (`a -> b -> c`: the link `a -> b` to `c`, or `a` to `b -> c`; which one cannot be told)
                unsure = t.windows(4).any(|w| w == b" -> ");
                (n, Some(t.to_vec()))
            }
            None => (name, Some(Vec::new())),
        },
        _ => (name, None),
    };
    let num = |w: &[u8]| text(w).parse::<u32>().ok();
    let named = |w: &[u8]| Some(text(w).to_string()).filter(|s| !s.is_empty() && !s.bytes().all(|b| b.is_ascii_digit()));
    Some(Rec { name: name.to_vec(), kind, size, mtime, mode: Some(mode), uid: num(owner), gid: num(group), owner: named(owner), group: named(group), target, unsure })
}

/// What an `ls` listing of one folder could not tell for sure. A delete, move, overwrite or chmod of an item it covers is
/// refused ([`refuses`]): on an `ls`-only host a name with a line break can print a line that reads as another entry
/// (`config\nzzz` shows a `config` beside the real one), and a link named `backup -> old` reads as `backup`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Doubt {
    /// Every name of the folder: a name in it holds a control character, and `ls -q` is not there to show it as `?`.
    pub all: bool,
    /// These names: shown with `?` by `ls -q` (another character is there), or a link line that holds ` -> ` twice.
    pub names: std::collections::HashSet<Vec<u8>>,
}

impl Doubt {
    pub fn is_empty(&self) -> bool {
        !self.all && self.names.is_empty()
    }
}

/// The start of an `ls` listing's output when a name in the folder holds a control character (the shell's own glob looked).
const CTL_MARK: &[u8] = b"leaf-ctl\n";

/// What an `ls` listing printed (`q`: it was asked with `-q` when [`CTL_MARK`] came first), with what to doubt in it and how
/// many lines could not be read.
pub fn read_ls(out: &[u8], q: bool, now: i64) -> (Vec<Rec>, Doubt, usize) {
    let ctl = out.starts_with(CTL_MARK);
    let (recs, skipped) = parse_ls_counted(out.get(if ctl { CTL_MARK.len() } else { 0 }..).unwrap_or_default(), now);
    let mut d = Doubt { all: ctl && !q, ..Doubt::default() };
    for r in &recs {
        if r.unsure || (ctl && r.name.contains(&b'?')) || r.name.iter().any(|b| b.is_ascii_control()) {
            d.names.insert(r.name.clone());
        }
    }
    (recs, d, skipped)
}

/// Whether `doubt` (of the folder an item is in) forbids changing the item `name`.
pub fn refuses(doubt: &Doubt, name: &str) -> bool {
    doubt.all || doubt.names.contains(name.as_bytes())
}

/// What leaf says when it does not touch an item whose name it may have read wrong.
fn unreliable() -> FsError {
    FsError::Other(
        "Names in this folder cannot be read reliably on this server (it has only ls, and a name here holds a line break, a \
         control character or \" -> \"), so leaf does not delete, move, overwrite or change this item. Rename the odd name on \
         the server first."
            .into(),
    )
}

/// The doubts of the `ls` listings of this run, by `authority` and folder (on the server). Kept for the whole run, not per
/// connection: the next connection to the host acts on entries the last listing showed.
fn doubts() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Doubt>> {
    static D: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Doubt>>> = std::sync::OnceLock::new();
    D.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The free space a `df -Pk` run printed (the line after the header; a file system name with blanks is read from the right).
pub fn parse_df(out: &str) -> Option<Space> {
    let line = out.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let w: Vec<&str> = line.split_whitespace().collect();
    let pct = w.iter().rposition(|x| x.ends_with('%'))?;
    if pct < 3 {
        return None;
    }
    let avail: u64 = w[pct - 1].parse().ok()?;
    let total: u64 = w[pct - 3].parse().ok()?;
    Some(Space { free: avail.saturating_mul(1024), total: Some(total.saturating_mul(1024)) })
}

// ---------------------------------------------------------------------------------------------------------- commands

/// Every command starts in the C locale (English messages, plain dates).
fn cmd(body: &str) -> String {
    format!("LC_ALL=C; export LC_ALL; {body}")
}

/// The listing command of folder `dir` (a quoted, absolute path).
fn list_command(st: &ShellState, dir: &str) -> String {
    let check = format!("D={dir}; if [ ! -e \"$D\" ]; then exit {E_NOT_FOUND}; fi; if [ ! -d \"$D\" ]; then exit {E_NOT_DIR}; fi; cd \"$D\" 2>/dev/null || exit {E_DENIED}; ");
    let readlink = if st.readlink { "readlink -- \"./$f\" 2>/dev/null" } else { "printf ''" };
    let body = match st.lister {
        // (-H: a link given as the folder is followed, as SFTP's OPENDIR does; nothing under it is)
        Lister::FindPrintf => format!("{check}exec find -H \"$D\" -mindepth 1 -maxdepth 1 -printf '{}'", find_format(false)),
        Lister::StatC | Lister::StatF => {
            let (flag, f, _) = stat_format(st.lister);
            format!(
                "{check}for f in * .[!.]* ..?*; do if [ -e \"$f\" ] || [ -L \"$f\" ]; then \
                 s=$(stat {flag} '{f}' -- \"./$f\" 2>/dev/null) || continue; t=''; if [ -L \"$f\" ]; then t=$({readlink}); fi; \
                 printf '%s\\000%s\\000%s\\000' \"$f\" \"$s\" \"$t\"; fi; done; exit 0"
            )
        }
        Lister::Ls { epoch } => {
            let ls = if epoch { "ls -la --time-style=+%s --quoting-style=literal" } else { "ls -la" };
            // a name with a control character (the shell's glob looks, `c` holds them all): `leaf-ctl` first, and `ls -q` when
            // the server has it (the character shown as `?`; not always: in the C locale it turns every non-ASCII byte into `?`)
            let q = if st.ls_q { " -q" } else { "" };
            format!(
                "{check}c=$(printf '\\001\\002\\003\\004\\005\\006\\007\\010\\011\\012\\013\\014\\015\\016\\017\\020\\021\\022\\023\\024\\025\\026\\027\\030\\031\\032\\033\\034\\035\\036\\037\\177x'); \
                 c=${{c%x}}; for f in * .[!.]* ..?*; do case \"$f\" in *[$c]*) echo leaf-ctl; exec {ls}{q};; esac; done; exec {ls}"
            )
        }
    };
    cmd(&body)
}

/// The command that describes one path (never following a link).
fn stat_command(st: &ShellState, p: &str) -> String {
    let check = format!("P={p}; if [ ! -e \"$P\" ] && [ ! -L \"$P\" ]; then exit {E_NOT_FOUND}; fi; ");
    let body = match st.lister {
        Lister::FindPrintf => format!("{check}exec find \"$P\" -maxdepth 0 -printf '{}'", find_format(true)),
        Lister::StatC | Lister::StatF => {
            let (flag, f, names) = stat_format(st.lister);
            let readlink = if st.readlink { "readlink -- \"$P\" 2>/dev/null" } else { "printf ''" };
            format!(
                "{check}s=$(stat {flag} '{f}' -- \"$P\") || exit 1; o=$(stat {flag} '{names}' -- \"$P\" 2>/dev/null); t=''; \
                 if [ -L \"$P\" ]; then t=$({readlink}); fi; printf 'x\\000%s\\000%s\\000%s\\000' \"$s\" \"$t\" \"$o\""
            )
        }
        Lister::Ls { epoch: true } => format!("{check}exec ls -ld --time-style=+%s --quoting-style=literal -- \"$P\""),
        Lister::Ls { epoch: false } => format!("{check}exec ls -ld -- \"$P\""),
    };
    cmd(&body)
}

fn parse_records(st: &ShellState, out: &[u8], one: bool) -> Vec<Rec> {
    match st.lister {
        Lister::FindPrintf => parse_find(out, one),
        Lister::StatC => parse_stat(out, true, one),
        Lister::StatF => parse_stat(out, false, one),
        Lister::Ls { .. } => parse_ls(out, leaf_now()),
    }
}

fn leaf_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// What went wrong, from a command's exit code and the start of its error text (C locale).
fn failure(o: &RawOut) -> FsError {
    let err = String::from_utf8_lossy(&o.stderr);
    let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(200).collect::<String>();
    match o.code {
        Some(E_NOT_FOUND) => return FsError::NotFound,
        Some(E_EXISTS) => return FsError::Exists,
        Some(E_DENIED) => return FsError::Denied,
        Some(E_NOT_DIR) => return FsError::Other("This is not a folder on the server".into()),
        Some(E_LINK) => return FsError::Other("This is a link on the server; leaf does not write through it".into()),
        Some(E_BACKUP_DIR) => return FsError::Other("A folder has the backup's name".into()),
        Some(E_CUT) => return FsError::Other("The server could not cut the file at the point to resume from".into()),
        Some(127) => return FsError::Other(format!("The server lacks a program leaf needs for this ({line})")),
        _ => {}
    }
    let l = err.to_ascii_lowercase();
    if l.contains("no such file") || l.contains("not found") && !l.contains("command not found") {
        FsError::NotFound
    } else if l.contains("permission denied") || l.contains("operation not permitted") {
        FsError::Denied
    } else if l.contains("file exists") {
        FsError::Exists
    } else if l.contains("not empty") {
        FsError::NotEmpty
    } else if l.contains("read-only file system") {
        FsError::Other("The server's disk is read-only here".into())
    } else if l.contains("no space left") {
        FsError::Other("The server's disk is full".into())
    } else if line.is_empty() {
        FsError::Other(format!("The command on the server failed (exit code {})", o.code.map_or("unknown".to_string(), |c| c.to_string())))
    } else {
        FsError::Other(format!("The command on the server failed ({line})"))
    }
}

// ------------------------------------------------------------------------------------------------------------- ShellFs

/// How long a command may take (a listing of a big folder on a slow router: a few minutes).
const LONG: Duration = Duration::from_secs(5 * 60);

/// The files of one shell connection. Made per call by `SftpFs` from the live connection (so a broken one is made again
/// by the pool), and used like any other `Fs`.
pub(crate) struct ShellFs {
    c: Arc<Conn>,
    st: Arc<ShellState>,
    limit: Duration,
}

impl ShellFs {
    pub(crate) fn new(c: Arc<Conn>, limit: Duration) -> Option<ShellFs> {
        let st = c.shell.clone()?;
        Some(ShellFs { c, st, limit })
    }

    /// The server's path: `/~` and `/~/...` stand for the login folder.
    fn spath(&self, v: &VPath) -> String {
        let p = v.path();
        match p.strip_prefix("/~") {
            Some("") => self.st.home.clone(),
            Some(rest) if rest.starts_with('/') => format!("{}{rest}", self.st.home.trim_end_matches('/')),
            _ => p.to_string(),
        }
    }

    fn run(&self, command: &str, limit: Duration, cancelled: &dyn Fn() -> bool) -> Result<RawOut, FsError> {
        let c = self.c.clone();
        c.busy.fetch_add(1, Ordering::Relaxed);
        let r = conn::block_on(cancelled, c.exec_raw(command, limit));
        c.busy.fetch_sub(1, Ordering::Relaxed);
        r
    }

    /// Runs `command`; a non-zero exit is the error it stands for.
    fn ok(&self, command: &str) -> Result<RawOut, FsError> {
        let o = self.run(command, self.limit, &|| false)?;
        if o.code == Some(0) { Ok(o) } else { Err(failure(&o)) }
    }

    /// What is at `path` (a server path), `None` when nothing is.
    fn lstat_path(&self, path: &str) -> Result<Option<Rec>, FsError> {
        let o = self.run(&stat_command(&self.st, &qp(path)?), self.limit, &|| false)?;
        match o.code {
            Some(0) => parse_records(&self.st, &o.stdout, true).into_iter().next().map(Some).ok_or_else(|| FsError::Other("The server's answer about this item could not be read".into())),
            Some(E_NOT_FOUND) => Ok(None),
            _ => match failure(&o) {
                FsError::NotFound => Ok(None),
                e => Err(e),
            },
        }
    }

    /// Refuses `sp` when it is a home folder (by any of the names the probe found for it) or a folder above one.
    fn home_guard(&self, sp: &str) -> Result<(), FsError> {
        if self.st.homes.iter().chain(std::iter::once(&self.st.home)).any(|h| home_at_or_under(sp, h)) {
            return Err(FsError::Denied);
        }
        Ok(())
    }

    /// Refuses to change `v` when the `ls` listing it was shown in may have read its name wrong (see [`Doubt`]).
    fn sure(&self, v: &VPath) -> Result<(), FsError> {
        if !matches!(self.st.lister, Lister::Ls { .. }) {
            return Ok(());
        }
        let name = v.name();
        if name.bytes().any(|b| b.is_ascii_control()) {
            return Err(unreliable());
        }
        let Some(parent) = v.parent() else { return Ok(()) };
        if doubts().get(&doubt_key(v, &self.spath(&parent))).is_some_and(|d| refuses(d, name)) {
            return Err(unreliable());
        }
        Ok(())
    }

    /// The file a link at `path` leads to (`readlink -f` or `realpath`), for a write through it.
    fn resolve(&self, path: &str) -> Result<String, FsError> {
        let Some(tool) = self.st.resolve else {
            return Err(FsError::Other(
                "This is a link on the server, and the server has neither readlink -f nor realpath to find the file it leads to, so \
                 leaf does not write through it"
                    .into(),
            ));
        };
        // (the path is absolute: never an option)
        let o = self.ok(&cmd(&format!("exec {tool} {}", qp(path)?)))?;
        let out = o.stdout.strip_suffix(b"\n").unwrap_or(&o.stdout);
        match std::str::from_utf8(out) {
            Ok(s) if s.starts_with('/') && s != "/" && !s.contains('\n') => Ok(s.to_string()),
            _ => Err(FsError::Other("leaf could not read where this link on the server leads".into())),
        }
    }

    /// Runs a command that may take long and can stop half way (a delete, a move to another disk): no Cancel reaches this far
    /// (the `Fs` calls have none), so after [`LONG`] its process group is stopped on the server (`Conn::exec_watch` with
    /// `leaf-pid`) and the error says it may be partly done.
    fn run_stoppable(&self, command: &str) -> Result<RawOut, FsError> {
        let started = std::time::Instant::now();
        let c = self.c.clone();
        let mut text = String::new();
        c.busy.fetch_add(1, Ordering::Relaxed);
        let r = conn::rt().block_on(c.exec_watch(
            &format!("exec 2>&1; echo leaf-pid $$; {command}"),
            None,
            &|| started.elapsed() > LONG,
            &|| false,
            &mut |t: &str| {
                if text.len() < 16 * 1024 {
                    text.push_str(t);
                }
            },
            &mut |_| {},
        ));
        c.busy.fetch_sub(1, Ordering::Relaxed);
        let partly = |more: &str| FsError::Offline(format!("{more}. It may be partly done: look at the folder before you try again."));
        match r {
            Ok(end) if end.unstopped => Err(partly("The server did not finish in time, and leaf could not stop the command there (it may still run)")),
            Ok(end) if end.code.is_none() => Err(partly("The connection ended before the server said the command was done")),
            Ok(end) => {
                let err: String = text.lines().filter(|l| !l.trim().starts_with("leaf-pid ")).collect::<Vec<_>>().join("\n");
                Ok(RawOut { stdout: Vec::new(), stderr: err.into_bytes(), code: end.code, truncated: false })
            }
            Err(FsError::Cancelled) => Err(partly(&format!("The server did not finish in {} minutes, and leaf stopped the command there", LONG.as_secs() / 60))),
            Err(e) => Err(e),
        }
    }

    /// `open_write` with `name.leaf-bak`: the file there now becomes `backup` (a sibling name) before the new one takes the
    /// name (H10 over a shell host).
    ///
    /// A new file is made under `umask 077` (no one else can read it while it uploads, or when a broken upload leaves it); the
    /// mode it ends with is set at the move: the replaced file's, or for a new file what the server's umask gives
    /// (`0666 & ~umask`). Replacing through a link writes the file the link leads to (as the local editor does), and the link
    /// stays; a server that cannot tell where it leads gets a refusal, never the link replaced by a file.
    pub(crate) fn writer(&self, p: &VPath, mode: WriteMode, backup: Option<&str>) -> Result<Box<dyn WriteSink>, FsError> {
        if p.is_root() {
            return Err(FsError::Denied);
        }
        let parent = p.parent().ok_or(FsError::Denied)?;
        let mut target = self.spath(p);
        let mut existing = self.lstat_path(&target)?;
        if existing.as_ref().is_some_and(|r| r.kind == Kind::Dir) {
            return Err(FsError::Exists);
        }
        if existing.is_some() && mode != WriteMode::CreateNew {
            self.sure(p)?;
        }
        let (command, base, place) = match mode {
            WriteMode::At(off) => {
                // (never used by leaf's copies today: they write a part file anew; kept exact for a caller that resumes)
                match &existing {
                    Some(r) if r.kind == Kind::Link => return Err(FsError::Other("This is a link on the server; leaf does not write through it".into())),
                    None => return Err(FsError::NotFound),
                    Some(r) if off > r.size => return Err(FsError::Other("the offset is past the end of the file".into())),
                    Some(r) if off < r.size && !self.st.dd => return Err(FsError::Unsupported),
                    Some(_) => {}
                }
                // cut at the offset only when the file is longer (dd's `seek=` with nothing to copy, which cuts there on GNU,
                // BusyBox and BSD), the size checked afterwards, then append
                let body = format!(
                    "P={}; if [ -L \"$P\" ]; then exit {E_LINK}; fi; if [ ! -f \"$P\" ]; then exit {E_NOT_FOUND}; fi; \
                     if [ $(( $(wc -c < \"$P\") )) -ne {off} ]; then dd if=/dev/null of=\"$P\" bs=1 seek={off} 2>/dev/null || exit 1; \
                     [ $(( $(wc -c < \"$P\") )) -eq {off} ] || exit {E_CUT}; fi; exec 3>>\"$P\" || exit 1; {}",
                    qp(&target)?,
                    stream("")
                );
                (cmd(&body), off, None)
            }
            WriteMode::CreateNew | WriteMode::Replace => {
                if mode == WriteMode::CreateNew && existing.is_some() {
                    return Err(FsError::Exists);
                }
                if backup.is_some() && existing.as_ref().is_none_or(|r| r.kind != Kind::File) {
                    return Err(FsError::Other("There is no file on the server to keep a backup of".into()));
                }
                // (`set -C`: the file is made new, never written through something that is there; `umask 077`: only the user
                // can read it until it is in place; `M`: the umask before, printed for the mode at the end)
                let new_file = |path: &str, end: &str| -> Result<String, FsError> {
                    Ok(cmd(&format!(
                        "P={}; M=$(umask); case \"$M\" in ''|*[!0-7]*) M=022;; esac; umask 077; set -C; exec 3>\"$P\" || exit 1; \
                         echo \"leaf-umask $M\"; {}",
                        qp(path)?,
                        stream(end)
                    )))
                };
                if crate::sftp::is_part_name(p.name()) && backup.is_none() {
                    if existing.is_some() {
                        return Err(FsError::Exists);
                    }
                    // (the copy's own part file takes the real name with a plain rename later: it gets its final mode here)
                    (new_file(&target, "chmod \"$(printf %o $((0666 & ~0$M)))\" -- \"$P\" || exit 1; ")?, 0, None)
                } else {
                    if existing.as_ref().is_some_and(|r| r.kind == Kind::Link) {
                        let real = self.resolve(&target)?;
                        match self.lstat_path(&real)? {
                            None => return Err(FsError::Other("This link on the server leads nowhere; leaf does not write through it".into())),
                            Some(r) if r.kind != Kind::File => return Err(FsError::Other("This link on the server does not lead to a file; leaf does not write through it".into())),
                            r => existing = r,
                        }
                        target = real;
                    }
                    let (dir, name) = target.rsplit_once('/').ok_or(FsError::Denied)?;
                    let temp = format!("{dir}/{}", crate::sftp::side_name(name, "leaf-up"));
                    let backup = backup.map(|b| self.spath(&parent.join(b)));
                    // (a new version keeps the mode bits of the file it replaces)
                    let keep = existing.as_ref().and_then(|r| r.mode);
                    (new_file(&temp, "")?, 0, Some(Place { temp, target, replace: mode == WriteMode::Replace, backup, mode: keep }))
                }
            }
        };
        let mut ch = conn::block_on(&|| false, self.c.open_exec(&command))?;
        // the file is open on the server before a byte is sent (a folder that cannot be written fails here, not at the end)
        let head = conn::block_on(&|| false, opened(&mut ch, self.limit))?;
        let mut place = place;
        if let Some(pl) = place.as_mut()
            && pl.mode.is_none()
        {
            // a new file: what the server's umask gives it; unknown, only the user's (never more than asked)
            let umask = String::from_utf8_lossy(&head).lines().find_map(|l| l.trim().strip_prefix("leaf-umask ").and_then(|m| u32::from_str_radix(m.trim(), 8).ok()));
            pl.mode = Some(umask.map_or(0o600, |u| 0o666 & !u));
        }
        self.c.busy.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Writer { fs: ShellFs { c: self.c.clone(), st: self.st.clone(), limit: self.limit }, ch: Some(ch), buf: Vec::new(), sent: 0, base, place, done: false, failed: false }))
    }
}

/// The key of a folder's doubts: the address's authority and the folder's server path.
fn doubt_key(v: &VPath, dir: &str) -> String {
    format!("{}\u{0}{dir}", v.authority())
}

/// The end of a write command, once the shell has the file open on descriptor 3: says so, copies its input there, closes
/// it, runs `end`, and prints the size the file has now.
fn stream(end: &str) -> String {
    format!("echo leaf-open; cat >&3 || exit 1; exec 3>&-; {end}printf 'leaf-size %s\\n' \"$(wc -c < \"$P\")\"")
}

/// Waits for a write command's `leaf-open` line; its failure (the file could not be made) as the error. What it printed up
/// to that line comes back.
async fn opened(ch: &mut russh::Channel<russh::client::Msg>, limit: Duration) -> Result<Vec<u8>, FsError> {
    let mut out = RawOut::default();
    let r = tokio::time::timeout(limit, async {
        while let Some(m) = ch.wait().await {
            match m {
                russh::ChannelMsg::Data { data } if out.stdout.len() < 64 * 1024 => {
                    out.stdout.extend_from_slice(&data);
                    if out.stdout.windows(10).any(|w| w == b"leaf-open\n") {
                        return true;
                    }
                }
                russh::ChannelMsg::ExtendedData { data, ext: 1 } if out.stderr.len() < 16 * 1024 => out.stderr.extend_from_slice(&data),
                russh::ChannelMsg::ExitStatus { exit_status } => out.code = Some(exit_status),
                russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        false
    })
    .await;
    match r {
        Ok(true) => Ok(out.stdout),
        Ok(false) => {
            let _ = ch.close().await;
            Err(if out.code == Some(0) { FsError::Other("The server did not open the file".into()) } else { failure(&out) })
        }
        Err(_) => {
            let _ = ch.close().await;
            Err(FsError::Offline("the server did not open the file in time".into()))
        }
    }
}

fn entry_of(name: &str, r: &Rec) -> Entry {
    let m = r.mtime.saturating_mul(1000);
    let mut e = match r.kind {
        Kind::Dir => Entry::new(name, EntryKind::Dir, 0, m, m, rf_core::attrs::DIRECTORY),
        Kind::File => Entry::new(name, EntryKind::File, r.size, m, m, 0),
        Kind::Link => {
            let mut e = Entry::new(name, EntryKind::File, 0, m, m, rf_core::attrs::REPARSE_POINT);
            e.link = true;
            e
        }
    };
    e.mode = r.mode.map_or(0, |p| p as u16);
    e.uid = r.uid.unwrap_or(u32::MAX);
    e
}

fn meta_of(r: &Rec) -> Meta {
    Meta { kind: r.kind, size: r.size, modified_ms: r.mtime.saturating_mul(1000), created_ms: 0, mode: r.mode }
}

impl Fs for ShellFs {
    fn caps(&self) -> Caps {
        Caps { write: true, permissions: true, set_times: true, links: true, free_space: self.st.df, exec: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let o = self.run(&list_command(&self.st, &qp(&self.spath(dir))?), LONG, cancelled)?;
        // (GNU find ends with 1 when an item vanished while it looked: what it printed is still right)
        let fine = o.code == Some(0) || (self.st.lister == Lister::FindPrintf && o.code == Some(1) && !o.stdout.is_empty());
        if !fine {
            return Err(failure(&o));
        }
        // a cut listing is never shown as the whole folder (a sync or a delete of what is not in it would act on it)
        if o.truncated {
            log::warn!("remote: the listing of a folder on {} was cut at its size limit", dir.host());
            return Err(FsError::Other("This folder holds more than leaf reads in one listing over shell commands (4 MiB of names), so leaf does not show a part of it as the whole folder".into()));
        }
        let recs = match self.st.lister {
            Lister::Ls { .. } => {
                let (recs, doubt, skipped) = read_ls(&o.stdout, self.st.ls_q, leaf_now());
                if skipped > 0 {
                    log::warn!("remote: {skipped} lines of an ls listing on {} could not be read and are left out", dir.host());
                }
                let key = doubt_key(dir, &self.spath(dir));
                let mut all = doubts();
                if doubt.is_empty() {
                    all.remove(&key);
                } else {
                    log::warn!("remote: names in a folder on {} cannot be read reliably with ls; changing them is refused", dir.host());
                    all.insert(key, doubt);
                }
                recs
            }
            _ => parse_records(&self.st, &o.stdout, false),
        };
        let mut sent = 0;
        let mut left_out = 0usize;
        let mut pending = Vec::new();
        for r in recs {
            if cancelled() {
                return Err(FsError::Cancelled);
            }
            // a name that is not UTF-8 or not one part of an address is left out and counted, never guessed
            match std::str::from_utf8(&r.name) {
                Ok(n) if leaf_vfs::names::address_problem(n).is_none() => pending.push(entry_of(n, &r)),
                Ok("." | "..") => {}
                _ => left_out += 1,
            }
            if pending.len() >= batch.max(1) {
                sent += pending.len();
                on_batch(std::mem::take(&mut pending));
            }
        }
        if !pending.is_empty() {
            sent += pending.len();
            on_batch(pending);
        }
        if left_out > 0 {
            log::warn!("remote: {left_out} names in a listing of {} cannot be used (not UTF-8, empty, / or \\) and are left out", dir.host());
        }
        Ok(sent)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        self.lstat_path(&self.spath(p))?.map(|r| meta_of(&r)).ok_or(FsError::NotFound)
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        let q = qp(&self.spath(p))?;
        let check = format!("P={q}; if [ ! -e \"$P\" ]; then exit {E_NOT_FOUND}; fi; if [ -d \"$P\" ]; then exit {E_NOT_DIR}; fi; if [ ! -r \"$P\" ]; then exit {E_DENIED}; fi; ");
        const BLOCK: u64 = 64 * 1024;
        let body = if offset == 0 {
            format!("{check}exec cat -- \"$P\"")
        } else if offset % BLOCK == 0 && self.st.dd {
            format!("{check}exec dd if=\"$P\" bs={BLOCK} skip={}", offset / BLOCK)
        } else {
            format!("{check}exec tail -c +{} -- \"$P\"", offset + 1)
        };
        let mut ch = conn::block_on(&|| false, self.c.open_exec(&cmd(&body)))?;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, FsError>>(16);
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        conn::rt().spawn(async move {
            let mut out = RawOut::default();
            loop {
                tokio::select! {
                    _ = &mut stopped => {
                        let _ = ch.close().await;
                        return;
                    }
                    m = ch.wait() => match m {
                        Some(russh::ChannelMsg::Data { data }) => {
                            if tx.send(Ok(data.to_vec())).await.is_err() {
                                let _ = ch.close().await;
                                return;
                            }
                        }
                        Some(russh::ChannelMsg::ExtendedData { data, ext: 1 }) if out.stderr.len() < 16 * 1024 => out.stderr.extend_from_slice(&data),
                        Some(russh::ChannelMsg::ExitStatus { exit_status }) => out.code = Some(exit_status),
                        Some(russh::ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
            }
            match out.code {
                Some(0) => {}
                // (no exit code: the end of the bytes cannot be told from a cut line)
                None => {
                    let _ = tx.send(Err(FsError::Offline("the server ended the read without saying it was complete".into()))).await;
                }
                Some(_) => {
                    let _ = tx.send(Err(failure(&out))).await;
                }
            }
        });
        self.c.busy.fetch_add(1, Ordering::Relaxed);
        let mut r = Reader { c: self.c.clone(), rx, buf: Vec::new(), pos: 0, stop: Some(stop), limit: self.limit };
        // the first answer: a file that cannot be read fails here, as an SFTP open would
        match r.next() {
            Err(e) => Err(e),
            Ok(()) => Ok(Box::new(r)),
        }
    }

    fn open_write(&self, p: &VPath, o: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
        self.writer(p, o.mode, None)
    }

    fn mkdir(&self, p: &VPath) -> Result<(), FsError> {
        let body = format!("P={}; if [ -e \"$P\" ] || [ -L \"$P\" ]; then exit {E_EXISTS}; fi; exec mkdir -- \"$P\"", qp(&self.spath(p))?);
        match self.ok(&cmd(&body)) {
            Ok(_) => Ok(()),
            // (a missing parent says "No such file")
            Err(e) => Err(e),
        }
    }

    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), FsError> {
        if from.scheme() != to.scheme() || from.authority() != to.authority() {
            return Err(FsError::Unsupported);
        }
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) {
            return Err(FsError::Other("a folder cannot be moved into itself".into()));
        }
        if from.is_root() || to.is_root() {
            return Err(FsError::Denied);
        }
        self.sure(from)?;
        self.sure(to)?;
        let (a, b) = (self.spath(from), self.spath(to));
        let src = self.lstat_path(&a)?.ok_or(FsError::NotFound)?;
        if src.kind == Kind::Dir {
            self.home_guard(&a)?;
            if self.lstat_path(&b)?.is_some() {
                return Err(FsError::Exists);
            }
        }
        // (a move to another disk of the server copies: it may take a while, and is stopped there after `LONG`)
        let o = self.run_stoppable(&cmd(&move_command(&a, &b, replace, None, None, KeepOwner::No, self.st.mv_t)?))?;
        if o.code == Some(0) { Ok(()) } else { Err(failure(&o)) }
    }

    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), FsError> {
        if p.is_root() || p.path() == "/~" {
            return Err(FsError::Denied);
        }
        self.sure(p)?;
        let sp = self.spath(p);
        self.home_guard(&sp)?;
        let r = self.lstat_path(&sp)?.ok_or(FsError::NotFound)?;
        // (a link is unlinked, never followed: `rm` does not follow it, and it is never removed as a folder; callers pass
        // `recursive` for every item, links too: what is there decides)
        let recursive = recursive && r.kind == Kind::Dir;
        let o = self.run_stoppable(&cmd(&rm_command(&sp, &self.st.homes, recursive)?))?;
        match o.code {
            Some(0) => Ok(()),
            _ => match failure(&o) {
                // rmdir of a folder with something in it
                FsError::Other(_) if r.kind == Kind::Dir && !recursive => Err(FsError::NotEmpty),
                e => Err(e),
            },
        }
    }

    fn set_meta(&self, p: &VPath, m: &MetaChange) -> Result<(), FsError> {
        if m.modified_ms.is_none() && m.mode.is_none() {
            return Ok(());
        }
        self.sure(p)?;
        let sp = self.spath(p);
        // chmod and touch follow a link: a link's target is never changed through it
        let r = self.lstat_path(&sp)?.ok_or(FsError::NotFound)?;
        if r.kind == Kind::Link {
            return Err(FsError::Unsupported);
        }
        let mut body = format!("P={}; ", qp(&sp)?);
        if let Some(mode) = m.mode {
            body.push_str(&format!("chmod {:o} -- \"$P\" || exit 1; ", mode & 0o7777));
        }
        if let Some(ms) = m.modified_ms {
            let t = touch_stamp(ms.max(0) / 1000);
            // (`-m`: the access time stays; a touch without `-m` sets both)
            body.push_str(&format!("TZ=UTC0 touch -c -m -t {t} -- \"$P\" 2>/dev/null || TZ=UTC0 touch -c -t {t} -- \"$P\" || exit 1; "));
        }
        body.push_str("exit 0");
        self.ok(&cmd(&body)).map(|_| ())
    }

    fn details(&self, p: &VPath) -> Result<Details, FsError> {
        let r = self.lstat_path(&self.spath(p))?.ok_or(FsError::NotFound)?;
        let link = r.kind == Kind::Link;
        Ok(Details {
            mode: r.mode,
            uid: r.uid,
            gid: r.gid,
            owner: r.owner.clone(),
            group: r.group.clone(),
            link_target: r.target.as_ref().map(|t| String::from_utf8_lossy(t).into_owned()).filter(|t| !t.is_empty()),
            can_chmod: !link && r.mode.is_some(),
        })
    }

    fn free_space(&self, p: &VPath) -> Result<Option<Space>, FsError> {
        if !self.st.df {
            return Ok(None);
        }
        let o = self.run(&cmd(&format!("exec df -Pk -- {}", qp(&self.spath(p))?)), self.limit, &|| false)?;
        let s = parse_df(&String::from_utf8_lossy(&o.stdout));
        if s.is_some() {
            *self.st.last_free.lock().unwrap_or_else(|e| e.into_inner()) = s;
        }
        Ok(s)
    }
}

// -------------------------------------------------------------------------------------------------------- read / write

struct Reader {
    c: Arc<Conn>,
    rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, FsError>>,
    buf: Vec<u8>,
    pos: usize,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    limit: Duration,
}

impl Reader {
    /// The next piece into `buf` (empty at the end). Waits at most the request time limit for it.
    fn next(&mut self) -> Result<(), FsError> {
        let limit = self.limit;
        let got = conn::rt().block_on(async { tokio::time::timeout(limit, self.rx.recv()).await });
        self.c.touch();
        self.pos = 0;
        match got {
            Err(_) => Err(FsError::Offline("the server did not answer in time".into())),
            Ok(None) => {
                self.buf.clear();
                Ok(())
            }
            Ok(Some(Ok(b))) => {
                self.buf = b;
                Ok(())
            }
            Ok(Some(Err(e))) => Err(e),
        }
    }
}

fn io_of(e: FsError) -> std::io::Error {
    match e {
        FsError::NotFound => std::io::Error::from(std::io::ErrorKind::NotFound),
        FsError::Denied => std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        e => std::io::Error::other(e.to_string()),
    }
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.buf.len() {
            self.next().map_err(io_of)?;
            if self.buf.is_empty() {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        self.c.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Where a written temporary file goes in `finish`.
struct Place {
    temp: String,
    target: String,
    replace: bool,
    backup: Option<String>,
    mode: Option<u32>,
}

const CHUNK: usize = 64 * 1024;

/// A file being written by `cat > file` (or `cat >>` when resuming): the bytes go to the command's standard input.
struct Writer {
    fs: ShellFs,
    ch: Option<russh::Channel<russh::client::Msg>>,
    buf: Vec<u8>,
    sent: u64,
    /// The size the file had before (resuming: the offset).
    base: u64,
    place: Option<Place>,
    done: bool,
    failed: bool,
}

impl Writer {
    fn send(&mut self, data: &[u8]) -> Result<(), FsError> {
        let Some(ch) = self.ch.as_ref() else { return Err(FsError::Other("the upload broke off".into())) };
        let limit = self.fs.limit;
        let r = conn::rt().block_on(async { tokio::time::timeout(limit, ch.data(data)).await });
        self.fs.c.touch();
        match r {
            Err(_) => Err(FsError::Offline("the server did not take the data in time".into())),
            Ok(Err(e)) => Err(FsError::Offline(e.to_string())),
            Ok(Ok(())) => {
                self.sent += data.len() as u64;
                Ok(())
            }
        }
    }

    fn drain(&mut self) -> Result<(), FsError> {
        while !self.buf.is_empty() {
            let n = self.buf.len().min(CHUNK);
            let chunk: Vec<u8> = self.buf.drain(..n).collect();
            if let Err(e) = self.send(&chunk) {
                self.failed = true;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Ends the input and waits for the command: its exit code and the size it found.
    fn end(&mut self) -> Result<(), FsError> {
        let Some(mut ch) = self.ch.take() else { return Err(FsError::Other("the upload broke off".into())) };
        let limit = self.fs.limit;
        let r = conn::rt().block_on(async {
            ch.eof().await.map_err(|e| FsError::Offline(e.to_string()))?;
            let mut out = RawOut::default();
            let read = async {
                while let Some(m) = ch.wait().await {
                    match m {
                        russh::ChannelMsg::Data { data } if out.stdout.len() < 4096 => out.stdout.extend_from_slice(&data),
                        russh::ChannelMsg::ExtendedData { data, ext: 1 } if out.stderr.len() < 16 * 1024 => out.stderr.extend_from_slice(&data),
                        russh::ChannelMsg::ExitStatus { exit_status } => out.code = Some(exit_status),
                        russh::ChannelMsg::Close => break,
                        _ => {}
                    }
                }
            };
            // (the last bytes may still be on their way to a slow disk: a few minutes at most)
            tokio::time::timeout(limit.max(Duration::from_secs(120)), read).await.map_err(|_| FsError::Offline("the server did not finish writing in time".into()))?;
            Ok::<RawOut, FsError>(out)
        })?;
        if r.code != Some(0) {
            return Err(failure(&r));
        }
        let size = String::from_utf8_lossy(&r.stdout).lines().find_map(|l| l.trim().strip_prefix("leaf-size ").and_then(|n| n.trim().parse::<u64>().ok()));
        let want = self.base + self.sent;
        match size {
            Some(n) if n == want => Ok(()),
            Some(n) => Err(FsError::Other(format!("The file on the server has {n} bytes, but {want} were sent"))),
            None => Err(FsError::Other("The server did not say how much it wrote".into())),
        }
    }
}

impl std::io::Write for Writer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.failed || self.ch.is_none() {
            return Err(std::io::Error::other("the upload broke off"));
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.drain().map_err(io_of)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("the upload broke off"));
        }
        self.drain().map_err(io_of)
    }
}

impl WriteSink for Writer {
    fn finish(mut self: Box<Self>) -> Result<(), FsError> {
        let r = (|| {
            self.drain()?;
            self.end()?;
            match &self.place {
                Some(p) => self.fs.ok(&cmd(&move_command(&p.temp, &p.target, p.replace, p.backup.as_deref(), p.mode, self.fs.st.keep_owner(), self.fs.st.mv_t)?)).map(|_| ()),
                None => Ok(()),
            }
        })();
        if r.is_err() {
            self.failed = true;
        } else {
            self.done = true;
            self.place = None;
        }
        r
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.fs.c.busy.fetch_sub(1, Ordering::Relaxed);
        if self.done {
            return;
        }
        // not finished: the command's input is closed (it ends), and the temporary file removed (best effort; the real
        // name was never touched). A resumed file and the copy's own part file stay: the copy decides about them.
        let ch = self.ch.take();
        let rm = self.place.take().and_then(|p| sh_quote(&p.temp).ok().filter(|_| p.temp.starts_with('/')).map(|q| cmd(&format!("rm -f -- {q}"))));
        let c = self.fs.c.clone();
        conn::rt().spawn(async move {
            if let Some(ch) = ch {
                let _ = ch.close().await;
            }
            if let Some(rm) = rm {
                // (after the command ended: a moment for the server to see the closed channel)
                tokio::time::sleep(Duration::from_millis(300)).await;
                let _ = c.exec_raw(&rm, Duration::from_secs(30)).await;
            }
        });
    }
}

// ------------------------------------------------------------------------------------------------------------ chown

/// The command that changes the owner (and group) of `path`, never through a link (`-h`). Owner and group are names or
/// numbers of the usual form (letters, digits, `.`, `_`, `-`, not starting with `-`).
pub fn chown_command(path: &str, owner: &str, group: Option<&str>) -> Result<String, FsError> {
    let ok = |s: &str| !s.is_empty() && !s.starts_with('-') && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok(owner) || group.is_some_and(|g| !ok(g)) {
        return Err(FsError::Other("This is not a user or group name the server takes".into()));
    }
    let who = match group {
        Some(g) => format!("{owner}:{g}"),
        None => owner.to_string(),
    };
    Ok(cmd(&format!("exec chown -h -- {} {}", sh_quote(&who)?, qp(path)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one quoting function: what it writes, and that a POSIX shell reads it back as the same text.
    #[test]
    fn shell_quoting_table() {
        let table: &[(&str, &str)] = &[
            ("", "''"),
            ("plain", "'plain'"),
            ("with space", "'with space'"),
            ("it's", r"'it'\''s'"),
            ("''", r"''\'''\'''"),
            ("$HOME", "'$HOME'"),
            ("`id`", "'`id`'"),
            ("$(rm -rf /)", "'$(rm -rf /)'"),
            ("a\nb", "'a\nb'"),
            ("-rf", "'-rf'"),
            ("--", "'--'"),
            ("Grüße ☃", "'Grüße ☃'"),
            ("back\\slash", "'back\\slash'"),
            ("*?[a]", "'*?[a]'"),
            ("semi;colon|pipe&", "'semi;colon|pipe&'"),
        ];
        for (raw, want) in table {
            let q = sh_quote(raw).unwrap();
            assert_eq!(&q, want, "quoting {raw:?}");
            assert_eq!(unquote(&q).as_deref(), Some(*raw), "read back {raw:?}");
        }
        // a NUL cannot be carried: refused, never cut
        assert!(sh_quote("a\0b").is_err());
        // paths must be absolute, so a leading dash can never be an option
        assert!(qp("-rf").is_err());
        assert!(qp("").is_err());
        assert_eq!(qp("/-rf").unwrap(), "'/-rf'");
    }

    #[test]
    fn shell_rm_never_for_dangerous_paths() {
        // the login folder as pwd, pwd -P and $HOME named it (FreeBSD: /home is a link to /usr/home)
        let homes = vec!["/home/user".to_string(), "/usr/home/user".to_string()];
        for bad in ["", "/", "//", "/home", "/home/", "/home/user", "/home/user/", "/usr", "/usr/home", "/usr/home/user", "relative", "-rf", "/a/../b", "/a/./b", "/a//b", "/a/b/", "/a\0b", "/..", "/."] {
            for recursive in [true, false] {
                assert!(rm_command(bad, &homes, recursive).is_err(), "{bad:?} must be refused");
            }
        }
        // (a home of `/`: nothing is under it to be refused for, but `/` itself still is)
        assert!(rm_command("/", &["/".to_string()], true).is_err());
        let c = rm_command("/srv/it's a $dir", &homes, true).unwrap();
        assert!(c.starts_with("P='/srv/it'\\''s a $dir'; if [ ! -e \"$P\" ] && [ ! -L \"$P\" ]; then exit 40; fi; "), "{c}");
        assert!(c.ends_with("exec rm -rf -- \"$P\""), "{c}");
        // the server resolves the folder's links and refuses / and every home and folder above one there as well
        assert!(c.contains("R=$(cd -P \"$P\" 2>/dev/null && pwd -P) || exit 43; case \"$R\" in ''|/) exit 43;; esac; for H in '/home/user' '/usr/home/user'; do case \"$H/\" in \"$R\"/*) exit 43;; esac; done; "), "{c}");
        let c = rm_command("/home/user/-x", &homes, false).unwrap();
        assert!(c.contains("rmdir -- \"$P\"") && c.contains("rm -f -- \"$P\"") && !c.contains("-rf"));
        // the path is quoted once and only used through "$P"
        assert_eq!(c.matches("'/home/user/-x'").count(), 1);
        // `..` never reaches any command
        assert!(qp("/a/../b").is_err() && qp("/a/./b").is_err() && qp("/a/b..c").is_ok());
    }

    /// Review item 1: names an `ls`-only host may have printed wrong are never deleted.
    #[test]
    fn shell_ls_unreliable_names_refused_for_delete() {
        let now = days_from_civil(2024, 6, 1) * 86400;
        // `config\nzzz` printed raw: its first line reads as a second `config` beside the real one (no `ls -q` there)
        let raw = b"leaf-ctl\n-rw-r--r-- 1 user user 5 Jan  2  2024 config\nzzz\n-rw-r--r-- 1 root root 9 Jan  2  2024 config\n";
        let (r, d, _) = read_ls(raw, false, now);
        assert_eq!(r.iter().filter(|r| r.name == b"config").count(), 2);
        assert!(d.all && refuses(&d, "config") && refuses(&d, "anything"));
        // the same folder with `ls -q`: the odd name is `config?zzz`, which is refused; the real `config` is not
        let q = b"leaf-ctl\n-rw-r--r-- 1 user user 5 Jan  2  2024 config?zzz\n-rw-r--r-- 1 root root 9 Jan  2  2024 config\n";
        let (_, d, _) = read_ls(q, true, now);
        assert!(!d.all && refuses(&d, "config?zzz") && !refuses(&d, "config"));
        // a link named `backup -> old` beside a real folder `backup`: `backup` is refused (a recursive remove would hit the folder)
        let link = b"drwxr-xr-x 2 root root 4096 Jan  2  2024 backup\nlrwxrwxrwx 1 root root 1 Jan  2  2024 backup -> old -> x\n";
        let (r, d, _) = read_ls(link, false, now);
        assert_eq!(r.len(), 2);
        assert!(!d.all && refuses(&d, "backup") && !refuses(&d, "other"));
        // a plain folder: nothing to doubt
        let (_, d, _) = read_ls(b"-rw-r--r-- 1 root root 9 Jan  2  2024 config\nlrwxrwxrwx 1 root root 4 Jan  2  2024 l -> /tmp\n", false, now);
        assert!(d.is_empty());
    }

    /// Review item 2: forged `ls` lines never panic (the release build aborts on a panic); a line that does not parse is
    /// skipped and counted.
    #[test]
    fn shell_ls_forged_lines_do_not_panic() {
        let forged: &[&[u8]] = &[
            "-rw-r--r-- 1 u u 5 2024-é1-02 03:04 a".as_bytes(),
            "-rw-r--r-- 1 u u 5 2024-01-é2 03:04 a".as_bytes(),
            "-rw-r--r-- 1 u u 5 2024-01-0é 03:04 a".as_bytes(),
            "-rw-r--r-- 1 u u 5 2024-01-02 é3:04 a".as_bytes(),
            b"-rw-r--r-- 1 u u 5 9999-99-99 99:99 a",
            b"-rw-r--r-- 1 u u 5 Jan  2  999999999999999999 a",
            b"-rw-r--r-- 1 u u 5 Jan  2  -9223372036854775808 a",
            b"-rw-r--r-- 1 u u 5 Jan  99999999999999 2024 a",
            b"-rw-r--r-- 1 u u 5 Jan  2 99999999999:99 a",
            b"-rw-r--r-- 1 u u 5 99999999999999999999999 a",
            b"-rw-r--r-- 1 u u 5 9223372036854775807 a",
            "-rw-r--r-- 1 u u 5 Jan  2 03:é4 a".as_bytes(),
            "lrwxrwxrwx 1 u u 5 Jan  2  2024 é -> ".as_bytes(),
            b"l",
            b"-",
            b"-rw-r--r--",
            b"\xff\xfe 1 u u 5 Jan  2  2024 a",
        ];
        let now = days_from_civil(2024, 6, 1) * 86400;
        let mut all = Vec::new();
        for l in forged {
            let _ = parse_ls_line(l, now);
            all.extend_from_slice(l);
            all.push(b'\n');
        }
        all.extend_from_slice(b"-rw-r--r-- 1 u u 5 Jan  2  2024 fine\n");
        let (r, skipped) = parse_ls_counted(&all, now);
        assert!(r.iter().any(|r| r.name == b"fine") && skipped >= 10, "{skipped}");
        // whatever was read has a time within the years 1..=9999
        assert!(r.iter().all(|r| (T_MIN..=T_MAX).contains(&r.mtime)));
        let _ = parse_ls(&[0xe2, 0x82], i64::MAX);
        let _ = parse_ls(b"-rw-r--r-- 1 u u 5 Jan  2 03:04 a\n", i64::MIN);
    }

    #[test]
    fn shell_parse_gnu() {
        // GNU find -printf (find_format(false)): a file, a folder, a link, and names with a newline, a quote and a leading dash
        let mut out = Vec::new();
        for f in [
            ["f", "644", "1234", "1704164645.1234567890", "1000", "1000", "notes.txt", ""],
            ["d", "755", "4096", "1704164000.0000000000", "0", "0", "my dir", ""],
            ["l", "777", "11", "1704164001.5000000000", "1000", "100", "-link", "/etc/passwd"],
            ["f", "4755", "0", "1704164002.0", "0", "0", "a\nb'c", ""],
        ] {
            for x in f {
                out.extend_from_slice(x.as_bytes());
                out.push(0);
            }
        }
        out.extend_from_slice(b"f\0644\0"); // a cut record is dropped
        let r = parse_find(&out, false);
        assert_eq!(r.len(), 4);
        assert_eq!((r[0].name.as_slice(), r[0].kind, r[0].size, r[0].mtime, r[0].mode, r[0].uid), (&b"notes.txt"[..], Kind::File, 1234, 1704164645, Some(0o644), Some(1000)));
        assert_eq!((r[1].name.as_slice(), r[1].kind), (&b"my dir"[..], Kind::Dir));
        assert_eq!((r[2].name.as_slice(), r[2].kind, r[2].target.as_deref()), (&b"-link"[..], Kind::Link, Some(&b"/etc/passwd"[..])));
        assert_eq!((r[3].name.as_slice(), r[3].mode), (&b"a\nb'c"[..], Some(0o4755)));
        // with names (one path)
        let one = parse_find(b"f\0600\05\01700000000\01000\01000\0x\0\0user\0users\0", true);
        assert_eq!((one[0].owner.as_deref(), one[0].group.as_deref()), (Some("user"), Some("users")));
        // GNU stat -c '%f %s %Y %u %g' (hex mode) in the shell loop
        let r = parse_stat(b"a b\081a4 12 1704164645 1000 1000\0\0lnk\0a1ff 4 1704164645 0 0\0target dir\0d\x80\0041ed 4096 1704164645 0 0\0\0", true, false);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].kind, r[0].mode, r[0].size), (Kind::File, Some(0o644), 12));
        assert_eq!((r[1].kind, r[1].target.as_deref()), (Kind::Link, Some(&b"target dir"[..])));
        // a name that is not UTF-8 comes through as its bytes (the listing leaves it out, it is never guessed)
        assert_eq!((r[2].name.as_slice(), r[2].kind), (&b"d\x80"[..], Kind::Dir));
        // GNU ls with --time-style=+%s
        let r = parse_ls(b"total 8\ndrwxr-xr-x 2 user user 4096 1704164645 .\n-rw-r--r-- 1 user user 5 1704164645  two  spaces\nlrwxrwxrwx 1 root root 7 1704164645 l -> /x y\n", 1_710_000_000);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].name.as_slice(), r[0].mtime, r[0].owner.as_deref()), (&b" two  spaces"[..], 1704164645, Some("user")));
        assert_eq!((r[1].name.as_slice(), r[1].target.as_deref()), (&b"l"[..], Some(&b"/x y"[..])));
        // GNU df -Pk
        let s = parse_df("Filesystem     1024-blocks    Used Available Capacity Mounted on\n/dev/sda1         102400   2048     100352       2% /\n").unwrap();
        assert_eq!((s.free, s.total), (100352 * 1024, Some(102400 * 1024)));
    }

    #[test]
    fn shell_parse_busybox() {
        // BusyBox stat -c (same format letters as GNU)
        let r = parse_stat(b"config\081b4 900 1700000000 0 0\0\0", true, false);
        assert_eq!((r[0].name.as_slice(), r[0].mode, r[0].mtime), (&b"config"[..], Some(0o664), 1700000000));
        // one path with the owner line
        let one = parse_stat(b"x\041ed 0 1700000000 0 0\0\0root root\0", true, true);
        assert_eq!((one[0].kind, one[0].owner.as_deref()), (Kind::Dir, Some("root")));
        // BusyBox ls -la: padded columns, a date this half year (no year) and an older one, a device, a link
        let now = days_from_civil(2024, 3, 1) * 86400;
        let out = b"drwxr-xr-x    2 root     root             0 Jan  2 03:04 .\n\
drwxr-xr-x   17 root     root             0 Jan  1  1970 ..\n\
-rw-r--r--    1 root     root           512 Feb 28 23:59 dhcp.leases\n\
-rw-------    1 1000     1000            12 Dec 31 10:00 last year\n\
-rw-r--r--    1 root     root          4096 Mar  5  2021 old.conf\n\
crw-rw-rw-    1 root     root        1,   3 Jan  2 03:04 null\n\
lrwxrwxrwx    1 root     root            12 Jan  2 03:04 resolv.conf -> /tmp/resolv.conf\n";
        let r = parse_ls(out, now);
        assert_eq!(r.len(), 5);
        assert_eq!((r[0].name.as_slice(), r[0].size, r[0].mtime), (&b"dhcp.leases"[..], 512, days_from_civil(2024, 2, 28) * 86400 + 23 * 3600 + 59 * 60));
        // December with no year, seen in March: last year's
        assert_eq!((r[1].name.as_slice(), r[1].mtime, r[1].uid, r[1].mode), (&b"last year"[..], days_from_civil(2023, 12, 31) * 86400 + 36000, Some(1000), Some(0o600)));
        assert_eq!(r[2].mtime, days_from_civil(2021, 3, 5) * 86400);
        assert_eq!((r[3].name.as_slice(), r[3].size), (&b"null"[..], 0));
        assert_eq!((r[4].kind, r[4].target.as_deref()), (Kind::Link, Some(&b"/tmp/resolv.conf"[..])));
        // BusyBox df -Pk
        let s = parse_df("Filesystem           1024-blocks    Used Available Capacity Mounted on\n/dev/root                 5120      5120         0 100% /rom\n").unwrap();
        assert_eq!(s.free, 0);
    }

    #[test]
    fn shell_parse_freebsd() {
        // FreeBSD stat -f '%p %z %m %u %g' (octal mode with the type)
        let r = parse_stat(b"rc.conf\0100644 2048 1704164645 0 0\0\0usr\040755 512 1704164645 0 0\0\0sh\0120755 7 1704164645 0 0\0/bin/sh\0", false, false);
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].kind, r[0].mode, r[0].size), (Kind::File, Some(0o644), 2048));
        assert_eq!(r[1].kind, Kind::Dir);
        assert_eq!((r[2].kind, r[2].target.as_deref()), (Kind::Link, Some(&b"/bin/sh"[..])));
        // one path with stat -f '%Su %Sg'
        let one = parse_stat(b"x\0104755 10 1704164645 0 0\0\0root wheel\0", false, true);
        assert_eq!((one[0].mode, one[0].group.as_deref()), (Some(0o4755), Some("wheel")));
        // FreeBSD ls -la (sticky bit, a file flag column is not printed without -o)
        let now = days_from_civil(2024, 6, 1) * 86400;
        let r = parse_ls(b"total 16\ndrwxrwxrwt  2 root  wheel   512 May 30 12:00 tmp\n-rw-r--r--  1 root  wheel  1234 Jan  2  2023 motd\n", now);
        assert_eq!((r[0].kind, r[0].mode), (Kind::Dir, Some(0o1777)));
        assert_eq!((r[1].name.as_slice(), r[1].mtime), (&b"motd"[..], days_from_civil(2023, 1, 2) * 86400));
        // FreeBSD df -Pk
        let s = parse_df("Filesystem 1024-blocks Used Avail Capacity  Mounted on\nzroot/ROOT/default 20000000 1000000 19000000     5%    /\n").unwrap();
        assert_eq!(s.free, 19000000 * 1024);
        // the one check: a menu is told, never parsed; a real answer is read
        assert_eq!(parse_probe("  0) Logout                              7) Ping host\n  1) Assign interfaces   8) Shell\n  2) Set interface IP address 9) pfTop\nEnter an option: ").unwrap_err(), NoShell::Menu);
        assert_eq!(parse_probe("").unwrap_err(), NoShell::NoAnswer);
        let st = parse_probe("banner\nleaf-shell-begin\nleaf-uname FreeBSD\nleaf-pwd /home/user\nleaf-stat-f\nleaf-has readlink\nleaf-has df\nleaf-shell-end\n").unwrap();
        assert_eq!((st.lister, st.family, st.home.as_str(), st.dd, st.df), (Lister::StatF, HostFamily::FreeBsd, "/home/user", false, true));
        // touch -t in UTC
        assert_eq!(touch_stamp(1704164645), "202401020304.05");
    }
}
