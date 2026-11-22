//! The Review page's work: the changes of a repository file by file and hunk by hunk, and what accepting or reverting them does.
//!
//! In a repository a file's hunks are what differs between the index and the work tree (`git diff`), so "Accept" is staging:
//! a whole file with `git add -A -- <file>`, one hunk by handing that hunk as a patch to `git apply --cached`. What is staged
//! already shows as accepted (`git diff --cached`). [`commit`] runs `git commit` with the user's message and nothing else.
//!
//! "Revert" of one hunk is done here, not by git: the work file is read, checked against the stamp taken when the diff was made
//! (a file changed since is refused), the hunk's lines are put back, the result is written beside the file and renamed in, and the
//! file's previous content goes to the caller's `put_away` (leaf: the Trash) first. Only the hunk's own lines change: every other
//! byte of the file stays as it was, and the lines put back get the file's own line end (a CRLF file stays CRLF).
//!
//! git is run by argument list, never through a shell; everything blocks: call it off the UI thread.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::gitread::{self, DiffLine, FileDiff, LineKind};

/// The most diff lines read for one file ("show all" draws them all; past this the diff is cut).
pub const MAX_LINES: usize = 20_000;

/// One hunk of a file's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    /// Line texts without their line end (decoded for display: a byte that is not UTF-8 shows as U+FFFD).
    pub lines: Vec<DiffLine>,
    /// The same lines as the file's own bytes (without the line end and a CR before it): what a revert compares and writes.
    pub bytes: Vec<Vec<u8>>,
    /// `bytes` are the file's exact bytes. `false` for a hunk decoded from text that was not valid UTF-8 (a checkpoint comparison
    /// of such a file): it is shown but never reverted.
    pub exact: bool,
    /// The hunk exactly as git printed it, byte for byte (header, lines, "\ No newline" markers): the patch for
    /// `git apply --cached`. `None` for a hunk not made by `git diff` (a new file, a checkpoint comparison).
    pub raw: Option<Vec<u8>>,
    /// git marked the old side's last line "\ No newline at end of file": a revert writes it without a line end.
    pub old_no_eol: bool,
    /// All its lines were read (a diff cut short leaves its last hunk incomplete: nothing may be done with it).
    pub complete: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RDiff {
    pub hunks: Vec<RHunk>,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    pub truncated: bool,
}

/// What the work file looked like when its diff was made: a revert is refused when it no longer does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub hash: u64,
}

impl Stamp {
    pub fn of_bytes(b: &[u8]) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        Self { len: b.len() as u64, hash: h }
    }

    /// The stamp of the file now, `None` when it cannot be read (gone, a folder).
    pub fn of_file(p: &Path) -> Option<Self> {
        // read in blocks: the same value as `of_bytes` of the whole file, without holding a big file in memory
        use std::io::Read;
        let mut f = std::fs::File::open(p).ok()?;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut len = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            for &x in &buf[..n] {
                h ^= x as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            len += n as u64;
        }
        Some(Self { len, hash: h })
    }
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    let mut it = s.splitn(2, ',');
    let start = it.next()?.parse().ok()?;
    let count = match it.next() {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

/// Parses `git diff` output of one file into hunks that keep git's own text. At most `max_lines` lines; then `truncated` is set and
/// the hunk that was cut is marked incomplete.
pub fn parse_hunks(text: &[u8], max_lines: usize) -> RDiff {
    let mut d = RDiff::default();
    let mut kept = 0usize;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut in_hunk = false;
    for raw_line in text.split_inclusive(|&b| b == b'\n') {
        let line_b = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
        // (only for the header lines and the display; the bytes of a line are kept as they are)
        let line_s = String::from_utf8_lossy(line_b);
        let line: &str = &line_s;
        if let Some(rest) = line.strip_prefix("@@ -") {
            let mut parts = rest.split_whitespace();
            let o = parts.next().and_then(parse_range);
            let n = parts.next().and_then(|s| s.strip_prefix('+')).and_then(parse_range);
            let (Some((os, oc)), Some((ns, nc))) = (o, n) else {
                in_hunk = false;
                continue;
            };
            if kept >= max_lines {
                d.truncated = true;
                break;
            }
            old_no = os;
            new_no = ns;
            in_hunk = true;
            d.hunks.push(RHunk {
                old_start: os,
                old_count: oc,
                new_start: ns,
                new_count: nc,
                header: line.to_string(),
                lines: Vec::new(),
                bytes: Vec::new(),
                exact: true,
                raw: Some(raw_line.to_vec()),
                old_no_eol: false,
                complete: true,
            });
            continue;
        }
        if line.starts_with("diff --git ") {
            // (a second file: not ours)
            if !d.hunks.is_empty() {
                break;
            }
            continue;
        }
        if !in_hunk {
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                d.binary = true;
            }
            continue;
        }
        let Some(h) = d.hunks.last_mut() else { continue };
        let kind = match line.as_bytes().first() {
            Some(b'+') => LineKind::Added,
            Some(b'-') => LineKind::Removed,
            Some(b' ') => LineKind::Context,
            // (an empty line inside a hunk: a blank context line printed without its space)
            None => LineKind::Context,
            Some(b'\\') => {
                if let Some(r) = h.raw.as_mut() {
                    r.extend_from_slice(raw_line);
                }
                // (the marker is about the line before it: a removed or unchanged one is the old side's last)
                if h.lines.last().is_some_and(|l| l.kind != LineKind::Added) {
                    h.old_no_eol = true;
                }
                continue;
            }
            _ => continue,
        };
        if kept >= max_lines {
            d.truncated = true;
            h.complete = false;
            break;
        }
        kept += 1;
        if let Some(r) = h.raw.as_mut() {
            r.extend_from_slice(raw_line);
        }
        let body_b = line_b.get(1..).unwrap_or_default();
        let body_b = body_b.strip_suffix(b"\r").unwrap_or(body_b);
        h.bytes.push(body_b.to_vec());
        let body = String::from_utf8_lossy(body_b).into_owned();
        let (o, n) = match kind {
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            LineKind::Added => {
                d.added += 1;
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                d.removed += 1;
                old_no += 1;
                (Some(old_no - 1), None)
            }
        };
        h.lines.push(DiffLine { kind, old_no: o, new_no: n, text: body });
    }
    // a hunk with fewer lines than its header says was cut
    for h in &mut d.hunks {
        let old = h.lines.iter().filter(|l| l.kind != LineKind::Added).count() as u32;
        let new = h.lines.iter().filter(|l| l.kind != LineKind::Removed).count() as u32;
        if old != h.old_count || new != h.new_count {
            h.complete = false;
        }
    }
    d
}

/// A diff made another way (a new file's lines, a checkpoint comparison) as review hunks. `swap` exchanges the sides (the
/// checkpoint diff has the file now as its old side).
pub fn from_file_diff(f: FileDiff, swap: bool) -> RDiff {
    let mut d = RDiff { binary: f.binary, truncated: f.truncated, ..Default::default() };
    let last = f.hunks.len().saturating_sub(1);
    for (i, h) in f.hunks.into_iter().enumerate() {
        let mut lines = h.lines;
        let (mut os, mut ns) = (h.old_start, h.new_start);
        if swap {
            std::mem::swap(&mut os, &mut ns);
            for l in &mut lines {
                std::mem::swap(&mut l.old_no, &mut l.new_no);
                l.kind = match l.kind {
                    LineKind::Added => LineKind::Removed,
                    LineKind::Removed => LineKind::Added,
                    k => k,
                };
            }
        }
        let oc = lines.iter().filter(|l| l.kind != LineKind::Added).count() as u32;
        let nc = lines.iter().filter(|l| l.kind != LineKind::Removed).count() as u32;
        d.added += lines.iter().filter(|l| l.kind == LineKind::Added).count() as u32;
        d.removed += lines.iter().filter(|l| l.kind == LineKind::Removed).count() as u32;
        let header = format!("@@ -{os},{oc} +{ns},{nc} @@");
        let bytes: Vec<Vec<u8>> = lines.iter().map(|l| l.text.as_bytes().to_vec()).collect();
        // (text decoded with replacement characters is not the file's bytes)
        let exact = !lines.iter().any(|l| l.text.contains('\u{FFFD}'));
        d.hunks.push(RHunk { old_start: os, old_count: oc, new_start: ns, new_count: nc, header, lines, bytes, exact, raw: None, old_no_eol: false, complete: !(f.truncated && i == last) });
    }
    d
}

fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).args(["--no-pager", "-c", "core.quotepath=false"]);
    // (a path is a path: "[ab].txt" or ":(top)x" must never match other files)
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_LITERAL_PATHSPECS", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Runs a git command that writes, with `input` on its standard input. `Err` is git's own text (or a short reason).
fn run_write(root: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let mut c = git(root);
    c.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let _turn = leaf_proc::GIT.enter();
    let mut child = leaf_proc::spawn_helper(&mut c).map_err(|e| format!("git could not be started: {e}"))?;
    if let (Some(data), Some(mut w)) = (input, child.stdin.take()) {
        w.write_all(data).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        let e = text(&out.stderr);
        Err(if e.is_empty() { text(&out.stdout) } else { e })
    }
}

fn read_diff(root: &Path, args: &[&str]) -> Option<RDiff> {
    let (raw, cut) = gitread::run(root, args, gitread::TIMEOUT, gitread::MAX_OUT)?;
    let mut d = parse_hunks(&raw, MAX_LINES);
    d.truncated |= cut;
    if cut && let Some(h) = d.hunks.last_mut() {
        h.complete = false;
    }
    Some(d)
}

/// What is not staged yet of one file (index against work tree). `None` when git could not answer.
pub fn unstaged(root: &Path, rel: &str) -> Option<RDiff> {
    read_diff(root, &["-c", "diff.suppressBlankEmpty=false", "diff", "--no-color", "--no-ext-diff", "--no-textconv", "-U3", "--", rel])
}

/// What is staged of one file (HEAD against the index). `from`: a rename's old name.
pub fn staged(root: &Path, rel: &str, from: Option<&str>) -> Option<RDiff> {
    let mut args = vec!["-c", "diff.suppressBlankEmpty=false", "diff", "--cached", "--no-color", "--no-ext-diff", "--no-textconv", "-M", "-U3", "--"];
    if let Some(f) = from {
        args.push(f);
    }
    args.push(rel);
    read_diff(root, &args)
}

/// An untracked file as all added lines (the first MiB).
pub fn untracked(path: &Path) -> Option<RDiff> {
    let len = std::fs::metadata(path).ok()?.len();
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(1024 * 1024).read_to_end(&mut buf).ok()?;
    let mut d = from_file_diff(gitread::all_added(&buf, MAX_LINES), false);
    if len > 1024 * 1024 {
        d.truncated = true;
        if let Some(h) = d.hunks.last_mut() {
            h.complete = false;
        }
    }
    Some(d)
}

/// The bytes of a file as git holds it: `spec` is `HEAD:path` or `:path` (the index). At most 32 MiB; `None` when it is not there.
pub fn blob(root: &Path, spec: &str) -> Option<Vec<u8>> {
    let (raw, cut) = gitread::run(root, &["show", "--no-textconv", spec], gitread::TIMEOUT, 32 * 1024 * 1024)?;
    (!cut).then_some(raw)
}

/// Lines added and removed per file: `git diff --numstat` (unstaged) or with `cached` (staged). A binary file counts (0, 0).
pub fn numstat(root: &Path, cached: bool) -> Option<std::collections::HashMap<String, (u32, u32)>> {
    let mut args = vec!["diff", "--numstat", "-z", "--no-renames", "--no-color"];
    if cached {
        args.insert(1, "--cached");
    }
    let (raw, _) = gitread::run(root, &args, gitread::TIMEOUT, gitread::MAX_OUT)?;
    Some(parse_numstat(&String::from_utf8_lossy(&raw)))
}

pub fn parse_numstat(text: &str) -> std::collections::HashMap<String, (u32, u32)> {
    text.split('\0')
        .filter_map(|rec| {
            let mut f = rec.splitn(3, '\t');
            let a = f.next()?.trim().parse().unwrap_or(0);
            let r = f.next()?.trim().parse().unwrap_or(0);
            let p = f.next()?.trim_start_matches(['\n', '\r']);
            (!p.is_empty()).then(|| (p.to_string(), (a, r)))
        })
        .collect()
}

fn check_rel(rel: &str) -> Result<(), String> {
    if rel.is_empty() || rel.starts_with('-') {
        return Err("The file name is not valid".into());
    }
    Ok(())
}

/// Accept a whole file: stage it as it is now (a deleted file: its deletion).
pub fn stage_file(root: &Path, rel: &str) -> Result<(), String> {
    check_rel(rel)?;
    run_write(root, &["add", "-A", "--", rel], None).map(|_| ())
}

/// Takes a file out of the index again (what "Accept" did), keeping the work file.
pub fn unstage_file(root: &Path, rel: &str) -> Result<(), String> {
    check_rel(rel)?;
    run_write(root, &["restore", "--staged", "--", rel], None).map(|_| ())
}

/// git's way of writing a path in a patch header: as it is, or C-quoted when it holds a quote, a backslash or a control character.
fn patch_name(prefix: &str, rel: &str) -> String {
    if rel.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        let mut s = String::from("\"");
        s.push_str(prefix);
        for c in rel.chars() {
            match c {
                '"' => s.push_str("\\\""),
                '\\' => s.push_str("\\\\"),
                '\t' => s.push_str("\\t"),
                '\n' => s.push_str("\\n"),
                c => s.push(c),
            }
        }
        s.push('"');
        s
    } else {
        format!("{prefix}{rel}")
    }
}

/// The patch of one hunk of `rel` (for `git apply`).
pub fn hunk_patch(rel: &str, hunk: &RHunk) -> Option<Vec<u8>> {
    let raw = hunk.raw.as_ref()?;
    let (a, b) = (patch_name("a/", rel), patch_name("b/", rel));
    let mut p = format!("diff --git {a} {b}\n--- {a}\n+++ {b}\n").into_bytes();
    p.extend_from_slice(raw);
    if !p.ends_with(b"\n") {
        p.push(b'\n');
    }
    Some(p)
}

/// Accept one hunk: stage just it (`git apply --cached`). Only a complete hunk made by `git diff` can be staged alone.
pub fn stage_hunk(root: &Path, rel: &str, hunk: &RHunk) -> Result<(), String> {
    check_rel(rel)?;
    if !hunk.complete {
        return Err("This part of the diff was not read in full".into());
    }
    let patch = hunk_patch(rel, hunk).ok_or("This part can only be accepted with the whole file")?;
    run_write(root, &["apply", "--cached", "--whitespace=nowarn", "-"], Some(&patch)).map(|_| ())
}

/// Commits what is staged with `message` as it is (`git commit -F -`, so hooks and the author from git's config apply). Never
/// pushes. Returns git's short answer.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("The commit message is empty".into());
    }
    run_write(root, &["commit", "-q", "-F", "-"], Some(message.as_bytes()))
}

/// The work file's text with one hunk taken back. `None` when the hunk's new side is not where the hunk says (the file changed).
pub fn reverted_bytes(bytes: &[u8], hunk: &RHunk) -> Option<Vec<u8>> {
    if !hunk.exact || hunk.bytes.len() != hunk.lines.len() {
        return None;
    }
    // the file's lines with their own ends
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = bytes[at..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |i| at + i + 1);
        lines.push(&bytes[at..end]);
        at = end;
    }
    let crlf = lines.iter().filter(|l| l.ends_with(b"\r\n")).count();
    let lf = lines.iter().filter(|l| l.ends_with(b"\n")).count();
    let eol: &[u8] = if crlf * 2 > lf { b"\r\n" } else { b"\n" };
    let strip = |l: &[u8]| -> Vec<u8> {
        let l = l.strip_suffix(b"\n").unwrap_or(l);
        l.strip_suffix(b"\r").unwrap_or(l).to_vec()
    };
    let start = if hunk.new_count == 0 { hunk.new_start as usize } else { (hunk.new_start as usize).checked_sub(1)? };
    let end = start + hunk.new_count as usize;
    if end > lines.len() {
        return None;
    }
    // the new side must be in the file exactly there
    let new_side: Vec<&[u8]> = hunk.lines.iter().zip(&hunk.bytes).filter(|(l, _)| l.kind != LineKind::Removed).map(|(_, b)| b.as_slice()).collect();
    if new_side.len() != hunk.new_count as usize {
        return None;
    }
    for (l, f) in new_side.iter().zip(&lines[start..end]) {
        if strip(f) != *l {
            return None;
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    for l in &lines[..start] {
        out.extend_from_slice(l);
    }
    let mut ptr = start;
    for (l, b) in hunk.lines.iter().zip(&hunk.bytes) {
        match l.kind {
            LineKind::Context => {
                out.extend_from_slice(lines[ptr]);
                ptr += 1;
            }
            LineKind::Added => ptr += 1,
            LineKind::Removed => {
                out.extend_from_slice(b);
                out.extend_from_slice(eol);
            }
        }
    }
    let wrote_old = hunk.lines.iter().any(|l| l.kind != LineKind::Added);
    if hunk.raw.is_some() {
        // git said whether the old side's last line had a line end: it is written back the same way
        if hunk.old_no_eol && wrote_old {
            if out.ends_with(b"\n") {
                out.pop();
                if out.ends_with(b"\r") {
                    out.pop();
                }
            }
        }
    } else {
        // (no marker known) a file without a line end at its end keeps it that way
        let at_end = end == lines.len();
        let had_end = bytes.last().is_none_or(|&b| b == b'\n');
        if at_end && !had_end && hunk.lines.last().is_some_and(|l| l.kind == LineKind::Removed) {
            out.truncate(out.len() - eol.len());
        }
    }
    for l in &lines[end..] {
        out.extend_from_slice(l);
    }
    Some(out)
}

/// A new file beside `path` for its reverted content, `.<name>.<pid><n>.leaf-review`, made with `create_new` (a name that exists
/// is never removed or reused: two reverts never share one).
pub fn temp_beside(path: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut last = None;
    for _ in 0..64 {
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut name = std::ffi::OsString::from(".");
        name.push(path.file_name().unwrap_or_default());
        name.push(format!(".{}{n}.leaf-review", std::process::id()));
        let tmp = path.with_file_name(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free name for the temporary file")))
}

/// Takes one hunk back in the work file `path`. Refused when the file is not as `stamp` saw it. The new content is written beside
/// the file; then `put_away` keeps the current file (leaf: moves it to the Trash) and the new one is renamed in.
pub fn revert_hunk(path: &Path, stamp: Stamp, hunk: &RHunk, put_away: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
    if !hunk.complete {
        return Err("This part of the diff was not read in full".into());
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| "The file is not there any more".to_string())?;
    if !meta.is_file() {
        return Err("This is not a plain file".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if Stamp::of_bytes(&bytes) != stamp {
        return Err("The file changed after the difference was shown. Look at it again first".into());
    }
    if !hunk.exact {
        return Err("This file's text is not UTF-8 and the difference was read as text: revert the whole file instead".into());
    }
    let new = reverted_bytes(&bytes, hunk).ok_or("The file does not match the difference shown. Look at it again first")?;
    let (tmp, mut file) = temp_beside(path).map_err(|e| e.to_string())?;
    let written = file.write_all(&new).and_then(|_| file.sync_all()).and_then(|_| {
        drop(file);
        std::fs::set_permissions(&tmp, meta.permissions())
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    // (one more look: the file may have changed while the new one was written)
    if Stamp::of_file(path) != Some(stamp) {
        let _ = std::fs::remove_file(&tmp);
        return Err("The file changed after the difference was shown. Look at it again first".into());
    }
    if let Err(e) = put_away(path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // (a put-away that returned without moving the file, e.g. a cancelled Trash step: its content is kept nowhere, so it is
    // never replaced)
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(&tmp);
        return Err("The current version did not reach the Trash; nothing was changed".into());
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("the new content could not be put in place ({e}); the previous one is in the Trash")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("leaf-review-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The trash seam of the tests: the file is moved into `trash/` beside.
    fn trash_into(dir: &Path) -> impl Fn(&Path) -> Result<(), String> {
        let t = dir.join("trash");
        std::fs::create_dir_all(&t).unwrap();
        move |p: &Path| std::fs::rename(p, t.join(p.file_name().unwrap())).map_err(|e| e.to_string())
    }

    fn diff_of(old: &str, new: &str) -> RDiff {
        let dir = temp(&format!("d{}", Stamp::of_bytes(new.as_bytes()).hash));
        std::fs::write(dir.join("a"), old).unwrap();
        std::fs::write(dir.join("b"), new).unwrap();
        let out = Command::new("git").current_dir(&dir).args(["diff", "--no-index", "--no-color", "-U1", "--", "a", "b"]).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        parse_hunks(&out.stdout, MAX_LINES)
    }

    fn diff_of_bytes(old: &[u8], new: &[u8]) -> RDiff {
        let dir = temp(&format!("b{}", Stamp::of_bytes(new).hash));
        std::fs::write(dir.join("a"), old).unwrap();
        std::fs::write(dir.join("b"), new).unwrap();
        let out = Command::new("git").current_dir(&dir).args(["diff", "--no-index", "--no-color", "-U1", "--", "a", "b"]).output().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        parse_hunks(&out.stdout, MAX_LINES)
    }

    #[test]
    fn a_file_named_like_a_pattern_stages_and_reverts_only_itself() {
        let dir = temp("glob");
        let g = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "{args:?}");
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        g(&["init", "-q"]);
        for n in ["a.txt", "b.txt", "[ab].txt"] {
            std::fs::write(dir.join(n), "one\n").unwrap();
        }
        g(&["add", "-A"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "one"]);
        for n in ["a.txt", "b.txt", "[ab].txt"] {
            std::fs::write(dir.join(n), "two\n").unwrap();
        }
        stage_file(&dir, "[ab].txt").unwrap();
        assert_eq!(g(&["diff", "--cached", "--name-only"]).trim(), "[ab].txt");
        unstage_file(&dir, "[ab].txt").unwrap();
        stage_file(&dir, "a.txt").unwrap();
        unstage_file(&dir, "[ab].txt").unwrap();
        assert_eq!(g(&["diff", "--cached", "--name-only"]).trim(), "a.txt", "unstaging [ab].txt left a.txt staged");
        let kept = dir.join("kept.txt");
        let k2 = kept.clone();
        gitread::revert_file(&dir, "[ab].txt", &move |p| std::fs::rename(p, &k2).map_err(|e| e.to_string())).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("[ab].txt")).unwrap().trim(), "one");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "two\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_latin1_file_keeps_its_bytes_through_a_hunk_revert() {
        // Latin-1: "caf\xe9" (not UTF-8) on a removed line and on untouched lines of another hunk
        let old: &[u8] = b"caf\xe9\nb\nc\nd\ne\nf\ng\nna\xefve\n";
        let new: &[u8] = b"cafe\nb\nc\nd\ne\nf\ng\nNA\xcfVE\n";
        let d = diff_of_bytes(old, new);
        assert_eq!(d.hunks.len(), 2);
        let dir = temp("latin1");
        let f = dir.join("f.txt");
        std::fs::write(&f, new).unwrap();
        revert_hunk(&f, Stamp::of_file(&f).unwrap(), &d.hunks[0], &trash_into(&dir)).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"caf\xe9\nb\nc\nd\ne\nf\ng\nNA\xcfVE\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunk_revert_leaves_other_hunks_and_trashes_the_old_content() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let new = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n";
        let d = diff_of(old, new);
        assert_eq!(d.hunks.len(), 2);
        let dir = temp("one");
        let f = dir.join("f.txt");
        std::fs::write(&f, new).unwrap();
        let stamp = Stamp::of_file(&f).unwrap();
        revert_hunk(&f, stamp, &d.hunks[0], &trash_into(&dir)).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n");
        assert_eq!(std::fs::read(dir.join("trash").join("f.txt")).unwrap(), new.as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_put_away_that_keeps_nothing_never_replaces_the_file() {
        let d = diff_of("a\nb\nc\n", "a\nX\nc\n");
        let dir = temp("cancelled");
        let f = dir.join("f.txt");
        std::fs::write(&f, "a\nX\nc\n").unwrap();
        let stamp = Stamp::of_file(&f).unwrap();
        // (the Trash step was cancelled: it says Ok and leaves the file where it is)
        assert!(revert_hunk(&f, stamp, &d.hunks[0], &|_| Ok(())).is_err());
        assert_eq!(std::fs::read(&f).unwrap(), b"a\nX\nc\n");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("f.txt")], "no temp file is left and nothing was renamed in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_reverts_use_their_own_temp_and_the_second_is_refused() {
        let dir = temp("twice");
        let f = dir.join("f.txt");
        let (a, _fa) = temp_beside(&f).unwrap();
        let (b, _fb) = temp_beside(&f).unwrap();
        assert_ne!(a, b);
        drop((_fa, _fb));
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
        // two hunks of one diff, both taken back with the stamp of the same look
        let d = diff_of("1\n2\n3\n4\n5\n6\n7\n", "1\nX\n3\n4\n5\nY\n7\n");
        std::fs::write(&f, "1\nX\n3\n4\n5\nY\n7\n").unwrap();
        let stamp = Stamp::of_file(&f).unwrap();
        let trash = trash_into(&dir);
        revert_hunk(&f, stamp, &d.hunks[0], &trash).unwrap();
        assert!(revert_hunk(&f, stamp, &d.hunks[1], &trash).is_err(), "the file changed since the second hunk was shown");
        assert_eq!(std::fs::read(&f).unwrap(), b"1\n2\n3\n4\n5\nY\n7\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunk_revert_is_refused_when_the_file_changed() {
        let d = diff_of("a\nb\nc\n", "a\nB\nc\n");
        let dir = temp("changed");
        let f = dir.join("f.txt");
        std::fs::write(&f, "a\nB\nc\n").unwrap();
        let stamp = Stamp::of_file(&f).unwrap();
        std::fs::write(&f, "a\nB\nc\nd\n").unwrap();
        assert!(revert_hunk(&f, stamp, &d.hunks[0], &trash_into(&dir)).is_err());
        assert_eq!(std::fs::read(&f).unwrap(), b"a\nB\nc\nd\n");
        assert!(!dir.join("trash").join("f.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hunk_revert_keeps_crlf() {
        let old = "a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng\r\n";
        let new = "a\r\nB\r\nc\r\nd\r\ne\r\nF\r\ng\r\n";
        let d = diff_of(old, new);
        assert_eq!(d.hunks.len(), 2);
        let dir = temp("crlf");
        let f = dir.join("f.txt");
        std::fs::write(&f, new).unwrap();
        let stamp = Stamp::of_file(&f).unwrap();
        revert_hunk(&f, stamp, &d.hunks[0], &trash_into(&dir)).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"a\r\nb\r\nc\r\nd\r\ne\r\nF\r\ng\r\n");
        // a hunk git showed without the CRs (core.autocrlf: the index holds LF) still lands as CRLF
        let lf = diff_of("x\nY\nz\n", "x\ny\nz\n");
        let g = dir.join("g.txt");
        std::fs::write(&g, "x\r\ny\r\nz\r\n").unwrap();
        revert_hunk(&g, Stamp::of_file(&g).unwrap(), &lf.hunks[0], &trash_into(&dir)).unwrap();
        assert_eq!(std::fs::read(&g).unwrap(), b"x\r\nY\r\nz\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
