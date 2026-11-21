//! Secret guard: files that look like they hold secrets, by name and by content.

use regex::Regex;
use std::sync::OnceLock;

/// What kind of secret a file name or a finding points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    EnvFile,
    PrivateKey,
    Certificate,
    Credentials,
    PasswordStore,
    AccessToken,
    CloudKey,
    Assignment,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::EnvFile => "environment file",
            Kind::PrivateKey => "private key",
            Kind::Certificate => "key or certificate bundle",
            Kind::Credentials => "credentials file",
            Kind::PasswordStore => "password store",
            Kind::AccessToken => "access token",
            Kind::CloudKey => "cloud or service key",
            Kind::Assignment => "password or secret assignment",
        }
    }
}

/// One content finding: 1-based line and an excerpt with the secret value cut short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub line: usize,
    pub excerpt: String,
}

/// Judges a file by its name alone (no path, no reading).
pub fn looks_secret(name: &str) -> Option<Kind> {
    let n = name.rsplit(['\\', '/']).next().unwrap_or(name).to_ascii_lowercase();
    if n == ".env" || (n.starts_with(".env.") && !is_env_template(&n)) || (n.ends_with(".env") && !n.contains("example")) {
        return Some(Kind::EnvFile);
    }
    if matches!(n.as_str(), "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") {
        return Some(Kind::PrivateKey);
    }
    if n == ".npmrc" || n == ".netrc" || n == "_netrc" || n == ".pypirc" || n == ".git-credentials" {
        return Some(Kind::AccessToken);
    }
    if n.starts_with("credentials") && !n.ends_with(".md") {
        return Some(Kind::Credentials);
    }
    let ext = n.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "pem" | "key" | "ppk" =>Some(Kind::PrivateKey),
        "pfx" | "p12" | "jks" | "keystore" => Some(Kind::Certificate),
        "kdbx" | "kdb" | "1pux" | "opvault" => Some(Kind::PasswordStore),
        _ => None,
    }
}

fn is_env_template(n: &str) -> bool {
    n.ends_with(".example") || n.ends_with(".sample") || n.ends_with(".template") || n.ends_with(".dist") || n.ends_with(".tpl")
}

struct Rule {
    kind: Kind,
    re: Regex,
}

fn rules() -> &'static [Rule] {
    static R: OnceLock<Vec<Rule>> = OnceLock::new();
    R.get_or_init(|| {
        let mk = |kind, p: &str| Rule { kind, re: Regex::new(p).expect("secret pattern") };
        vec![
            mk(Kind::PrivateKey, r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----"),
            mk(Kind::CloudKey, r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            mk(Kind::AccessToken, r"\bgh[pousr]_[A-Za-z0-9]{36,}"),
            mk(Kind::AccessToken, r"\bgithub_pat_[A-Za-z0-9_]{22,}"),
            mk(Kind::AccessToken, r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
            mk(Kind::AccessToken, r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}"),
            mk(Kind::AccessToken, r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
            mk(Kind::AccessToken, r"\bsk-(?:proj-)?[A-Za-z0-9_-]{32,}"),
        ]
    })
}

fn assign_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?i)\b(?:password|passwd|pwd|secret|api_?key|secret_?key|access_?token|auth_?token)["']?\s*[:=]\s*["']?([^\s"'`,;]{6,})"#).expect("assign pattern")
    })
}

fn placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    if v.contains(['<', '>', '{', '}', '$', '%', '(', ')', '[', ']']) || v.starts_with("...") {
        return true;
    }
    const WORDS: [&str; 14] = [
        "xxx", "changeme", "change_me", "example", "your", "placeholder", "password", "secret", "dummy", "sample", "none", "null", "redacted", "todo",
    ];
    if WORDS.iter().any(|w| l.contains(w)) || matches!(l.as_str(), "true" | "false" | "undefined") {
        return true;
    }
    if v.chars().all(|c| c == '*' || c == '.' || c == '#' || c == '-' || c == '_') {
        return true;
    }
    let first = v.chars().next();
    v.chars().all(|c| Some(c) == first)
}

fn redact(line: &str, start: usize, end: usize) -> String {
    let secret = &line[start..end];
    let keep: String = secret.chars().take(4).collect();
    let mut out = String::new();
    out.push_str(line[..start].trim_start());
    out.push_str(&keep);
    out.push_str("****");
    out.push_str(line[end..].trim_end());
    if out.chars().count() > 100 {
        out = out.chars().take(100).collect::<String>() + "...";
    }
    out
}

/// Scans text for secrets; at most one finding per line.
pub fn scan(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.len() > 4000 {
            continue;
        }
        let mut hit = None;
        for r in rules() {
            if let Some(m) = r.re.find(line) {
                // a private-key header has no value to cut short
                if r.kind == Kind::PrivateKey {
                    hit = Some((r.kind, line.trim().to_string()));
                } else {
                    hit = Some((r.kind, redact(line, m.start(), m.end())));
                }
                break;
            }
        }
        if hit.is_none() {
            if let Some(c) = assign_re().captures(line) {
                let v = c.get(1).unwrap();
                if !placeholder(v.as_str()) {
                    hit = Some((Kind::Assignment, redact(line, v.start(), v.end())));
                }
            }
        }
        if let Some((kind, excerpt)) = hit {
            out.push(Finding { kind, line: i + 1, excerpt });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_contents() {
        for (n, want) in [
            (".env", true),
            (".env.local", true),
            (".env.example", false),
            ("id_rsa", true),
            ("id_rsa.pub", false),
            ("server.PEM", true),
            ("a.key", true),
            ("x.pfx", true),
            (".npmrc", true),
            ("credentials.json", true),
            ("vault.kdbx", true),
            ("main.rs", false),
            ("keyboard.rs", false),
        ] {
            assert_eq!(looks_secret(n).is_some(), want, "{n}");
        }
        // samples are built from parts so no literal looks like a real key
        let aws = format!("{}{}", "AKIA", "B".repeat(16));
        let gh = format!("{}_{}", "ghp", "c".repeat(36));
        let ant = format!("{}-{}", "sk-ant", "d1".repeat(15));
        let hdr = format!("-----BEGIN {} KEY-----", "RSA PRIVATE");
        let pw = format!("password = \"{}\"", "q7Zr!x92");
        let text = format!("fine line\nid = {aws}\ntoken {gh}\n{ant}\n{hdr}\n{pw}\npassword = \"changeme\"\npassword = get_pw()\n");
        let f = scan(&text);
        let lines: Vec<usize> = f.iter().map(|x| x.line).collect();
        assert_eq!(lines, vec![2, 3, 4, 5, 6]);
        assert!(f[0].excerpt.contains("AKIA****") && !f[0].excerpt.contains(&aws));
        assert_eq!(f[4].kind, Kind::Assignment);
    }
}
