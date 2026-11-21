//! Linux backend of rf_win::fileinfo. Status: STUB

use leaf_common::{stub, stub_err};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A data operation (it writes attributes): `Err`, and `cancel` is never asked.
pub fn set_attrs_tree(_dir: &Path, _set: u32, _clear: u32, _cancel: &dyn Fn() -> bool) -> std::io::Result<usize> {
    stub_err!("fileinfo::set_attrs_tree")
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
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The Windows-style attribute bits that mean something here: read-only (no write permission bit at all) and hidden (a name
/// starting with a dot). Nothing else is reported.
fn attr_bits(path: &Path, m: &std::fs::Metadata) -> u32 {
    let mut a = 0;
    if m.mode() & 0o222 == 0 {
        a |= 0x1;
    }
    if path.file_name().is_some_and(|n| n.as_encoded_bytes().starts_with(b".")) {
        a |= 0x2;
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

/// No default-application lookup yet: nothing is shown after "Opens with".
pub fn opens_with(_path: &Path) -> Option<String> {
    stub::hit("fileinfo::opens_with");
    None
}

pub fn zone_blocked(_path: &Path) -> bool {
    stub!("fileinfo::zone_blocked")
}

pub fn streams(_path: &Path) -> Vec<(String, u64)> {
    stub!("fileinfo::streams")
}

pub fn mark_downloaded(_path: &Path, _from: &str) -> std::io::Result<()> {
    stub_err!("fileinfo::mark_downloaded")
}

pub fn unblock(_path: &Path) -> std::io::Result<()> {
    stub_err!("fileinfo::unblock")
}

pub const ARCHIVE: u32 = 0x20;

pub const NOT_INDEXED: u32 = 0x2000;

pub const COMPRESSED: u32 = 0x800;

pub const ENCRYPTED: u32 = 0x4000;

pub fn set_attrs(_path: &Path, _set: u32, _clear: u32) -> std::io::Result<()> {
    stub_err!("fileinfo::set_attrs")
}

pub fn set_compressed(_path: &Path, _on: bool) -> std::io::Result<()> {
    stub_err!("fileinfo::set_compressed")
}

pub fn set_encrypted(_path: &Path, _on: bool) -> std::io::Result<()> {
    stub_err!("fileinfo::set_encrypted")
}

pub fn volume_caps(_path: &Path) -> (bool, bool) {
    stub!("fileinfo::volume_caps")
}

pub fn share_of(_dir: &Path) -> Option<String> {
    stub!("fileinfo::share_of")
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
}
