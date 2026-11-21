//! Checksums computed on the server (H2): `sha256sum` or `xxhsum` run over the existing SSH connection, so a big file is not
//! read back over the line just to compare it. Plus the `.sha256` side files (`hash  name` lines) that sit beside a file.
//!
//! Pure parts here (quoting, the command, parsing, the side file); the connection part is `crate::remote_sum`.

/// The sum programs a server is asked to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algo {
    Sha256,
    /// `xxhsum` (XXH64 unless the server's build says otherwise); a speed check, not a security one.
    Xxh,
}

impl Algo {
    pub fn program(self) -> &'static str {
        match self {
            Algo::Sha256 => "sha256sum",
            Algo::Xxh => "xxhsum",
        }
    }
}

/// `s` as one word for a POSIX shell: in single quotes, a quote inside as `'\''`.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\''"))
}

/// The path of an address as a shell word: `/~` and `/~/x` stand for the user's home folder (as the SFTP side takes them).
pub fn shell_path(p: &str) -> String {
    match p.strip_prefix("/~") {
        Some("") => "\"$HOME\"".to_string(),
        Some(rest) if rest.starts_with('/') => format!("\"$HOME\"{}", quote(rest)),
        _ => quote(p),
    }
}

/// The command line that prints the sum of the file at `path`.
pub fn command(algo: Algo, path: &str) -> String {
    format!("{} -- {}", algo.program(), shell_path(path))
}

fn is_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The hash a `sha256sum`/`xxhsum` run printed (`hash  name`), in lower case; none when the output is not one.
pub fn parse_output(algo: Algo, out: &str) -> Option<String> {
    let first = out.lines().find(|l| !l.trim().is_empty())?.split_whitespace().next()?;
    let lens: &[usize] = match algo {
        Algo::Sha256 => &[64],
        Algo::Xxh => &[8, 16, 32],
    };
    is_hex(first, lens).then(|| first.to_ascii_lowercase())
}

/// The SHA-256 a side file (`file.sha256`) holds for the file called `name`: GNU lines (`hash  name`, `hash *name`), BSD lines
/// (`SHA256 (name) = hash`), or a file of one bare hash.
pub fn parse_sidecar(text: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    for l in &lines {
        if let Some(rest) = l.strip_prefix("SHA256 (")
            && let Some((n, h)) = rest.split_once(") = ")
            && n == name
            && is_hex(h.trim(), &[64])
        {
            return Some(h.trim().to_ascii_lowercase());
        }
        if let Some((h, n)) = l.split_once(char::is_whitespace)
            && is_hex(h, &[64])
        {
            let n = n.trim_start().trim_start_matches('*');
            if n == name {
                return Some(h.to_ascii_lowercase());
            }
        }
    }
    match lines.as_slice() {
        [one] if is_hex(one, &[64]) => Some(one.to_ascii_lowercase()),
        _ => None,
    }
}

/// The text of a side file for one file: `hash  name`, as `sha256sum -c` reads it.
pub fn format_sidecar(sha256: &str, name: &str) -> String {
    format!("{}  {name}\n", sha256.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn quoting_and_home() {
        assert_eq!(quote("a'b"), "'a'\''b'");
        assert_eq!(command(Algo::Sha256, "/srv/x y"), "sha256sum -- '/srv/x y'");
        assert_eq!(command(Algo::Xxh, "/~/a"), "xxhsum -- \"$HOME\"'/a'");
    }

    #[test]
    fn parses_output_and_sidecars() {
        assert_eq!(parse_output(Algo::Sha256, &format!("{H}  /srv/f\n")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Sha256, "sha256sum: f: No such file"), None);
        assert_eq!(parse_output(Algo::Xxh, "0123456789ABCDEF  f").as_deref(), Some("0123456789abcdef"));
        assert_eq!(parse_sidecar(&format!("{H} *f.iso\n"), "f.iso").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("SHA256 (f.iso) = {H}"), "f.iso").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("{H}\n"), "other").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("{H}  a\n{H}  b\n"), "c"), None);
        assert_eq!(parse_sidecar(&format_sidecar(H, "f"), "f").as_deref(), Some(H));
    }
}
