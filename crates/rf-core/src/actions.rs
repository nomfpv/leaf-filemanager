//! Custom actions: the user's own commands, written in `actions.toml` in the config folder. Pure (no I/O, no OS): reading the
//! file's text, deciding where an action is offered, and turning its command into the argument list for a selection.
//!
//! ```toml
//! [[action]]
//! name = "Open in VS Code"
//! command = "code {path}"
//! where = ["menu-file", "menu-folder", "palette"]
//! shortcut = "ctrl-alt-v"        # optional, GPUI key syntax
//! extensions = ["png", "jpg"]    # optional: only files of these types
//! confirm = true                 # optional: ask before running
//! terminal = false               # optional: run in a terminal window instead of hidden
//! icon = "code"                  # optional
//! ```
//!
//! The command is split into words the way a command line is (spaces separate, double or single quotes group, a backslash is
//! only a backslash so Windows paths work). Placeholders are expanded after that, so a path with spaces or quotes stays ONE
//! argument and no shell ever sees it: `{path}` the first selected item, `{files}` every selected item (as many arguments),
//! `{dir}` the current folder, `{name}` the first selected item's name. A `{files}` that is only part of a word is written out
//! as the items in double quotes, separated by spaces.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where an action is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    MenuFile,
    MenuFolder,
    MenuBackground,
    Palette,
    Toolbar,
}

impl Place {
    pub const ALL: [Place; 5] = [Place::MenuFile, Place::MenuFolder, Place::MenuBackground, Place::Palette, Place::Toolbar];

    /// The word in `actions.toml`.
    pub fn key(self) -> &'static str {
        match self {
            Place::MenuFile => "menu-file",
            Place::MenuFolder => "menu-folder",
            Place::MenuBackground => "menu-background",
            Place::Palette => "palette",
            Place::Toolbar => "toolbar",
        }
    }

    pub fn from_key(s: &str) -> Option<Place> {
        Place::ALL.into_iter().find(|p| p.key() == s.trim())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CustomAction {
    pub name: String,
    pub command: String,
    /// Empty: the command palette only.
    #[serde(rename = "where", default)]
    pub places: Vec<Place>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "action")]
    actions: Vec<CustomAction>,
}

/// The actions of the file's text. An error (broken TOML, a wrong `where` word) is for the caller to log and ignore; nothing is
/// half-read. An action without a name or a command is left out; the shortcut and the extensions are tidied.
pub fn parse(text: &str) -> Result<Vec<CustomAction>, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out: Vec<CustomAction> = Vec::new();
    for mut a in file.actions {
        a.name = a.name.trim().to_string();
        a.command = a.command.trim().to_string();
        if a.name.is_empty() || a.command.is_empty() || out.iter().any(|o| o.name.eq_ignore_ascii_case(&a.name)) {
            continue;
        }
        a.shortcut = a.shortcut.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        a.extensions = a.extensions.iter().map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect();
        a.icon = a.icon.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if a.places.is_empty() {
            a.places.push(Place::Palette);
        }
        out.push(a);
    }
    Ok(out)
}

/// The file's text for `actions` (what the settings page writes).
pub fn to_toml(actions: &[CustomAction]) -> String {
    toml::to_string_pretty(&File { actions: actions.to_vec() }).unwrap_or_default()
}

impl CustomAction {
    pub fn offered_in(&self, place: Place) -> bool {
        self.places.contains(&place)
    }

    fn extension_ok(&self, path: &Path) -> bool {
        if self.extensions.is_empty() {
            return true;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        self.extensions.iter().any(|e| *e == ext)
    }

    /// Whether the right-click menu of these selected items (path, is a folder) has this action: files need `menu-file` and a
    /// matching extension, folders `menu-folder`; a mix needs both.
    pub fn matches_items(&self, items: &[(PathBuf, bool)]) -> bool {
        if items.is_empty() {
            return false;
        }
        let (mut files, mut dirs) = (false, false);
        for (p, is_dir) in items {
            if *is_dir {
                dirs = true;
            } else {
                files = true;
                if !self.extension_ok(p) {
                    return false;
                }
            }
        }
        (!files || self.offered_in(Place::MenuFile)) && (!dirs || self.offered_in(Place::MenuFolder))
    }

    pub fn matches_background(&self) -> bool {
        self.offered_in(Place::MenuBackground)
    }
}

/// What a command's placeholders stand for.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub files: &'a [PathBuf],
    pub dir: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandError {
    /// The command uses `{path}`, `{files}` or `{name}` and nothing is selected.
    NeedsSelection,
    /// The command uses `{dir}` and there is no folder.
    NeedsFolder,
    /// A quote that is never closed.
    Unclosed,
    /// Nothing to run.
    Empty,
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ExpandError::NeedsSelection => "Select a file or folder first.",
            ExpandError::NeedsFolder => "Open a folder first.",
            ExpandError::Unclosed => "A quote in the command is not closed.",
            ExpandError::Empty => "The command is empty.",
        })
    }
}

/// The words of a command line: spaces separate, `"..."` and `'...'` group (and are dropped), a backslash is literal. An empty
/// quoted word (`""`) stays a word.
pub fn split_words(text: &str) -> Result<Vec<String>, ExpandError> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut live = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                live = true;
            }
            None if c.is_whitespace() => {
                if live {
                    words.push(std::mem::take(&mut cur));
                    live = false;
                }
            }
            None => {
                cur.push(c);
                live = true;
            }
        }
    }
    if quote.is_some() {
        return Err(ExpandError::Unclosed);
    }
    if live {
        words.push(cur);
    }
    Ok(words)
}

fn quoted(p: &Path) -> OsString {
    let mut s = OsString::from("\"");
    s.push(p.as_os_str().to_string_lossy().replace('"', "\\\""));
    s.push("\"");
    s
}

/// The command as program and arguments for `ctx`; the first element is the program.
pub fn expand(command: &str, ctx: &Context) -> Result<Vec<OsString>, ExpandError> {
    let words = split_words(command)?;
    if words.is_empty() {
        return Err(ExpandError::Empty);
    }
    let first = ctx.files.first();
    let mut out: Vec<OsString> = Vec::with_capacity(words.len() + ctx.files.len());
    for w in &words {
        if w == "{files}" {
            if ctx.files.is_empty() {
                return Err(ExpandError::NeedsSelection);
            }
            out.extend(ctx.files.iter().map(|f| f.as_os_str().to_owned()));
            continue;
        }
        let mut cur = OsString::new();
        let mut rest = w.as_str();
        while let Some(at) = rest.find('{') {
            let Some(len) = rest[at..].find('}') else { break };
            let name = &rest[at + 1..at + len];
            let value: Option<OsString> = match name {
                "path" => Some(first.ok_or(ExpandError::NeedsSelection)?.as_os_str().to_owned()),
                "name" => Some(first.ok_or(ExpandError::NeedsSelection)?.file_name().unwrap_or_default().to_owned()),
                "dir" => Some(ctx.dir.ok_or(ExpandError::NeedsFolder)?.as_os_str().to_owned()),
                "files" => {
                    if ctx.files.is_empty() {
                        return Err(ExpandError::NeedsSelection);
                    }
                    let mut all = OsString::new();
                    for (i, f) in ctx.files.iter().enumerate() {
                        if i > 0 {
                            all.push(" ");
                        }
                        all.push(quoted(f));
                    }
                    Some(all)
                }
                _ => None,
            };
            match value {
                Some(v) => {
                    cur.push(&rest[..at]);
                    cur.push(v);
                    rest = &rest[at + len + 1..];
                }
                None => {
                    // not one of ours: written as it is
                    cur.push(&rest[..at + len + 1]);
                    rest = &rest[at + len + 1..];
                }
            }
        }
        cur.push(rest);
        out.push(cur);
    }
    Ok(out)
}

/// The same facts as environment variables for the program (a script can read them without any quoting question):
/// `LEAF_PATH`, `LEAF_NAME`, `LEAF_DIR`, `LEAF_FILES` (one per line).
pub fn environment(ctx: &Context) -> Vec<(&'static str, OsString)> {
    let mut v = Vec::new();
    if let Some(f) = ctx.files.first() {
        v.push(("LEAF_PATH", f.as_os_str().to_owned()));
        v.push(("LEAF_NAME", f.file_name().unwrap_or_default().to_owned()));
        let mut all = OsString::new();
        for (i, f) in ctx.files.iter().enumerate() {
            if i > 0 {
                all.push("\n");
            }
            all.push(f);
        }
        v.push(("LEAF_FILES", all));
    }
    if let Some(d) = ctx.dir {
        v.push(("LEAF_DIR", d.as_os_str().to_owned()));
    }
    v
}

/// The ready-made examples the settings page offers (never installed by itself).
pub fn examples() -> Vec<CustomAction> {
    vec![
        CustomAction {
            name: "Open in VS Code".into(),
            command: "code {path}".into(),
            places: vec![Place::MenuFile, Place::MenuFolder, Place::Palette],
            icon: Some("code".into()),
            ..Default::default()
        },
        CustomAction {
            name: "Copy SHA-256".into(),
            command: "powershell -NoProfile -Command \"(Get-FileHash -LiteralPath $env:LEAF_PATH -Algorithm SHA256).Hash | Set-Clipboard\"".into(),
            places: vec![Place::MenuFile, Place::Palette],
            icon: Some("hash".into()),
            ..Default::default()
        },
        CustomAction {
            name: "Open Git Bash here".into(),
            command: "git-bash --cd={dir}".into(),
            places: vec![Place::MenuBackground, Place::Palette],
            icon: Some("square-terminal".into()),
            ..Default::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn parsing_and_placeholders() {
        let text = r#"
[[action]]
name = " Resize "
command = 'magick {files} -resize 50% "{dir}/out dir"'
where = ["menu-file", "palette"]
shortcut = "Ctrl-Alt-R"
extensions = [".PNG", "jpg"]
confirm = true

[[action]]
name = "No place"
command = "tool {name}"

[[action]]
name = ""
command = "dropped"

[[action]]
name = "resize"
command = "same name, dropped"
"#;
        let a = parse(text).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "Resize");
        assert_eq!(a[0].places, [Place::MenuFile, Place::Palette]);
        assert_eq!(a[0].shortcut.as_deref(), Some("ctrl-alt-r"));
        assert_eq!(a[0].extensions, ["png", "jpg"]);
        assert!(a[0].confirm && !a[0].terminal);
        assert_eq!(a[1].places, [Place::Palette]);
        // what the page writes reads back the same
        assert_eq!(parse(&to_toml(&a)).unwrap(), a);
        // a broken file is an error, not half a list
        assert!(parse("[[action]\nname =").is_err());
        assert!(parse("[[action]]\nname='x'\ncommand='y'\nwhere=['nowhere']").is_err());

        let files = [PathBuf::from("/work/a b.png"), PathBuf::from("/work/it's \"q\".jpg")];
        let dir = PathBuf::from("/work");
        let ctx = Context { files: &files, dir: Some(&dir) };
        // several files: one argument each, spaces and quotes untouched; quotes in the command group words
        assert_eq!(expand(&a[0].command, &ctx).unwrap(), os(&["magick", "/work/a b.png", "/work/it's \"q\".jpg", "-resize", "50%", "/work/out dir"]));
        // inside a longer word: the items in quotes
        let e = expand("run --in={files}", &ctx).unwrap();
        assert_eq!(e[1], OsString::from("--in=\"/work/a b.png\" \"/work/it's \\\"q\\\".jpg\""));
        assert_eq!(expand("tool {name} {path} {nope}", &ctx).unwrap(), os(&["tool", "a b.png", "/work/a b.png", "{nope}"]));
        // nothing selected
        let none = Context { files: &[], dir: Some(&dir) };
        assert_eq!(expand("tool {path}", &none), Err(ExpandError::NeedsSelection));
        assert_eq!(expand("tool {files}", &none), Err(ExpandError::NeedsSelection));
        assert_eq!(expand("tool {name}", &none), Err(ExpandError::NeedsSelection));
        assert_eq!(expand("tool {dir}", &none).unwrap(), os(&["tool", "/work"]));
        assert_eq!(expand("tool {dir}", &Context { files: &[], dir: None }), Err(ExpandError::NeedsFolder));
        assert_eq!(expand("tool \"open", &ctx), Err(ExpandError::Unclosed));
        assert_eq!(expand("  ", &ctx), Err(ExpandError::Empty));
        assert_eq!(expand("tool \"\" x", &ctx).unwrap(), os(&["tool", "", "x"]));

        // where it is offered
        let png = vec![(PathBuf::from("/work/a.PNG"), false)];
        let txt = vec![(PathBuf::from("/work/a.txt"), false)];
        let folder = vec![(PathBuf::from("/work/d"), true)];
        assert!(a[0].matches_items(&png));
        assert!(!a[0].matches_items(&txt));
        assert!(!a[0].matches_items(&folder));
        assert!(!a[0].matches_background());
        assert!(examples().iter().all(|e| parse(&to_toml(std::slice::from_ref(e))).unwrap() == [e.clone()]));
    }
}
