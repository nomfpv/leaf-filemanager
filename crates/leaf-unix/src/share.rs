//! Linux backend of rf_win::share ("Share" with the system's share sheet). Status: PARTIAL (KDE Connect only; never run on a desktop)
//!
//! Windows opens its share sheet. A Linux desktop has no common one, so the twin offers what KDE has: the reachable paired KDE Connect
//! devices (`kdeconnect-cli -a`). One device: the items go to it; several: a kdialog / zenity list asks which. Without KDE Connect, or
//! with no device in reach, `prepare` says so (an error the app logs and shows no sheet for), so nothing answers success it did not do.

use std::path::PathBuf;

// TYPE-COPY(rf-win share.rs): the storage-item reference dropped
pub struct Prepared {
    paths: Vec<PathBuf>,
    devices: Vec<(String, String)>,
}

pub fn prepare(paths: &[PathBuf]) -> anyhow::Result<Prepared> {
    anyhow::ensure!(!paths.is_empty(), "nothing to share");
    anyhow::ensure!(crate::shell::which("kdeconnect-cli").is_some(), "no sharing program is installed (KDE Connect)");
    let devices = crate::sendto::kdeconnect_devices();
    anyhow::ensure!(!devices.is_empty(), "no paired KDE Connect device is in reach");
    Ok(Prepared { paths: paths.to_vec(), devices })
}

pub fn show(_hwnd: isize, items: Prepared, title: &str) -> anyhow::Result<()> {
    let i = if items.devices.len() == 1 {
        0
    } else {
        let names: Vec<String> = items.devices.iter().map(|(_, n)| n.clone()).collect();
        match crate::openwith::choose_index(if title.is_empty() { "Share" } else { title }, "Share with", &names)? {
            Some(i) => i,
            None => return Ok(()),
        }
    };
    let (id, _) = items.devices.get(i).ok_or_else(|| anyhow::anyhow!("no such device"))?;
    crate::sendto::kdeconnect_share(&items.paths, id)
}
