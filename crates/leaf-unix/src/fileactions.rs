//! Linux backend of rf_win::fileactions. Status: REAL (set_wallpaper through `plasma-apply-wallpaperimage`, else `gsettings`;
//! install_font copies into `~/.local/share/fonts`, never over a file that is there). "Run as administrator" does not exist here.

use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_image(p: &Path) -> bool {
    matches!(ext(p).as_str(), "jpg" | "jpeg" | "png" | "bmp" | "gif" | "tif" | "tiff" | "webp" | "jfif" | "heic" | "avif")
}

pub fn is_font(p: &Path) -> bool {
    matches!(ext(p).as_str(), "ttf" | "otf" | "ttc" | "otc" | "woff" | "woff2")
}

/// There is no "Run as administrator" entry here.
pub fn is_runnable(_p: &Path) -> bool {
    false
}

/// `Ok(true)` ran and succeeded, `Ok(false)` ran and failed, `Err` the program is not installed. Argument list, no shell.
fn run(prog: &str, args: &[&std::ffi::OsStr]) -> std::io::Result<bool> {
    Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success())
}

/// `file://` and the path with everything outside the safe set percent-encoded.
fn file_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

pub fn set_wallpaper(path: &Path) -> anyhow::Result<()> {
    use std::ffi::OsStr;
    let path = std::fs::canonicalize(path)?;
    if run("plasma-apply-wallpaperimage", &[path.as_os_str()]).unwrap_or(false) {
        return Ok(());
    }
    let uri = file_uri(&path);
    let mut done = false;
    for key in ["picture-uri", "picture-uri-dark"] {
        match run("gsettings", &[OsStr::new("set"), OsStr::new("org.gnome.desktop.background"), OsStr::new(key), OsStr::new(&uri)]) {
            Ok(true) => done = true,
            Ok(false) => {}
            Err(_) => anyhow::bail!("Setting the wallpaper is not available (neither plasma-apply-wallpaperimage nor gsettings is installed)"),
        }
    }
    anyhow::ensure!(done, "The desktop did not take the wallpaper");
    Ok(())
}

/// Copies the font into `~/.local/share/fonts` (a file of that name already there stays as it is) and refreshes the font cache when
/// `fc-cache` exists. Returns the installed copy.
pub fn install_font_for_user(path: &Path) -> anyhow::Result<PathBuf> {
    let dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("The user's data folder is not known"))?.join("fonts");
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().ok_or_else(|| anyhow::anyhow!("invalid name"))?;
    let dest = dir.join(name);
    let mut src = std::fs::File::open(path)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&dest) {
        Ok(mut out) => {
            if let Err(e) = std::io::copy(&mut src, &mut out) {
                drop(out);
                let _ = std::fs::remove_file(&dest);
                return Err(e.into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let _ = run("fc-cache", &[dir.as_os_str()]);
    Ok(dest)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_wallpaper_uri_is_percent_encoded() {
        assert_eq!(super::file_uri(std::path::Path::new("/home/a b/ü.png")), "file:///home/a%20b/%C3%BC.png");
    }
}
