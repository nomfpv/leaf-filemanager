//! Linux backend of rf_win::rotate. Status: REAL for JPEG only
//!
//! A JPEG turns by its EXIF orientation tag (`leaf_common::exif_orient`): the picture data is never re-encoded. The file is written
//! to a temp name beside it, synced, and renamed over the original only when complete; the mode and the modified time stay. Other
//! picture formats are not offered (turning their pixels would mean writing them again, which Windows does through WIC).

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

pub fn can_rotate(p: &Path) -> bool {
    let e = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(e.as_str(), "jpg" | "jpeg" | "jfif" | "jpe")
}

pub fn rotate(path: &Path, clockwise: bool) -> Result<()> {
    anyhow::ensure!(can_rotate(path), "This file type can't be rotated");
    // a link is turned at its target, and stays a link
    let real = std::fs::canonicalize(path).with_context(|| format!("{}", path.display()))?;
    let meta = std::fs::metadata(&real)?;
    let src = std::fs::read(&real)?;
    let out = leaf_common::exif_orient::rotate_jpeg(&src, clockwise)?;
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = real.with_file_name(format!(".{name}.leaf-rotate-{}", std::process::id()));
    let write = || -> Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&out)?;
        f.set_permissions(meta.permissions())?;
        if let Ok(m) = meta.modified() {
            let _ = f.set_modified(m);
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &real)?;
        Ok(())
    };
    let r = write();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_jpeg_turns_on_a_temp_copy_keeping_mode_and_time() {
        let dir = std::env::temp_dir().join(format!("leaf-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.jpg");
        let img = image::RgbImage::from_fn(8, 4, |x, y| image::Rgb([x as u8 * 30, y as u8 * 60, 9]));
        img.save(&f).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
        let before = std::fs::metadata(&f).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        let orient = |p: &Path| {
            let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(std::fs::File::open(p).unwrap())).unwrap();
            ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap()
        };
        rotate(&f, true).unwrap();
        assert_eq!(orient(&f), 6);
        rotate(&f, true).unwrap();
        assert_eq!(orient(&f), 3);
        let after = std::fs::metadata(&f).unwrap();
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert_eq!(after.modified().unwrap(), t);
        assert!(before.len() < after.len() + 40);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file is left");
        assert!(image::open(&f).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
