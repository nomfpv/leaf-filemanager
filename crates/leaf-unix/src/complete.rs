//! Linux backend of rf_win::complete: address-bar path completion. Folders whose name starts with what the user typed after the
//! last `/` (case-insensitive: it is a suggestion list, not a lookup), one directory read, names only, bounded. Blocking: call off the
//! UI thread.
//!
//! `~` and `~/...` stand for the home folder. A folder whose name starts with a dot is offered only when the typed part starts with a
//! dot too (or the caller asks for hidden ones).

use std::path::PathBuf;

/// `~` / `~/x` with the home folder in place; other text as it is.
fn expand_tilde(input: &str) -> String {
    let home = || dirs::home_dir().map(|h| h.to_string_lossy().trim_end_matches('/').to_string());
    if input == "~" {
        if let Some(h) = home() {
            return format!("{h}/");
        }
    } else if let Some(rest) = input.strip_prefix("~/")
        && let Some(h) = home()
    {
        return format!("{h}/{rest}");
    }
    input.to_string()
}

/// Splits `/home/user/Do` into (`/home/user/`, `Do`). `None` when there is no directory part.
fn split(input: &str) -> Option<(PathBuf, String)> {
    let cut = input.rfind('/')?;
    let (dir, rest) = input.split_at(cut + 1);
    Some((PathBuf::from(dir), rest.to_string()))
}

/// Folder completions for `input` (case-insensitive prefix), sorted naturally, hidden folders only when the typed part starts with a
/// dot or `include_hidden`. Returns full paths.
pub fn complete_dirs(input: &str, limit: usize, include_hidden: bool) -> Vec<PathBuf> {
    let input = input.trim().trim_matches('"');
    let input = expand_tilde(input);
    let Some((dir, prefix)) = split(&input) else { return Vec::new() };
    if !dir.is_absolute() {
        return Vec::new();
    }
    let show_hidden = include_hidden || prefix.starts_with('.');
    let prefix = prefix.to_lowercase();
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if !name.to_lowercase().starts_with(&prefix) {
            continue;
        }
        // a link to a folder is a folder
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir {
            out.push(dir.join(&name));
            if out.len() >= limit * 4 {
                break;
            }
        }
    }
    out.sort_by(|a, b| {
        rf_core::sort::natural_cmp(&a.file_name().unwrap_or_default().to_string_lossy(), &b.file_name().unwrap_or_default().to_string_lossy())
    });
    out.truncate(limit);
    out
}

/// True if `p` looks like something the completion can work on (absolute, or the home folder's `~`).
pub fn completable(p: &str) -> bool {
    let p = p.trim().trim_matches('"');
    p.starts_with('/') || p == "~" || p.starts_with("~/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_folders_by_prefix_and_keeps_hidden_ones_for_a_dot() {
        let root = std::env::temp_dir().join(format!("leaf-complete-{}", std::process::id()));
        for d in ["Docs", "dev", ".config", "other"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("deploy.txt"), b"").unwrap();
        let r = complete_dirs(&format!("{}/d", root.display()), 10, false);
        assert_eq!(r, vec![root.join("dev"), root.join("Docs")], "case-insensitive, folders only, natural order");
        assert!(complete_dirs(&format!("{}/", root.display()), 10, false).iter().all(|p| !p.ends_with(".config")));
        assert_eq!(complete_dirs(&format!("{}/.", root.display()), 10, false), vec![root.join(".config")]);
        assert!(complete_dirs("relative/x", 5, false).is_empty());
        assert!(completable("~/x") && completable("/usr") && !completable("usr"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
