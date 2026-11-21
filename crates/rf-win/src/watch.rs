//! Moved to leaf-common (it works on every system); re-exported here at its old path. Its tests stay here: they read the folder back
//! through rf-win's own listing.

pub use leaf_common::watch::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// The changed names come with the report: an added, a renamed and a deleted file.
    #[test]
    fn reports_the_changed_names() {
        let dir = std::env::temp_dir().join(format!("rf-watch-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("eski.txt"), b"x").unwrap();
        std::fs::write(dir.join("silinecek.txt"), b"x").unwrap();
        let got = Arc::new(std::sync::Mutex::new(Vec::<Changes>::new()));
        let g2 = got.clone();
        let _w = watch_dir_changes(&dir, Duration::from_millis(100), move |c| g2.lock().unwrap().push(c)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.join("yeni şarkı.txt"), b"x").unwrap();
        std::fs::rename(dir.join("eski.txt"), dir.join("Yeni Ad.txt")).unwrap();
        std::fs::remove_file(dir.join("silinecek.txt")).unwrap();
        let start = Instant::now();
        let mut names = std::collections::BTreeSet::new();
        while start.elapsed() < Duration::from_secs(3) {
            for c in got.lock().unwrap().iter() {
                assert!(!c.rescan, "{c:?}");
                names.extend(c.names.iter().cloned());
            }
            if names.len() >= 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let want: std::collections::BTreeSet<String> = ["yeni şarkı.txt", "eski.txt", "Yeni Ad.txt", "silinecek.txt"].iter().map(|s| s.to_string()).collect();
        assert_eq!(names, want);
        // and each reads back as listed: present or gone
        assert!(crate::stat_entry(&dir, "Yeni Ad.txt").unwrap().is_some());
        assert!(crate::stat_entry(&dir, "silinecek.txt").unwrap().is_none());
        assert_eq!(&*crate::stat_entry(&dir, "yeni ad.txt").unwrap().unwrap().name, "Yeni Ad.txt");
        drop(_w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_burst_without_pause_still_reports() {
        let dir = std::env::temp_dir().join(format!("rf-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let _w = watch_dir(&dir, Duration::from_millis(150), move || {
            c2.fetch_add(1, Ordering::Relaxed);
        })
        .unwrap();
        // a change every 30 ms for 2 s: never 150 ms of quiet
        let start = Instant::now();
        let mut i = 0;
        while start.elapsed() < Duration::from_secs(2) {
            std::fs::write(dir.join(format!("f{}.txt", i % 50)), format!("{i}")).unwrap();
            i += 1;
            std::thread::sleep(Duration::from_millis(30));
        }
        let during = calls.load(Ordering::Relaxed);
        assert!(during >= 2, "reported {during} times during a 2 s burst");
        // and once more after it settles
        std::thread::sleep(Duration::from_millis(400));
        assert!(calls.load(Ordering::Relaxed) > during);
        drop(_w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
