//! Linux backend of rf_win::rotate. Status: REAL for JPEG, PNG, BMP, TIFF and WebP
//!
//! A JPEG turns by its EXIF orientation tag (`leaf_common::exif_orient`): the picture data is never re-encoded. The file is written
//! to a temp name beside it, synced, and renamed over the original only when complete; the mode and the modified time stay.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn is_jpeg(p: &Path) -> bool {
    matches!(ext(p).as_str(), "jpg" | "jpeg" | "jfif" | "jpe")
}

fn pixel_format(p: &Path) -> Option<image::ImageFormat> {
    match ext(p).as_str() {
        "png" => Some(image::ImageFormat::Png),
        "bmp" => Some(image::ImageFormat::Bmp),
        "tif" | "tiff" => Some(image::ImageFormat::Tiff),
        "webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

pub fn can_rotate(p: &Path) -> bool {
    is_jpeg(p) || pixel_format(p).is_some()
}

pub fn rotate(path: &Path, clockwise: bool) -> Result<()> {
    anyhow::ensure!(can_rotate(path), "This file type can't be rotated");
    // a link is turned at its target, and stays a link
    let real = std::fs::canonicalize(path).with_context(|| format!("{}", path.display()))?;
    let meta = std::fs::metadata(&real)?;
    let src = std::fs::read(&real)?;
    let out = if is_jpeg(&real) {
        leaf_common::exif_orient::rotate_jpeg(&src, clockwise)?
    } else {
        let fmt = pixel_format(&real).context("This file type can't be rotated")?;
        let img = image::load_from_memory_with_format(&src, fmt)?;
        let img = if clockwise { img.rotate90() } else { img.rotate270() };
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, fmt)?;
        buf.into_inner()
    };
    replace(&real, &meta, &out, |a, b| std::fs::rename(a, b))
}

/// `out` goes to a temp file beside `real`, then `rename` puts it over `real`; on any failure the temp file is removed and `real`
/// is as it was.
fn replace(real: &Path, meta: &std::fs::Metadata, out: &[u8], rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>) -> Result<()> {
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = real.with_file_name(format!(".{name}.leaf-rotate-{}", std::process::id()));
    let write = || -> Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(out)?;
        f.set_permissions(meta.permissions())?;
        if let Ok(m) = meta.modified() {
            let _ = f.set_modified(m);
        }
        f.sync_all()?;
        rename(&tmp, real)?;
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
    use image::GenericImageView;
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

    #[test]
    fn a_failed_write_leaves_the_original_and_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("leaf-rot-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.png");
        image::RgbImage::from_fn(6, 3, |x, y| image::Rgb([x as u8, y as u8, 7])).save(&f).unwrap();
        let before = std::fs::read(&f).unwrap();
        let meta = std::fs::metadata(&f).unwrap();
        let r = replace(&f, &meta, b"new bytes", |_, _| Err(std::io::Error::other("disk full")));
        assert!(r.is_err());
        assert_eq!(std::fs::read(&f).unwrap(), before);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        rotate(&f, true).unwrap();
        assert_eq!(image::open(&f).unwrap().dimensions(), (3, 6));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
