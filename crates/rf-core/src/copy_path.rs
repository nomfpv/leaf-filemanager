//! "Copy path as": the text variants of an item's path (name, folder, forward slashes, repository
//! relative, `@path`, WSL, file URL, quoted). Pure string logic, so every platform shares it; only
//! `repo_root` touches the disk.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyPathKind {
    Full,
    Name,
    Folder,
    Slashes,
    Relative,
    At,
    Wsl,
    Url,
    Quoted,
}

/// Nearest parent (or the path itself) holding a `.git` entry.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

fn fwd(s: &str) -> String {
    s.replace('\\', "/")
}

/// Length of the root part of a forward-slash path ("C:/", "/", "//server/share/"), without the trailing slash
/// for drive and posix roots handled by the callers.
fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn trim(s: &str) -> &str {
    let t = s.trim_end_matches('/');
    if t.is_empty() { s } else { t }
}

fn name_of(p: &str) -> String {
    let f = fwd(p);
    let t = trim(&f);
    t.rsplit('/').next().unwrap_or(t).to_string()
}

fn folder_of(p: &str) -> String {
    let sep = if p.contains('\\') { '\\' } else { '/' };
    let f = fwd(p);
    let t = trim(&f);
    let out = match t.rfind('/') {
        None => t.to_string(),
        Some(0) => "/".to_string(),
        Some(i) if is_drive(t) && i == 2 => format!("{}/", &t[..2]),
        Some(i) => t[..i].to_string(),
    };
    // drive root keeps its own form ("C:" -> "C:\")
    let out = if is_drive(&out) && out.len() == 2 { format!("{out}/") } else { out };
    out.replace('/', &sep.to_string())
}

/// `path` below `root`, forward slashes ("." for the root itself); None when not inside it.
fn relative(path: &str, root: &str) -> Option<String> {
    let p = fwd(path);
    let r = fwd(root);
    let (p, r) = (trim(&p), trim(&r));
    let win = is_drive(p) || p.starts_with("//");
    let eq = |a: &str, b: &str| if win { a.eq_ignore_ascii_case(b) } else { a == b };
    if p.len() == r.len() && eq(p, r) {
        return Some(".".into());
    }
    if p.len() > r.len() && p.is_char_boundary(r.len()) && eq(&p[..r.len()], r) {
        let rest = &p[r.len()..];
        if let Some(rest) = rest.strip_prefix('/') {
            return Some(rest.to_string());
        }
        if r.ends_with('/') {
            return Some(rest.to_string());
        }
    }
    None
}

fn wsl(path: &str) -> Option<String> {
    let f = fwd(path);
    let t = trim(&f);
    let lower = t.to_ascii_lowercase();
    for pre in ["//wsl.localhost/", "//wsl$/"] {
        if lower.starts_with(pre) {
            let rest = &t[pre.len()..];
            return Some(match rest.find('/') {
                Some(i) => rest[i..].to_string(),
                None => "/".into(),
            });
        }
    }
    if is_drive(t) {
        let drive = t[..1].to_ascii_lowercase();
        let rest = &t[2..];
        return Some(format!("/mnt/{drive}{rest}"));
    }
    if t.starts_with('/') && !t.starts_with("//") {
        return Some(t.to_string()); // already a posix path (Linux)
    }
    None
}

fn pct(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:!$&'()*+,;=@".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

fn url(path: &str) -> String {
    let f = fwd(path);
    if let Some(rest) = f.strip_prefix("//") {
        format!("file://{}", pct(rest)) // UNC: the server becomes the host
    } else if is_drive(&f) {
        format!("file:///{}", pct(&f))
    } else {
        format!("file://{}", pct(&f))
    }
}

/// One variant of one path. `root` is the repository root of that path, if any. None when the variant does not
/// apply (relative / `@` never; WSL for a non-WSL network path; relative outside the repository).
pub fn format_path(kind: CopyPathKind, path: &str, root: Option<&str>) -> Option<String> {
    Some(match kind {
        CopyPathKind::Full => path.to_string(),
        CopyPathKind::Name => name_of(path),
        CopyPathKind::Folder => folder_of(path),
        CopyPathKind::Slashes => fwd(path),
        CopyPathKind::Relative => relative(path, root?)?,
        CopyPathKind::At => match root.and_then(|r| relative(path, r)) {
            Some(r) => format!("@{r}"),
            None => format!("@{}", fwd(path)),
        },
        CopyPathKind::Wsl => wsl(path)?,
        CopyPathKind::Url => url(path),
        CopyPathKind::Quoted => format!("\"{path}\""),
    })
}

/// The text for a selection, one path per line; None when the variant fits none of them.
pub fn format_all(kind: CopyPathKind, items: &[(String, Option<String>)], eol: &str) -> Option<String> {
    let lines: Vec<String> = items.iter().filter_map(|(p, r)| format_path(kind, p, r.as_deref())).collect();
    if lines.is_empty() { None } else { Some(lines.join(eol)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CopyPathKind::*;

    #[test]
    fn variants() {
        let p = r"C:\Users\Me\My Repo\src\a b.txt";
        let root = Some(r"C:\Users\Me\My Repo");
        let f = |k, p, r| format_path(k, p, r);
        assert_eq!(f(Full, p, root).unwrap(), p);
        assert_eq!(f(Name, p, root).unwrap(), "a b.txt");
        assert_eq!(f(Folder, p, root).unwrap(), r"C:\Users\Me\My Repo\src");
        assert_eq!(f(Slashes, p, root).unwrap(), "C:/Users/Me/My Repo/src/a b.txt");
        assert_eq!(f(Relative, p, root).unwrap(), "src/a b.txt");
        assert_eq!(f(Relative, p, None), None);
        assert_eq!(f(At, p, root).unwrap(), "@src/a b.txt");
        assert_eq!(f(At, p, None).unwrap(), "@C:/Users/Me/My Repo/src/a b.txt");
        assert_eq!(f(Wsl, p, root).unwrap(), "/mnt/c/Users/Me/My Repo/src/a b.txt");
        assert_eq!(f(Url, p, root).unwrap(), "file:///C:/Users/Me/My%20Repo/src/a%20b.txt");
        assert_eq!(f(Quoted, p, root).unwrap(), format!("\"{p}\""));
        // UNC into WSL, and a plain UNC share
        let u = r"\\wsl.localhost\Ubuntu\home\u\x y";
        assert_eq!(f(Wsl, u, None).unwrap(), "/home/u/x y");
        assert_eq!(f(Wsl, r"\\wsl.localhost\Ubuntu", None).unwrap(), "/");
        assert_eq!(f(Wsl, r"\\srv\share\a", None), None);
        assert_eq!(f(Url, r"\\srv\share\a b", None).unwrap(), "file://srv/share/a%20b");
        assert_eq!(f(Folder, u, None).unwrap(), r"\\wsl.localhost\Ubuntu\home\u");
        // drive root
        assert_eq!(f(Name, r"C:\", None).unwrap(), "C:");
        assert_eq!(f(Folder, r"C:\", None).unwrap(), r"C:\");
        assert_eq!(f(Folder, r"C:\x", None).unwrap(), r"C:\");
        assert_eq!(f(Wsl, r"C:\", None).unwrap(), "/mnt/c");
        assert_eq!(f(Url, r"C:\", None).unwrap(), "file:///C:/");
        assert_eq!(f(Relative, r"c:\users\me\my repo", root).unwrap(), ".");
        // several items: one per line
        let items = vec![(p.to_string(), None), (r"C:\b".to_string(), None)];
        assert_eq!(format_all(Name, &items, "\n").unwrap(), "a b.txt\nb");
    }
}
