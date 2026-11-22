//! Explorer's command line (`explorer.exe [/n][/e][,/root,<object>][[,/select],<sub object>]`, Microsoft's KB 152457), read the way
//! Explorer reads it, so a launch meant for Explorer (Win+E, "Show in folder", `explorer.exe <path>` from another program) opens the
//! right thing in leaf, and what leaf cannot show goes back to Explorer.
//!
//! Pure: no file system. The caller decides what a target is (folder, file, a namespace leaf lists itself) and makes relative paths
//! absolute.

/// What an Explorer-style command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// No target (Win+E, a bare `explorer.exe`, `/e,` alone): leaf's usual start.
    Home,
    /// Open this (a folder, or a path the caller still has to check: Explorer also takes files).
    Open(String),
    /// Open `folder` with `file` selected (`/select,<file>`).
    Select { folder: String, file: String },
    /// A shell namespace (`::{CLSID}`, `shell:…`): leaf opens it only if it lists that one itself, else Explorer.
    Namespace(String),
    /// Something leaf does not read (`/idlist`, an unknown switch, a namespace item to select): the whole line goes to Explorer.
    FallThrough,
}

/// Whether `s` names a shell namespace rather than a file system path.
pub fn is_namespace(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    // (a `search-ms:` link and a Settings page are no paths either: leaf reads the first itself, Explorer gets the rest)
    l.starts_with("::") || l.starts_with("shell:") || l.contains("::{") || l.starts_with("search-ms:") || l.starts_with("ms-settings:")
}

/// True when the command line starts with one of Explorer's switches (so it is read with [`parse`], not as a list of folders).
pub fn starts_with_switch(line: &str) -> bool {
    let l = line.trim_start().trim_start_matches('"').to_ascii_lowercase();
    ["/select", "/e", "/n", "/root", "/separate", "/idlist", "-select,"].iter().any(|s| l.starts_with(s) && l[s.len()..].chars().next().is_none_or(|c| c == ',' || c.is_whitespace() || c == '"'))
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"').trim()
}

/// A path is relative to the `/root,` folder when it is not absolute itself (`C:\…`, `\\server\…`, `\…`).
fn is_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('\\') || s.starts_with('/') || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
}

fn join(root: &str, rel: &str) -> String {
    format!("{}\\{}", root.trim_end_matches(['\\', '/']), rel.trim_start_matches(['\\', '/']))
}

/// The folder a file is in (`C:\a\b.txt` -> `C:\a`, `C:\b.txt` -> `C:\`); None for a drive or server root (Explorer would select it
/// in This PC or Network, which leaf does not show).
fn parent(file: &str) -> Option<String> {
    let t = file.trim_end_matches(['\\', '/']);
    let cut = t.rfind(['\\', '/'])?;
    let dir = &t[..cut];
    if dir.is_empty() || dir.trim_start_matches(['\\', '/']).is_empty() {
        return None; // `\x` or `\\server`
    }
    // `\\server\share` itself has no folder above it we could open with the share selected
    if t.starts_with(r"\\") && !dir[2..].contains(['\\', '/']) {
        return None;
    }
    Some(if dir.len() == 2 && dir.ends_with(':') { format!("{dir}\\") } else { dir.to_string() })
}

/// Reads an Explorer command line: the arguments after the program's name, joined with spaces (`std::env::args` has taken the
/// quotes off; joining gives back paths with spaces that a caller left unquoted, which Explorer accepts too).
///
/// The value of `/select,` runs to the end of the line (it is the last part in Explorer's syntax), so a file name with a comma in it
/// stays whole. Other values end at the next `,/` switch.
pub fn parse(line: &str) -> Launch {
    let mut rest = line.trim();
    let mut root: Option<String> = None;
    let mut target: Option<String> = None;
    let mut select: Option<String> = None;
    loop {
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        if rest.is_empty() {
            break;
        }
        let quoted_switch = rest.starts_with("\"/");
        let r = if quoted_switch { &rest[1..] } else { rest };
        if r.starts_with('/') || r.to_ascii_lowercase().starts_with("-select,") {
            let end = r.find([',', ' ', '\t', '"']).unwrap_or(r.len());
            let name = r[1..end].to_ascii_lowercase();
            let mut after = &r[end..];
            if quoted_switch {
                after = after.strip_prefix('"').unwrap_or(after);
            }
            match name.as_str() {
                "n" | "e" | "separate" => rest = after,
                "select" => {
                    let v = unquote(after.trim_start_matches(|c: char| c == ',' || c.is_whitespace()));
                    select = Some(v.to_string()).filter(|v| !v.is_empty());
                    break;
                }
                "root" => {
                    let a = after.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
                    let end = a.find(",/").unwrap_or(a.len());
                    root = Some(unquote(&a[..end]).to_string()).filter(|v| !v.is_empty());
                    rest = &a[end..];
                }
                // `/idlist,:<handle>:<pid>` (an ID list in another process's memory) and anything we do not know
                _ => return Launch::FallThrough,
            }
        } else {
            let end = rest.find(",/").unwrap_or(rest.len());
            let v = unquote(&rest[..end]);
            if !v.is_empty() {
                target = Some(v.to_string());
            }
            rest = &rest[end..];
        }
    }
    // a relative object or sub object is inside the root
    let in_root = |v: String| match &root {
        Some(r) if !is_absolute(&v) && !is_namespace(&v) && !is_namespace(r) => join(r, &v),
        _ => v,
    };
    if let Some(file) = select {
        let file = in_root(file);
        if is_namespace(&file) {
            return Launch::FallThrough;
        }
        return match parent(&file) {
            Some(folder) => Launch::Select { folder, file },
            None => Launch::FallThrough,
        };
    }
    match target.map(in_root).or(root) {
        None => Launch::Home,
        Some(t) if is_namespace(&t) => Launch::Namespace(t),
        Some(t) => Launch::Open(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(folder: &str, file: &str) -> Launch {
        Launch::Select { folder: folder.into(), file: file.into() }
    }

    #[test]
    fn explorer_command_lines() {
        let open = |s: &str| Launch::Open(s.into());
        let ns = |s: &str| Launch::Namespace(s.into());
        let table: &[(&str, Launch)] = &[
            ("", Launch::Home),
            ("   ", Launch::Home),
            ("/e,", Launch::Home),
            ("/n", Launch::Home),
            // plain paths, quoted or not, with spaces
            (r"C:\Users\user\Documents", open(r"C:\Users\user\Documents")),
            (r#""C:\Users\user\My Files""#, open(r"C:\Users\user\My Files")),
            (r"C:\Users\user\My Files", open(r"C:\Users\user\My Files")),
            (r"\\nas\share\photos", open(r"\\nas\share\photos")),
            (r"D:\", open(r"D:\")),
            // switches before the path
            (r"/e,C:\Users\user", open(r"C:\Users\user")),
            (r"/n,/e,C:\Users\user", open(r"C:\Users\user")),
            (r"/separate,/e,C:\Users\user", open(r"C:\Users\user")),
            (r"/E, C:\Users\user", open(r"C:\Users\user")),
            (r#""/e,C:\Users\user""#, open(r"C:\Users\user")),
            // select: the file's folder, the file selected
            (r"/select,C:\Users\user\a.txt", sel(r"C:\Users\user", r"C:\Users\user\a.txt")),
            (r"/select, C:\Users\user\a.txt", sel(r"C:\Users\user", r"C:\Users\user\a.txt")),
            (r#"/select,"C:\Users\user\My Files\a b.txt""#, sel(r"C:\Users\user\My Files", r"C:\Users\user\My Files\a b.txt")),
            (r"/select,C:\Users\user\My Files\a b.txt", sel(r"C:\Users\user\My Files", r"C:\Users\user\My Files\a b.txt")),
            (r"/SELECT,C:\Users\user\a,b.txt", sel(r"C:\Users\user", r"C:\Users\user\a,b.txt")),
            (r"-select,C:\Users\user\a.txt", sel(r"C:\Users\user", r"C:\Users\user\a.txt")),
            (r"/e,/select,C:\Users\user\a.txt", sel(r"C:\Users\user", r"C:\Users\user\a.txt")),
            (r"/select,C:\a.txt", sel(r"C:\", r"C:\a.txt")),
            (r"/select,C:\Users\user\folder\", sel(r"C:\Users\user", r"C:\Users\user\folder\")),
            (r"/select,\\nas\share\a.txt", sel(r"\\nas\share", r"\\nas\share\a.txt")),
            (r"/select,", Launch::Home),
            // a drive or a share selected in This PC / Network: Explorer's
            (r"/select,C:\", Launch::FallThrough),
            (r"/select,\\nas\share", Launch::FallThrough),
            // root
            (r"/root,C:\Users\user", open(r"C:\Users\user")),
            (r"/root,C:\Users\user,Documents", open(r"C:\Users\user,Documents")),
            (r"/root,C:\Users\user,/select,a.txt", sel(r"C:\Users\user", r"C:\Users\user\a.txt")),
            // namespaces: leaf may list them itself, else Explorer
            ("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", ns("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")),
            ("shell:::{26EE0668-A00A-44D7-9371-BEB064C98683}", ns("shell:::{26EE0668-A00A-44D7-9371-BEB064C98683}")),
            ("shell:Downloads", ns("shell:Downloads")),
            ("/e,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", ns("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")),
            ("/root,::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", ns("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")),
            (r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\::{21EC2020-3AEA-1069-A2DD-08002B30309D}", ns(r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\::{21EC2020-3AEA-1069-A2DD-08002B30309D}")),
            // Network, the Recycle Bin, Libraries, This PC, a phone, a search link, a Settings page: namespaces (what leaf lists opens
            // in leaf, the rest goes to Explorer: decided by the caller)
            ("shell:NetworkPlacesFolder", ns("shell:NetworkPlacesFolder")),
            ("/e,shell:NetworkPlacesFolder", ns("shell:NetworkPlacesFolder")),
            ("/root,shell:RecycleBinFolder", ns("shell:RecycleBinFolder")),
            ("::{031E4825-7B94-4DC3-B131-E946B44C8DD5}", ns("::{031E4825-7B94-4DC3-B131-E946B44C8DD5}")),
            ("/n,/e,shell:MyComputerFolder", ns("shell:MyComputerFolder")),
            ("shell:ControlPanelFolder", ns("shell:ControlPanelFolder")),
            (
                r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\?\usb#vid_18d1&pid_4ee1#3a1#{6ac27878-a6fa-4155-ba85-f98f491d4f33}",
                ns(r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\?\usb#vid_18d1&pid_4ee1#3a1#{6ac27878-a6fa-4155-ba85-f98f491d4f33}"),
            ),
            (r"/e,search-ms:query=report&crumb=location:C%3A%5CUsers%5Cuser", ns(r"search-ms:query=report&crumb=location:C%3A%5CUsers%5Cuser")),
            ("ms-settings:display", ns("ms-settings:display")),
            ("/select,search-ms:query=x", Launch::FallThrough),
            // what leaf does not read
            ("/idlist,:123:456", Launch::FallThrough),
            ("/factory,{75dff2b7-6936-4c06-a8bb-676a7b00b24b}", Launch::FallThrough),
            ("/select,::{645FF040-5081-101B-9F08-00AA002F954E}", Launch::FallThrough),
        ];
        for (line, want) in table {
            assert_eq!(&parse(line), want, "{line:?}");
        }
    }

    #[test]
    fn switch_lines_are_told_from_folder_lists() {
        assert!(starts_with_switch(r"/select,C:\a.txt"));
        assert!(starts_with_switch(r"/e,C:\a"));
        assert!(starts_with_switch("/e"));
        assert!(starts_with_switch(r#""/select,C:\a b.txt""#));
        assert!(starts_with_switch("/idlist,:1:2"));
        assert!(!starts_with_switch(r"C:\Users\user"));
        assert!(!starts_with_switch("/home/user"), "a Linux path is not a switch");
        assert!(!starts_with_switch("/epic"));
        assert!(!starts_with_switch("--new-tab"));
    }
}
