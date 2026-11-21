//! The one Windows string helper the shared modules need (a copy of rf-win's `wide::long_path`; rf-win keeps its own).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

/// Adds the `\\?\` prefix so paths longer than MAX_PATH work. With that prefix Windows
/// takes the path literally, so `/` separators are turned into backslashes first.
pub fn long_path(p: &Path) -> Vec<u16> {
    let s = p.as_os_str();
    let mut raw: Vec<u16> = s.encode_wide().collect();
    for c in raw.iter_mut() {
        if *c == u16::from(b'/') {
            *c = u16::from(b'\\');
        }
    }
    let has_prefix = raw.starts_with(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]);
    let is_unc = raw.starts_with(&[b'\\' as u16, b'\\' as u16]);
    let mut out: Vec<u16> = Vec::with_capacity(raw.len() + 9);
    if !has_prefix {
        if is_unc {
            out.extend(r"\\?\UNC".encode_utf16());
            out.extend_from_slice(&raw[1..]); // keep one leading backslash
        } else {
            out.extend(r"\\?\".encode_utf16());
            out.extend_from_slice(&raw);
        }
    } else {
        out.extend_from_slice(&raw);
    }
    out.push(0);
    out
}
