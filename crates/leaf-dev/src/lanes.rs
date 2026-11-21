//! Lanes (PLAN F1): a lane is a git worktree on a branch of its own, made beside the repository for one piece of work (often an
//! agent's). This file is the git side of it: make one ([`add`]), read how far it is ([`progress`], [`end_info`]) and end it:
//! [`merge`] it into its base branch, [`remove`] it (`git worktree remove`, never forced) or [`discard`] it (its folder handed to
//! `put_away`, which is the Trash in leaf, then `git worktree prune`).
//!
//! Whatever could lose work is refused before anything is changed: a merge needs a clean lane and a clean base worktree and is
//! tried in git's object store first (`merge-tree`), so a conflict changes neither tree; a remove refuses uncommitted files and
//! commits the base does not have; a discard only ever hands over the lane's own worktree folder (never a link, never the main
//! worktree or a folder that holds it). A branch is deleted only with `git branch -d`, which keeps one that is not merged.
//!
//! Every call blocks (it runs git): off the UI thread. No shell; arguments as a list.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::gitread::{self, Worktree};

/// Why a lane could not be made or ended. Nothing was changed when one of these comes back, except where a variant says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneError {
    /// The lane's folder has changes that are not committed (paths relative to it).
    Uncommitted(Vec<String>),
    /// The worktree of the base branch has changes that are not committed.
    BaseUncommitted(Vec<String>),
    /// Submodules of the lane (paths) with commits that no remote has: their repositories live in git's record of the lane.
    SubmoduleWork(Vec<String>),
    /// Another worktree of the repository lies inside the lane folder (its path).
    HoldsWorktree(String),
    /// Ignored files and folders in the lane (`git worktree remove` would delete them for good; Discard keeps them in the Trash).
    Ignored(Vec<String>),
    /// Commits of the lane that the base branch does not have (`abc1234 subject`).
    Unmerged(Vec<String>),
    /// Merging would conflict in these files.
    Conflicts(Vec<String>),
    /// The base branch is not checked out in any worktree, so there is no folder to merge in.
    BaseNotCheckedOut(String),
    /// The folder is not a linked worktree of the repository (or it is the main one, or a link to one).
    NotALane(String),
    /// git refused or failed: what was being done, and git's own text.
    Git { what: String, detail: String },
}

impl fmt::Display for LaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneError::Uncommitted(v) => write!(f, "{} uncommitted files in the lane", v.len()),
            LaneError::BaseUncommitted(v) => write!(f, "{} uncommitted files in the base worktree", v.len()),
            LaneError::Ignored(v) => write!(f, "{} ignored entries in the lane", v.len()),
            LaneError::SubmoduleWork(v) => write!(f, "{} submodules with commits no remote has", v.len()),
            LaneError::HoldsWorktree(p) => write!(f, "another worktree lies inside the lane: {p}"),
            LaneError::Unmerged(v) => write!(f, "{} commits not in the base branch", v.len()),
            LaneError::Conflicts(v) => write!(f, "the merge would conflict in {} files", v.len()),
            LaneError::BaseNotCheckedOut(b) => write!(f, "the base branch {b} is not checked out in any worktree"),
            LaneError::NotALane(why) => write!(f, "not a lane: {why}"),
            LaneError::Git { what, detail } => write!(f, "{what}: {detail}"),
        }
    }
}

impl std::error::Error for LaneError {}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    crate::checkpoint::clean_env(&mut c);
    c.arg("-C").arg(dir).args(["--no-pager", "-c", "core.quotepath=false"]);
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// What one git call answered.
struct Out {
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn exec<I, S>(dir: &Path, args: I) -> Result<Out, LaneError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let o = git(dir).args(args).output().map_err(|e| LaneError::Git { what: "git could not be started".into(), detail: e.to_string() })?;
    Ok(Out {
        ok: o.status.success(),
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    })
}

fn failed(what: &str, o: &Out) -> LaneError {
    LaneError::Git { what: what.into(), detail: if o.stderr.is_empty() { o.stdout.trim().to_string() } else { o.stderr.clone() } }
}

/// A path as a comparison key when it cannot be resolved: forward slashes, no trailing one, lower case on Windows.
fn key(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s).trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn real(p: &Path) -> Option<PathBuf> {
    p.canonicalize().ok()
}

/// `a` and `b` are the same folder (resolved when both exist; else compared as written).
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (real(a), real(b)) {
        (Some(x), Some(y)) => x == y,
        _ => key(a) == key(b),
    }
}

/// `a` is `b` or inside it.
fn within(a: &Path, b: &Path) -> bool {
    match (real(a), real(b)) {
        (Some(x), Some(y)) => x.starts_with(&y),
        _ => {
            let (a, b) = (key(a), key(b));
            a == b || a.starts_with(&format!("{b}/"))
        }
    }
}

/// The linked (not the main) worktree at `dir`.
fn linked<'a>(list: &'a [Worktree], dir: &Path) -> Option<&'a Worktree> {
    list.iter().skip(1).find(|w| same_path(&w.path, dir))
}

/// The repository's common git folder, absolute (`<repo>/.git` for every worktree of it).
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let o = exec(dir, ["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?;
    let t = o.stdout.trim();
    (o.ok && !t.is_empty()).then(|| PathBuf::from(t))
}

/// `dir` is, right now, a linked worktree of `repo`: git lists it, its `.git` is a file (a worktree's, not a repository's own
/// folder), and git asked inside it answers with the repository's common folder and `dir` itself as the top. A lane folder deleted
/// by hand and made again (a fresh clone, `git init`) is still listed by git, but fails this.
fn own_worktree(repo: &Path, list: &[Worktree], dir: &Path) -> bool {
    if linked(list, dir).is_none() || !dir.join(".git").is_file() {
        return false;
    }
    let Some(common) = common_dir(repo) else { return false };
    let Ok(o) = exec(dir, ["rev-parse", "--path-format=absolute", "--git-common-dir", "--show-toplevel"]) else { return false };
    let mut lines = o.stdout.lines().map(str::trim);
    match (o.ok, lines.next(), lines.next()) {
        (true, Some(c), Some(top)) => same_path(Path::new(c), &common) && same_path(Path::new(top), dir),
        _ => false,
    }
}

fn worktrees(repo: &Path) -> Result<Vec<Worktree>, LaneError> {
    gitread::worktrees(repo).filter(|l| !l.is_empty()).ok_or_else(|| LaneError::Git { what: "the worktrees could not be read".into(), detail: String::new() })
}

/// Every changed path of `dir` (untracked ones too), or only the tracked ones.
fn changes(dir: &Path, with_untracked: bool) -> Result<Vec<String>, LaneError> {
    let list = gitread::status(dir).ok_or_else(|| LaneError::Git { what: "git status could not be read".into(), detail: dir.display().to_string() })?;
    Ok(list.into_iter().filter(|c| with_untracked || !c.untracked).map(|c| c.path).collect())
}

/// The ignored files and folders of `dir` (a folder that is ignored as a whole is one entry, ending in `/`).
fn ignored(dir: &Path) -> Result<Vec<String>, LaneError> {
    let o = exec(dir, ["status", "--porcelain=v1", "-z", "--ignored=matching", "--untracked-files=normal"])?;
    if !o.ok {
        return Err(failed("git status could not be read", &o));
    }
    Ok(o.stdout.split('\0').filter_map(|r| r.strip_prefix("!! ")).filter(|p| !p.is_empty()).map(String::from).collect())
}

/// The commits of `head` (run in `dir`) that `base` does not have, newest first, as `abc1234 subject` (at most 200).
fn commits_not_in(dir: &Path, head: &str, base: &str) -> Result<Vec<String>, LaneError> {
    let o = exec(dir, ["log", "-n200", "--no-color", "--format=%h %s", &format!("{base}..{head}")])?;
    if !o.ok {
        return Err(failed("the commits could not be compared", &o));
    }
    Ok(o.stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
}

/// A branch name git accepts (`git check-ref-format --branch`).
pub fn valid_branch(repo: &Path, name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && exec(repo, ["check-ref-format", "--branch", name]).is_ok_and(|o| o.ok)
}

/// Makes a lane: `git worktree add -b <branch> <folder> <base>`. `base` is a branch or commit (`HEAD` for the current head).
/// `folder` must not exist or be empty. It adds a folder and a branch; nothing that exists is changed.
pub fn add(repo: &Path, branch: &str, base: &str, folder: &Path) -> Result<(), LaneError> {
    if !valid_branch(repo, branch) {
        return Err(LaneError::Git { what: "the branch name is not valid".into(), detail: branch.into() });
    }
    if base.is_empty() || base.starts_with('-') {
        return Err(LaneError::Git { what: "the base is not valid".into(), detail: base.into() });
    }
    if folder.exists() && folder.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
        return Err(LaneError::Git { what: "the folder already exists and is not empty".into(), detail: folder.display().to_string() });
    }
    if let Some(parent) = folder.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let o = exec(repo, [OsStr::new("worktree"), OsStr::new("add"), OsStr::new("-b"), OsStr::new(branch), OsStr::new("--"), folder.as_os_str(), OsStr::new(base)])?;
    if o.ok { Ok(()) } else { Err(failed("git did not add the worktree", &o)) }
}

/// How far a lane is: commits ahead of and behind its base, files changed and not committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub ahead: u32,
    pub behind: u32,
    pub changed: usize,
}

/// `None` when the folder is gone or git cannot answer.
pub fn progress(dir: &Path, base: &str) -> Option<Progress> {
    if !dir.is_dir() {
        return None;
    }
    let o = exec(dir, ["rev-list", "--left-right", "--count", &format!("{base}...HEAD")]).ok()?;
    let (ahead, behind) = if o.ok { gitread::parse_counts(&o.stdout).unwrap_or((0, 0)) } else { (0, 0) };
    let changed = gitread::status(dir)?.len();
    Some(Progress { ahead, behind, changed })
}

/// What ending the lane would meet, shown before anything is done.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndInfo {
    /// The folder is a linked worktree of the repository.
    pub is_lane: bool,
    /// Commits of the lane the base does not have.
    pub ahead: Vec<String>,
    pub behind: u32,
    /// Uncommitted files in the lane (untracked ones too).
    pub uncommitted: Vec<String>,
    /// Ignored files and folders in the lane: Remove refuses while there are any.
    pub ignored: Vec<String>,
    /// The worktree that has the base branch checked out, and its uncommitted tracked files.
    pub base_dir: Option<PathBuf>,
    pub base_dirty: Vec<String>,
}

pub fn end_info(repo: &Path, dir: &Path, base: &str) -> EndInfo {
    let list = gitread::worktrees(repo).unwrap_or_default();
    let is_lane = own_worktree(repo, &list, dir);
    let base_dir = list.iter().find(|w| w.branch.as_deref() == Some(base)).map(|w| w.path.clone());
    let base_dirty = base_dir.as_deref().and_then(|d| changes(d, false).ok()).unwrap_or_default();
    let progress = progress(dir, base).unwrap_or_default();
    EndInfo {
        is_lane,
        ahead: if dir.is_dir() { commits_not_in(dir, "HEAD", base).unwrap_or_default() } else { Vec::new() },
        behind: progress.behind,
        uncommitted: if dir.is_dir() { changes(dir, true).unwrap_or_default() } else { Vec::new() },
        ignored: if dir.is_dir() { ignored(dir).unwrap_or_default() } else { Vec::new() },
        base_dir,
        base_dirty,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merged {
    /// The base branch was moved up to the lane's commit.
    FastForward,
    /// A merge commit was made on the base branch.
    MergeCommit,
    /// The base already had everything.
    Nothing,
}

/// Merges the lane's branch into `base`, in the worktree that has `base` checked out. Only when the lane is clean (untracked
/// files count) and the base worktree has no uncommitted tracked change. A merge that would conflict is found before any tree is
/// touched and changes nothing. Fast-forward when it can, else a merge commit.
pub fn merge(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<Merged, LaneError> {
    if branch == base {
        return Err(LaneError::Git { what: "the lane is on its base branch".into(), detail: branch.into() });
    }
    let list = worktrees(repo)?;
    if !own_worktree(repo, &list, dir) {
        return Err(LaneError::NotALane(dir.display().to_string()));
    }
    let Some(base_dir) = list.iter().find(|w| w.branch.as_deref() == Some(base)).map(|w| w.path.clone()) else {
        return Err(LaneError::BaseNotCheckedOut(base.into()));
    };
    let dirty = changes(dir, true)?;
    if !dirty.is_empty() {
        return Err(LaneError::Uncommitted(dirty));
    }
    let base_dirty = changes(&base_dir, false)?;
    if !base_dirty.is_empty() {
        return Err(LaneError::BaseUncommitted(base_dirty));
    }
    if commits_not_in(dir, "HEAD", base)?.is_empty() {
        return Ok(Merged::Nothing);
    }
    // the lane's head (the branch it has checked out may not be the one recorded: its HEAD is what is merged)
    let head = exec(dir, ["rev-parse", "--verify", "HEAD"])?;
    if !head.ok {
        return Err(failed("the lane's head could not be read", &head));
    }
    let head = head.stdout.trim().to_string();
    let ff = exec(&base_dir, ["merge-base", "--is-ancestor", "HEAD", &head])?;
    if ff.ok {
        let o = exec(&base_dir, ["merge", "--ff-only", &head])?;
        return if o.ok { Ok(Merged::FastForward) } else { Err(failed("git did not fast-forward the base branch", &o)) };
    }
    // try it in the object store first: exit 1 lists the conflicted files and nothing is touched (git 2.38 and later)
    let probe = exec(&base_dir, ["merge-tree", "--write-tree", "--name-only", "--no-messages", "HEAD", &head])?;
    match probe.code {
        Some(0) => {}
        Some(1) => {
            let mut files: Vec<String> = Vec::new();
            for l in probe.stdout.lines().skip(1).map(str::trim).take_while(|l| !l.is_empty()) {
                if !files.iter().any(|f| f == l) {
                    files.push(l.to_string());
                }
            }
            return Err(LaneError::Conflicts(files));
        }
        // an older git: try the merge without committing and take it back on a conflict (the base worktree was clean)
        _ => {
            let o = exec(&base_dir, ["merge", "--no-commit", "--no-ff", &head])?;
            if !o.ok {
                let files: Vec<String> = gitread::status(&base_dir).unwrap_or_default().into_iter().filter(|c| c.kind == gitread::Kind::Conflicted).map(|c| c.path).collect();
                let _ = exec(&base_dir, ["merge", "--abort"]);
                return Err(if files.is_empty() { failed("git did not merge", &o) } else { LaneError::Conflicts(files) });
            }
            let c = exec(&base_dir, ["commit", "--no-edit"])?;
            if !c.ok {
                let _ = exec(&base_dir, ["merge", "--abort"]);
                return Err(failed("git did not make the merge commit", &c));
            }
            return Ok(Merged::MergeCommit);
        }
    }
    let msg = format!("Merge lane {branch} into {base}");
    let o = exec(&base_dir, ["merge", "--no-ff", "--no-edit", "-m", &msg, &head])?;
    if o.ok {
        Ok(Merged::MergeCommit)
    } else {
        let _ = exec(&base_dir, ["merge", "--abort"]);
        Err(failed("git did not merge", &o))
    }
}

/// Removes the lane's worktree (`git worktree remove`, without `--force`): refused with the list when it has uncommitted files,
/// commits the base does not have, or ignored files (which git would delete for good). Then the branch is deleted when it is merged (`git branch -d`); `Ok(true)` when it was.
pub fn remove(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<bool, LaneError> {
    let list = worktrees(repo)?;
    if !own_worktree(repo, &list, dir) {
        return Err(LaneError::NotALane(dir.display().to_string()));
    }
    let dirty = changes(dir, true)?;
    if !dirty.is_empty() {
        return Err(LaneError::Uncommitted(dirty));
    }
    let ahead = commits_not_in(dir, "HEAD", base)?;
    if !ahead.is_empty() {
        return Err(LaneError::Unmerged(ahead));
    }
    // `worktree remove` deletes ignored files for good (`.env`, local settings, databases): only Discard, through the Trash
    let kept = ignored(dir)?;
    if !kept.is_empty() {
        return Err(LaneError::Ignored(kept));
    }
    let o = exec(repo, [OsStr::new("worktree"), OsStr::new("remove"), dir.as_os_str()])?;
    if !o.ok {
        return Err(failed("git did not remove the worktree", &o));
    }
    Ok(delete_merged_branch(repo, branch))
}

/// `git branch -d`: git keeps a branch that is not merged.
fn delete_merged_branch(repo: &Path, branch: &str) -> bool {
    !branch.is_empty() && !branch.starts_with('-') && exec(repo, ["branch", "-d", branch]).is_ok_and(|o| o.ok)
}

/// The lane's own record in the repository, `<common>/worktrees/<id>`: the entry whose `gitdir` file names `<dir>/.git`. Only a
/// folder directly in `<common>/worktrees` is ever given back.
fn admin_of(repo: &Path, dir: &Path) -> Result<Option<PathBuf>, LaneError> {
    let common = common_dir(repo).ok_or_else(|| LaneError::Git { what: "the repository's git folder could not be found".into(), detail: repo.display().to_string() })?;
    let root = common.join("worktrees");
    let Ok(entries) = std::fs::read_dir(&root) else { return Ok(None) };
    let want = dir.join(".git");
    for e in entries.flatten() {
        let admin = e.path();
        if !e.file_type().is_ok_and(|t| t.is_dir()) || admin.parent() != Some(root.as_path()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(admin.join("gitdir")) else { continue };
        let named = PathBuf::from(text.trim());
        let named = if named.is_absolute() { named } else { admin.join(named) };
        if same_path(&named, &want) {
            return Ok(Some(admin));
        }
    }
    Ok(None)
}

/// The submodules checked out in `dir` whose head or local branches have commits that no remote of theirs has.
fn unpushed_submodules(dir: &Path) -> Result<Vec<String>, LaneError> {
    let o = exec(dir, ["ls-files", "-s", "-z"])?;
    if !o.ok {
        return Err(failed("git could not list the lane's files", &o));
    }
    let mut out = Vec::new();
    for rec in o.stdout.split('\0') {
        let Some((meta, path)) = rec.split_once('\t') else { continue };
        if !meta.starts_with("160000 ") {
            continue;
        }
        let sub = dir.join(path);
        if !sub.join(".git").exists() {
            continue;
        }
        let r = exec(&sub, ["rev-list", "-n1", "HEAD", "--branches", "--not", "--remotes"])?;
        if r.ok && !r.stdout.trim().is_empty() {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

/// What a discard did besides putting the folder away.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Discarded {
    /// The lane's branch was merged and is deleted.
    pub branch_deleted: bool,
    /// A branch made to keep commits that only the lane's own (detached) HEAD held.
    pub kept: Option<String>,
}

/// When `dir`'s HEAD is detached on commits no branch or tag holds (an agent's `git checkout <sha>` and commits, a rebase stopped
/// halfway), makes `lane/<name>-kept-<short hash>` at it: the worktree's own HEAD is all that keeps them, and it goes with the lane.
fn keep_detached_head(dir: &Path, name: &str) -> Result<Option<String>, LaneError> {
    if exec(dir, ["symbolic-ref", "-q", "HEAD"])?.ok {
        return Ok(None);
    }
    let head = exec(dir, ["rev-parse", "--verify", "-q", "HEAD"])?;
    let sha = head.stdout.trim().to_string();
    if !head.ok || sha.len() < 7 {
        return Ok(None);
    }
    let held = exec(dir, ["for-each-ref", "--count=1", "--format=%(refname)", "--contains", &sha, "refs/heads", "refs/tags"])?;
    if !held.ok {
        return Err(failed("git could not tell which branches hold the lane's head", &held));
    }
    if !held.stdout.trim().is_empty() {
        return Ok(None);
    }
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let branch = format!("lane/{}-kept-{}", safe.trim_matches(['-', '.']), &sha[..7]);
    let made = exec(dir, ["branch", "--end-of-options", &branch, &sha])?;
    if made.ok {
        return Ok(Some(branch));
    }
    // made by an earlier try that stopped later on
    let there = exec(dir, ["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")])?;
    if there.ok && there.stdout.trim() == sha { Ok(Some(branch)) } else { Err(failed("git did not make a branch to keep the lane's commits", &made)) }
}

/// Discards the lane, whatever is in it: its worktree folder goes to `put_away` (leaf: the Trash), then its own record in the
/// repository (`<common>/worktrees/<id>`, which holds its HEAD, its reflog and the repositories of its submodules) goes the same
/// way, so git forgets it. No `git worktree prune`: that would forget every worktree whose folder is missing just now (an
/// unplugged drive). The branch is kept unless it is merged. Only a linked worktree of `repo` is handed over: never a link (its
/// target could be anything), never the main worktree or a folder that holds it. For a folder already gone only the record goes.
/// Commits only a detached HEAD holds get a branch first (`lane/<name>-kept-<hash>`).
pub fn discard(repo: &Path, dir: &Path, name: &str, branch: &str, put_away: &dyn Fn(&Path) -> Result<(), String>) -> Result<Discarded, LaneError> {
    let mut kept = None;
    let list = worktrees(repo)?;
    let main = list[0].path.clone();
    let meta = std::fs::symlink_metadata(dir);
    if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(LaneError::NotALane(format!("{} is a link", dir.display())));
    }
    if within(&main, dir) {
        return Err(LaneError::NotALane(format!("{} holds the main worktree", dir.display())));
    }
    // another worktree inside the lane folder would go to the Trash with it
    if let Some(w) = list.iter().skip(1).find(|w| !same_path(&w.path, dir) && within(&w.path, dir)) {
        return Err(LaneError::HoldsWorktree(w.path.display().to_string()));
    }
    if meta.is_ok() {
        if !own_worktree(repo, &list, dir) {
            return Err(LaneError::NotALane(dir.display().to_string()));
        }
        if !dir.is_dir() {
            return Err(LaneError::NotALane(format!("{} is not a folder", dir.display())));
        }
        // a submodule's repository lives in git's record of the lane, not in the folder: work no remote has is refused here
        let subs = unpushed_submodules(dir)?;
        if !subs.is_empty() {
            return Err(LaneError::SubmoduleWork(subs));
        }
        kept = keep_detached_head(dir, name)?;
    } else if !list.iter().skip(1).any(|w| key(&w.path) == key(dir)) {
        return Err(LaneError::NotALane(dir.display().to_string()));
    }
    let admin = admin_of(repo, dir)?;
    if meta.is_ok() {
        put_away(dir).map_err(|e| LaneError::Git { what: "the lane folder could not be moved to the Trash".into(), detail: e })?;
        if std::fs::symlink_metadata(dir).is_ok() {
            return Err(LaneError::Git { what: "the lane folder is still there".into(), detail: dir.display().to_string() });
        }
    }
    if let Some(admin) = admin {
        put_away(&admin).map_err(|e| LaneError::Git { what: "the lane folder is in the Trash, but git's record of it could not be moved there".into(), detail: e })?;
    }
    Ok(Discarded { branch_deleted: delete_merged_branch(repo, branch), kept })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary repository with one commit on `main` and `a.txt`.
    struct Repo {
        top: PathBuf,
        main: PathBuf,
    }

    impl Repo {
        fn new(tag: &str) -> Repo {
            let top = std::env::temp_dir().join(format!("leaf-lanes-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&top);
            let main = top.join("app");
            std::fs::create_dir_all(&main).unwrap();
            let r = Repo { top, main };
            r.git(&r.main, &["init", "-q"]);
            r.git(&r.main, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            for (k, v) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false"), ("core.autocrlf", "false")] {
                r.git(&r.main, &["config", k, v]);
            }
            std::fs::write(r.main.join("a.txt"), "one\n").unwrap();
            r.git(&r.main, &["add", "a.txt"]);
            r.git(&r.main, &["commit", "-q", "-m", "one"]);
            r
        }
        fn git(&self, dir: &Path, args: &[&str]) -> String {
            let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        fn lane(&self, name: &str) -> PathBuf {
            let dir = self.top.join(format!("app-wt/{name}"));
            add(&self.main, name, "main", &dir).unwrap();
            dir
        }
        fn commit(&self, dir: &Path, file: &str, text: &str) {
            std::fs::write(dir.join(file), text).unwrap();
            self.git(dir, &["add", file]);
            self.git(dir, &["commit", "-q", "-m", file]);
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.top);
        }
    }

    #[test]
    fn lane_remove_refuses_uncommitted_work() {
        let r = Repo::new("dirty");
        let dir = r.lane("feat");
        std::fs::write(dir.join("new.txt"), "mine\n").unwrap();
        match remove(&r.main, &dir, "feat", "main") {
            Err(LaneError::Uncommitted(files)) => assert_eq!(files, vec!["new.txt".to_string()]),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "mine\n");
        // a clean lane with nothing new goes, and its merged branch with it
        std::fs::remove_file(dir.join("new.txt")).unwrap();
        assert_eq!(remove(&r.main, &dir, "feat", "main"), Ok(true));
        assert!(!dir.exists());
    }

    #[test]
    fn lane_remove_refuses_unmerged_commits() {
        let r = Repo::new("unmerged");
        let dir = r.lane("feat");
        r.commit(&dir, "b.txt", "lane\n");
        match remove(&r.main, &dir, "feat", "main") {
            Err(LaneError::Unmerged(c)) => assert!(c.len() == 1 && c[0].ends_with(" b.txt"), "{c:?}"),
            other => panic!("{other:?}"),
        }
        assert!(dir.join("b.txt").is_file());
        assert!(r.git(&r.main, &["branch", "--list", "feat"]).contains("feat"));
    }

    #[test]
    fn lane_merge_stops_on_conflict_and_changes_nothing() {
        let r = Repo::new("conflict");
        let dir = r.lane("feat");
        r.commit(&dir, "a.txt", "lane\n");
        r.commit(&r.main, "a.txt", "main\n");
        let (main_head, lane_head) = (r.git(&r.main, &["rev-parse", "HEAD"]), r.git(&dir, &["rev-parse", "HEAD"]));
        match merge(&r.main, &dir, "feat", "main") {
            Err(LaneError::Conflicts(files)) => assert_eq!(files, vec!["a.txt".to_string()]),
            other => panic!("{other:?}"),
        }
        assert_eq!(r.git(&r.main, &["rev-parse", "HEAD"]), main_head);
        assert_eq!(r.git(&dir, &["rev-parse", "HEAD"]), lane_head);
        assert_eq!(std::fs::read_to_string(r.main.join("a.txt")).unwrap(), "main\n");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "lane\n");
        assert_eq!(r.git(&r.main, &["status", "--porcelain"]), "");
        assert_eq!(r.git(&dir, &["status", "--porcelain"]), "");
        assert!(!r.main.join(".git/MERGE_HEAD").exists());
        // without the conflict it merges with a merge commit
        r.git(&dir, &["reset", "-q", "--hard", "main~1"]);
        r.commit(&dir, "c.txt", "lane\n");
        assert_eq!(merge(&r.main, &dir, "feat", "main"), Ok(Merged::MergeCommit));
        assert!(r.main.join("c.txt").is_file());
    }

    fn link_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(target, link).is_err() {
            // no symlink rights: a junction, which needs none
            let ok = Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap().status.success();
            assert!(ok, "no link could be made");
        }
    }

    #[test]
    fn lane_discard_never_touches_a_path_outside_the_lane() {
        let r = Repo::new("discard");
        let dir = r.lane("feat");
        let outside = r.top.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep\n").unwrap();
        // a link inside the lane to a folder outside it, and a link that stands for the lane folder itself
        link_dir(&outside, &dir.join("link"));
        let alias = r.top.join("alias");
        link_dir(&dir, &alias);
        let trash = r.top.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let handed = std::cell::RefCell::new(Vec::<PathBuf>::new());
        let put_away = |p: &Path| -> Result<(), String> {
            handed.borrow_mut().push(p.to_path_buf());
            let n = handed.borrow().len();
            std::fs::rename(p, trash.join(n.to_string())).map_err(|e| e.to_string())
        };
        // another worktree whose folder is away just now (an unplugged drive): it stays registered
        let away = r.lane("away");
        std::fs::rename(&away, r.top.join("away-unplugged")).unwrap();
        // the link to the lane, the main worktree, a plain folder: refused, nothing handed over
        assert!(matches!(discard(&r.main, &alias, "feat", "feat", &put_away), Err(LaneError::NotALane(_))));
        assert!(matches!(discard(&r.main, &r.main, "feat", "feat", &put_away), Err(LaneError::NotALane(_))));
        assert!(matches!(discard(&r.main, &r.top, "feat", "feat", &put_away), Err(LaneError::NotALane(_))));
        assert!(matches!(discard(&r.main, &outside, "feat", "feat", &put_away), Err(LaneError::NotALane(_))));
        assert!(handed.borrow().is_empty());
        // the lane itself: only its folder is handed over; what the link inside points to stays
        r.commit(&dir, "b.txt", "lane\n");
        assert_eq!(discard(&r.main, &dir, "feat", "feat", &put_away).map(|d| d.branch_deleted), Ok(false));
        // the folder, then git's own record of it (in .git/worktrees)
        let got = handed.borrow().clone();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], dir);
        assert!(same_path(got[1].parent().unwrap(), &r.main.join(".git").join("worktrees")), "{got:?}");
        assert!(!dir.exists());
        assert_eq!(std::fs::read_to_string(outside.join("keep.txt")).unwrap(), "keep\n");
        assert!(r.main.join("a.txt").is_file());
        // forgotten, the unmerged branch kept, the other missing worktree still known
        let listed = r.git(&r.main, &["worktree", "list"]);
        assert!(!listed.contains("feat"), "{listed}");
        assert!(listed.contains("away"), "{listed}");
        assert!(r.git(&r.main, &["branch", "--list", "feat"]).contains("feat"));
    }

    #[test]
    fn lane_discard_keeps_commits_of_a_detached_head_on_a_branch() {
        let r = Repo::new("detached");
        let dir = r.lane("feat");
        let start = r.git(&dir, &["rev-parse", "HEAD"]);
        r.git(&dir, &["checkout", "-q", "--detach", &start]);
        r.commit(&dir, "lost.txt", "only here\n");
        let sha = r.git(&dir, &["rev-parse", "HEAD"]);
        let n = std::cell::Cell::new(0);
        std::fs::create_dir_all(r.top.join("trash")).unwrap();
        let put_away = |p: &Path| -> Result<(), String> {
            n.set(n.get() + 1);
            std::fs::rename(p, r.top.join("trash").join(n.get().to_string())).map_err(|e| e.to_string())
        };
        let done = discard(&r.main, &dir, "feat", "feat", &put_away).unwrap();
        let kept = format!("lane/feat-kept-{}", &sha[..7]);
        assert_eq!(done.kept.as_deref(), Some(kept.as_str()));
        assert_eq!(r.git(&r.main, &["rev-parse", &format!("refs/heads/{kept}")]), sha);
        // a lane on its branch needs none
        let other = r.lane("plain");
        r.commit(&other, "c.txt", "x\n");
        assert_eq!(discard(&r.main, &other, "plain", "plain", &put_away).unwrap().kept, None);
    }

    #[test]
    fn lane_discard_refuses_submodule_commits_no_remote_has() {
        let r = Repo::new("submodule");
        // a library repository, added to the project as a submodule
        let lib = r.top.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        r.git(&lib, &["init", "-q"]);
        r.git(&lib, &["config", "user.name", "t"]);
        r.git(&lib, &["config", "user.email", "t@t"]);
        r.commit(&lib, "lib.txt", "one\n");
        let url = lib.to_string_lossy().replace('\\', "/");
        r.git(&r.main, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", &url, "lib"]);
        r.git(&r.main, &["commit", "-q", "-m", "lib"]);
        let dir = r.lane("feat");
        r.git(&dir, &["-c", "protocol.file.allow=always", "submodule", "update", "-q", "--init"]);
        let sub = dir.join("lib");
        r.git(&sub, &["config", "user.name", "t"]);
        r.git(&sub, &["config", "user.email", "t@t"]);
        r.commit(&sub, "work.txt", "not pushed\n");
        let handed = std::cell::Cell::new(false);
        let put_away = |_: &Path| -> Result<(), String> {
            handed.set(true);
            Err("must not be called".into())
        };
        match discard(&r.main, &dir, "feat", "feat", &put_away) {
            Err(LaneError::SubmoduleWork(v)) => assert_eq!(v, vec!["lib".to_string()]),
            other => panic!("{other:?}"),
        }
        assert!(!handed.get());
        assert!(sub.join("work.txt").is_file());
    }

    #[test]
    fn lane_remove_refuses_ignored_files() {
        let r = Repo::new("ignored");
        r.commit(&r.main, ".gitignore", "*.env\ncache/\n");
        let dir = r.lane("feat");
        std::fs::write(dir.join("local.env"), "SECRET=1\n").unwrap();
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        std::fs::write(dir.join("cache/db"), "x").unwrap();
        assert_eq!(end_info(&r.main, &dir, "main").ignored, vec!["cache/".to_string(), "local.env".to_string()]);
        match remove(&r.main, &dir, "feat", "main") {
            Err(LaneError::Ignored(v)) => assert_eq!(v.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(dir.join("local.env")).unwrap(), "SECRET=1\n");
        assert!(dir.join("cache/db").is_file());
    }

    #[test]
    fn lane_discard_refuses_a_repository_made_again_at_the_lane_path() {
        let r = Repo::new("remade");
        let dir = r.lane("feat");
        // deleted by hand (git still lists it), then a repository of its own made at the same place
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        r.git(&dir, &["init", "-q"]);
        r.git(&dir, &["config", "user.name", "t"]);
        r.git(&dir, &["config", "user.email", "t@t"]);
        r.commit(&dir, "own.txt", "mine\n");
        let handed = std::cell::Cell::new(false);
        let put_away = |_: &Path| -> Result<(), String> {
            handed.set(true);
            Err("must not be called".into())
        };
        assert!(matches!(discard(&r.main, &dir, "feat", "feat", &put_away), Err(LaneError::NotALane(_))));
        assert!(!handed.get());
        assert!(dir.join(".git").is_dir() && dir.join("own.txt").is_file());
        assert!(matches!(remove(&r.main, &dir, "feat", "main"), Err(LaneError::NotALane(_))));
        assert!(!end_info(&r.main, &dir, "main").is_lane);
    }
}
