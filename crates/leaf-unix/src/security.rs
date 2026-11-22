//! Linux backend of rf_win::security. Status: REAL for what Linux has (owner, group, the mode; chmod; `getfacl` text when the tool
//! exists). There is no Windows-style ACL list here: `read` answers the owner and an empty list, and the app shows the mode grid of
//! `unix_info` instead (the pure part of it is `leaf_common::perms`).

use anyhow::{Result, anyhow};
use leaf_common::perms::Mode;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

// TYPE-COPY(rf-win src/security.rs:33)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Security {
    pub owner: Option<String>,
    pub entries: Vec<Entry>,
    pub null_dacl: bool,
    pub protected: bool,
}

pub fn read(path: &Path) -> Result<Security> {
    let m = std::fs::symlink_metadata(path)?;
    Ok(Security { owner: Some(user_name(m.uid())), ..Default::default() })
}

/// What the Security tab shows on Linux.
#[derive(Debug, Clone, PartialEq)]
pub struct UnixInfo {
    pub owner: String,
    pub group: String,
    pub mode: Mode,
    pub is_dir: bool,
    pub is_link: bool,
    /// The user running leaf may change the mode (owner or root).
    pub can_change: bool,
}

fn user_name(uid: u32) -> String {
    let mut buf = vec![0u8; 4096];
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
    if rc == 0 && !out.is_null() {
        unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().into_owned()
    } else {
        uid.to_string()
    }
}

fn group_name(gid: u32) -> String {
    let mut buf = vec![0u8; 4096];
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
    if rc == 0 && !out.is_null() {
        unsafe { std::ffi::CStr::from_ptr(gr.gr_name) }.to_string_lossy().into_owned()
    } else {
        gid.to_string()
    }
}

/// Owner, group and mode of `path` (a link answers its own, not its target's).
pub fn unix_info(path: &Path) -> Result<UnixInfo> {
    let m = std::fs::symlink_metadata(path)?;
    let euid = unsafe { libc::geteuid() };
    Ok(UnixInfo {
        owner: user_name(m.uid()),
        group: group_name(m.gid()),
        mode: Mode::new(m.mode()),
        is_dir: m.is_dir(),
        is_link: m.file_type().is_symlink(),
        can_change: euid == 0 || euid == m.uid(),
    })
}

fn not_yours(path: &Path) -> anyhow::Error {
    anyhow!("Only the owner of \"{}\" can change its permissions.", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

fn chmod_one(path: &Path, mode: Mode) -> Result<bool> {
    let m = std::fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Ok(false);
    }
    let euid = unsafe { libc::geteuid() };
    if euid != 0 && euid != m.uid() {
        return Err(not_yours(path));
    }
    if m.mode() & 0o7777 == mode.0 {
        return Ok(false);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode.0)).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Sets the mode of one item (a link is left alone: chmod would follow it). Only the owner (or root) may; the error text says so.
pub fn chmod(path: &Path, mode: Mode) -> Result<()> {
    chmod_one(path, mode).map(|_| ())
}

/// `chmod` on `dir` and everything under it. Links are neither changed nor entered. The special bits go to `dir` only, and the contents
/// get the nine read/write/execute bits. The first error stops; `cancel` is polled per item and ends the walk with what was done.
/// Returns how many items changed.
pub fn chmod_tree(dir: &Path, mode: Mode, cancel: &dyn Fn() -> bool) -> Result<usize> {
    if cancel() {
        return Ok(0);
    }
    let mut changed = usize::from(chmod_one(dir, mode)?);
    let inner = Mode::new(mode.0 & 0o777);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| anyhow!("{}: {e}", d.display()))? {
            if cancel() {
                return Ok(changed);
            }
            let p = e?.path();
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                stack.push(p.clone());
            }
            if chmod_one(&p, inner)? {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// The output of `getfacl` for `path` when that tool is installed (a few lines of text); `None` when it is not, or has nothing to say.
pub fn acl_text(path: &Path) -> Option<String> {
    use std::process::Command;
    let mut cmd = Command::new("getfacl");
    cmd.arg("-p").arg("--").arg(path);
    // (a time limit, and what getfacl prints is read whole: a few lines)
    let o = leaf_proc::output_helper(&mut cmd, Some(std::time::Duration::from_secs(10))).ok()?;
    let out = String::from_utf8_lossy(&o.stdout[..o.stdout.len().min(64 * 1024)]).into_owned();
    let ok = o.status.success();
    let t: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
    (ok && !t.is_empty()).then(|| t.join("\n"))
}

// TYPE-COPY(rf-win src/security.rs:186)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Right {
    Full,
    Modify,
    ReadExecute,
    List,
    Read,
    Write,
    Special,
}

pub const RIGHTS: [Right; 7] = [
    Right::Full,
    Right::Modify,
    Right::ReadExecute,
    Right::List,
    Right::Read,
    Right::Write,
    Right::Special,
];

/// No Windows access masks here: nothing is ticked.
pub fn ticks(_mask: u32, _is_dir: bool) -> Vec<Right> {
    Vec::new()
}

// TYPE-COPY(rf-win security.rs:21)
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub sid: String,
    pub allow: u32,
    pub deny: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chmod_changes_own_files_and_trees_but_not_links() {
        let dir = std::env::temp_dir().join(format!("leaf-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("sub/b.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("a.txt"), dir.join("link")).unwrap();
        chmod(&dir.join("a.txt"), Mode::new(0o600)).unwrap();
        assert_eq!(unix_info(&dir.join("a.txt")).unwrap().mode.octal(), "600");
        assert!(unix_info(&dir.join("a.txt")).unwrap().can_change);
        let n = chmod_tree(&dir, Mode::new(0o755), &|| false).unwrap();
        assert!(n >= 2, "{n}");
        assert_eq!(std::fs::metadata(dir.join("sub/b.txt")).unwrap().mode() & 0o777, 0o755);
        assert!(unix_info(&dir.join("link")).unwrap().is_link);
        assert_eq!(chmod_tree(&dir, Mode::new(0o700), &|| true).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
