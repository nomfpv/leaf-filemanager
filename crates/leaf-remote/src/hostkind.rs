//! What kind of machine an SSH server is (H11), asked once per connection with `uname -s` over an exec channel and kept with
//! the connection. The exec features (checksums, server copy) pick their tool set from it; where a tool matters they also probe
//! for the tool itself (`checksum::command`, `server_copy::PROBE`), because a family only says what is usual.
//!
//! A server that runs no commands (an sftp-only account, Windows OpenSSH), a login that lands in a menu shell (OPNsense's root
//! shell) or one that does not answer in 5 seconds is [`HostFamily::Unknown`]: never an error, never a hang.

use std::time::Duration;

/// How long the question may take.
pub const ASK_LIMIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HostFamily {
    Linux,
    FreeBsd,
    OpenBsd,
    NetBsd,
    MacOs,
    /// Linux with BusyBox's small tools (routers, containers, NAS boxes).
    BusyBox,
    /// No exec, no answer, or an answer that is not `uname`'s.
    #[default]
    Unknown,
}

impl HostFamily {
    /// A BSD (the BSD tool set: `sha256 -r`, `stat -f`, `df -k`, `tail -F`).
    pub fn is_bsd(self) -> bool {
        matches!(self, HostFamily::FreeBsd | HostFamily::OpenBsd | HostFamily::NetBsd | HostFamily::MacOs)
    }

    /// The name leaf shows ("FreeBSD").
    pub fn name(self) -> &'static str {
        match self {
            HostFamily::Linux => "Linux",
            HostFamily::FreeBsd => "FreeBSD",
            HostFamily::OpenBsd => "OpenBSD",
            HostFamily::NetBsd => "NetBSD",
            HostFamily::MacOs => "macOS",
            HostFamily::BusyBox => "BusyBox",
            HostFamily::Unknown => "unknown",
        }
    }
}

/// The question: prints `leaf-uname <uname -s>` and, when the system's own `ls` is BusyBox's, `leaf-busybox`. The command is
/// written for a POSIX shell (the exec channel wraps it in `sh -c`, so a csh login shell parses nothing of it).
pub const PROBE: &str = "echo \"leaf-uname $(uname -s 2>/dev/null)\"; if ls --help 2>&1 | head -n 3 | grep -qi busybox; then echo leaf-busybox; fi";

/// The family a [`PROBE`] run's output names. Anything that does not hold a `leaf-uname` line (a menu shell's text, an error)
/// is `Unknown`; the last such line counts (a login banner may print anything before it).
pub fn parse_probe(out: &str) -> HostFamily {
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let Some(uname) = lines.iter().rev().find_map(|l| l.strip_prefix("leaf-uname ")) else { return HostFamily::Unknown };
    let busybox = lines.contains(&"leaf-busybox");
    match uname.trim().to_ascii_lowercase().as_str() {
        "linux" if busybox => HostFamily::BusyBox,
        "linux" => HostFamily::Linux,
        "freebsd" | "dragonfly" | "midnightbsd" | "hardenedbsd" => HostFamily::FreeBsd,
        "openbsd" => HostFamily::OpenBsd,
        "netbsd" => HostFamily::NetBsd,
        "darwin" => HostFamily::MacOs,
        _ => HostFamily::Unknown,
    }
}

/// `command` as one `sh -c` word: a server whose login shell is csh or tcsh (FreeBSD's root, OPNsense) has the POSIX syntax of
/// leaf's commands read by `sh` and not by itself.
///
/// The login shell still parses the outer word, and csh/tcsh do not read a single-quoted word as POSIX does: `!` is history
/// expansion even inside quotes, a line break inside quotes is an error, tcsh's `backslash_quote` makes `\'` inside quotes
/// a quote, fish reads `\\` and `\'` inside quotes. So the outer word holds only letters, digits, blanks and `_./-=:,+@`: every
/// other byte of the command is written as a `printf` octal escape (`\047`, a `%` as `%%`), and `sh` rebuilds the command
/// with its own `printf` and runs it with `eval`. (The command cannot come on standard input: uploads send the file there.)
pub fn posix(command: &str) -> String {
    let mut fmt = String::with_capacity(command.len() * 2);
    for (i, b) in command.bytes().enumerate() {
        match b {
            b'%' => fmt.push_str("%%"),
            // (a leading `-` would be an option of printf)
            b'-' if i == 0 => fmt.push_str("\\055"),
            b if b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'.' | b'/' | b'-' | b'=' | b':' | b',' | b'+' | b'@') => fmt.push(b as char),
            b => fmt.push_str(&format!("\\{b:03o}")),
        }
    }
    // (`exec`: a login shell that runs its last command as a child would leave `$$`, the pid the copy prints, not the leader of its
    // process group, and `kill -- -pid` would fail; csh, tcsh and fish have `exec`. `eval` runs in that same `sh`: `$$` and an
    // `exec` inside the command keep that pid.)
    format!("exec sh -c 'eval \"$(printf \"{fmt}\")\"'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_probe_parsing() {
        assert_eq!(parse_probe("leaf-uname Linux\n"), HostFamily::Linux);
        assert_eq!(parse_probe("leaf-uname FreeBSD\n"), HostFamily::FreeBsd);
        assert_eq!(parse_probe("leaf-uname OpenBSD\n"), HostFamily::OpenBsd);
        assert_eq!(parse_probe("leaf-uname NetBSD\n"), HostFamily::NetBsd);
        assert_eq!(parse_probe("leaf-uname Darwin\n"), HostFamily::MacOs);
        // BusyBox is Linux with BusyBox's tools
        assert_eq!(parse_probe("leaf-uname Linux\nleaf-busybox\n"), HostFamily::BusyBox);
        // a banner before the answer does not matter; the last answer counts
        assert_eq!(parse_probe("Welcome to the box\nleaf-uname FreeBSD\n"), HostFamily::FreeBsd);
        // a menu shell (OPNsense root), an error, nothing, another system: unknown, never a failure
        assert_eq!(parse_probe("\n  0) Logout                              7) Ping host\n  1) Assign interfaces   8) Shell\nEnter an option: "), HostFamily::Unknown);
        assert_eq!(parse_probe("sh: uname: not found\n"), HostFamily::Unknown);
        assert_eq!(parse_probe(""), HostFamily::Unknown);
        assert_eq!(parse_probe("leaf-uname \n"), HostFamily::Unknown);
        assert_eq!(parse_probe("leaf-uname MINGW64_NT-10.0\n"), HostFamily::Unknown);
        assert!(HostFamily::FreeBsd.is_bsd() && !HostFamily::Linux.is_bsd() && !HostFamily::Unknown.is_bsd());
    }

    #[test]
    fn posix_wrapper_quotes_everything() {
        let w = posix("echo 'a b'; echo $HOME 100% !x\\");
        assert_eq!(w, r#"exec sh -c 'eval "$(printf "echo \047a b\047\073 echo \044HOME 100%% \041x\134")"'"#);
        // the part the login shell reads holds no quote, `!`, line break or backslash before a quote
        let inner = &w["exec sh -c '".len()..w.len() - 1];
        assert!(!inner.contains(['\'', '!', '\n']) && !inner.contains("\\'") && !inner.contains("\\\\"));
        assert!(posix("-x").contains("printf \"\\055x\""));
    }
}
