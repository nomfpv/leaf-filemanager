//! Reading a git repository, never changing it: the repository root, `status`, and the `diff` of one file against HEAD.
//!
//! Only `rev-parse`, `status`, `diff` and `ls-files` are run (no shell; `GIT_OPTIONAL_LOCKS=0` so even `status` takes no lock and
//! writes nothing), each with a time limit and a cap on the output. Everything here blocks: call it off the UI thread. The
//! parsers are plain functions on text so they can be tested without a repository.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one git call may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of one git answer that are read (the rest is dropped and the answer marked cut).
pub const MAX_OUT: usize = 4 * 1024 * 1024;
/// The most diff lines kept for one file; more is cut with `truncated` set.
pub const MAX_LINES: usize = 4000;
/// A file bigger than this is not read to show an untracked file as all added.
const MAX_UNTRACKED: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Not in HEAD: untracked, or added to the index.
    New,
    Changed,
    Deleted,
    Renamed,
    Conflicted,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::New => "New",
            Kind::Changed => "Changed",
            Kind::Deleted => "Deleted",
            Kind::Renamed => "Renamed",
            Kind::Conflicted => "Conflicted",
        }
    }

    /// The letter of the small mark: A (added), M, D, R, U (unmerged).
    pub fn letter(self, untracked: bool) -> char {
        match self {
            Kind::New if untracked => '?',
            Kind::New => 'A',
            Kind::Changed => 'M',
            Kind::Deleted => 'D',
            Kind::Renamed => 'R',
            Kind::Conflicted => 'U',
        }
    }
}

/// One changed path of `git status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Relative to the repository root, with `/`.
    pub path: String,
    /// For a rename: where it was.
    pub from: Option<String>,
    pub kind: Kind,
    /// The index differs from HEAD for it (staged).
    pub staged: bool,
    /// Not known to git at all.
    pub untracked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    /// Lines (or bytes) were left out: past [`MAX_LINES`] or [`MAX_OUT`].
    pub truncated: bool,
}

fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).args(["--no-pager", "-c", "core.quotepath=false"]);
    c.env("GIT_OPTIONAL_LOCKS", "0").env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Runs git with `args` in `dir`: the output (at most `cap` bytes) and whether it was cut, or `None` when git is missing, fails or
/// takes longer than `timeout`.
fn run(dir: &Path, args: &[&str], timeout: Duration, cap: usize) -> Option<(Vec<u8>, bool)> {
    let mut child = git(dir).args(args).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let mut cut = false;
        loop {
            match out.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                    if n > room || buf.len() >= cap {
                        cut = true;
                        break;
                    }
                }
            }
        }
        (buf, cut)
    });
    let start = Instant::now();
    let mut timed_out = false;
    while !reader.is_finished() {
        if start.elapsed() > timeout {
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let (buf, cut) = if timed_out {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    } else {
        reader.join().ok()?
    };
    if cut {
        let _ = child.kill();
    }
    let status = child.wait().ok()?;
    (status.success() || cut).then_some((buf, cut))
}

fn run_text(dir: &Path, args: &[&str]) -> Option<String> {
    let (b, _) = run(dir, args, Duration::from_secs(5), 64 * 1024)?;
    Some(String::from_utf8_lossy(&b).trim().to_string())
}

/// The top of the repository `dir` is in (`git rev-parse --show-toplevel`), `None` outside one or without git.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let t = run_text(dir, &["rev-parse", "--show-toplevel"])?;
    (!t.is_empty()).then(|| PathBuf::from(t))
}

/// For a file: its repository root and its path relative to it (with `/`).
pub fn locate(file: &Path) -> Option<(PathBuf, String)> {
    let dir = file.parent()?;
    let name = file.file_name()?.to_string_lossy().into_owned();
    let t = run_text(dir, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
    let mut lines = t.lines();
    let root = PathBuf::from(lines.next()?.trim());
    let prefix = lines.next().unwrap_or("").trim();
    (!root.as_os_str().is_empty()).then(|| (root, format!("{prefix}{name}")))
}

/// Every changed, new, deleted, renamed and conflicted path of the repository (ignored files left out; untracked files one by one).
pub fn status(root: &Path) -> Option<Vec<Change>> {
    let (raw, _) = run(root, &["status", "--porcelain=v2", "-z", "--untracked-files=all"], TIMEOUT, MAX_OUT * 2)?;
    Some(parse_status(&raw))
}

/// Parses `git status --porcelain=v2 -z` output. Records it does not know are skipped.
pub fn parse_status(raw: &[u8]) -> Vec<Change> {
    let text = String::from_utf8_lossy(raw);
    let mut out = Vec::new();
    let mut recs = text.split('\0');
    while let Some(rec) = recs.next() {
        let Some(tag) = rec.chars().next() else { continue };
        match tag {
            '1' => {
                // 1 XY sub mH mI mW hH hI path
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() < 9 {
                    continue;
                }
                let xy: Vec<char> = f[1].chars().collect();
                let (x, y) = (xy.first().copied().unwrap_or('.'), xy.get(1).copied().unwrap_or('.'));
                let kind = if x == 'D' || y == 'D' {
                    Kind::Deleted
                } else if x == 'A' {
                    Kind::New
                } else {
                    Kind::Changed
                };
                out.push(Change { path: f[8].to_string(), from: None, kind, staged: x != '.', untracked: false });
            }
            '2' => {
                // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let orig = recs.next().map(str::to_string);
                if f.len() < 10 {
                    continue;
                }
                let x = f[1].chars().next().unwrap_or('.');
                out.push(Change { path: f[9].to_string(), from: orig, kind: Kind::Renamed, staged: x != '.', untracked: false });
            }
            'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() < 11 {
                    continue;
                }
                out.push(Change { path: f[10].to_string(), from: None, kind: Kind::Conflicted, staged: false, untracked: false });
            }
            '?' => {
                if let Some(p) = rec.get(2..).filter(|p| !p.is_empty()) {
                    out.push(Change { path: p.to_string(), from: None, kind: Kind::New, staged: false, untracked: true });
                }
            }
            _ => {}
        }
    }
    out
}

/// The changes of one file against HEAD, with `context` unchanged lines round each change (a big number shows the whole file).
/// `from` is the old path of a rename. An untracked file shows as all added. `None` when git could not answer.
pub fn diff_file(root: &Path, rel: &str, from: Option<&str>, context: u32) -> Option<FileDiff> {
    let u = format!("-U{context}");
    let mut args = vec!["diff", "HEAD", "--no-color", "--no-ext-diff", "-M", &u, "--"];
    if let Some(f) = from {
        args.push(f);
    }
    args.push(rel);
    let first = run(root, &args, TIMEOUT, MAX_OUT);
    // a repository without a commit has no HEAD: what is staged is the difference
    let first = first.or_else(|| {
        args[1] = "--cached";
        run(root, &args, TIMEOUT, MAX_OUT)
    });
    if let Some((raw, cut)) = &first {
        if !raw.is_empty() {
            let mut d = parse_diff(&String::from_utf8_lossy(raw), MAX_LINES);
            d.truncated |= *cut;
            return Some(d);
        }
    }
    // nothing against HEAD: an untracked file is all added
    let (listed, _) = run(root, &["ls-files", "--others", "--exclude-standard", "-z", "--", rel], Duration::from_secs(5), 64 * 1024)?;
    if listed.is_empty() {
        return first.map(|_| FileDiff::default());
    }
    let path = root.join(rel);
    let len = std::fs::metadata(&path).ok()?.len();
    let mut buf = Vec::new();
    std::fs::File::open(&path).ok()?.take(MAX_UNTRACKED).read_to_end(&mut buf).ok()?;
    let mut d = all_added(&buf, MAX_LINES);
    d.truncated |= len > MAX_UNTRACKED;
    Some(d)
}

/// A new file's bytes as a diff of added lines only (binary when it holds NUL bytes).
pub fn all_added(bytes: &[u8], max_lines: usize) -> FileDiff {
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return FileDiff { binary: true, ..Default::default() };
    }
    let text = String::from_utf8_lossy(bytes);
    let mut d = FileDiff::default();
    let mut lines = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if lines.len() >= max_lines {
            d.truncated = true;
            break;
        }
        lines.push(DiffLine { kind: LineKind::Added, old_no: None, new_no: Some(i as u32 + 1), text: l.to_string() });
    }
    d.added = lines.len() as u32;
    if !lines.is_empty() {
        d.hunks.push(Hunk { old_start: 0, new_start: 1, header: format!("@@ -0,0 +1,{} @@", lines.len()), lines });
    }
    d
}

fn parse_range(s: &str, sign: char) -> Option<u32> {
    let s = s.strip_prefix(sign)?;
    s.split(',').next()?.parse().ok()
}

/// Parses a unified diff of one file (what `git diff` prints) into hunks with the old and new line number of every line. Keeps at
/// most `max_lines` lines and then sets `truncated`.
pub fn parse_diff(text: &str, max_lines: usize) -> FileDiff {
    let mut d = FileDiff::default();
    let mut kept = 0usize;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let mut parts = rest.split_whitespace();
            let (o, n) = (parts.next().and_then(|s| parse_range(s, '-')), parts.next().and_then(|s| parse_range(s, '+')));
            let (Some(o), Some(n)) = (o, n) else { continue };
            old_no = o;
            new_no = n;
            d.hunks.push(Hunk { old_start: o, new_start: n, header: line.to_string(), lines: Vec::new() });
            continue;
        }
        let Some(h) = d.hunks.last_mut() else {
            if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
                d.binary = true;
            }
            continue;
        };
        let (kind, body) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            None => (LineKind::Context, ""),
            // "\ No newline at end of file", or the next file's header
            _ => continue,
        };
        if kept >= max_lines {
            d.truncated = true;
            break;
        }
        kept += 1;
        let (o, n) = match kind {
            LineKind::Context => {
                let r = (Some(old_no), Some(new_no));
                old_no += 1;
                new_no += 1;
                r
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
        h.lines.push(DiffLine { kind, old_no: o, new_no: n, text: body.to_string() });
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_records() {
        let raw = "1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs\0\
                   1 A. N... 000000 100644 100644 0000 cccc docs/new one.md\0\
                   1 .D N... 100644 100644 000000 dddd eeee old.txt\0\
                   2 R. N... 100644 100644 100644 ffff 1111 R100 renamed/to.rs\0renamed/from.rs\0\
                   u UU N... 100644 100644 100644 100644 2222 3333 4444 merge.c\0\
                   ? scratch/notes.txt\0\
                   # unknown\0";
        let got = parse_status(raw.as_bytes());
        let flat: Vec<(&str, Kind, bool, bool, Option<&str>)> =
            got.iter().map(|c| (c.path.as_str(), c.kind, c.staged, c.untracked, c.from.as_deref())).collect();
        assert_eq!(
            flat,
            vec![
                ("src/main.rs", Kind::Changed, false, false, None),
                ("docs/new one.md", Kind::New, true, false, None),
                ("old.txt", Kind::Deleted, false, false, None),
                ("renamed/to.rs", Kind::Renamed, true, false, Some("renamed/from.rs")),
                ("merge.c", Kind::Conflicted, false, false, None),
                ("scratch/notes.txt", Kind::New, false, true, None),
            ]
        );
        assert_eq!(Kind::New.letter(true), '?');
        assert_eq!(Kind::New.letter(false), 'A');
    }

    #[test]
    fn diff_hunks_and_numbers() {
        let text = "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n\
                    @@ -3,4 +3,5 @@ fn section\n keep one\n-gone\n+new a\n+new b\n keep two\n\\ No newline at end of file\n\
                    @@ -20,2 +21,1 @@\n tail\n-last\n";
        let d = parse_diff(text, 100);
        assert_eq!((d.added, d.removed, d.hunks.len(), d.binary, d.truncated), (2, 2, 2, false, false));
        let h = &d.hunks[0];
        assert_eq!((h.old_start, h.new_start), (3, 3));
        let rows: Vec<(LineKind, Option<u32>, Option<u32>, &str)> = h.lines.iter().map(|l| (l.kind, l.old_no, l.new_no, l.text.as_str())).collect();
        assert_eq!(
            rows,
            vec![
                (LineKind::Context, Some(3), Some(3), "keep one"),
                (LineKind::Removed, Some(4), None, "gone"),
                (LineKind::Added, None, Some(4), "new a"),
                (LineKind::Added, None, Some(5), "new b"),
                (LineKind::Context, Some(5), Some(6), "keep two"),
            ]
        );
        assert_eq!(d.hunks[1].lines[1].old_no, Some(21));
        // cut, binary and an untracked file
        assert!(parse_diff(text, 3).truncated);
        assert!(parse_diff("diff --git a/x b/x\nBinary files a/x and b/x differ\n", 10).binary);
        let a = all_added(b"one\ntwo\n", 10);
        assert_eq!((a.added, a.hunks[0].lines[1].new_no), (2, Some(2)));
        assert!(all_added(b"ab\0cd", 10).binary);
    }
}
