//! Names that come from somewhere else (a server's listing, an archive) before they become part of a path. A hostile server
//! chooses its names; none of them may lead out of the folder a copy, a download or a cache goes into.
//!
//! - [`address_problem`]: the name cannot be one part of an address at all (empty, `.`, `..`, a `/` or `\`, NUL). `VPath::join`
//!   would read such a name as more parts or as "go up", so a listing leaves these out.
//! - [`local_problem`]: the name cannot be a file name on this PC without changing its meaning: the above, plus control
//!   characters, a colon (`C:` turns a join into another drive, `a:b` names a stream), over-long names, and on Windows the
//!   characters `* ? " < > |`, the device names (`CON`, `NUL`, `COM1`, `LPT1`..., with or without an extension) and a final dot or
//!   space (Windows drops them, so `a.` would land on `a`).
//! - [`local_child`] / [`vchild`]: the checked join, which also verifies that the result's parent IS the folder.
//! - [`safe_local_name`]: a usable local name made from any name (for cache files, which also carry a hash).

use std::path::{Component, Path, PathBuf};

use crate::VPath;

/// The longest name kept (NTFS and most Unix file systems stop at 255 units).
pub const MAX_NAME: usize = 255;

/// Why `name` cannot be one part of an address, or `None` when it can.
pub fn address_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("an empty name")
    } else if name == "." || name == ".." {
        Some("the name . or ..")
    } else if name.contains(['/', '\\']) {
        Some("a / or \\ in the name")
    } else if name.contains('\0') {
        Some("a NUL character in the name")
    } else {
        None
    }
}

/// Why `name` cannot be a file name on this PC, or `None` when it can.
pub fn local_problem(name: &str) -> Option<&'static str> {
    if let Some(p) = address_problem(name) {
        return Some(p);
    }
    if name.chars().any(char::is_control) {
        return Some("a control character in the name");
    }
    if name.contains(':') {
        return Some("a colon in the name (a drive or a stream)");
    }
    let units = if cfg!(windows) { name.encode_utf16().count() } else { name.len() };
    if units > MAX_NAME {
        return Some("the name is too long");
    }
    if cfg!(windows) {
        if name.contains(['*', '?', '"', '<', '>', '|']) {
            return Some("a character Windows does not allow in names");
        }
        if name.ends_with(['.', ' ']) {
            return Some("the name ends with a dot or a space");
        }
        if is_device_name(name) {
            return Some("a Windows device name (CON, NUL, COM1...)");
        }
    }
    None
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9` (also with the superscript digits), `CONIN$`, `CONOUT$`: with or
/// without an extension, in any case, with spaces before the dot.
fn is_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        s => {
            let mut c = s.chars();
            let head: String = c.by_ref().take(3).collect();
            let rest: Vec<char> = c.collect();
            (head == "COM" || head == "LPT") && rest.len() == 1 && (rest[0].is_ascii_digit() || matches!(rest[0], '¹' | '²' | '³'))
        }
    }
}

/// `dir` joined with `name`, refused unless `name` is a usable local name and the result is exactly one level below `dir`.
pub fn local_child(dir: &Path, name: &str) -> Result<PathBuf, &'static str> {
    if let Some(p) = local_problem(name) {
        return Err(p);
    }
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(n)), None) if n == name => {}
        _ => return Err("the name is not one plain part of a path"),
    }
    let child = dir.join(name);
    if child.parent() != Some(dir) || child.file_name().is_none_or(|n| n != name) {
        return Err("the name leads out of the folder");
    }
    Ok(child)
}

/// The same check for an address: `dir.join(name)` only when its parent is `dir` and its name is `name` again.
pub fn vchild(dir: &VPath, name: &str) -> Result<VPath, &'static str> {
    if let Some(p) = local_problem(name) {
        return Err(p);
    }
    let child = dir.join(name);
    if child.parent().as_ref() != Some(dir) || child.name() != name {
        return Err("the name leads out of the folder");
    }
    Ok(child)
}

/// A local file name made from any name: characters that are not allowed become `_`, a final dot or space is dropped, a
/// device name gets a `_` in front, and a long name is cut (keeping a short extension). Never empty, `.` or `..`.
pub fn safe_local_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    if s.is_empty() || s.chars().all(|c| c == '.') {
        s = "file".into();
    }
    if is_device_name(&s) {
        s.insert(0, '_');
    }
    if s.encode_utf16().count() > 120 {
        let ext: String = match s.rfind('.') {
            Some(i) if s.len() - i <= 16 => s[i..].to_string(),
            _ => String::new(),
        };
        let mut stem: String = s.chars().take(100).collect();
        while stem.ends_with(['.', ' ']) {
            stem.pop();
        }
        s = format!("{stem}{ext}");
    }
    debug_assert!(local_problem(&s).is_none(), "{s:?}");
    s
}
