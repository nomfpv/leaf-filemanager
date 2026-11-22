//! The checks behind file operations an agent asks leaf to do (plan F9, `docs/mcp.md`). `leaf-mcp` is another program and its
//! requests are untrusted input: they are parsed here into typed requests ([`parse_request`]) and every path in them is checked
//! against the project folder of the session ([`Root`]) and the agent rules of the project ([`Rules`]) before leaf does anything.
//!
//! The rules, in short:
//! - The project folder (the root) is given once per session, resolved once to its real path, and may not be a drive root, the
//!   user folder itself or a folder that holds it, the Windows or Program Files folders, AppData or leaf's own folders.
//! - Every path an operation touches (each source and each target) is compared on its REAL path: the folder it is in is resolved
//!   (links, junctions, `subst` drives, short names followed) and must be inside the root; an item that is itself a link is
//!   refused (leaf does not act through links for an agent); `..` is refused before anything is looked at.
//! - A target must not exist. Nothing is overwritten, and a name that exists is a refusal that names the conflict.
//! - `trash` takes at most [`MAX_TRASH`] items and never the root.
//! - A path the agent rules deny (Claude Code's `permissions.deny`, the "Protect from agents" rules of leaf's agent rules editor)
//!   is refused, also when it is somewhere inside a folder being moved, copied, renamed or trashed.
//! - Texts returned to the agent hold paths inside the root only ([`Root::shown`]); a refusal about a path outside names the item
//!   by its place in the request, never by where it leads.
//!
//! CHECK-THEN-ACT. These are checks made before the operation runs, not locks: what is on the disk can change between a check
//! and the act. Where the act itself refuses (a copy or a new folder that would land on an existing name, a no-replace rename),
//! the gap is closed by the act; where it does not (a folder replaced by a link after the check), the gap remains. The gaps are
//! listed in `docs/mcp.md`.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::organize::safety::{ProtectedPaths, Refusal, is_inside, norm};

/// Most items one `trash` call may move to the Recycle Bin / Trash.
pub const MAX_TRASH: usize = 50;
/// Most sources of one `copy` or `move`.
pub const MAX_SOURCES: usize = 200;
/// Most results of a `search`, and how many when the agent does not say.
pub const MAX_SEARCH: usize = 200;
pub const DEFAULT_SEARCH: usize = 50;
/// Most entries of a `list_folder`.
pub const MAX_LIST: usize = 1000;
/// The longest request taken (bytes of JSON).
pub const MAX_REQUEST: usize = 256 * 1024;
/// Most items looked at inside a folder to check it against the agent rules; a bigger folder is refused.
const DEEP_CAP: usize = 50_000;
/// Most entries a search walks where leaf's index does not cover the folder.
const WALK_CAP: usize = 200_000;
const MAX_TEXT: usize = 4096;

// ---------------------------------------------------------------------------------------------------------------------------
// the request
// ---------------------------------------------------------------------------------------------------------------------------

/// One request of `leaf-mcp`: the session it belongs to, the agent's name for itself, the session's project folder, and the
/// operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub session: String,
    pub agent: String,
    pub root: String,
    pub op: Op,
}

/// The operations. Paths are absolute, or relative to the root.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Search {
        query: String,
        #[serde(default)]
        contents: bool,
        #[serde(default)]
        max: Option<u32>,
    },
    ListFolder {
        path: String,
    },
    Copy {
        sources: Vec<String>,
        destination: String,
    },
    Move {
        sources: Vec<String>,
        destination: String,
    },
    Rename {
        path: String,
        new_name: String,
    },
    CreateFolder {
        path: String,
    },
    Trash {
        paths: Vec<String>,
    },
}

impl Op {
    /// Changes files (needs the switch and the user's consent).
    pub fn mutating(&self) -> bool {
        !matches!(self, Op::Search { .. } | Op::ListFolder { .. })
    }

    /// What it does, for the consent dialog ("wants to: copy files").
    pub fn verb(&self) -> &'static str {
        match self {
            Op::Search { .. } => "search",
            Op::ListFolder { .. } => "list a folder",
            Op::Copy { .. } => "copy files",
            Op::Move { .. } => "move files",
            Op::Rename { .. } => "rename an item",
            Op::CreateFolder { .. } => "create a folder",
            Op::Trash { .. } => "move items to the Recycle Bin / Trash",
        }
    }

    fn texts(&self) -> Vec<&str> {
        match self {
            Op::Search { query, .. } => vec![query],
            Op::ListFolder { path } | Op::CreateFolder { path } => vec![path],
            Op::Copy { sources, destination } | Op::Move { sources, destination } => sources.iter().map(String::as_str).chain([destination.as_str()]).collect(),
            Op::Rename { path, new_name } => vec![path, new_name],
            Op::Trash { paths } => paths.iter().map(String::as_str).collect(),
        }
    }
}

/// Parses and bounds one request. The agent's name is cut to something fit to show (printable, at most 60 characters).
pub fn parse_request(text: &str) -> Result<Request, String> {
    if text.len() > MAX_REQUEST {
        return Err("the request is too long".into());
    }
    let mut r: Request = serde_json::from_str(text).map_err(|e| format!("the request is not understood ({e})"))?;
    if !(16..=64).contains(&r.session.len()) || !r.session.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the request has no valid session id".into());
    }
    let agent: String = r.agent.chars().filter(|c| !c.is_control()).take(60).collect();
    r.agent = if agent.trim().is_empty() { "an agent".into() } else { agent.trim().to_string() };
    if r.root.is_empty() || r.root.len() > MAX_TEXT || r.root.contains('\0') {
        return Err("the request has no valid project folder".into());
    }
    for t in r.op.texts() {
        if t.len() > MAX_TEXT || t.contains('\0') {
            return Err("a path or name in the request is too long or holds a NUL character".into());
        }
    }
    Ok(r)
}

// ---------------------------------------------------------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------------------------------------------------------

/// `p` without the `\\?\` prefix `canonicalize` gives on Windows (`\\?\UNC\server\share` becomes `\\server\share`).
pub fn simplify(p: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return p;
    }
    let s = p.to_string_lossy();
    if let Some(r) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{r}"));
    }
    if let Some(r) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(r);
    }
    p
}

fn real(p: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(p).map(simplify)
}

/// A link of any kind (symbolic link, junction, mount point) by the item's own metadata.
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

/// A name fit for a new item on every system leaf runs on: one component, no separator, none of `<>:"|?*`, no control
/// character, not `.` or `..`, no trailing dot or space, not a device name (`CON`, `NUL`, `COM1`...).
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 255 {
        return Err("the name is empty or longer than 255 characters".into());
    }
    if name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        return Err(format!("\"{name}\" is not a usable name (no leading or trailing space, no trailing dot)"));
    }
    if name.chars().any(|c| c.is_control() || "/\\<>:\"|?*".contains(c)) {
        return Err(format!("\"{name}\" holds a character that is not allowed in a name (one of / \\ < > : \" | ? *)"));
    }
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if device {
        return Err(format!("\"{name}\" is a device name on Windows"));
    }
    Ok(())
}

/// The project folder of a session, resolved once.
#[derive(Debug, Clone)]
pub struct Root {
    real: PathBuf,
    key: String,
    /// The folder as it was given (made absolute, not resolved): an absolute path the agent writes in that form is taken as the
    /// same place under the real root, then checked like any other.
    given_key: String,
}

fn refusal_text(r: Refusal) -> &'static str {
    match r {
        Refusal::NotAbsolute => "the project folder must be an absolute path",
        Refusal::DriveRoot => "the project folder may not be the root of a drive",
        Refusal::WindowsFolder => "the project folder may not be in the Windows folder",
        Refusal::ProgramFiles => "the project folder may not be in Program Files or ProgramData",
        Refusal::UserProfile => "the project folder may not be the user folder itself or a folder that holds it",
        Refusal::AppData => "the project folder may not be in AppData",
        Refusal::AppOwn => "the project folder may not be one of leaf's own folders",
    }
}

impl Root {
    /// Resolves the project folder given at connection. `protected` are the places no agent may work in (the organizer's list).
    pub fn open(given: &str, protected: &ProtectedPaths) -> Result<Root, String> {
        let p = Path::new(given);
        if !p.is_absolute() {
            return Err(refusal_text(Refusal::NotAbsolute).into());
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("the project folder may not contain \"..\"".into());
        }
        let r = real(p).map_err(|_| "the project folder does not exist or cannot be read".to_string())?;
        if !r.is_dir() {
            return Err("the project folder is not a folder".into());
        }
        // the text and the real place both (a link from an innocent name to C:\ is refused like C:\)
        for f in [p, r.as_path()] {
            if let Some(why) = protected.refuse(f, true) {
                return Err(refusal_text(why).into());
            }
        }
        Ok(Root { key: norm(&r), given_key: norm(p), real: r })
    }

    pub fn path(&self) -> &Path {
        &self.real
    }

    /// Is the real path `p` the root or inside it?
    pub fn holds(&self, p: &Path) -> bool {
        is_inside(&norm(p), &self.key)
    }

    fn is_root(&self, p: &Path) -> bool {
        norm(p) == self.key
    }

    /// A path inside the root as the agent may see it (absolute); anything else as a word that says it is not shown.
    pub fn shown(&self, p: &Path) -> String {
        if self.holds(p) { p.display().to_string() } else { "(a place outside the project folder)".into() }
    }

    /// The agent's text as a path, before anything is looked at: relative to the root or absolute, never with `..`, never a
    /// relative path that names a drive or starts at a root (`C:x`, `\x`). Lexically inside the root.
    fn lexical(&self, s: &str, what: &str) -> Result<PathBuf, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err(format!("{what} is empty"));
        }
        let p = Path::new(s);
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("{what} contains \"..\", which is not taken"));
        }
        let full = if p.is_absolute() {
            let k = norm(p);
            if !is_inside(&k, &self.key) && is_inside(&k, &self.given_key) {
                let n = Path::new(&self.given_key).components().count();
                self.real.join(p.components().skip(n).collect::<PathBuf>())
            } else {
                p.to_path_buf()
            }
        } else if p.has_root() || p.components().any(|c| matches!(c, Component::Prefix(_))) {
            return Err(format!("{what} is outside the project folder"));
        } else {
            self.real.join(p)
        };
        // `.` and a trailing separator go (`root/.` and `root/` are the root)
        let full: PathBuf = full.components().collect();
        if !self.holds(&full) {
            return Err(format!("{what} is outside the project folder"));
        }
        Ok(full)
    }

    /// An existing item inside the root that is not the root and not a link: its real path. The folder it is in is resolved and
    /// must be inside the root; the item resolved must be the same place (no link on the way).
    pub fn item(&self, s: &str, what: &str) -> Result<PathBuf, String> {
        let full = self.lexical(s, what)?;
        if self.is_root(&full) {
            return Err(format!("{what} is the project folder itself"));
        }
        let outside = || format!("{what} is outside the project folder");
        let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else { return Err(format!("{what} is the project folder itself")) };
        let parent = real(parent).map_err(|_| format!("{what} does not exist"))?;
        if !self.holds(&parent) {
            return Err(outside());
        }
        let at = parent.join(name);
        let meta = std::fs::symlink_metadata(&at).map_err(|_| format!("{what} does not exist"))?;
        if is_link(&meta) {
            return Err(format!("{what} is a link; leaf does not act on links for an agent"));
        }
        let resolved = real(&at).map_err(|_| format!("{what} cannot be read"))?;
        if norm(&resolved) != norm(&at) || !self.holds(&resolved) {
            return Err(outside());
        }
        if self.is_root(&resolved) {
            return Err(format!("{what} is the project folder itself"));
        }
        Ok(resolved)
    }

    /// An existing folder inside the root (the root itself too): its real path.
    pub fn folder(&self, s: &str, what: &str) -> Result<PathBuf, String> {
        let full = self.lexical(s, what)?;
        self.folder_at(&full, what)
    }

    fn folder_at(&self, full: &Path, what: &str) -> Result<PathBuf, String> {
        let r = real(full).map_err(|_| format!("{what} does not exist"))?;
        if !self.holds(&r) {
            return Err(format!("{what} is outside the project folder"));
        }
        if !r.is_dir() {
            return Err(format!("{what} is not a folder"));
        }
        Ok(r)
    }

    /// `name` in the real folder `dir` (inside the root), which must not exist yet: nothing is ever overwritten.
    pub fn free_target(&self, dir: &Path, name: &str) -> Result<PathBuf, String> {
        valid_name(name)?;
        let t = dir.join(name);
        if !self.holds(&t) {
            return Err("the target is outside the project folder".into());
        }
        match std::fs::symlink_metadata(&t) {
            Ok(_) => Err(format!("{} already exists; nothing is overwritten, choose another name or place", self.shown(&t))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(t),
            Err(_) => Err(format!("whether {} exists cannot be checked", self.shown(&t))),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// the agent rules (protected folders)
// ---------------------------------------------------------------------------------------------------------------------------

/// The deny rules of the agent's settings that name files (`Read(...)`, `Edit(...)`, `Write(...)`, `MultiEdit(...)`,
/// `NotebookEdit(...)`): a path one of them matches is protected, and an operation on it is refused. Patterns as Claude Code reads
/// them (gitignore style): `/x` is relative to the settings' project folder, `//x` is absolute, `~/x` is in the home folder, `x`
/// matches at any depth. A rule with no pattern (`Edit`) protects everything; a rule that cannot be understood also protects
/// everything (refusing is safer than guessing).
#[derive(Default)]
pub struct Rules {
    sets: Vec<(PathBuf, ignore::gitignore::Gitignore)>,
    everything: Option<String>,
}

const FILE_TOOLS: &[&str] = &["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

impl Rules {
    pub fn new() -> Rules {
        Rules::default()
    }

    /// No rule at all (nothing to look at inside folders).
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty() && self.everything.is_none()
    }

    /// Everything is refused, with this reason (a settings file that could not be read).
    pub fn refuse_all(&mut self, why: impl Into<String>) {
        self.everything.get_or_insert(why.into());
    }

    /// Adds the deny rules of a settings file whose project folder is `base` (a real path). `home`: the real home folder.
    pub fn add(&mut self, base: &Path, rules: &[String], home: Option<&Path>) {
        for rule in rules {
            let rule = rule.trim();
            let (tool, spec) = match rule.find('(') {
                Some(i) if rule.ends_with(')') => (&rule[..i], Some(rule[i + 1..rule.len() - 1].trim())),
                Some(_) => continue,
                None => (rule, None),
            };
            if !FILE_TOOLS.contains(&tool) {
                continue;
            }
            let Some(spec) = spec.filter(|s| !s.is_empty()) else {
                self.refuse_all(format!("the agent rules deny {tool} everywhere"));
                continue;
            };
            if spec.starts_with('!') {
                self.refuse_all("an agent rule could not be understood");
                continue;
            }
            let (at, pattern): (PathBuf, String) = if let Some(abs) = spec.strip_prefix("//") {
                absolute_pattern(abs)
            } else if let Some(h) = spec.strip_prefix("~/") {
                match home {
                    Some(home) => (home.to_path_buf(), format!("/{h}")),
                    None => {
                        self.refuse_all("an agent rule names the home folder, which is not known");
                        continue;
                    }
                }
            } else if let Some(r) = spec.strip_prefix("./") {
                (base.to_path_buf(), format!("/{r}"))
            } else {
                (base.to_path_buf(), spec.to_string())
            };
            let mut b = ignore::gitignore::GitignoreBuilder::new(&at);
            let added = b.case_insensitive(cfg!(windows)).is_ok() && b.add_line(None, &pattern).is_ok();
            let built = if added { b.build().ok() } else { None };
            match built {
                Some(g) => self.sets.push((at, g)),
                None => self.refuse_all("an agent rule could not be understood"),
            }
        }
    }

    /// Does a rule protect the real path `p` (or a folder it is in)?
    pub fn blocks(&self, p: &Path, is_dir: bool) -> bool {
        if self.everything.is_some() {
            return true;
        }
        let key = norm(p);
        self.sets.iter().any(|(base, g)| {
            if !is_inside(&key, &norm(base)) {
                return false;
            }
            let rel: PathBuf = p.components().skip(base.components().count()).collect();
            if rel.as_os_str().is_empty() {
                // the settings' own folder: its rules are about what is inside it
                return false;
            }
            // a folder whose contents a rule covers (`dir/**`) is protected itself: moving or trashing it takes them along
            g.matched_path_or_any_parents(&rel, is_dir).is_ignore() || (is_dir && g.matched_path_or_any_parents(rel.join("leaf-any-item"), false).is_ignore())
        })
    }

    /// Refuses `p` when it is protected, and with `deep` also when anything inside the folder `p` is (links inside are not
    /// followed; a folder with more than a set number of items inside is refused rather than half checked).
    pub fn check(&self, root: &Root, p: &Path, deep: bool, what: &str) -> Result<(), String> {
        if let Some(why) = &self.everything {
            return Err(format!("{what} is protected: {why}"));
        }
        let is_dir = std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
        if self.blocks(p, is_dir) {
            return Err(format!("{} is protected by the agent rules of this project", root.shown(p)));
        }
        if !deep || !is_dir || self.is_empty() {
            return Ok(());
        }
        let mut seen = 0usize;
        let mut stack = vec![p.to_path_buf()];
        while let Some(d) = stack.pop() {
            let rd = std::fs::read_dir(&d).map_err(|_| format!("{} cannot be read to check it against the agent rules", root.shown(&d)))?;
            for e in rd {
                let e = e.map_err(|_| format!("{} cannot be read to check it against the agent rules", root.shown(&d)))?;
                seen += 1;
                if seen > DEEP_CAP {
                    return Err(format!("{what} holds too many items to check against the agent rules; it is refused"));
                }
                let ft = e.file_type().map_err(|_| format!("{} cannot be read", root.shown(&e.path())))?;
                let dir = ft.is_dir() && !ft.is_symlink();
                if self.blocks(&e.path(), dir) {
                    return Err(format!("{what} holds {}, which is protected by the agent rules of this project", root.shown(&e.path())));
                }
                if dir {
                    stack.push(e.path());
                }
            }
        }
        Ok(())
    }
}

/// `//x/y` of a rule: the absolute place it is matched from and its pattern. On Windows `//c/x` is `C:\x`.
fn absolute_pattern(abs: &str) -> (PathBuf, String) {
    if cfg!(windows) {
        let b = abs.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b'/' {
            return (PathBuf::from(format!("{}:\\", (b[0] as char).to_ascii_uppercase())), format!("/{}", &abs[2..]));
        }
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return (PathBuf::from(format!("{}:\\", (b[0] as char).to_ascii_uppercase())), format!("/{}", abs[2..].trim_start_matches(['/', '\\'])));
        }
    }
    (PathBuf::from("/"), format!("/{abs}"))
}

// ---------------------------------------------------------------------------------------------------------------------------
// the plan of a request
// ---------------------------------------------------------------------------------------------------------------------------

/// A checked operation: real paths, all inside the root, every target free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Search { query: String, contents: bool, max: usize },
    List { dir: PathBuf },
    Transfer { moving: bool, sources: Vec<PathBuf>, dest: PathBuf, targets: Vec<PathBuf> },
    Rename { from: PathBuf, to: PathBuf },
    CreateFolder { path: PathBuf },
    Trash { paths: Vec<PathBuf> },
}

fn nth(what: &str, i: usize, n: usize) -> String {
    if n == 1 { what.to_string() } else { format!("{what} {}", i + 1) }
}

/// Checks `op` against the root and the rules. Nothing on the disk is changed.
pub fn check(root: &Root, rules: &Rules, op: &Op) -> Result<Plan, String> {
    match op {
        Op::Search { query, contents, max } => {
            let q = query.trim();
            if q.is_empty() || q.chars().count() > 200 || q.chars().any(char::is_control) {
                return Err("the search text is empty, longer than 200 characters or holds control characters".into());
            }
            let max = max.map_or(DEFAULT_SEARCH, |m| (m as usize).clamp(1, MAX_SEARCH));
            Ok(Plan::Search { query: q.to_string(), contents: *contents, max })
        }
        Op::ListFolder { path } => {
            let dir = root.folder(path, "the folder")?;
            if rules.blocks(&dir, true) {
                return Err(format!("{} is protected by the agent rules of this project", root.shown(&dir)));
            }
            Ok(Plan::List { dir })
        }
        Op::Copy { sources, destination } | Op::Move { sources, destination } => {
            let moving = matches!(op, Op::Move { .. });
            if sources.is_empty() || sources.len() > MAX_SOURCES {
                return Err(format!("give between 1 and {MAX_SOURCES} sources"));
            }
            let dest = root.folder(destination, "the destination")?;
            rules.check(root, &dest, false, "the destination")?;
            let n = sources.len();
            let mut srcs = Vec::with_capacity(n);
            let mut targets: Vec<PathBuf> = Vec::with_capacity(n);
            for (i, s) in sources.iter().enumerate() {
                let what = nth("source", i, n);
                let src = root.item(s, &what)?;
                if srcs.iter().any(|p: &PathBuf| norm(p) == norm(&src)) {
                    return Err(format!("{what} is listed twice"));
                }
                if is_inside(&norm(&dest), &norm(&src)) {
                    return Err(format!("{what} is a folder the destination is in; it cannot go into itself"));
                }
                // a copy only reads the source, a move takes it away: both are refused on a protected one
                rules.check(root, &src, true, &what)?;
                let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let target = root.free_target(&dest, &name)?;
                if targets.iter().any(|t| norm(t) == norm(&target)) {
                    return Err(format!("two sources have the name \"{name}\"; they would land on one target"));
                }
                rules.check(root, &target, false, "the target")?;
                srcs.push(src);
                targets.push(target);
            }
            Ok(Plan::Transfer { moving, sources: srcs, dest, targets })
        }
        Op::Rename { path, new_name } => {
            let from = root.item(path, "the item")?;
            rules.check(root, &from, true, "the item")?;
            let dir = from.parent().ok_or("the item has no folder")?;
            let to = root.free_target(dir, new_name.trim_end_matches(['\r', '\n']))?;
            rules.check(root, &to, false, "the new name")?;
            Ok(Plan::Rename { from, to })
        }
        Op::CreateFolder { path } => {
            let full = root.lexical(path, "the new folder")?;
            let (Some(parent), Some(name)) = (full.parent(), full.file_name()) else { return Err("the new folder is the project folder itself".into()) };
            let parent = root.folder_at(parent, "the folder the new folder goes in")?;
            let path = root.free_target(&parent, &name.to_string_lossy())?;
            rules.check(root, &path, false, "the new folder")?;
            Ok(Plan::CreateFolder { path })
        }
        Op::Trash { paths } => {
            if paths.is_empty() {
                return Err("nothing to move to the Recycle Bin / Trash".into());
            }
            if paths.len() > MAX_TRASH {
                return Err(format!("at most {MAX_TRASH} items go to the Recycle Bin / Trash in one call; this asked for {}", paths.len()));
            }
            let n = paths.len();
            let mut out: Vec<PathBuf> = Vec::with_capacity(n);
            for (i, s) in paths.iter().enumerate() {
                let what = nth("item", i, n);
                let p = root.item(s, &what)?;
                if out.iter().any(|q| is_inside(&norm(&p), &norm(q)) || is_inside(&norm(q), &norm(&p))) {
                    return Err(format!("{what} is listed twice or is inside another listed item"));
                }
                rules.check(root, &p, true, &what)?;
                out.push(p);
            }
            Ok(Plan::Trash { paths: out })
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// the read-only answers
// ---------------------------------------------------------------------------------------------------------------------------

/// One entry of `list_folder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    /// "folder", "file" or "link"
    pub kind: &'static str,
    pub size: u64,
}

/// The entries of the checked folder `dir` (at most [`MAX_LIST`]; protected entries are left out). `true` with the list when it
/// was cut.
pub fn list(rules: &Rules, dir: &Path) -> Result<(Vec<Listed>, bool), String> {
    let rd = std::fs::read_dir(dir).map_err(|_| "the folder cannot be read".to_string())?;
    let mut out = Vec::new();
    let mut cut = false;
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let link = ft.is_symlink() || e.metadata().is_ok_and(|m| is_link(&m));
        let is_dir = ft.is_dir() && !link;
        if rules.blocks(&e.path(), is_dir) {
            continue;
        }
        if out.len() >= MAX_LIST {
            cut = true;
            break;
        }
        let size = if is_dir || link { 0 } else { e.metadata().map(|m| m.len()).unwrap_or(0) };
        out.push(Listed { name: e.file_name().to_string_lossy().into_owned(), kind: if link { "link" } else if is_dir { "folder" } else { "file" }, size });
    }
    out.sort_by(|a, b| (a.kind != "folder").cmp(&(b.kind != "folder")).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok((out, cut))
}

/// Of what a search found, what may go back to the agent: inside the root, not protected, at most `max`.
pub fn keep_found(root: &Root, rules: &Rules, found: impl IntoIterator<Item = PathBuf>, max: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in found {
        if out.len() >= max {
            break;
        }
        if root.holds(&p) && !root.is_root(&p) && !rules.blocks(&p, p.is_dir()) {
            out.push(p);
        }
    }
    out
}

/// A name search by walking the root (where leaf's index does not cover it): names that hold `query` (case ignored), links not
/// followed, protected folders not entered. At most `max` results and a bounded walk.
pub fn walk_search(root: &Root, rules: &Rules, query: &str, max: usize) -> Vec<PathBuf> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![root.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > WALK_CAP || out.len() >= max {
                return out;
            }
            let Ok(ft) = e.file_type() else { continue };
            let dir = ft.is_dir() && !ft.is_symlink() && !e.metadata().is_ok_and(|m| is_link(&m));
            let p = e.path();
            if rules.blocks(&p, dir) {
                continue;
            }
            if e.file_name().to_string_lossy().to_lowercase().contains(&q) {
                out.push(p.clone());
            }
            if dir {
                stack.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A temp folder with `proj` (the root, holding `a.txt` and `sub/`) and `outside` (holding `secret.txt`) beside it.
    fn setup(tag: &str) -> (Tmp, Root) {
        let base = std::env::temp_dir().join(format!("leaf-agent-ops-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("proj").join("sub")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(base.join("proj").join("a.txt"), b"a").unwrap();
        std::fs::write(base.join("outside").join("secret.txt"), b"s").unwrap();
        let root = Root::open(&base.join("proj").to_string_lossy(), &ProtectedPaths::default()).unwrap();
        (Tmp(base), root)
    }

    /// A link `at` -> `to` (a junction on Windows, which needs no rights); false when the system would not make one.
    fn link_dir(at: &Path, to: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(at).arg(to).output().is_ok_and(|o| o.status.success())
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(to, at).is_ok()
        }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn agent_ops_outside_root_is_refused_also_through_a_link() {
        let (tmp, root) = setup("outside");
        let rules = Rules::new();
        let outside = tmp.0.join("outside");
        let secret = outside.join("secret.txt");
        let refused = |op: Op| {
            let e = check(&root, &rules, &op).unwrap_err();
            assert!(!e.contains("outside\\") && !e.contains("outside/") && !e.contains("secret"), "no outside path in the text: {e}");
            e
        };
        // a source outside, by its path and by `..`
        refused(Op::Copy { sources: vec![s(&secret)], destination: ".".into() });
        refused(Op::Copy { sources: vec!["../outside/secret.txt".into()], destination: ".".into() });
        // a target outside
        refused(Op::Copy { sources: vec!["a.txt".into()], destination: s(&outside) });
        refused(Op::Move { sources: vec!["a.txt".into()], destination: "../outside".into() });
        refused(Op::CreateFolder { path: s(&outside.join("new")) });
        refused(Op::Trash { paths: vec![s(&secret)] });
        refused(Op::ListFolder { path: s(&outside) });
        // another drive or a rooted relative path
        #[cfg(windows)]
        refused(Op::Trash { paths: vec![r"\Windows".into()] });
        // through a link inside the root that leads outside
        let link = root.path().join("door");
        if link_dir(&link, &outside) {
            refused(Op::Trash { paths: vec!["door/secret.txt".into()] });
            refused(Op::Copy { sources: vec!["door/secret.txt".into()], destination: ".".into() });
            refused(Op::Copy { sources: vec!["a.txt".into()], destination: "door".into() });
            refused(Op::CreateFolder { path: "door/new".into() });
            refused(Op::Rename { path: "door/secret.txt".into(), new_name: "x.txt".into() });
            // the link itself is not acted on either
            refused(Op::Trash { paths: vec!["door".into()] });
            refused(Op::ListFolder { path: "door".into() });
        } else {
            eprintln!("no link could be made here: the link part was not run");
        }
        assert!(secret.exists(), "nothing outside was touched");
        // inside is fine
        assert!(check(&root, &rules, &Op::Copy { sources: vec!["a.txt".into()], destination: "sub".into() }).is_ok());
    }

    #[test]
    fn agent_ops_existing_target_is_not_overwritten() {
        let (_tmp, root) = setup("conflict");
        let rules = Rules::new();
        std::fs::write(root.path().join("sub").join("a.txt"), b"keep").unwrap();
        std::fs::write(root.path().join("b.txt"), b"bee").unwrap();
        for op in [
            Op::Copy { sources: vec!["a.txt".into()], destination: "sub".into() },
            Op::Move { sources: vec!["a.txt".into()], destination: "sub".into() },
            Op::Rename { path: "a.txt".into(), new_name: "b.txt".into() },
            // a different case is the same name on Windows; on Linux it is free, so only the exact name is checked there
            #[cfg(windows)]
            Op::Rename { path: "a.txt".into(), new_name: "B.TXT".into() },
            Op::CreateFolder { path: "sub".into() },
            Op::Copy { sources: vec!["a.txt".into()], destination: ".".into() },
        ] {
            let e = check(&root, &rules, &op).unwrap_err();
            assert!(e.contains("already exists"), "{op:?}: {e}");
        }
        assert_eq!(std::fs::read(root.path().join("sub").join("a.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read(root.path().join("b.txt")).unwrap(), b"bee");
        assert_eq!(std::fs::read(root.path().join("a.txt")).unwrap(), b"a");
    }

    #[test]
    fn agent_ops_trash_of_root_and_of_more_than_50_is_refused() {
        let (_tmp, root) = setup("trash");
        let rules = Rules::new();
        let whole = s(root.path());
        for op in [Op::Trash { paths: vec![whole.clone()] }, Op::Trash { paths: vec![".".into()] }, Op::Trash { paths: vec![format!("{whole}/")] }] {
            let e = check(&root, &rules, &op).unwrap_err();
            assert!(e.contains("project folder itself"), "{op:?}: {e}");
        }
        for i in 0..51 {
            std::fs::write(root.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let many: Vec<String> = (0..51).map(|i| format!("f{i}.txt")).collect();
        let e = check(&root, &rules, &Op::Trash { paths: many.clone() }).unwrap_err();
        assert!(e.contains("at most 50"), "{e}");
        assert!(matches!(check(&root, &rules, &Op::Trash { paths: many[..50].to_vec() }), Ok(Plan::Trash { paths }) if paths.len() == 50));
        assert!(root.path().join("f50.txt").exists());
    }

    #[test]
    fn agent_ops_protected_folder_refuses() {
        let (_tmp, root) = setup("rules");
        std::fs::create_dir_all(root.path().join("sub").join("keys")).unwrap();
        let mut rules = Rules::new();
        rules.add(root.path(), &["Edit(/sub/keys/**)".into(), "Bash(rm *)".into()], None);
        let e = check(&root, &rules, &Op::Trash { paths: vec!["sub".into()] }).unwrap_err();
        assert!(e.contains("protected"), "{e}");
        assert!(check(&root, &rules, &Op::CreateFolder { path: "sub/keys/new".into() }).is_err());
        assert!(check(&root, &rules, &Op::Trash { paths: vec!["a.txt".into()] }).is_ok());
        let mut all = Rules::new();
        all.add(root.path(), &["Edit".into()], None);
        assert!(check(&root, &all, &Op::Trash { paths: vec!["a.txt".into()] }).is_err());
    }
}
