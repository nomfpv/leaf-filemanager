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

    /// What a message calls the sum ("SHA-256": the program differs by system).
    pub fn label(self) -> &'static str {
        match self {
            Algo::Sha256 => "SHA-256",
            Algo::Xxh => "xxhsum",
        }
    }
}

/// `s` as one word for a POSIX shell: in single quotes, a quote inside as `'\''`.
pub fn quote(s: &str) -> String {
    // (a raw string: in a plain one `\'` is just a quote, and `'''` would leave the word open)
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The path of an address as a shell word: `/~` and `/~/x` stand for the user's home folder (as the SFTP side takes them).
pub fn shell_path(p: &str) -> String {
    match p.strip_prefix("/~") {
        Some("") => "\"$HOME\"".to_string(),
        Some(rest) if rest.starts_with('/') => format!("\"$HOME\"{}", quote(rest)),
        _ => quote(p),
    }
}

/// The command line that prints the sum of the file at `path`. The tool is picked on the server by asking the shell which one
/// exists (not by the kind of system alone): `sha256sum` (GNU, BusyBox), `sha256 -r` (FreeBSD, OpenBSD, NetBSD), `shasum -a 256`
/// (macOS, Perl); `xxhsum` or `xxh64sum`. Exit code 127 when the server has none of them. The path is one quoted word.
pub fn command(algo: Algo, path: &str) -> String {
    let f = shell_path(path);
    match algo {
        Algo::Sha256 => format!(
            "f={f}; if command -v sha256sum >/dev/null 2>&1; then sha256sum -- \"$f\"; \
             elif command -v sha256 >/dev/null 2>&1; then sha256 -r \"$f\" 2>/dev/null || sha256 \"$f\"; \
             elif command -v shasum >/dev/null 2>&1; then shasum -a 256 -- \"$f\"; else exit 127; fi"
        ),
        Algo::Xxh => format!(
            "f={f}; if command -v xxhsum >/dev/null 2>&1; then xxhsum -- \"$f\"; \
             elif command -v xxh64sum >/dev/null 2>&1; then xxh64sum -- \"$f\"; else exit 127; fi"
        ),
    }
}

fn is_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The hash a `sha256sum`/`xxhsum` run printed (`hash  name`), in lower case; none when the output is not one.
pub fn parse_output(algo: Algo, out: &str) -> Option<String> {
    let line = out.lines().find(|l| !l.trim().is_empty())?;
    let lens: &[usize] = match algo {
        Algo::Sha256 => &[64],
        Algo::Xxh => &[8, 16, 32],
    };
    // (`hash  name` first: a file name that looks like the BSD form must not be read as one; the BSD tools without `-r` print
    // `SHA256 (name) = hash`, and the hash is what follows the last `) = `)
    let first = line.split_whitespace().next()?;
    if is_hex(first, lens) {
        return Some(first.to_ascii_lowercase());
    }
    if line.starts_with("SHA256 (") && let Some((_, hash)) = line.rsplit_once(") = ") {
        let hash = hash.trim();
        return is_hex(hash, lens).then(|| hash.to_ascii_lowercase());
    }
    None
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
        assert_eq!(quote("a'b"), r"'a'\''b'");
        // the path is one quoted word, the tool is the one the server has
        let c = command(Algo::Sha256, "/srv/x y");
        assert!(c.starts_with("f='/srv/x y'; if command -v sha256sum") && c.contains("sha256 -r \"$f\"") && c.contains("shasum -a 256 -- \"$f\""));
        assert!(command(Algo::Xxh, "/~/a").starts_with("f=\"$HOME\"'/a'; if command -v xxhsum"));
    }

    #[test]
    fn parses_output_and_sidecars() {
        assert_eq!(parse_output(Algo::Sha256, &format!("{H}  /srv/f\n")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Sha256, "sha256sum: f: No such file"), None);
        // BSD: `sha256 -r` (hash name) and plain `sha256` (tagged); a menu shell's text is no sum
        assert_eq!(parse_output(Algo::Sha256, &format!("{H} /srv/f\n")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Sha256, &format!("SHA256 (/srv/f) = {H}\n")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Sha256, "Enter an option: "), None);
        // a file named like the tagged form does not choose the hash
        let fake = "f".repeat(64);
        assert_eq!(parse_output(Algo::Sha256, &format!("{H}  /dir/x (y) = {fake}
")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Sha256, &format!("SHA256 (x) = {fake}) = {H}
")).as_deref(), Some(H));
        assert_eq!(parse_output(Algo::Xxh, "0123456789ABCDEF  f").as_deref(), Some("0123456789abcdef"));
        assert_eq!(parse_sidecar(&format!("{H} *f.iso\n"), "f.iso").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("SHA256 (f.iso) = {H}"), "f.iso").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("{H}\n"), "other").as_deref(), Some(H));
        assert_eq!(parse_sidecar(&format!("{H}  a\n{H}  b\n"), "c"), None);
        assert_eq!(parse_sidecar(&format_sidecar(H, "f"), "f").as_deref(), Some(H));
    }
}
