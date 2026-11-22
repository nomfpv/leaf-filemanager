//! Linux backend of rf_win::removal: "safely remove".
//!
//! Windows asks the app to let go of a drive before it is ejected ([`start`]); nothing on Linux asks (a busy mount refuses to go and
//! says so), so [`start`] has nobody to call back. What the user needs here is the other half, [`unmount`]: the volume is
//! unmounted through `udisksctl` (a block device) or `gio mount -u` (a network or gvfs mount), whichever is installed, and with
//! `eject` a removable disk is powered off afterwards. It is never forced: when something still uses the volume the tool's own words
//! come back as the error, to be shown as they are.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

/// Nothing asks the app to release a volume on Linux, so `on_query_remove` is never called.
pub fn start(_on_query_remove: impl Fn(char, Sender<()>) + Send + 'static) {}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// The device a mount point is made from (`/dev/sdb1`), from the text of `/proc/self/mountinfo`; the last mount at that point wins.
fn device_of(mountinfo: &str, mount_point: &Path) -> Option<String> {
    mountinfo
        .lines()
        .filter_map(crate::drives::parse_mount_line)
        .filter(|(m, _, _)| Path::new(m) == mount_point)
        .map(|(_, _, source)| source)
        .next_back()
}

/// The whole disk a partition lives on (`/dev/sdb1` -> `/dev/sdb`, `/dev/nvme0n1p2` -> `/dev/nvme0n1`) when it is removable
/// (`/sys/block/<disk>/removable` is 1); None for anything else (a fixed disk is never powered off).
fn removable_parent(dev: &str) -> Option<String> {
    let name = dev.strip_prefix("/dev/")?;
    let part = std::fs::canonicalize(format!("/sys/class/block/{name}")).ok()?;
    if !part.join("partition").exists() {
        return None;
    }
    let disk = part.parent()?.file_name()?.to_string_lossy().into_owned();
    (std::fs::read_to_string(format!("/sys/block/{disk}/removable")).ok()?.trim() == "1").then(|| format!("/dev/{disk}"))
}

fn run(mut c: Command) -> Result<(), String> {
    // (a flush of written data can take a while: five minutes at most)
    let out = leaf_proc::output_helper(&mut c, Some(std::time::Duration::from_secs(300))).map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
    Err(if text.is_empty() { format!("{} failed ({})", c.get_program().to_string_lossy(), out.status) } else { text })
}

/// Unmounts the volume mounted at `mount_point`; with `eject` a removable disk is powered off after that. Blocking (a flush of
/// written data can take a while): call from a worker thread. Never forced.
pub fn unmount(mount_point: &Path, eject: bool) -> Result<(), String> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| e.to_string())?;
    let dev = device_of(&info, mount_point).ok_or_else(|| format!("{} is not a mount point", mount_point.display()))?;
    if dev.starts_with("/dev/")
        && let Some(ud) = which("udisksctl")
    {
        let mut c = Command::new(&ud);
        c.args(["unmount", "-b", &dev]);
        run(c)?;
        if eject && let Some(disk) = removable_parent(&dev) {
            let mut c = Command::new(&ud);
            c.args(["power-off", "-b", &disk]);
            run(c)?;
        }
        return Ok(());
    }
    if let Some(gio) = which("gio") {
        let mut c = Command::new(gio);
        c.args(["mount", if eject { "-e" } else { "-u" }]).arg(mount_point);
        return run(c);
    }
    Err("neither udisksctl nor gio is installed: unmount it from a terminal".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_of_a_mount_point_is_read_from_mountinfo() {
        let t = "36 25 8:17 / /run/media/neo/My\\040Stick rw,nosuid shared:1 - vfat /dev/sdb1 rw\n\
                 40 25 0:40 / /mnt/nas rw - cifs //nas/share rw\n";
        assert_eq!(device_of(t, Path::new("/run/media/neo/My Stick")).as_deref(), Some("/dev/sdb1"));
        assert_eq!(device_of(t, Path::new("/mnt/nas")).as_deref(), Some("//nas/share"));
        assert_eq!(device_of(t, Path::new("/mnt/none")), None);
    }
}
