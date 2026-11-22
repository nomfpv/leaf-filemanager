//! Build junk: well-known folders that a build or an install makes again (`node_modules`, `target`, `.venv`...). The name rules, the
//! walk that finds them and the guards that decide whether one may be offered or cleaned live here, pure and testable; the app only
//! draws the list, measures sizes and moves the ticked folders to the Recycle Bin.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::gitignore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NodeModules,
    Target,
    Venv,
    PyCache,
    PytestCache,
    MypyCache,
    Dist,
    Build,
    Next,
    Nuxt,
    Gradle,
    Bin,
    Obj,
    Turbo,
    ParcelCache,
    DerivedData,
    Terraform,
}

impl Kind {
    /// What the list shows as the kind.
    pub fn label(self) -> &'static str {
        match self {
            Kind::NodeModules => "Node packages",
            Kind::Target => "Rust build",
            Kind::Venv => "Python environment",
            Kind::PyCache => "Python cache",
            Kind::PytestCache => "pytest cache",
            Kind::MypyCache => "mypy cache",
            Kind::Dist => "dist output",
            Kind::Build => "build output",
            Kind::Next => "Next.js cache",
            Kind::Nuxt => "Nuxt cache",
            Kind::Gradle => "Gradle cache",
            Kind::Bin => ".NET output (bin)",
            Kind::Obj => ".NET output (obj)",
            Kind::Turbo => "Turborepo cache",
            Kind::ParcelCache => "Parcel cache",
            Kind::DerivedData => "Xcode build data",
            Kind::Terraform => "Terraform plugins",
        }
    }
}

/// The folder name alone: which kind it could be (the other rules, beside what it must stand, come in [`kind_of`]).
pub fn kind_by_name(name: &str) -> Option<Kind> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        "node_modules" => Kind::NodeModules,
        "target" => Kind::Target,
        ".venv" | "venv" => Kind::Venv,
        "__pycache__" => Kind::PyCache,
        ".pytest_cache" => Kind::PytestCache,
        ".mypy_cache" => Kind::MypyCache,
        "dist" => Kind::Dist,
        "build" => Kind::Build,
        ".next" => Kind::Next,
        ".nuxt" => Kind::Nuxt,
        ".gradle" => Kind::Gradle,
        "bin" => Kind::Bin,
        "obj" => Kind::Obj,
        ".turbo" => Kind::Turbo,
        ".parcel-cache" => Kind::ParcelCache,
        "deriveddata" => Kind::DerivedData,
        ".terraform" => Kind::Terraform,
        _ => return None,
    })
}

fn has_csproj(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("csproj"))))
}

/// The kind of the folder at `path` when its name AND what stands around it fit: `target` beside a `Cargo.toml`, a venv holding
/// `pyvenv.cfg`, `bin`/`obj` beside a `.csproj`, `dist`/`build` only when git ignores them. Looks at the disk, does not change it.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy();
    let kind = kind_by_name(&name)?;
    let parent = path.parent()?;
    let ok = match kind {
        Kind::Target => parent.join("Cargo.toml").is_file(),
        Kind::Venv => path.join("pyvenv.cfg").is_file(),
        Kind::Bin | Kind::Obj => has_csproj(parent),
        Kind::Dist | Kind::Build => gitignore::matcher_for(parent).is_some_and(|m| m.is_ignored(path, true)),
        _ => true,
    };
    ok.then_some(kind)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Not inside the folder that was scanned (or the folder itself).
    Outside,
    /// Gone, or no longer a folder.
    NotAFolder,
    /// A link or junction: its target is not the junk.
    Link,
    /// The name rule (with what must stand beside it) no longer fits.
    NotJunk,
    /// Git tracks files in it.
    Tracked,
    /// Inside a repository, git could not be asked and the folder is not git-ignored: not offered.
    Unsure,
}

fn is_link(path: &Path) -> Option<bool> {
    let m = std::fs::symlink_metadata(path).ok()?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    Some(m.file_type().is_symlink() || reparse)
}

/// Does git track at least one file under `dir`? Asks `git ls-files --cached` (no shell, first line only). `None` when git cannot be
/// run or fails (not installed, not a repository, an error).
pub fn git_tracks(dir: &Path) -> Option<bool> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["ls-files", "--cached", "--", "."]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _turn = leaf_proc::GIT.enter();
    let (mut child, tree) = leaf_proc::spawn_helper_tree(&mut c).ok()?;
    let mut line = String::new();
    let got = BufReader::new(child.stdout.take()?).read_line(&mut line).ok()?;
    // the answer is in: stop git rather than let it list a committed node_modules
    leaf_proc::kill_tree(&mut child, &tree);
    let status = child.wait().ok()?;
    if got > 0 {
        return Some(true);
    }
    // no output: nothing tracked, unless git failed
    status.success().then_some(false)
}

/// [`check_with`] asking real git.
pub fn check(path: &Path, root: &Path) -> Result<Kind, Reject> {
    check_with(path, root, &git_tracks)
}

/// May `path` be offered and cleaned? Run again just before cleaning. `tracked` says whether git tracks files under a folder
/// (`Some(true)`), does not (`Some(false)`), or cannot say (`None`). Inside a repository an unanswered question leaves the folder
/// out unless the git-ignore matcher says it is ignored; outside one the name rules alone decide.
pub fn check_with(path: &Path, root: &Path, tracked: &dyn Fn(&Path) -> Option<bool>) -> Result<Kind, Reject> {
    if path == root || !path.starts_with(root) || path.components().any(|c| c.as_os_str() == "..") {
        return Err(Reject::Outside);
    }
    match is_link(path) {
        None => return Err(Reject::NotAFolder),
        Some(true) => return Err(Reject::Link),
        Some(false) => {}
    }
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        return Err(Reject::NotAFolder);
    }
    let kind = kind_of(path).ok_or(Reject::NotJunk)?;
    if gitignore::repo_root(path).is_some() {
        match tracked(path) {
            Some(true) => return Err(Reject::Tracked),
            Some(false) => {}
            None => {
                let ignored = path.parent().and_then(gitignore::matcher_for).is_some_and(|m| m.is_ignored(path, true));
                if !ignored {
                    return Err(Reject::Unsure);
                }
            }
        }
    }
    Ok(kind)
}

#[derive(Debug, Clone)]
pub struct Junk {
    pub path: PathBuf,
    pub kind: Kind,
    /// The folder it belongs to.
    pub project: PathBuf,
}

/// Walks `root` for junk folders (not into the ones it finds, links or `.git`), handing each that passes [`check`] to `found`.
/// Stops when `stop` is set.
pub fn scan(root: &Path, stop: &AtomicBool, tracked: &dyn Fn(&Path) -> Option<bool>, found: &mut dyn FnMut(Junk)) {
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() || ft.is_symlink() {
                continue;
            }
            let path = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            if kind_by_name(&e.file_name().to_string_lossy()).is_some() {
                if let Ok(kind) = check_with(&path, root, tracked) {
                    found(Junk { project: dir.clone(), path, kind });
                    continue;
                }
            }
            if is_link(&path) != Some(false) {
                continue;
            }
            todo.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_on_throwaway_folders() {
        let root = std::env::temp_dir().join(format!("leaf-dev-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |p: &str| std::fs::create_dir_all(root.join(p)).unwrap();
        mk("rs/target/debug");
        mk("rs/src");
        mk("plain/target");
        mk("web/node_modules/x");
        mk("py/.venv");
        std::fs::write(root.join("rs/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(root.join("py/.venv/pyvenv.cfg"), "home = x\n").unwrap();
        let none = |_: &Path| Some(false);

        // found: target beside Cargo.toml, node_modules, a venv with pyvenv.cfg; not: target without Cargo.toml
        let mut got = Vec::new();
        scan(&root, &AtomicBool::new(false), &none, &mut |j| got.push((j.path.strip_prefix(&root).unwrap().to_path_buf(), j.kind)));
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<_> = got.iter().map(|(p, _)| p.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(names, ["py/.venv", "rs/target", "web/node_modules"]);

        // the name rule must still hold when cleaning
        assert_eq!(check_with(&root.join("plain/target"), &root, &none), Err(Reject::NotJunk));
        assert_eq!(check_with(&root.join("rs/target"), &root, &none), Ok(Kind::Target));
        std::fs::remove_file(root.join("rs/Cargo.toml")).unwrap();
        assert_eq!(check_with(&root.join("rs/target"), &root, &none), Err(Reject::NotJunk));
        // inside the scanned root only; a file is no folder
        assert_eq!(check_with(&root.join("web"), &root.join("rs"), &none), Err(Reject::Outside));
        assert_eq!(check_with(&root.join("web/node_modules"), &root.join("web/node_modules"), &none), Err(Reject::Outside));
        std::fs::write(root.join("web/dist"), "").unwrap();
        assert_eq!(check_with(&root.join("web/dist"), &root, &none), Err(Reject::NotAFolder));
        // git tracks files in it: never offered (inside a repository only)
        mk("web/.git");
        assert_eq!(check_with(&root.join("web/node_modules"), &root, &|_| Some(true)), Err(Reject::Tracked));
        assert_eq!(check_with(&root.join("web/node_modules"), &root, &|_| Some(false)), Ok(Kind::NodeModules));
        let _ = std::fs::remove_dir_all(&root);
        committed_node_modules_is_never_offered();
    }

    /// A repository where `node_modules/x.js` is committed: never offered. Skipped quietly without git.
    fn committed_node_modules_is_never_offered() {
        let git = |dir: &Path, args: &[&str]| std::process::Command::new("git").arg("-C").arg(dir).args(args).output().ok().filter(|o| o.status.success());
        let root = std::env::temp_dir().join(format!("leaf-dev-junk-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/node_modules")).unwrap();
        std::fs::create_dir_all(root.join("b/node_modules")).unwrap();
        std::fs::write(root.join("a/node_modules/x.js"), "1").unwrap();
        std::fs::write(root.join("b/node_modules/y.js"), "2").unwrap();
        std::fs::write(root.join(".gitignore"), "b/node_modules/\n").unwrap();
        if git(&root, &["init", "-q"]).is_none() {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let ok = git(&root, &["add", "a", ".gitignore"]).is_some()
            && git(&root, &["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "x"]).is_some();
        assert!(ok);
        assert_eq!(check(&root.join("a/node_modules"), &root), Err(Reject::Tracked));
        assert_eq!(check(&root.join("b/node_modules"), &root), Ok(Kind::NodeModules));
        let mut got = Vec::new();
        scan(&root, &AtomicBool::new(false), &git_tracks, &mut |j| got.push(j.path));
        assert_eq!(got, vec![root.join("b/node_modules")]);
        // git cannot answer: only the git-ignored one is offered
        assert_eq!(check_with(&root.join("a/node_modules"), &root, &|_| None), Err(Reject::Unsure));
        assert_eq!(check_with(&root.join("b/node_modules"), &root, &|_| None), Ok(Kind::NodeModules));
        let _ = std::fs::remove_dir_all(&root);
    }
}
