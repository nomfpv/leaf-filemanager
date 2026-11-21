//! Path identity that is right on both systems. Windows paths compare without case and
//! with either slash; on Linux a path is exact and only `/` separates. These helpers are
//! the one place that says so, so no call site needs its own `cfg`.

use std::path::Path;

/// The separator this system writes.
pub const SEP: char = std::path::MAIN_SEPARATOR;

pub fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// Text folded the way this system's file names compare: lowercase on Windows, as is on Linux.
pub fn fold(s: &str) -> String {
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// ASCII-only fold, the same rule.
pub fn fold_ascii(s: &str) -> String {
    if cfg!(windows) { s.to_ascii_lowercase() } else { s.to_string() }
}

/// Whether two file or folder names are the same name here.
pub fn name_eq(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// A path as the key two spellings of one place share.
pub fn path_key(p: &Path) -> String {
    fold(&p.to_string_lossy())
}

/// Like `path_key`, without trailing separators.
pub fn path_key_trimmed(p: &Path) -> String {
    fold(trim_seps(&p.to_string_lossy()))
}

/// `s` without trailing separators of this system.
pub fn trim_seps(s: &str) -> &str {
    s.trim_end_matches(is_sep)
}

/// Whether two paths are the same place by spelling.
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key_trimmed(a) == path_key_trimmed(b)
}

/// Whether two names differ only in case (a case-only rename). Never on Linux: there
/// `a.txt` and `A.txt` are two files.
pub fn differ_only_in_case(a: &str, b: &str) -> bool {
    cfg!(windows) && a != b && a.to_lowercase() == b.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_system() {
        let a = Path::new("/Work/Pics/");
        let b = Path::new("/work/pics");
        assert_eq!(same_path(a, b), cfg!(windows));
        assert!(same_path(a, Path::new("/Work/Pics")));
        assert_eq!(differ_only_in_case("A.txt", "a.txt"), cfg!(windows));
        assert!(!name_eq("A", "B"));
        assert_eq!(trim_seps("/x//"), "/x");
    }
}
