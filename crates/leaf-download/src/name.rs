//! The name a download is saved under: from the server's Content-Disposition, else the end of the address, and always made safe
//! to join to the chosen folder (no separators, no `..`, no characters or names Windows refuses).

/// The file name for an answer: `Content-Disposition` when it names a file, else the last part of the address path.
pub fn file_name(content_disposition: Option<&str>, url: &str) -> String {
    let raw = content_disposition.and_then(disposition_name).or_else(|| url_name(url)).unwrap_or_default();
    sanitize(&raw)
}

fn disposition_name(cd: &str) -> Option<String> {
    let lower = cd.to_ascii_lowercase();
    if let Some(at) = lower.find("filename*=") {
        let v = cd[at + 10..].split(';').next().unwrap_or("").trim();
        // charset'language'percent-encoded
        let mut parts = v.splitn(3, '\'');
        let (_charset, _lang, enc) = (parts.next(), parts.next(), parts.next());
        if let Some(enc) = enc {
            let s = percent_decode(enc.trim_matches('"'));
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
    }
    // (`filename=` and not `xfilename=`)
    let mut from = 0;
    while let Some(i) = lower[from..].find("filename=") {
        let at = from + i;
        from = at + 9;
        if at > 0 && !matches!(lower.as_bytes()[at - 1], b';' | b' ' | b'\t') {
            continue;
        }
        let v = cd[at + 9..].trim_start();
        let name = if let Some(q) = v.strip_prefix('"') {
            // a quoted name may hold `\"`
            let mut out = String::new();
            let mut esc = false;
            for c in q.chars() {
                if esc {
                    out.push(c);
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    break;
                } else {
                    out.push(c);
                }
            }
            out
        } else {
            v.split(';').next().unwrap_or("").trim().to_string()
        };
        if !name.trim().is_empty() {
            return Some(name);
        }
    }
    None
}

fn url_name(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let path = rest.split_once('/').map(|(_, p)| p)?;
    let last = path.rsplit('/').next().unwrap_or("");
    (!last.is_empty()).then(|| percent_decode(last))
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A single, harmless file name: what is left after the last `/` or `\`, characters that no file system takes replaced, a
/// reserved Windows device name (`CON`, `NUL.txt`, ...) prefixed with `_`, never empty, `.` or `..`.
pub fn sanitize(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = last
        .chars()
        .filter(|c| !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200f}'))
        .map(|c| if c < ' ' || c == '\u{7f}' || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    s = s.trim_matches(|c: char| c == ' ' || c == '.').to_string();
    if s.is_empty() {
        return "download".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    // long names keep their extension
    if s.chars().count() > 180 {
        let (stem, ext) = match s.rfind('.') {
            Some(i) if s[i..].chars().count() <= 16 => (&s[..i], &s[i..]),
            _ => (s.as_str(), ""),
        };
        let keep = 180usize.saturating_sub(ext.chars().count());
        s = format!("{}{ext}", stem.chars().take(keep).collect::<String>());
    }
    s
}
