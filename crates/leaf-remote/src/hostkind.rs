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
/// leaf's commands read by `sh` and not by itself. Single-quoted by the same function every path goes through.
pub fn posix(command: &str) -> String {
    // (`exec`: a login shell that runs its last command as a child would leave `$$`, the pid the copy prints, not the leader of its
    // process group, and `kill -- -pid` would fail; csh, tcsh and fish have `exec`)
    format!("exec sh -c {}", crate::checksum::quote(command))
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
        assert_eq!(posix("echo 'a b'; echo $HOME"), r"exec sh -c 'echo '\''a b'\''; echo $HOME'");
    }
}
