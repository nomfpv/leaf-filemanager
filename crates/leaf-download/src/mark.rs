//! The Mark of the Web: a finished download gets a `Zone.Identifier` stream (zone 3, the Internet) as a browser would give it, so
//! SmartScreen and Office's Protected View still warn about a `setup.exe` or a document from a web page. Windows only; a disk that
//! has no streams (FAT, exFAT, a network share that refuses them) leaves the file unmarked, as it would for a browser.

#[cfg(any(windows, test))]
/// The address as it goes into the mark: without a login (`user@`, `user:secret@`) and without the `#fragment`.
fn clean(url: &str) -> String {
    let url = url.trim();
    let url = &url[..url.find('#').unwrap_or(url.len())];
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?']).unwrap_or(rest.len());
            let (authority, tail) = rest.split_at(end);
            let host = authority.rsplit('@').next().unwrap_or("");
            format!("{scheme}://{host}{tail}")
        }
        None => url.to_string(),
    }
}

#[cfg(any(windows, test))]
/// The text of the stream: zone 3, the page the download came from (when the browser said) and the file's address.
pub(crate) fn zone_text(url: &str, referer: Option<&str>) -> String {
    let mut s = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(r) = referer.map(clean).filter(|r| r.starts_with("http://") || r.starts_with("https://")) {
        s.push_str(&format!("ReferrerUrl={r}\r\n"));
    }
    s.push_str(&format!("HostUrl={}\r\n", clean(url)));
    s
}

/// Marks `file` as come from the Internet (see the module). A failure is logged, never an error of the download.
#[cfg(windows)]
pub(crate) fn mark(file: &std::path::Path, url: &str, referer: Option<&str>) {
    let mut stream = file.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if let Err(e) = std::fs::write(std::path::Path::new(&stream), zone_text(url, referer)) {
        log::info!("download: the file could not be marked as from the Internet ({e})");
    }
}

#[cfg(not(windows))]
pub(crate) fn mark(_file: &std::path::Path, _url: &str, _referer: Option<&str>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_names_the_addresses_without_a_login() {
        let t = zone_text("https://me:secret@example.com/a/setup.exe?x=1#top", Some("https://user@example.com/page"));
        assert_eq!(t, "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/page\r\nHostUrl=https://example.com/a/setup.exe?x=1\r\n");
        assert!(!zone_text("https://example.com/f", Some("javascript:x")).contains("ReferrerUrl"));
    }
}
