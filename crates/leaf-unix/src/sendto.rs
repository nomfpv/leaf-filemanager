//! Linux backend of rf_win::sendto. Status: REAL (never run on a desktop)
//!
//! Windows' "Send to" lists the SendTo folder's shortcuts. Here the targets are the programs a desktop has for it, each only when it is
//! installed: `bluedevil-sendfile` (Bluetooth device, KDE's wizard), `xdg-email` (a new mail with the files attached) and every reachable
//! paired KDE Connect device (`kdeconnect-cli`). A target's `path` is what `send` reads it back from: the program's own path, or for a
//! KDE Connect device `<kdeconnect-cli path>#<device id>`. A folder (the app adds the removable and network drives itself) receives a copy
//! of the items: never over a name that exists (`name (2)`), links stay links. Nothing is moved or deleted.
//! A compressed archive is not offered here: leaf's archive code lives in a crate this one does not depend on.

use crate::Owner;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// TYPE-COPY(rf-win src/sendto.rs:24)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub name: String,
}

pub fn targets() -> Vec<Target> {
    let mut v = Vec::new();
    if let Some(p) = crate::shell::which("bluedevil-sendfile") {
        v.push(Target { path: p, name: "Bluetooth device".to_string() });
    }
    if let Some(p) = crate::shell::which("xdg-email") {
        v.push(Target { path: p, name: "Mail recipient".to_string() });
    }
    if let Some(k) = crate::shell::which("kdeconnect-cli") {
        for (id, name) in kdeconnect_devices() {
            let mut s = k.clone().into_os_string();
            s.push("#");
            s.push(&id);
            v.push(Target { path: PathBuf::from(s), name });
        }
    }
    v
}

/// The reachable paired KDE Connect devices as `(id, name)`: `kdeconnect-cli -a --id-name-only` prints `<id> <name>` per line. Empty when the
/// program is missing, the daemon is not running or nothing is reachable.
pub(crate) fn kdeconnect_devices() -> Vec<(String, String)> {
    let Some(k) = crate::shell::which("kdeconnect-cli") else { return Vec::new() };
    let Ok(o) = Command::new(k).args(["-a", "--id-name-only"]).env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null()).output() else { return Vec::new() };
    if !o.status.success() {
        return Vec::new();
    }
    parse_devices(&String::from_utf8_lossy(&o.stdout))
}

fn parse_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (id, name) = l.trim().split_once(' ')?;
            (!id.is_empty() && !id.starts_with('-') && !name.trim().is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')).then(|| (id.to_string(), name.trim().to_string()))
        })
        .collect()
}

/// Shares `items` with the KDE Connect device `id`: one `kdeconnect-cli --share` per item (the program takes one).
pub(crate) fn kdeconnect_share(items: &[PathBuf], id: &str) -> anyhow::Result<()> {
    let k = crate::shell::which("kdeconnect-cli").ok_or_else(|| anyhow::anyhow!("kdeconnect-cli is not installed"))?;
    anyhow::ensure!(!id.is_empty() && !id.starts_with('-'), "no such device");
    for it in items {
        let st = Command::new(&k).arg("--device").arg(id).arg("--share").arg(it).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
        anyhow::ensure!(st.success(), "KDE Connect could not send {}", it.display());
    }
    Ok(())
}

fn spawn_detached(mut c: Command) -> anyhow::Result<()> {
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let _ = child.wait();
    })?;
    Ok(())
}

pub fn send(_owner: Owner, target: &Path, items: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::ensure!(!items.is_empty(), "nothing to send");
    if target.is_dir() {
        return copy_into(target, items);
    }
    let text = target.to_string_lossy();
    if let Some((prog, id)) = text.split_once('#')
        && Path::new(prog).file_name().is_some_and(|n| n == "kdeconnect-cli")
    {
        return kdeconnect_share(items, id);
    }
    match target.file_name().and_then(|n| n.to_str()) {
        Some("xdg-email") => {
            let mut c = Command::new(target);
            for it in items {
                c.arg("--attach").arg(it);
            }
            spawn_detached(c)
        }
        Some("bluedevil-sendfile") => {
            let mut c = Command::new(target);
            for it in items {
                c.arg("-f").arg(it);
            }
            spawn_detached(c)
        }
        // (a data operation never answers success for a target it does not know)
        _ => Err(leaf_common::stub::Unsupported("sendto::send").into()),
    }
}

/// Copies each item into `dir`, never over an existing name.
fn copy_into(dir: &Path, items: &[PathBuf]) -> anyhow::Result<()> {
    for it in items {
        let name = it.file_name().ok_or_else(|| anyhow::anyhow!("{} has no name", it.display()))?;
        copy_one(it, &free_name(dir, name))?;
    }
    Ok(())
}

/// `dir/name`, or `dir/stem (2).ext`, `(3)`... when that exists (a dangling link counts as existing).
fn free_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| c.symlink_metadata().is_err())
        .unwrap_or(first)
}

/// A file (created new, never truncating), a link (made again as a link) or a folder (and what is in it).
fn copy_one(from: &Path, to: &Path) -> anyhow::Result<()> {
    let md = from.symlink_metadata()?;
    if md.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
    } else if md.is_dir() {
        // (a folder copied into itself would never end)
        anyhow::ensure!(!to.starts_with(from), "a folder cannot be sent into itself");
        std::fs::create_dir(to)?;
        let mut names: Vec<_> = std::fs::read_dir(from)?.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
        names.sort();
        for n in names {
            copy_one(&from.join(&n), &to.join(&n))?;
        }
        let _ = std::fs::set_permissions(to, md.permissions());
    } else if md.is_file() {
        let mut src = std::fs::File::open(from)?;
        let mut dst = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        std::io::copy(&mut src, &mut dst)?;
        let _ = std::fs::set_permissions(to, md.permissions());
    } else {
        anyhow::bail!("{} is not a file or folder", from.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdeconnect_devices_are_read() {
        assert_eq!(parse_devices("abc_123 My Phone\nno?id line that is not a device\n-x Dash\n\n"), vec![("abc_123".to_string(), "My Phone".to_string())]);
    }

    #[test]
    fn a_copy_into_a_folder_keeps_every_name_that_is_there() {
        let base = std::env::temp_dir().join(format!("leaf-sendto-{}", std::process::id()));
        let (src, dst) = (base.join("src"), base.join("dst"));
        std::fs::create_dir_all(src.join("d")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), "new").unwrap();
        std::fs::write(src.join("d/x"), "x").unwrap();
        std::fs::write(dst.join("a.txt"), "old").unwrap();
        copy_into(&dst, &[src.join("a.txt"), src.join("d")]).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dst.join("a (2).txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dst.join("d/x")).unwrap(), "x");
        assert!(copy_into(&src, &[src.join("d")]).is_ok());
        assert!(copy_one(&src.join("d"), &src.join("d").join("in")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
