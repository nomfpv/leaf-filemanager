use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use chrono::{Local, TimeZone};

static DECIMAL: AtomicBool = AtomicBool::new(false);
static DATE_STYLE: AtomicU8 = AtomicU8::new(0);
/// How the system writes a local date and time (Windows' short date and time, set by the app at start: `set_system_format`).
/// Unset (tests, other platforms): "05.11.2026 13:45".
static SYSTEM_FORMAT: std::sync::OnceLock<fn(&chrono::DateTime<Local>) -> Option<String>> = std::sync::OnceLock::new();

/// Gives the date code the system's own way of writing a local date and time (issue 59: "Format of the creation date or last
/// modified date should adhere to the date/time format settings defined by the OS"). Set once.
pub fn set_system_format(f: fn(&chrono::DateTime<Local>) -> Option<String>) {
    let _ = SYSTEM_FORMAT.set(f);
}

/// A local date and time as the system writes it, else "05.11.2026 13:45".
fn local(dt: &chrono::DateTime<Local>) -> String {
    SYSTEM_FORMAT.get().and_then(|f| f(dt)).unwrap_or_else(|| dt.format("%d.%m.%Y %H:%M").to_string())
}

/// How dates are shown (Settings → General → Date format).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateStyle {
    /// "Today, 10:05", "Yesterday, 18:02", older ones "18 Nov 2026".
    Calendar,
    /// The system's short date and time (Windows' regional format), "05.11.2026 13:45" where there is none.
    Local,
    /// ISO 8601 "2026-11-05 13:45"
    Universal,
}

pub fn set_date_style(s: DateStyle) {
    DATE_STYLE.store(s as u8, Ordering::Relaxed);
}

fn date_style() -> DateStyle {
    match DATE_STYLE.load(Ordering::Relaxed) {
        0 => DateStyle::Calendar,
        1 => DateStyle::Local,
        _ => DateStyle::Universal,
    }
}

/// Sizes in powers of 1000 instead of 1024 (Settings → Files & folders → Size format).
pub fn set_decimal_sizes(on: bool) {
    DECIMAL.store(on, Ordering::Relaxed);
}

/// "0 B", "512 B", "1.2 KB", "34.5 MB", "412 MB" (the mockups' way: one decimal under 100, none above).
pub fn size(bytes: u64) -> String {
    size_with(bytes, DECIMAL.load(Ordering::Relaxed))
}

fn size_with(bytes: u64, decimal: bool) -> String {
    let base: f64 = if decimal { 1000.0 } else { 1024.0 };
    let units: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if (bytes as f64) < base {
        return format!("{bytes} {}", units[0]);
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= base && u < units.len() - 1 {
        v /= base;
        u += 1;
    }
    let s = if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") };
    format!("{s} {}", units[u])
}

/// A size split into its number and unit, with one decimal from KB up ("100.0", "MB"; "512", "B"), so a column of them lines up.
pub fn size_parts(bytes: u64) -> (String, &'static str) {
    size_parts_with(bytes, DECIMAL.load(Ordering::Relaxed))
}

fn size_parts_with(bytes: u64, decimal: bool) -> (String, &'static str) {
    let base: f64 = if decimal { 1000.0 } else { 1024.0 };
    let units: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if (bytes as f64) < base {
        return (bytes.to_string(), units[0]);
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= base && u < units.len() - 1 {
        v /= base;
        u += 1;
    }
    (format!("{v:.1}"), units[u])
}

/// A file time in the chosen [`DateStyle`].
pub fn datetime(ms: i64) -> String {
    let now = chrono::Local::now().timestamp_millis();
    datetime_with(ms, date_style(), now)
}

/// Always the full local date-time "05.11.2026 03:51" (details, tooltips).
pub fn datetime_full(ms: i64) -> String {
    datetime_with(ms, DateStyle::Local, 0)
}

fn datetime_with(ms: i64, style: DateStyle, now: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let chrono::LocalResult::Single(dt) = Local.timestamp_millis_opt(ms) else { return String::new() };
    match style {
        DateStyle::Universal => dt.format("%Y-%m-%d %H:%M").to_string(),
        DateStyle::Local => local(&dt),
        DateStyle::Calendar => {
            // "Today, 10:05", "Yesterday, 18:02", else "18 Nov 2026" (the mockups' one format)
            let today = match Local.timestamp_millis_opt(if now > 0 { now } else { Local::now().timestamp_millis() }) {
                chrono::LocalResult::Single(n) => n.date_naive(),
                _ => return local(&dt),
            };
            let days = (today - dt.date_naive()).num_days();
            match days {
                0 => format!("Today, {}", dt.format("%H:%M")),
                1 => format!("Yesterday, {}", dt.format("%H:%M")),
                _ => dt.format("%-d %b %Y").to_string(),
            }
        }
    }
}

/// Windows FILETIME (100ns since 1601) → Unix ms.
pub fn filetime_to_unix_ms(ft: u64) -> i64 {
    if ft == 0 {
        return 0;
    }
    (ft / 10_000) as i64 - 11_644_473_600_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KB");
        assert_eq!(size(1536), "1.5 KB");
        assert_eq!(size(10 * 1024 * 1024 + 1), "10.0 MB");
        assert_eq!(size(500 * 1024 * 1024 * 1024), "500 GB");
    }

    #[test]
    fn calendar_and_formats() {
        // noon local time, so that an hour back is still today
        let now = Local.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap().timestamp_millis();
        assert_eq!(datetime_with(now - 3_600_000, DateStyle::Calendar, now), "Today, 11:00");
        assert_eq!(datetime_with(now - 86_400_000, DateStyle::Calendar, now), "Yesterday, 12:00");
        assert_eq!(datetime_with(now - 8 * 86_400_000, DateStyle::Calendar, now), "10 Nov 2026");
        assert_eq!(datetime_with(now, DateStyle::Universal, now).len(), 16);
        assert_eq!(size_with(1500, true), "1.5 KB");
        assert_eq!(size_with(1500, false), "1.5 KB");
    }

    #[test]
    fn filetime() {
        // 2000-01-01T00:00:00Z
        assert_eq!(filetime_to_unix_ms(125_911_584_000_000_000), 946_684_800_000);
        assert_eq!(filetime_to_unix_ms(0), 0);
    }
}
