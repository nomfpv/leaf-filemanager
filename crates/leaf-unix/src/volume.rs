//! Linux backend of rf_win::volume. Status: REAL for `volume_info` (statvfs, mountinfo); `set_label` stays a stub

use leaf_common::stub_err;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

// TYPE-COPY(rf-win src/volume.rs:17)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub root: PathBuf,
    pub label: String,
    pub filesystem: String,
    pub serial: u32,
    pub total: u64,
    pub free: u64,
    pub drive_type: String,
}

/// (total, free for an ordinary user) in bytes of the file system holding `path`, through statvfs.
pub(crate) fn space(path: &Path) -> std::io::Result<(u64, u64)> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let unit = s.f_frsize as u64;
    Ok((s.f_blocks as u64 * unit, s.f_bavail as u64 * unit))
}

pub fn volume_info(root: &Path) -> anyhow::Result<VolumeInfo> {
    let (total, free) = space(root)?;
    // the mount that holds `root`: the longest mount point that is a prefix
    let drives = crate::drives::list_drives();
    let d = drives.iter().filter(|d| root.starts_with(&d.root)).max_by_key(|d| d.root.as_os_str().len());
    let (label, filesystem, drive_type) = match d {
        Some(d) => (
            d.label.clone(),
            d.fs.clone(),
            match d.kind {
                crate::drives::DriveKind::Removable => "Removable Drive",
                crate::drives::DriveKind::Network => "Network Drive",
                crate::drives::DriveKind::Optical => "Optical Drive (CD/DVD)",
                crate::drives::DriveKind::RamDisk => "RAM Disk",
                _ => "Local Disk",
            }
            .to_string(),
        ),
        None => (String::new(), String::new(), "Local Disk".to_string()),
    };
    Ok(VolumeInfo { root: root.to_path_buf(), label, filesystem, serial: 0, total, free, drive_type })
}

pub fn set_label(_root: &Path, _label: &str) -> std::io::Result<()> {
    stub_err!("volume::set_label")
}
