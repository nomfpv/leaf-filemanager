//! Growth line: the size of a folder sampled at most once a day, kept in `growth.json` in the config folder, and a plain
//! straight-line forecast from the recent samples. The forecast is an estimate and is shown as one.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Samples kept per folder.
const KEEP: usize = 400;
/// The forecast looks at samples up to this many days back.
const WINDOW_DAYS: i64 = 90;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Samples {
    /// Folder path key to (day number since 1970, size in bytes), oldest first.
    #[serde(default)]
    pub folders: BTreeMap<String, Vec<(i64, u64)>>,
}

impl Samples {
    pub fn load(file: &Path) -> Self {
        std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, file: &Path) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(file, s);
        }
    }

    pub fn of(&self, folder: &Path) -> &[(i64, u64)] {
        self.folders.get(&crate::pathkey::path_key(folder)).map_or(&[], Vec::as_slice)
    }

    /// Whether `day` has its sample already (so nothing is measured again today).
    pub fn has_day(&self, folder: &Path, day: i64) -> bool {
        self.of(folder).last().is_some_and(|&(d, _)| d >= day)
    }

    /// Adds the sample of `day`; one per day (a second one on the same day replaces the first).
    pub fn add(&mut self, folder: &Path, day: i64, bytes: u64) {
        let v = self.folders.entry(crate::pathkey::path_key(folder)).or_default();
        match v.last_mut() {
            Some(last) if last.0 == day => last.1 = bytes,
            _ => v.push((day, bytes)),
        }
        if v.len() > KEEP {
            let cut = v.len() - KEEP;
            v.drain(..cut);
        }
    }
}

/// The straight line through the samples of the last 90 days.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Forecast {
    /// Bytes per day; negative when the folder shrinks.
    pub per_day: f64,
    /// The line's size today and the size it reaches in 30 days (never below zero).
    pub now: u64,
    pub in_30_days: u64,
}

/// Least-squares line over the samples within the window; needs two samples on different days.
pub fn forecast(samples: &[(i64, u64)]) -> Option<Forecast> {
    let last_day = samples.last()?.0;
    let pts: Vec<(f64, f64)> = samples.iter().filter(|(d, _)| last_day - d <= WINDOW_DAYS).map(|&(d, b)| ((d - last_day) as f64, b as f64)).collect();
    let n = pts.len() as f64;
    if pts.len() < 2 || pts.first()?.0 == pts.last()?.0 {
        return None;
    }
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mx, my) = (sx / n, sy / n);
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let at = |x: f64| (my + slope * (x - mx)).max(0.0) as u64;
    Some(Forecast { per_day: slope, now: at(0.0), in_30_days: at(30.0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_growth_is_forecast() {
        let f = forecast(&[(100, 1000), (101, 1100), (102, 1200)]).unwrap();
        assert!((f.per_day - 100.0).abs() < 1e-6);
        assert_eq!(f.in_30_days, 4200);
        assert!(forecast(&[(5, 10)]).is_none());
        assert!(forecast(&[(5, 10), (5, 12)]).is_none());
    }
}
