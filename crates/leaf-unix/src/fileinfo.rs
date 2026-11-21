//! Linux backend of rf_win::fileinfo. Status: REAL (reads; the attribute writers are plain chmod; the Windows-only extras answer "not applicable")
//!
//! Attribute bits here: READONLY = the owner may not write (writing it clears every write bit; clearing it gives the owner write
//! back); the ARCHIVE bit carries the EXECUTABLE permission of a file (the properties page labels it so off Windows); HIDDEN is a
//! name rule (a leading dot) and cannot be written: asking to change it is an error that says to rename instead; NOT_INDEXED,
//! COMPRESSED and ENCRYPTED have no meaning here and are ignored / refused quietly. Links are never changed (chmod would follow them).

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// `set_attrs` on the files under `dir` (and `dir` itself), links neither changed nor entered. Folders get the read-only and
/// executable bits only when they are `dir` itself (a folder without write or search permission would lock the tree). Returns how
/// many items changed; the first error stops; `cancel` is polled per item and ends the walk early with what was done so far.
pub fn set_attrs_tree(dir: &Path, set: u32, clear: u32, cancel: &dyn Fn() -> bool) -> std::io::Result<usize> {
    if cancel() {
        return Ok(0);
    }
    let mut changed = usize::from(set_attrs_changed(dir, set, clear)?);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        // a folder that cannot be listed (no read permission) is an error as on Windows
        for e in std::fs::read_dir(&d)? {
            if cancel() {
                return Ok(changed);
            }
            let p = e?.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                stack.push(p);
            } else if set_attrs_changed(&p, set, clear)? {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

// TYPE-COPY(rf-win src/fileinfo.rs:18)
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub is_dir: bool,
    pub size: u64,
    pub size_on_disk: Option<u64>,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub accessed_ms: i64,
    pub attrs: u32,
    pub opens_with: Option<String>,
    /// Names the file has (hard links, itself included); 1 for a folder or when not known.
    pub links: u32,
    /// Where a symbolic link points (as written); `None` for anything else.
    pub link_target: Option<std::path::PathBuf>,
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The Windows-style attribute bits that mean something here: read-only (the owner may not write: the same rule as the listing) and hidden (a name
/// starting with a dot). Nothing else is reported.
fn attr_bits(path: &Path, m: &std::fs::Metadata) -> u32 {
    let mut a = 0;
    if m.mode() & 0o200 == 0 {
        a |= 0x1;
    }
    if path.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b".")) {
        a |= 0x2;
    }
    // the ARCHIVE slot is the executable permission of a file
    if !m.is_dir() && m.mode() & 0o111 != 0 {
        a |= 0x20;
    }
    a
}

/// Facts about one item (follows a link to its target, as the Windows version does); blocking: call off the UI thread.
pub fn info(path: &Path) -> std::io::Result<Info> {
    let m = std::fs::metadata(path)?;
    let is_dir = m.is_dir();
    Ok(Info {
        is_dir,
        size: if is_dir { 0 } else { m.len() },
        size_on_disk: if is_dir { None } else { Some(m.blocks() * 512) },
        created_ms: ms(m.created()),
        modified_ms: ms(m.modified()),
        accessed_ms: ms(m.accessed()),
        attrs: attr_bits(path, &m),
        opens_with: None,
        links: if is_dir { 1 } else { m.nlink().min(u32::MAX as u64) as u32 },
        link_target: link_target(path),
    })
}

/// The permission bits as text: `drwxr-xr-x` (type letter, then rwx for owner, group, others; setuid/setgid/sticky as s/S/t/T).
pub fn mode_text(path: &Path) -> Option<String> {
    let m = std::fs::symlink_metadata(path).ok()?;
    let mode = m.mode();
    let kind = match mode & libc::S_IFMT {
        libc::S_IFDIR => 'd',
        libc::S_IFLNK => 'l',
        libc::S_IFCHR => 'c',
        libc::S_IFBLK => 'b',
        libc::S_IFIFO => 'p',
        libc::S_IFSOCK => 's',
        _ => '-',
    };
    let mut out = String::from(kind);
    for (i, (r, w, x)) in [(0o400, 0o200, 0o100), (0o040, 0o020, 0o010), (0o004, 0o002, 0o001)].into_iter().enumerate() {
        out.push(if mode & r != 0 { 'r' } else { '-' });
        out.push(if mode & w != 0 { 'w' } else { '-' });
        let special = [0o4000, 0o2000, 0o1000][i];
        let (on, off) = if i == 2 { ('t', 'T') } else { ('s', 'S') };
        out.push(match (mode & x != 0, mode & special != 0) {
            (true, true) => on,
            (false, true) => off,
            (true, false) => 'x',
            _ => '-',
        });
    }
    Some(out)
}

/// The name of the user that owns the item (the number when no such user is known).
pub fn owner_name(path: &Path) -> Option<String> {
    let uid = std::fs::symlink_metadata(path).ok()?.uid();
    Some(user_name(uid).unwrap_or_else(|| uid.to_string()))
}

/// The name of the item's group (the number when no such group is known).
pub fn group_name(path: &Path) -> Option<String> {
    let gid = std::fs::symlink_metadata(path).ok()?.gid();
    Some(group_name_of(gid).unwrap_or_else(|| gid.to_string()))
}

fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned())
}

fn group_name_of(gid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr().cast(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(gr.gr_name) }.to_string_lossy().into_owned())
}

/// Where a symbolic link points (as written in the link); `None` when the item is not a link.
pub fn link_target(path: &Path) -> Option<PathBuf> {
    std::fs::read_link(path).ok()
}

/// A guess at the media type from the extension alone (no content sniffing); `None` for an unknown extension.
pub fn mime_guess(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "txt" | "log" | "md" | "ini" | "cfg" | "conf" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "xz" => "application/x-xz",
        "bz2" => "application/x-bzip2",
        "zst" => "application/zstd",
        "iso" => "application/x-iso9660-image",
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "c" | "h" => "text/x-c",
        "cpp" | "hpp" | "cc" => "text/x-c++",
        "sh" => "application/x-shellscript",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "ico" => "image/vnd.microsoft.icon",
        "avif" => "image/avif",
        "jxl" => "image/jxl",
        "heic" | "heif" => "image/heic",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

/// The name of the program that opens the file's type by default (from `mimeapps.list` and the desktop entries).
pub fn opens_with(path: &Path) -> Option<String> {
    let mime = crate::openwith::mime_of(path)?;
    crate::openwith::default_app(&mime).map(|a| a.name)
}

/// Not applicable on Linux (no download zone marks): never blocked.
pub fn zone_blocked(_path: &Path) -> bool {
    false
}

/// Not applicable on Linux (no named data streams).
pub fn streams(_path: &Path) -> Vec<(String, u64)> {
    Vec::new()
}

/// Not applicable on Linux: nothing is marked, and that is not an error.
pub fn mark_downloaded(_path: &Path, _from: &str) -> std::io::Result<()> {
    Ok(())
}

/// Not applicable on Linux: there is nothing to unblock.
pub fn unblock(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub const ARCHIVE: u32 = 0x20;

pub const NOT_INDEXED: u32 = 0x2000;

pub const COMPRESSED: u32 = 0x800;

pub const ENCRYPTED: u32 = 0x4000;

/// Sets the bits in `set` and clears those in `clear` (see the module comment for what each means here). A link is left alone.
pub fn set_attrs(path: &Path, set: u32, clear: u32) -> std::io::Result<()> {
    set_attrs_changed(path, set, clear).map(|_| ())
}

fn set_attrs_changed(path: &Path, set: u32, clear: u32) -> std::io::Result<bool> {
    let m = std::fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Ok(false);
    }
    let want = |bit: u32| if set & bit != 0 { Some(true) } else if clear & bit != 0 { Some(false) } else { None };
    let hidden_now = path.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b"."));
    if let Some(h) = want(0x2)
        && h != hidden_now
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Hidden is a name rule on Linux: a name that starts with a dot is hidden. Rename the item to hide or show it.",
        ));
    }
    let mode = m.mode() & 0o7777;
    let mut new = mode;
    if let Some(ro) = want(0x1) {
        new = if ro { new & !0o222 } else { new | 0o200 };
    }
    // the executable bit is for files (a folder's x bit is the permission to enter it)
    if !m.is_dir()
        && let Some(x) = want(0x20)
    {
        new = if x { new | ((new & 0o444) >> 2) } else { new & !0o111 };
    }
    if new == mode {
        return Ok(false);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(new))?;
    Ok(true)
}

/// No compression switch on Linux (the file system decides).
pub fn set_compressed(_path: &Path, _on: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Compression is not a per-file setting on Linux."))
}

/// No per-file encryption switch on Linux.
pub fn set_encrypted(_path: &Path, _on: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Encryption is not a per-file setting on Linux."))
}

/// (compression, encryption) a volume offers per file: neither here (the properties page hides those rows).
pub fn volume_caps(_path: &Path) -> (bool, bool) {
    (false, false)
}

/// The network name a folder is known by: the remote of a network mount (`//server/share`, `host:/export`, `user@host:/path`) when
/// `dir` is the mount point, else the name of a Samba share (user shares and `smb.conf`) that exports it, as `smb://host/name`.
pub fn share_of(dir: &Path) -> Option<String> {
    let want = dir.to_string_lossy().trim_end_matches('/').to_string();
    let want = if want.is_empty() { "/".to_string() } else { want };
    if let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") {
        for line in text.lines() {
            if let Some((mount, fs, source)) = crate::drives::parse_mount_line(line)
                && mount == want
                && crate::drives::is_network_fs(&fs)
            {
                return Some(source);
            }
        }
    }
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_default();
    let found = samba_share(&want)?;
    Some(format!("smb://{host}/{found}"))
}

/// The share name exporting `want`: from `/var/lib/samba/usershares/*` (`path=` and the file's name) and `/etc/samba/smb.conf`
/// (a `[name]` section with a `path =` line); the share's path is compared without a trailing slash.
fn samba_share(want: &str) -> Option<String> {
    let same = |p: &str| p.trim().trim_end_matches('/') == want;
    if let Ok(rd) = std::fs::read_dir("/var/lib/samba/usershares") {
        for e in rd.flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            if t.lines().filter_map(|l| l.strip_prefix("path=")).any(same) {
                return Some(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    let conf = std::fs::read_to_string("/etc/samba/smb.conf").ok()?;
    let mut section = String::new();
    for l in conf.lines() {
        let l = l.trim();
        if let Some(n) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = n.to_string();
        } else if let Some((k, v)) = l.split_once('=')
            && k.trim().eq_ignore_ascii_case("path")
            && same(v)
            && !["global", "homes", "printers", "print$"].contains(&section.as_str())
            && !section.ends_with('$')
        {
            return Some(section);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_mode_link_and_mime() {
        let dir = std::env::temp_dir().join(format!("leaf-finfo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pic.PNG");
        std::fs::write(&f, b"12345").unwrap();
        let i = info(&f).unwrap();
        assert!(!i.is_dir && i.size == 5 && i.modified_ms > 0);
        assert_eq!(mime_guess(&f), Some("image/png"));
        let l = dir.join("l");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        assert_eq!(link_target(&l), Some(f.clone()));
        assert!(mode_text(&f).unwrap().starts_with('-') && mode_text(&dir).unwrap().starts_with('d'));
        assert!(owner_name(&f).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chmod_round_trip_readonly_exec_and_hidden_is_a_name() {
        let dir = std::env::temp_dir().join(format!("leaf-attrs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let f = dir.join("a.sh");
        std::fs::write(&f, b"x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        set_attrs(&f, 0x1 | 0x20, 0).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().mode() & 0o777, 0o555);
        let a = info(&f).unwrap().attrs;
        assert!(a & 0x1 != 0 && a & 0x20 != 0);
        set_attrs(&f, 0, 0x1 | 0x20).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().mode() & 0o777, 0o444 | 0o200);
        // hidden cannot be written, and the error says why
        let e = set_attrs(&f, 0x2, 0).unwrap_err();
        assert!(e.to_string().contains("name"));
        // a tree: files only, cancel honoured
        std::fs::write(dir.join("sub/b.txt"), b"x").unwrap();
        assert_eq!(set_attrs_tree(&dir, 0x1, 0, &|| false).unwrap(), 3);
        assert_eq!(std::fs::metadata(dir.join("sub/b.txt")).unwrap().mode() & 0o222, 0);
        set_attrs_tree(&dir, 0, 0x1, &|| false).unwrap();
        assert_eq!(set_attrs_tree(&dir, 0x1, 0, &|| true).unwrap(), 0);
        assert!(!zone_blocked(&f) && streams(&f).is_empty() && volume_caps(&f) == (false, false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
