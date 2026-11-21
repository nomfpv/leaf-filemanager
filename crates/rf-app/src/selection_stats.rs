//! Selection details (Settings > "Selection details"): beside the count and size in the status
//! line, how many lines the selected text files hold, how long the selected audio and video play
//! and how many pixels the selected pictures have. Read off the UI thread, with caps so a huge
//! selection never keeps the disk busy; the result says when a cap stopped the count.

use std::io::Read;
use std::path::PathBuf;

use crate::icons::Category;

/// Text files above this size are not counted line by line.
const TEXT_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// All text read for one selection stops at this much.
const TEXT_BUDGET: u64 = 64 * 1024 * 1024;
/// At most this many files of one kind are looked into.
const FILES_PER_KIND: usize = 200;
/// Media properties are slower to read, so fewer.
const MEDIA_MAX: usize = 60;

/// One selected file as the status line knows it.
#[derive(Debug, Clone)]
pub struct Picked {
    pub path: PathBuf,
    pub cat: Category,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// (category, files, bytes) in the order of the first appearance of the category.
    pub by_type: Vec<(Category, u32, u64)>,
    pub lines: u64,
    pub text_files: u32,
    pub play_secs: u64,
    pub media_files: u32,
    pub pixels: u64,
    pub pictures: u32,
    /// A cap stopped some of the counting: the numbers are "at least".
    pub capped: bool,
}

/// Newlines in `r`, plus one for a last line without one. Reads at most `budget` bytes; the second value
/// says whether that stopped it.
pub fn count_lines(mut r: impl Read, budget: u64) -> (u64, bool) {
    let mut buf = [0u8; 64 * 1024];
    let (mut lines, mut read, mut last) = (0u64, 0u64, b'\n');
    loop {
        let want = buf.len().min((budget - read.min(budget)) as usize);
        if want == 0 {
            return (lines, true);
        }
        match r.read(&mut buf[..want]) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                lines += buf[..n].iter().filter(|&&b| b == b'\n').count() as u64;
                last = buf[n - 1];
                read += n as u64;
            }
        }
    }
    if last != b'\n' {
        lines += 1;
    }
    (lines, false)
}

/// "1:02:03", "02:03" or "00:03:45" as seconds; None for anything else.
pub fn parse_duration(s: &str) -> Option<u64> {
    let mut secs = 0u64;
    let mut parts = 0;
    for p in s.trim().split(':') {
        let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        secs = secs.checked_mul(60)?.checked_add(digits.parse::<u64>().ok()?)?;
        parts += 1;
    }
    (1..=3).contains(&parts).then_some(secs)
}

/// "2560 x 1440" (any separator, any digit grouping marks around it) as (width, height).
pub fn parse_dimensions(s: &str) -> Option<(u32, u32)> {
    let mut n = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>().ok());
    Some((n.next()??, n.next()??))
}

/// "3 h 02 min", "4 min 05 s", "12 s".
pub fn play_time(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

/// "2.1 MP" (below one megapixel: the plain count).
pub fn megapixels(px: u64) -> String {
    if px >= 100_000 { format!("{:.1} MP", px as f64 / 1_000_000.0) } else { format!("{px} px") }
}

pub fn category_label(c: Category) -> &'static str {
    use crate::t;
    match c {
        Category::Folder => t!("Folders"),
        Category::Image => t!("Pictures"),
        Category::Video => t!("Videos"),
        Category::Audio => t!("Audio"),
        Category::Archive => t!("Archives"),
        Category::Code => t!("Code"),
        Category::Document => t!("Documents"),
        Category::Spreadsheet => t!("Spreadsheets"),
        Category::Pdf => t!("PDF files"),
        Category::Executable => t!("Programs"),
        Category::Text => t!("Text files"),
        Category::Other => t!("Other files"),
    }
}

/// The popover's rows: (label, value) per type, then the extra numbers.
pub fn breakdown(st: &Stats) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for &(cat, n, bytes) in &st.by_type {
        let value = if cat == Category::Folder { n.to_string() } else { format!("{n} · {}", rf_core::format::size(bytes)) };
        rows.push((category_label(cat).to_string(), value));
    }
    let more = if st.capped { "+" } else { "" };
    if st.text_files > 0 {
        rows.push((crate::t!("Lines").to_string(), format!("{}{more} ({} {})", st.lines, st.text_files, crate::t!("files"))));
    }
    if st.media_files > 0 {
        rows.push((crate::t!("Play time").to_string(), format!("{}{more} ({} {})", play_time(st.play_secs), st.media_files, crate::t!("files"))));
    }
    if st.pictures > 0 {
        rows.push((crate::t!("Pixels").to_string(), format!("{}{more} ({} {})", megapixels(st.pixels), st.pictures, crate::t!("files"))));
    }
    rows
}

/// Reads what the status line shows for these files. Blocking: run it off the UI thread.
pub fn compute(files: Vec<Picked>) -> Stats {
    let mut st = Stats::default();
    for f in &files {
        match st.by_type.iter_mut().find(|(c, _, _)| *c == f.cat) {
            Some(e) => {
                e.1 += 1;
                e.2 += f.size;
            }
            None => st.by_type.push((f.cat, 1, f.size)),
        }
    }
    let mut budget = TEXT_BUDGET;
    for f in files.iter().filter(|f| matches!(f.cat, Category::Text | Category::Code)) {
        if st.text_files as usize >= FILES_PER_KIND || budget == 0 || f.size > TEXT_MAX_BYTES {
            st.capped = true;
            continue;
        }
        if let Ok(file) = std::fs::File::open(&f.path) {
            let (n, stopped) = count_lines(file, budget.min(f.size.max(1) + 1));
            st.lines += n;
            st.text_files += 1;
            budget = budget.saturating_sub(f.size);
            st.capped |= stopped && f.size > budget;
        }
    }
    for f in files.iter().filter(|f| matches!(f.cat, Category::Audio | Category::Video)) {
        if st.media_files as usize >= MEDIA_MAX {
            st.capped = true;
            break;
        }
        let secs = leaf_sys::props::media_props(&f.path).into_iter().find(|(k, _)| *k == "Duration").and_then(|(_, v)| parse_duration(&v));
        if let Some(s) = secs {
            st.play_secs += s;
            st.media_files += 1;
        }
    }
    let mut looked = 0usize;
    for f in files.iter().filter(|f| f.cat == Category::Image) {
        if st.pictures as usize >= FILES_PER_KIND {
            st.capped = true;
            break;
        }
        // JPEG XL and AVIF have their own header reader; the others are asked of Windows (slower, so fewer)
        let dims = leaf_decode::dimensions(&f.path).or_else(|| {
            if looked >= MEDIA_MAX {
                st.capped = true;
                return None;
            }
            looked += 1;
            leaf_sys::props::media_props(&f.path).into_iter().find(|(k, _)| *k == "Dimensions").and_then(|(_, v)| parse_dimensions(&v))
        });
        if let Some((w, h)) = dims {
            st.pixels += w as u64 * h as u64;
            st.pictures += 1;
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_durations_and_the_cap() {
        assert_eq!(count_lines(&b"a\nb\nc"[..], 100), (3, false));
        assert_eq!(count_lines(&b"a\nb\n"[..], 100), (2, false));
        assert_eq!(count_lines(&b""[..], 100), (0, false));
        assert_eq!(count_lines(&b"a\nb\nc\nd\n"[..], 4), (2, true));
        assert_eq!(parse_duration("00:03:45"), Some(225));
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_dimensions("2560 x 1440"), Some((2560, 1440)));
        assert_eq!(parse_dimensions("640"), None);
        assert_eq!(play_time(3723), "1 h 02 min");
        assert_eq!(megapixels(2_073_600), "2.1 MP");
    }
}
