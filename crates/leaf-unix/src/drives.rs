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

/// Bytes read and written since boot on the disk under `path` (`/proc/diskstats`, sectors of 512 bytes, of the block device the mount
/// comes from): `None` for network and virtual mounts and when the device cannot be found. Two readings a while apart give the
/// throughput.
pub fn disk_counters(path: &Path) -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let source = text
        .lines()
        .filter_map(parse_mount_line)
        .filter(|(m, _, s)| s.starts_with("/dev/") && path.starts_with(m))
        .max_by_key(|(m, _, _)| m.len())?
        .2;
    let dev = std::fs::canonicalize(&source).ok()?;
    let name = dev.file_name()?.to_str()?.to_string();
    let stats = std::fs::read_to_string("/proc/diskstats").ok()?;
    stats.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.get(2) != Some(&name.as_str()) {
            return None;
        }
        Some((f.get(5)?.parse::<u64>().ok()? * 512, f.get(9)?.parse::<u64>().ok()? * 512))
    })
}

// ---------------------------------------------------------------- health and temperature

pub use leaf_common::drives::{DriveHealth, Health, HealthSource};

/// Disks have no numbers here: the administrator helper's health request is never needed.
pub fn disk_number(_root: &Path) -> Option<u32> {
    None
}

/// Whether reading wear and power-on hours on this system takes the administrator helper: no, the kernel and udisks2 hand out what
/// they have to every user.
pub const HEALTH_NEEDS_ADMIN: bool = false;

/// Nothing here needs rights, so the helper has nothing of its own to read.
pub fn disk_health_admin(_disk: Option<u32>) -> Vec<DriveHealth> {
    Vec::new()
}

/// The block device the mount under `path` comes from: its kernel name (`nvme0n1p2`) and the sysfs folder of the WHOLE disk.
fn block_of(path: &Path) -> Option<(String, PathBuf)> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let source = text
        .lines()
        .filter_map(parse_mount_line)
        .filter(|(m, _, s)| s.starts_with("/dev/") && path.starts_with(m))
        .max_by_key(|(m, _, _)| m.len())?
        .2;
    let dev = std::fs::canonicalize(&source).ok()?;
    let name = dev.file_name()?.to_str()?.to_string();
    let sys = std::fs::canonicalize(Path::new("/sys/class/block").join(&name)).ok()?;
    // (a partition's folder lies inside its disk's)
    let disk = if sys.join("partition").exists() { sys.parent()?.to_path_buf() } else { sys };
    Some((name, disk))
}

/// Degrees Celsius from a `temp1_input` file (thousandths of a degree).
fn parse_milli_celsius(text: &str) -> Option<i16> {
    let m: i64 = text.trim().parse().ok()?;
    let c = m / 1000;
    (-40..=150).contains(&c).then_some(c as i16)
}

/// The temperature the kernel's hardware monitor reports for a disk: NVMe controllers and SATA disks with the `drivetemp` module have
/// one under their device (`hwmon*/temp1_input`, or `hwmon/hwmon*/temp1_input`). Readable by every user.
fn hwmon_temperature(disk: &Path) -> Option<i16> {
    for dev in [disk.join("device"), disk.join("device/device")] {
        for base in [dev.clone(), dev.join("hwmon")] {
            let Ok(rd) = std::fs::read_dir(&base) else { continue };
            let mut names: Vec<_> = rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("hwmon")).map(|e| e.path()).collect();
            names.sort();
            for h in names {
                if let Some(t) = std::fs::read_to_string(h.join("temp1_input")).ok().as_deref().and_then(parse_milli_celsius) {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// What udisks2 keeps about the drive of the block device `name` (it reads the drive's SMART data itself): state, temperature, power-on
/// hours. Nothing is started: udisks2 is asked only when it already runs, and no SMART update is requested.
fn udisks_health(name: &str) -> Option<DriveHealth> {
    use zbus::blocking::{Proxy, fdo::DBusProxy};
    use zbus::zvariant::OwnedObjectPath;
    // (an object path holds letters, digits and underscores only: `dm-0` and the like have no drive of their own)
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    let bus = crate::bus::system()?;
    let running = DBusProxy::new(bus).ok()?.name_has_owner("org.freedesktop.UDisks2".try_into().ok()?).ok()?;
    if !running {
        return None;
    }
    let dest = "org.freedesktop.UDisks2";
    let block = Proxy::new(bus, dest, format!("/org/freedesktop/UDisks2/block_devices/{name}"), "org.freedesktop.UDisks2.Block").ok()?;
    let drive: OwnedObjectPath = block.get_property("Drive").ok()?;
    let mut h = DriveHealth::unknown(None);
    let kelvin = |k: f64| (k > 1.0).then(|| (k - 273.15).round() as i16).filter(|c| (-40..=150).contains(c));
    let ata = Proxy::new(bus, dest, drive.clone(), "org.freedesktop.UDisks2.Drive.Ata").ok()?;
    if let Ok(updated) = ata.get_property::<u64>("SmartUpdated") {
        // (0 = udisks never read the drive's SMART data: nothing is known)
        if updated > 0 {
            let failing: bool = ata.get_property("SmartFailing").unwrap_or(false);
            h.health = if failing { Health::Failing } else { Health::Good };
            h.temperature_c = ata.get_property::<f64>("SmartTemperature").ok().and_then(kelvin);
            h.power_on_hours = ata.get_property::<u64>("SmartPowerOnSeconds").ok().filter(|s| *s > 0).map(|s| (s / 3600).min(u32::MAX as u64) as u32);
            h.sources.push(HealthSource::Udisks);
        }
        return Some(h);
    }
    let nvme = Proxy::new(bus, dest, drive, "org.freedesktop.UDisks2.NVMe.Controller").ok()?;
    let updated: u64 = nvme.get_property("SmartUpdated").ok()?;
    if updated > 0 {
        let warnings: Vec<String> = nvme.get_property("SmartCriticalWarning").unwrap_or_default();
        h.health = if warnings.iter().any(|w| w == "degraded" || w == "readonly") {
            Health::Failing
        } else if warnings.is_empty() {
            Health::Good
        } else {
            Health::Warning
        };
        h.temperature_c = nvme.get_property::<u16>("SmartTemperature").ok().and_then(|k| kelvin(k as f64));
        h.power_on_hours = nvme.get_property::<u64>("SmartPowerOnHours").ok().map(|n| n.min(u32::MAX as u64) as u32);
        h.sources.push(HealthSource::Udisks);
    }
    Some(h)
}

/// What can be read about the disk under `root` (no rights needed): the kernel's temperature for it, and the health, temperature and
/// power-on hours udisks2 has from the drive's SMART data. `None` when `root` is not on a plain block device (network and virtual
/// mounts). Blocking (a bus call): off the UI thread, and not more than about once a minute.
pub fn disk_health(root: &Path) -> Option<DriveHealth> {
    let (name, disk) = block_of(root)?;
    let mut h = DriveHealth::unknown(None);
    if let Some(t) = hwmon_temperature(&disk) {
        h.temperature_c = Some(t);
        h.sources.push(HealthSource::Hwmon);
    }
    if let Some(u) = udisks_health(&name) {
        h.fill_from(&u);
    }
    Some(h)
}
