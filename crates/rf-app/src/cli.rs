//! The documented command line (`docs/cli.md`): `leaf <path>`, `leaf reveal <file>`, `leaf --new-tab <path>`.
//! The arguments are brought into the shape the start-up code reads (Explorer's `/select,<file>` for `reveal`, absolute paths,
//! no `--new-tab`), because the window that takes the request over may run in another folder.

use std::path::Path;

/// The marker a hand-over message carries when the folder must open as a tab of the running window, whatever the setting
/// "open folders from other programs in a new window" says.
pub const NEW_TAB_PREFIX: &str = "\u{1}new-tab:";

/// The command line after normalising: the arguments (the program's name first, as `std::env::args` gives them) and whether
/// `--new-tab` was asked for.
pub struct Cli {
    pub args: Vec<String>,
    pub new_tab: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Cli {
    let mut it = args.into_iter();
    let mut out: Vec<String> = it.next().into_iter().collect();
    let mut new_tab = false;
    let mut reveal_next = false;
    let mut first_positional = true;
    for a in it {
        if a == "--new-tab" {
            new_tab = true;
            continue;
        }
        if reveal_next {
            reveal_next = false;
            out.push(format!("/select,{}", absolute(&a)));
            continue;
        }
        if first_positional && a.eq_ignore_ascii_case("reveal") && !Path::new(&a).exists() {
            first_positional = false;
            reveal_next = true;
            continue;
        }
        if !a.starts_with('-') {
            first_positional = false;
        }
        out.push(if is_plain_relative(&a) { absolute(&a) } else { a });
    }
    Cli { args: out, new_tab }
}

/// A relative file name that is there (not a switch, a shell name or a URL).
fn is_plain_relative(a: &str) -> bool {
    let t = a.trim().trim_matches('"');
    !t.is_empty() && !t.starts_with(['-', '/', '\\']) && !t.contains("://") && !t.starts_with("::") && !t.to_ascii_lowercase().starts_with("shell:") && Path::new(t).is_relative() && Path::new(t).exists()
}

fn absolute(a: &str) -> String {
    let t = a.trim().trim_matches('"');
    match std::path::absolute(t) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => t.to_string(),
    }
}
