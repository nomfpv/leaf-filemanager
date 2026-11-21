//! The full path as text for the address bar, the tabs and the window title (settings `address_full_path`, `tab_full_path`,
//! `title_full_path`). Pure functions of a location, so they are tested without a window. Places that are no folder of the
//! disk (Home, Recycle Bin, search results, settings, FTP, ...) keep their own titles.

use std::path::Path;

use rf_core::history::{redact_credentials, Location};

/// The path text of a folder of the disk; `None` for every other place. An archive reads `<archive path> > inner/path`.
pub fn path_text(loc: &Location) -> Option<String> {
    let Location::Path(p) = loc else { return None };
    let s = p.to_string_lossy();
    if s.as_bytes().get(..6).is_some_and(|b| b.eq_ignore_ascii_case(b"ftp://")) {
        return None;
    }
    if let Some((archive, inner)) = archive_by_name(p) {
        let archive = redact_credentials(&archive.to_string_lossy());
        let inner: Vec<String> = inner.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        return Some(if inner.is_empty() { archive } else { format!("{archive} > {}", inner.join("/")) });
    }
    Some(redact_credentials(&s))
}

/// The outermost ancestor whose name is that of an archive, and the rest of the path (the name only: no disk access).
fn archive_by_name(p: &Path) -> Option<(&Path, &Path)> {
    let archive = p.ancestors().filter(|a| crate::archive_view::is_browsable_by_name(a)).last()?;
    Some((archive, p.strip_prefix(archive).ok()?))
}

/// A tab's title: the folder's `name`, or with `full` the path cut in the middle to `max_chars` characters.
pub fn tab_text(loc: &Location, name: &str, full: bool, max_chars: usize) -> String {
    match path_text(loc).filter(|_| full) {
        Some(t) => elide_middle(&t, max_chars),
        None => name.to_string(),
    }
}

/// The window title's text: the folder's `name`, or with `full` the whole path.
pub fn window_text(loc: &Location, name: &str, full: bool) -> String {
    path_text(loc).filter(|_| full).unwrap_or_else(|| name.to_string())
}

/// `s` cut to `max` characters by "…" in the middle; the last part (the last folder's name) is kept first.
pub fn elide_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 2 {
        return s.to_string();
    }
    let last_len = chars.iter().rev().take_while(|c| !matches!(c, '\\' | '/')).count() + 1;
    let room = max - 1;
    let head = if last_len >= room { 0 } else { (room / 3).min(room - last_len) };
    let tail = room - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> Location {
        Location::Path(s.into())
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows paths")]
    fn off_gives_the_names() {
        let l = path(r"C:\Users\user\Docs");
        assert_eq!(tab_text(&l, "Docs", false, 20), "Docs");
        assert_eq!(window_text(&l, "Docs", false), "Docs");
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows paths")]
    fn short_paths_stay_whole() {
        let l = path(r"C:\Users\user\Docs");
        assert_eq!(tab_text(&l, "Docs", true, 40), r"C:\Users\user\Docs");
        assert_eq!(window_text(&l, "Docs", true), r"C:\Users\user\Docs");
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows paths")]
    fn long_paths_are_cut_in_the_middle_and_keep_the_last_folder() {
        let l = path(r"C:\Users\user\Documents\Projects\Client\Invoices 2026");
        let t = tab_text(&l, "Invoices 2026", true, 30);
        assert_eq!(t.chars().count(), 30);
        assert!(t.starts_with("C:\\") && t.contains('…') && t.ends_with(r"\Invoices 2026"), "{t}");
        // the window title is never cut
        assert_eq!(window_text(&l, "x", true), r"C:\Users\user\Documents\Projects\Client\Invoices 2026");
        // a last name longer than the room is what is left
        let t = elide_middle(r"C:\a\a very long folder name indeed", 12);
        assert_eq!(t.chars().count(), 12);
        assert!(t.starts_with('…') && t.ends_with("indeed"), "{t}");
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows paths")]
    fn a_drive_root_and_a_unc_path() {
        assert_eq!(tab_text(&path(r"C:\"), "C:", true, 20), r"C:\");
        assert_eq!(tab_text(&path(r"\\server\share\team\plans"), "plans", true, 40), r"\\server\share\team\plans");
        let t = tab_text(&path(r"\\server\share\team\plans\2026\drafts"), "drafts", true, 24);
        assert_eq!(t.chars().count(), 24);
        assert!(t.starts_with(r"\\") && t.ends_with(r"\drafts"), "{t}");
    }

    #[test]
    fn virtual_places_keep_their_titles() {
        for l in [Location::Home, Location::RecycleBin, Location::Settings, Location::Search { root: "C:\\".into(), query: "x".into() }, path("ftp://example.com/pub")] {
            assert_eq!(tab_text(&l, "Title", true, 20), "Title");
            assert_eq!(window_text(&l, "Title", true), "Title");
            assert_eq!(path_text(&l), None);
        }
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows paths")]
    fn an_archive_reads_archive_then_inner_path() {
        assert_eq!(path_text(&path(r"C:\Users\user\a.zip")).as_deref(), Some(r"C:\Users\user\a.zip"));
        assert_eq!(path_text(&path(r"C:\Users\user\a.zip\inner\deep")).as_deref(), Some(r"C:\Users\user\a.zip > inner/deep"));
    }
}
