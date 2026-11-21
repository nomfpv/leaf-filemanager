//! Icon packs (X4): the `pack.toml` of a folder in `<config>/icons/<pack>/`, and the question "which file of the pack is the icon
//! of this name". Pure: no file is read here (the app reads the TOML and the pictures).
//!
//! ```toml
//! [pack]
//! name = "My icons"
//! author = "me"
//! licence = "CC0"
//! tint = false              # true: the pictures are one-colour shapes, painted in the theme's icon colour
//! [folder]
//! default = "folder.svg"    # every folder
//! [file]
//! default = "file.svg"      # every file nothing below names
//! [ext]
//! rs = "rust.svg"
//! [name]
//! "Cargo.toml" = "cargo.svg"
//! [folder_name]
//! src = "folder-src.svg"
//! ```

use std::collections::HashMap;

/// A parsed `pack.toml`. Every key of the three maps is lower case (names and extensions match without regard to case).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    pub author: String,
    pub licence: String,
    pub tint: bool,
    pub folder: Option<String>,
    pub file: Option<String>,
    pub ext: HashMap<String, String>,
    pub name_map: HashMap<String, String>,
    pub folder_name: HashMap<String, String>,
}

/// A file name inside a pack: relative, plain, no `..` (a pack cannot point out of its folder).
fn safe(f: &str) -> bool {
    let f = f.trim();
    let sep = |c: char| c == '/' || c == '\u{5c}';
    !f.is_empty() && !f.starts_with(sep) && !f.contains(':') && !f.split(sep).any(|p| p == "..")
}

fn str_of(v: Option<&toml::Value>) -> Option<String> {
    match v? {
        toml::Value::String(s) => Some(s.trim().to_string()),
        // `[folder]` / `[file]`: `default = "..."` (or `icon = "..."`)
        toml::Value::Table(t) => t.get("default").or_else(|| t.get("icon")).and_then(|i| i.as_str()).map(|s| s.trim().to_string()),
        _ => None,
    }
    .filter(|s| safe(s))
}

fn map_of(v: Option<&toml::Value>, strip_dot: bool) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(t) = v.and_then(|v| v.as_table()) {
        for (k, f) in t {
            let Some(f) = f.as_str().map(str::trim).filter(|f| safe(f)) else { continue };
            let k = k.trim().to_lowercase();
            let k = if strip_dot { k.trim_start_matches('.').to_string() } else { k };
            if !k.is_empty() {
                out.insert(k, f.to_string());
            }
        }
    }
    out
}

/// Reads a `pack.toml`. An error says what is wrong (the app logs it and skips the pack); a pack with no `name` is called `fallback_name`.
pub fn parse(text: &str, fallback_name: &str) -> Result<Pack, String> {
    let v: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
    let head = v.get("pack").and_then(|x| x.as_table());
    let s = |k: &str| head.and_then(|h| h.get(k)).and_then(|x| x.as_str()).map(|x| x.trim().to_string()).unwrap_or_default();
    let pack = Pack {
        name: Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| fallback_name.to_string()),
        author: s("author"),
        licence: s("licence"),
        tint: head.and_then(|h| h.get("tint")).and_then(|x| x.as_bool()).unwrap_or(false),
        folder: str_of(v.get("folder")),
        file: str_of(v.get("file")),
        ext: map_of(v.get("ext"), true),
        name_map: map_of(v.get("name"), false),
        folder_name: map_of(v.get("folder_name"), false),
    };
    if pack.folder.is_none() && pack.file.is_none() && pack.ext.is_empty() && pack.name_map.is_empty() && pack.folder_name.is_empty() {
        return Err("the pack names no icon".to_string());
    }
    Ok(pack)
}

impl Pack {
    /// The picture (a file name inside the pack) for an item, or `None` when the pack has none (the caller draws the built-in line
    /// icon). A folder: its own name, then the pack's folder picture. A file: its exact name, its extension, the pack's file picture.
    pub fn resolve(&self, is_dir: bool, name: &str, ext: &str) -> Option<&str> {
        let lname = name.to_lowercase();
        if is_dir {
            return self.folder_name.get(&lname).or(self.folder.as_ref()).map(String::as_str);
        }
        let lext = ext.trim_start_matches('.').to_lowercase();
        self.name_map
            .get(&lname)
            .or_else(|| (!lext.is_empty()).then(|| self.ext.get(&lext)).flatten())
            .or(self.file.as_ref())
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_resolves_names_then_extensions_then_the_defaults() {
        let p = parse(
            "[pack]\nname = \"Demo\"\ntint = true\n[folder]\ndefault = \"dir.svg\"\n[file]\ndefault = \"any.svg\"\n[ext]\nrs = \"rust.svg\"\n\".JSON\" = \"json.svg\"\n\
             [name]\n\"Cargo.toml\" = \"cargo.svg\"\n[folder_name]\nSRC = \"src.svg\"\nbad = \"../x.svg\"\n",
            "dir-name",
        )
        .unwrap();
        assert_eq!((p.name.as_str(), p.tint), ("Demo", true));
        let r = |dir: bool, n: &str, e: &str| p.resolve(dir, n, e).map(str::to_string);
        assert_eq!(r(false, "main.rs", "rs"), Some("rust.svg".into()));
        assert_eq!(r(false, "MAIN.RS", "RS"), Some("rust.svg".into()), "case does not matter");
        assert_eq!(r(false, "a.json", "json"), Some("json.svg".into()), "a leading dot in the key is fine");
        assert_eq!(r(false, "cargo.toml", "toml"), Some("cargo.svg".into()), "a name beats an extension");
        assert_eq!(r(false, "x.zzz", "zzz"), Some("any.svg".into()));
        assert_eq!(r(true, "src", ""), Some("src.svg".into()));
        assert_eq!(r(true, "other", ""), Some("dir.svg".into()));
        assert_eq!(r(true, "bad", ""), Some("dir.svg".into()), "a path out of the pack is dropped");
        // a pack with no folder picture leaves folders to the built-in icon; a broken or empty one is an error
        let q = parse("[ext]\nrs = \"r.svg\"\n", "fallback").unwrap();
        assert_eq!(q.name, "fallback");
        assert_eq!(q.resolve(true, "src", ""), None);
        assert_eq!(q.resolve(false, "a.txt", "txt"), None);
        assert!(parse("name = [", "x").is_err());
        assert!(parse("[pack]\nname = \"Empty\"", "x").is_err());
    }
}
