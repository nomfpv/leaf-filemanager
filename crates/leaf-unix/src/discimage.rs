//! Linux backend of rf_win::discimage: disc images (`.iso`, `.img`) mounted read-only, and optical discs, without root.
//!
//! In this order, whichever is installed:
//! - udisks: `udisksctl loop-setup -r -f <image>` gives a read-only loop device, `udisksctl mount -b <loop>` mounts it under
//!   `/run/media/<user>/<label>` (it then shows under Drives: `drives.rs` lists udf / iso9660 mounts as discs); unmount +
//!   `loop-delete` on Eject. Optical discs: `udisksctl mount -b /dev/srN`.
//! - gio (gvfs): `gio mount archive://<image>` (read through libarchive: ISO 9660 and older UDF), `gio mount -u` on Eject; optical
//!   discs: `gio mount -d /dev/srN`.
//! - fuseiso: `fuseiso -p <image> <folder>` in leaf's cache folder, `fusermount -u` on Eject (ISO 9660 only).
//!
//! Blocking: call from a worker thread. Errors are the tool's own words, for the details line; when none of them is installed the
//! error says which to install.

use std::path::{Path, PathBuf};
use std::process::Command;

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

fn run(mut c: Command) -> Result<String, String> {
    // (udisks and gio answer in seconds; a prompt that never comes is stopped after two minutes)
    let out = leaf_proc::output_helper(&mut c, Some(std::time::Duration::from_secs(120))).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        return Ok(text);
    }
    let err = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).trim().to_string();
    Err(if err.is_empty() { format!("{} failed ({})", c.get_program().to_string_lossy(), out.status) } else { err })
}

const NO_TOOL: &str = "leaf mounts images through udisks (udisksctl), gio or fuseiso, and none of them is installed";

/// A file this module can mount (by its extension).
pub fn can_mount(image: &Path) -> bool {
    image.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("iso") || e.eq_ignore_ascii_case("img")
    })
}

fn mountinfo() -> Vec<(String, String, String)> {
    std::fs::read_to_string("/proc/self/mountinfo").map(|t| t.lines().filter_map(crate::drives::parse_mount_line).collect()).unwrap_or_default()
}

/// The loop devices whose backing file is `image` (`/dev/loop3`).
fn loops_of(image: &Path) -> Vec<String> {
    let want = std::fs::canonicalize(image).unwrap_or_else(|_| image.to_path_buf());
    let Ok(rd) = std::fs::read_dir("/sys/block") else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("loop") {
            continue;
        }
        let Ok(b) = std::fs::read_to_string(e.path().join("loop/backing_file")) else { continue };
        if Path::new(b.trim()) == want {
            out.push(format!("/dev/{name}"));
        }
    }
    out
}

/// leaf's own fuseiso folder for an image.
fn fuse_dir(image: &Path) -> PathBuf {
    let stem = image.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("leaf").join("mnt").join(stem)
}

/// The gvfs folder of an image mounted through `gio mount archive://`.
fn gvfs_dir(image: &Path) -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let base = PathBuf::from(format!("/run/user/{uid}/gvfs"));
    let name = image.file_name()?.to_string_lossy().into_owned();
    // the folder's name holds the image's address, escaped twice ("archive:host=file%253A%252F%252F%252Fhome%252F...%252Fx.iso")
    let escaped: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%25{b:02X}") }).collect();
    std::fs::read_dir(base).ok()?.flatten().map(|e| e.path()).find(|p| {
        let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        n.starts_with("archive:host=") && (n.contains(&escaped) || n.contains(&name))
    })
}

/// Where `image` is mounted now, `None` when it is not.
pub fn mounted_at(image: &Path) -> Option<PathBuf> {
    let info = mountinfo();
    for dev in loops_of(image) {
        // the loop device itself, or its first partition (a hybrid image)
        if let Some((m, _, _)) = info.iter().find(|(_, _, s)| *s == dev || s.starts_with(&format!("{dev}p"))) {
            return Some(PathBuf::from(m));
        }
    }
    let fuse = fuse_dir(image);
    if info.iter().any(|(m, _, _)| Path::new(m) == fuse) {
        return Some(fuse);
    }
    gvfs_dir(image)
}

/// Whether the image is mounted (cheap: sysfs and mountinfo only).
pub fn is_mounted_quick(image: &Path) -> bool {
    mounted_at(image).is_some()
}

/// "Mounted /dev/loop0 at /run/media/neo/DISC." -> the folder.
fn mounted_folder(text: &str) -> Option<PathBuf> {
    let at = text.rfind(" at ")?;
    let p = text[at + 4..].trim().trim_end_matches('.');
    (!p.is_empty()).then(|| PathBuf::from(p))
}

fn udisks_mount(ud: &Path, dev: &str) -> Result<PathBuf, String> {
    let mut c = Command::new(ud);
    c.args(["mount", "--no-user-interaction", "-o", "ro", "-b", dev]);
    match run(c) {
        Ok(t) => mounted_folder(&t).ok_or_else(|| format!("udisksctl did not say where it mounted {dev}")),
        Err(e) => {
            // the desktop mounted it by itself meanwhile ("is already mounted at ...")
            if let Some((m, _, _)) = mountinfo().into_iter().find(|(_, _, s)| s == dev) {
                return Ok(PathBuf::from(m));
            }
            Err(e)
        }
    }
}

/// Mounts `image` read-only and gives the folder its content is in; an image already mounted gives its folder again.
pub fn mount(image: &Path) -> Result<PathBuf, String> {
    if let Some(at) = mounted_at(image) {
        return Ok(at);
    }
    if let Some(ud) = which("udisksctl") {
        let dev = match loops_of(image).into_iter().next() {
            Some(d) => d,
            None => {
                let mut c = Command::new(&ud);
                c.args(["loop-setup", "--no-user-interaction", "-r", "-f"]).arg(image);
                let t = run(c)?;
                // "Mapped file /x.iso as /dev/loop0."
                t.split_whitespace().map(|w| w.trim_end_matches('.')).find(|w| w.starts_with("/dev/loop")).map(str::to_string).ok_or_else(|| t.clone())?
            }
        };
        // the desktop may mount a new loop device by itself: give it a moment before mounting it ourselves
        for _ in 0..10 {
            if let Some(at) = mounted_at(image) {
                return Ok(at);
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        // a hybrid image has a partition table: mount the first partition
        let part = format!("{dev}p1");
        let target = if Path::new(&part).exists() { part } else { dev };
        return udisks_mount(&ud, &target);
    }
    if let Some(gio) = which("gio") {
        let mut c = Command::new(gio);
        let abs = std::fs::canonicalize(image).map_err(|e| e.to_string())?;
        let uri: String = format!("file://{}", abs.to_string_lossy())
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        let escaped: String = uri.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
        c.arg("mount").arg(format!("archive://{escaped}"));
        run(c)?;
        return gvfs_dir(image).ok_or_else(|| "gio mounted the image but its folder was not found".to_string());
    }
    if let Some(fi) = which("fuseiso") {
        let dir = fuse_dir(image);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut c = Command::new(fi);
        c.arg("-p").arg(image).arg(&dir);
        run(c)?;
        return Ok(dir);
    }
    Err(NO_TOOL.to_string())
}

/// Unmounts `image` and lets go of its loop device.
pub fn unmount(image: &Path) -> Result<(), String> {
    let info = mountinfo();
    let loops = loops_of(image);
    if !loops.is_empty()
        && let Some(ud) = which("udisksctl")
    {
        for dev in &loops {
            for (_, _, s) in info.iter().filter(|(_, _, s)| s == dev || s.starts_with(&format!("{dev}p"))) {
                let mut c = Command::new(&ud);
                c.args(["unmount", "--no-user-interaction", "-b", s.as_str()]);
                run(c)?;
            }
            let mut c = Command::new(&ud);
            c.args(["loop-delete", "--no-user-interaction", "-b", dev.as_str()]);
            // (udisks may have dropped an auto-clear loop device already)
            if let Err(e) = run(c)
                && Path::new(&format!("/sys/block/{}", dev.trim_start_matches("/dev/"))).exists()
            {
                return Err(e);
            }
        }
        return Ok(());
    }
    let fuse = fuse_dir(image);
    if info.iter().any(|(m, _, _)| Path::new(m) == fuse) {
        let tool = which("fusermount3").or_else(|| which("fusermount")).ok_or("fusermount is not installed")?;
        let mut c = Command::new(tool);
        c.arg("-u").arg(&fuse);
        run(c)?;
        let _ = std::fs::remove_dir(&fuse);
        return Ok(());
    }
    if let Some(dir) = gvfs_dir(image)
        && let Some(gio) = which("gio")
    {
        let mut c = Command::new(gio);
        c.args(["mount", "-u"]).arg(dir);
        run(c)?;
        return Ok(());
    }
    Err(format!("{} is not mounted", image.display()))
}

/// Mounts the optical discs that are in a drive but not mounted yet (`/dev/srN` with media), giving their folders.
pub fn mount_discs() -> Result<Vec<PathBuf>, String> {
    let info = mountinfo();
    let mut waiting = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/block") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("sr") {
                continue;
            }
            // no disc in the drive: size 0
            let size = std::fs::read_to_string(e.path().join("size")).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
            let dev = format!("/dev/{name}");
            if size > 0 && !info.iter().any(|(_, _, s)| *s == dev) {
                waiting.push(dev);
            }
        }
    }
    waiting.sort();
    let mut out = Vec::new();
    for dev in waiting {
        if let Some(ud) = which("udisksctl") {
            out.push(udisks_mount(&ud, &dev)?);
        } else if let Some(gio) = which("gio") {
            let mut c = Command::new(gio);
            c.args(["mount", "-d", dev.as_str()]);
            run(c)?;
            if let Some((m, _, _)) = mountinfo().into_iter().find(|(_, _, s)| *s == dev) {
                out.push(PathBuf::from(m));
            }
        } else {
            return Err(NO_TOOL.to_string());
        }
    }
    Ok(out)
}

