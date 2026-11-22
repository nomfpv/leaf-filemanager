//! Disc images (`.iso`, `.img`) mounted read-only as a drive, and optical discs (Linux twin: leaf-unix discimage.rs).
//!
//! Windows mounts an image itself (`Mount-DiskImage`, the Storage module Explorer's "Mount" uses), always read-only for ISO; the
//! drive then shows under Drives like any disc. The image path reaches PowerShell through an environment variable, never inside
//! the script text. An optical disc is mounted by the system when it is put in, so [`mount_discs`] has nothing to do here.
//!
//! Blocking (PowerShell takes a second or two): call from a worker thread. Errors are PowerShell's own words, for the details line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The images mounted through leaf in this session (what [`is_mounted_quick`] answers from).
static MOUNTED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

fn remember(image: &Path, on: bool) {
    let mut m = MOUNTED.lock().unwrap_or_else(|p| p.into_inner());
    let set = m.get_or_insert_with(HashSet::new);
    if on {
        set.insert(image.to_path_buf());
    } else {
        set.remove(image);
    }
}

/// A file this module can mount (by its extension).
pub fn can_mount(image: &Path) -> bool {
    image.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.eq_ignore_ascii_case("iso") || e.eq_ignore_ascii_case("img")
    })
}

fn ps(script: &str, image: &Path) -> Result<String, String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .env("LEAF_IMAGE", image)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        return Ok(text);
    }
    let err = String::from_utf8_lossy(&out.stderr).lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    Err(if err.is_empty() { format!("Mount-DiskImage failed ({})", out.status) } else { err })
}

fn letter_root(text: &str) -> Option<PathBuf> {
    let c = text.lines().map(str::trim).find(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_alphabetic()))?;
    Some(PathBuf::from(format!("{}:\\", c.to_ascii_uppercase())))
}

/// Where `image` is mounted now (its drive's root), `None` when it is not.
pub fn mounted_at(image: &Path) -> Option<PathBuf> {
    let t = ps("$i = Get-DiskImage -ImagePath $env:LEAF_IMAGE -ErrorAction Stop; if ($i.Attached) { ($i | Get-Volume).DriveLetter }", image).ok()?;
    letter_root(&t)
}

/// Whether the image was mounted through leaf in this session (cheap: for a menu; an image mounted elsewhere says false).
pub fn is_mounted_quick(image: &Path) -> bool {
    MOUNTED.lock().ok().and_then(|m| m.as_ref().map(|s| s.contains(image))).unwrap_or(false)
}

/// Mounts `image` read-only and gives its drive's root; an image already mounted gives its root again.
pub fn mount(image: &Path) -> Result<PathBuf, String> {
    if let Some(root) = mounted_at(image) {
        remember(image, true);
        return Ok(root);
    }
    let t = ps("(Mount-DiskImage -ImagePath $env:LEAF_IMAGE -StorageType ISO -PassThru -ErrorAction Stop | Get-Volume).DriveLetter", image)?;
    let root = letter_root(&t).or_else(|| mounted_at(image)).ok_or_else(|| "the image was mounted but has no drive letter".to_string())?;
    remember(image, true);
    Ok(root)
}

/// Unmounts `image` (ejects its drive).
pub fn unmount(image: &Path) -> Result<(), String> {
    ps("Dismount-DiskImage -ImagePath $env:LEAF_IMAGE -ErrorAction Stop | Out-Null", image)?;
    remember(image, false);
    Ok(())
}

/// Mounts the optical discs that are in a drive but not mounted yet, giving their roots. Windows mounts a disc when it is put in:
/// nothing to do.
pub fn mount_discs() -> Result<Vec<PathBuf>, String> {
    Ok(Vec::new())
}
