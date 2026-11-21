//! Linux backend of rf_win::drives. Status: REAL (mounts from `/proc/self/mountinfo`, without pseudo file systems)
//!
//! No drive letters here: `letter` is always `None`, and the hidden mask is an in-process value that nothing maps to a mount.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub use leaf_common::drives::{Drive, DriveKind};

/// File systems that are not storage (kernel views, containers' overlays, ...): never listed.
const PSEUDO: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "securityfs", "debugfs", "tracefs", "configfs",
    "fusectl", "pstore", "bpf", "mqueue", "hugetlbfs", "autofs", "binfmt_misc", "rpc_pipefs", "nsfs", "efivarfs",
    "overlay", "squashfs", "fuse.portal", "fuse.gvfsd-fuse", "fuse.snapfuse", "ramfs", "selinuxfs", "configfs",
];

const NETWORK: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "sshfs", "fuse.sshfs", "afs", "ceph", "9p", "davfs", "fuse.rclone"];

/// Undo mountinfo's octal escapes (`\040` for a space, ...).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One mountinfo line to (mount point, fs type, source), `None` for a line we cannot read.
pub(crate) fn parse_mount_line(line: &str) -> Option<(String, String, String)> {
    let (left, right) = line.split_once(" - ")?;
    let mount = unescape(left.split(' ').nth(4)?);
    let mut r = right.split(' ');
    let fs = r.next()?.to_string();
    let source = unescape(r.next().unwrap_or(""));
    Some((mount, fs, source))
}

/// Whether a file system type is a network one (nfs, cifs, sshfs, ...).
pub(crate) fn is_network_fs(fs: &str) -> bool {
    NETWORK.contains(&fs)
}

fn kind_of(fs: &str, source: &str, mount: &str) -> DriveKind {
    if NETWORK.contains(&fs) {
        DriveKind::Network
    } else if fs == "iso9660" || fs == "udf" {
        DriveKind::Optical
    } else if fs == "ramfs" {
        DriveKind::RamDisk
    } else if mount.starts_with("/run/media/") || mount.starts_with("/media/") {
        DriveKind::Removable
    } else if source.starts_with("/dev/") {
        DriveKind::Fixed
    } else {
        DriveKind::Unknown
    }
}

fn label_of(mount: &str) -> String {
    if mount == "/" {
        return String::new();
    }
    // removable media are mounted under a folder named after their label
    if mount.starts_with("/run/media/") || mount.starts_with("/media/") {
        return Path::new(mount).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    String::new()
}

fn from_mountinfo(text: &str) -> Vec<Drive> {
    let mut out: Vec<Drive> = Vec::new();
    // the device numbers of the btrfs mounts listed so far
    let mut btrfs_devs: Vec<&str> = Vec::new();
    for line in text.lines() {
        let Some((mount, fs, source)) = parse_mount_line(line) else { continue };
        if PSEUDO.contains(&fs.as_str()) || mount.starts_with("/proc") || mount.starts_with("/sys") || mount.starts_with("/dev")
            || mount.starts_with("/run/user") || mount.starts_with("/var/lib/docker") || mount.starts_with("/snap")
        {
            continue;
        }
        // a bind or repeat mount of the same place is listed once
        if out.iter().any(|d| d.root == Path::new(&mount)) {
            continue;
        }
        // the subvolumes of one btrfs file system (/, /home, /var/log, ...) share its device number: it is one drive
        if fs == "btrfs" {
            let dev = line.split(' ').nth(2).unwrap_or("");
            if btrfs_devs.contains(&dev) {
                continue;
            }
            btrfs_devs.push(dev);
        }
        // the usual boot partitions are not "drives" a person browses
        if mount == "/boot" || mount == "/boot/efi" {
            continue;
        }
        let (total, free) = crate::volume::space(Path::new(&mount)).unwrap_or((0, 0));
        out.push(Drive {
            kind: kind_of(&fs, &source, &mount),
            root: PathBuf::from(&mount),
            letter: None,
            label: label_of(&mount),
            total,
            free,
            fs,
        });
    }
    out
}

/// The mounted file systems, the root first.
pub fn list_drives() -> Vec<Drive> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut v = from_mountinfo(&text);
    v.sort_by_key(|d| d.root != Path::new("/"));
    v.extend(phones(&text));
    v
}

/// Phones and cameras that the desktop exposes as folders: GNOME's gvfs (`/run/user/<uid>/gvfs/mtp:host=...`, `gphoto2:...`) and
/// KDE's kio-fuse (`/run/user/<uid>/kio-fuse-*/mtp/...`, only once a program asked kio-fuse for the device). A phone that KDE reaches
/// only through its own KIO `mtp:/` worker (no kio-fuse installed) has no path and is not listed: leaf would need an MTP client
/// (libmtp) or kio-fuse for it. Their space is not asked (a phone answers slowly), so no bar.
fn phones(mountinfo: &str) -> Vec<Drive> {
    let mut out = Vec::new();
    for line in mountinfo.lines() {
        let Some((mount, fs, _)) = parse_mount_line(line) else { continue };
        let dirs: Vec<PathBuf> = match fs.as_str() {
            "fuse.gvfsd-fuse" => read_dirs(Path::new(&mount)).into_iter().filter(|p| is_device_name(p)).collect(),
            "fuse.kio-fuse" => read_dirs(&Path::new(&mount).join("mtp")),
            _ => continue,
        };
        for root in dirs {
            let label = root.file_name().map(|n| device_label(&n.to_string_lossy())).unwrap_or_default();
            out.push(Drive { kind: DriveKind::Removable, root, letter: None, label, total: 0, free: 0, fs: fs.clone() });
        }
    }
    out
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

fn is_device_name(p: &Path) -> bool {
    p.file_name().is_some_and(|n| {
        let n = n.to_string_lossy();
        n.starts_with("mtp:") || n.starts_with("gphoto2:") || n.starts_with("afc:")
    })
}

/// "mtp:host=SAMSUNG_Android_R58M" -> "SAMSUNG Android R58M"; a kio-fuse folder name is kept.
fn device_label(name: &str) -> String {
    let v = name.split_once("host=").map_or(name, |(_, h)| h);
    v.replace('_', " ").replace("%20", " ")
}

// No letters on Linux: the mask functions are inert, so settings that store a mask keep working.
pub fn letter_bit(_letter: char) -> Option<u32> {
    None
}

pub fn mask_of_letters<S: AsRef<str>>(_letters: &[S]) -> u32 {
    0
}

pub fn letters_of_mask(_mask: u32) -> Vec<char> {
    Vec::new()
}

static HIDDEN: AtomicU32 = AtomicU32::new(0);

pub fn set_hidden(mask: u32) {
    HIDDEN.store(mask, Ordering::Relaxed);
}

pub fn hidden() -> u32 {
    HIDDEN.load(Ordering::Relaxed)
}

pub fn policy_hidden() -> u32 {
    0
}

pub fn visible_drives() -> Vec<Drive> {
    list_drives()
}

/// Network and optical mounts are slow to ask.
pub fn slow_to_read(path: &Path) -> bool {
    list_drives()
        .iter()
        .filter(|d| path.starts_with(&d.root))
        .max_by_key(|d| d.root.as_os_str().len())
        .is_some_and(|d| matches!(d.kind, DriveKind::Network | DriveKind::Optical))
}

pub fn drive_mask() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_lines_are_read_and_pseudo_systems_skipped() {
        let t = "26 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
                 30 26 0:25 / /proc rw - proc proc rw\n\
                 40 26 8:3 / /run/media/me/My\\040Disk rw - vfat /dev/sdb1 rw\n\
                 41 26 0:50 / /run/user/1000 rw - tmpfs tmpfs rw\n\
                 42 26 0:36 /@home /home rw - btrfs /dev/sda2 rw,subvol=/@home\n\
                 43 26 0:36 /@log /var/log rw - btrfs /dev/sda2 rw,subvol=/@log\n";
        let d = from_mountinfo(t);
        assert_eq!(d.len(), 3, "the two subvolumes of one btrfs are one drive (the first)");
        assert_eq!(d[0].root, Path::new("/"));
        assert_eq!(d[1].root, Path::new("/run/media/me/My Disk"));
        assert_eq!(d[1].label, "My Disk");
        assert_eq!(d[1].kind, DriveKind::Removable);
        assert_eq!(d[2].root, Path::new("/home"));
        assert_eq!(d[2].fs, "btrfs");
    }
}


/// Whether the disk under `path` is a spinning one (`Some(true)`), from the kernel's `queue/rotational` of the block device the
/// mount comes from (a partition asks its whole disk); `None` for network and virtual mounts and when the device cannot be found.
pub fn seek_penalty(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let source = text
        .lines()
        .filter_map(parse_mount_line)
        .filter(|(m, _, s)| s.starts_with("/dev/") && path.starts_with(m))
        .max_by_key(|(m, _, _)| m.len())?
        .2;
    let dev = std::fs::canonicalize(&source).ok()?;
    let sys = std::fs::canonicalize(Path::new("/sys/class/block").join(dev.file_name()?)).ok()?;
    let read = |d: &Path| std::fs::read_to_string(d.join("queue/rotational")).ok().map(|t| t.trim() == "1");
    read(&sys).or_else(|| read(sys.parent()?))
}
