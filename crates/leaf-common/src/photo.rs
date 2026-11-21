//! A photo's own metadata, read from the file with no system API (the same on every system).

use std::path::Path;

/// When a photo was taken: EXIF DateTimeOriginal (else DateTime) from the file's own metadata
/// (JPEG, TIFF, HEIF, PNG, WebP), as local time in Unix milliseconds. `None`: not a picture,
/// no such field, or unreadable. Plain file reading, no system API (the bulk rename's `{taken}`).
pub fn exif_taken_ms(path: &Path) -> Option<i64> {
    use chrono::TimeZone;
    let f = std::fs::File::open(path).ok()?;
    let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)).ok()?;
    let field = ex.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).or_else(|| ex.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    let exif::Value::Ascii(ref v) = field.value else { return None };
    let dt = exif::DateTime::from_ascii(v.first()?).ok()?;
    let naive = chrono::NaiveDate::from_ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)?.and_hms_opt(dt.hour as u32, dt.minute as u32, dt.second as u32)?;
    chrono::Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp_millis())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_that_is_no_picture_has_no_date() {
        let p = std::env::temp_dir().join(format!("leaf-photo-test-{}.txt", std::process::id()));
        std::fs::write(&p, b"not a picture").unwrap();
        assert_eq!(super::exif_taken_ms(&p), None);
        let _ = std::fs::remove_file(&p);
        assert_eq!(super::exif_taken_ms(std::path::Path::new("/no/such/file.jpg")), None);
    }
}
