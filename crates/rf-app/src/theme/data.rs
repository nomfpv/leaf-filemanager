//! Themes as data: the token list, the TOML files (the built-in ones are compiled in, the user's are read from `<config>/themes`),
//! and how a theme, its mode (dark or light) and its visibility (Standard or High) become one set of values.
//!
//! A theme file holds tokens at four levels: the top of the file (every variant), `[dark]` and `[light]`, and `[dark_high]` and
//! `[light_high]` (what High changes). A theme resolves on top of its base (`base = "stem"`; a user theme without one sits on Canopy)
//! and every theme on top of Canopy, so a key that a file leaves out, or writes wrong, falls back to the base theme's value. A file
//! that is not TOML is logged and skipped; nothing a user writes can blank or stop the UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Color,
    Number,
    Text,
    Bool,
}

pub struct Token {
    pub key: &'static str,
    pub kind: Kind,
    pub doc: &'static str,
}

macro_rules! tokens {
    ($( $key:ident : $kind:ident = $doc:literal ),* $(,)?) => {
        pub const TOKENS: &[Token] = &[ $( Token { key: stringify!($key), kind: Kind::$kind, doc: $doc } ),* ];
    };
}

// The one list of keys: the palette reads these, the docs page lists these (a test keeps `docs/themes.md` equal to it).
tokens! {
    window: Color = "The window frame behind the title bar, the address row and the sidebar; over a backdrop it is the veil.",
    titlebar: Color = "The tab strip, over the window colour.",
    toolbar: Color = "The address row (back, forward, the address box).",
    sidebar: Color = "The sidebar.",
    content: Color = "The file panel: command bar, list, preview and status line.",
    raised: Color = "Menus, dialogs, pop-overs and cards that float over other content (keep it opaque).",
    field: Color = "The address box and other text fields.",
    field_border: Color = "The outline of a text field.",
    tab_active: Color = "The tab in front.",
    tile: Color = "Cards on the content panel (Home tiles, preview boxes).",
    tile_hover: Color = "A card under the pointer.",
    status: Color = "The status line; fully transparent means the panel's own colour.",
    status_text: Color = "Strong text in the status line (the selection count).",
    status_text_secondary: Color = "The other text in the status line.",
    stripe: Color = "Every second row of the details list (zebra); fully transparent means no stripes.",
    border: Color = "Lines around panels and fields.",
    divider: Color = "Separators inside a panel.",
    border_strong: Color = "Stronger lines: column header, focused fields.",
    text: Color = "Body text and file names.",
    text_secondary: Color = "Sizes, dates and other secondary text.",
    text_tertiary: Color = "Types, section labels, hints.",
    text_disabled: Color = "Disabled commands.",
    hover: Color = "A row or button under the pointer.",
    pressed: Color = "A pressed button, a stronger hover.",
    selection: Color = "The fill of a selected item in lists, the sidebar, menus and Settings.",
    selection_border: Color = "The outline of a selected row; fully transparent means none.",
    row_selection: Color = "The fill of a selected row or tile in the file list.",
    row_selection_text: Color = "Text on a selected row or tile in the file list.",
    cursor: Color = "The outline of the focused item inside a multiple selection.",
    focus: Color = "The marker of the focused pane and focus rings.",
    danger: Color = "Delete, errors.",
    warning: Color = "Warnings.",
    success: Color = "Done, healthy.",
    kind_folder: Color = "Folder icons.",
    kind_image: Color = "Picture icons.",
    kind_video: Color = "Video and audio icons.",
    kind_document: Color = "Document and PDF icons.",
    kind_code: Color = "Source code icons.",
    kind_text: Color = "Plain text and other files.",
    kind_archive: Color = "Archive icons.",
    kind_app: Color = "Programs and spreadsheets.",
    backdrop: Bool = "The theme lets the window backdrop (Mica, Acrylic, glass) show through its surfaces.",
    sheet_opacity: Number = "With a backdrop: how opaque the panel surfaces are, 0 to 1.",
    solid_window: Color = "The window colour when the backdrop is off or not available.",
    solid_sheet: Color = "The panel colour when the backdrop is off or not available.",
    solid_raised: Color = "The menu and dialog colour when the backdrop is off or not available.",
    radius: Number = "Corner radius of panels, in pixels.",
    control_radius: Number = "Corner radius of tabs, buttons and fields.",
    row_radius: Number = "Corner radius of list rows.",
    row_height: Number = "Height of a details row at the default size, in pixels.",
    marker_width: Number = "Width of the accent line on the left of a selected row; 0 means none.",
    focus_width: Number = "Thickness of the focused-pane marker.",
    tab_marker: Number = "Thickness of the accent line on top of the tab in front; 0 means none.",
    tag_dot: Number = "Diameter of tag dots.",
    name_weight: Number = "Font weight of file names (400 regular, 500 medium, 600 semibold).",
    font_ui: Text = "The interface font; empty means the system font. A font that is not installed is ignored.",
    font_mono: Text = "The font of number columns when mono_numbers is on.",
    mono_numbers: Bool = "Sizes and dates in the details list use font_mono.",
}

pub fn token(key: &str) -> Option<&'static Token> {
    TOKENS.iter().find(|t| t.key == key)
}

/// The keys of a file that are not tokens.
pub const META_KEYS: &[&str] = &["name", "base", "description", "accents"];
/// The tables of a file, in the order they apply: dark/light first, then what High changes.
pub const LAYERS: [&str; 4] = ["dark", "light", "dark_high", "light_high"];

/// A colour as a file writes it: `#rrggbb`, `#rrggbbaa`, or the accent (`accent`, `accent_text`), each optionally with `/alpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Rgba(u32, f32),
    Accent(f32),
    AccentText(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Color(Color),
    Number(f32),
    Text(String),
    Bool(bool),
}

pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    let (head, alpha) = match s.split_once('/') {
        Some((h, a)) => (h.trim(), a.trim().parse::<f32>().ok().filter(|a| (0.0..=1.0).contains(a))?),
        None => (s, 1.0),
    };
    match head {
        "accent" => return Some(Color::Accent(alpha)),
        "accent_text" => return Some(Color::AccentText(alpha)),
        _ => {}
    }
    let hex = head.strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => Some(Color::Rgba(u32::from_str_radix(hex, 16).ok()?, alpha)),
        8 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some(Color::Rgba(v >> 8, (v & 0xFF) as f32 / 255.0 * alpha))
        }
        _ => None,
    }
}

fn parse_value(kind: Kind, v: &toml::Value) -> Option<Value> {
    match (kind, v) {
        (Kind::Color, toml::Value::String(s)) => parse_color(s).map(Value::Color),
        (Kind::Number, toml::Value::Float(f)) if f.is_finite() => Some(Value::Number(*f as f32)),
        (Kind::Number, toml::Value::Integer(i)) => Some(Value::Number(*i as f32)),
        (Kind::Text, toml::Value::String(s)) => Some(Value::Text(s.trim().to_string())),
        (Kind::Bool, toml::Value::Boolean(b)) => Some(Value::Bool(*b)),
        _ => None,
    }
}

/// One accent of a theme's accent set: its colour and the text on it, for each mode.
#[derive(Clone, Debug, PartialEq)]
pub struct AccentDef {
    pub name: String,
    pub dark: u32,
    pub dark_text: u32,
    pub light: u32,
    pub light_text: u32,
}

pub type Layer = BTreeMap<&'static str, Value>;

#[derive(Clone, Debug)]
pub struct ThemeFile {
    /// The name settings use: a built-in's id, or a user file's name without `.toml`.
    pub id: String,
    /// The name shown in Settings.
    pub name: String,
    pub base: Option<String>,
    /// The top of the file, then `LAYERS` in their order.
    pub layers: [Layer; 5],
    pub accents: Vec<AccentDef>,
    pub builtin: bool,
}

/// Reads a theme file. `Err` only when the text is not TOML; everything else that is wrong (an unknown key, a value of the wrong
/// kind, a bad colour) is left out and named in the warnings, so the base theme's value stands in for it.
pub fn parse(id: &str, text: &str, builtin: bool) -> Result<(ThemeFile, Vec<String>), String> {
    let table: toml::Table = toml::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    let mut file = ThemeFile { id: id.to_string(), name: id.to_string(), base: None, layers: Default::default(), accents: Vec::new(), builtin };
    let read_layer = |t: &toml::Table, layer: &mut Layer, at: &str, warnings: &mut Vec<String>| {
        for (k, v) in t {
            match token(k) {
                Some(tok) => match parse_value(tok.kind, v) {
                    Some(val) => {
                        layer.insert(tok.key, val);
                    }
                    None => warnings.push(format!("{at}{k}: not a valid {:?}", tok.kind)),
                },
                None => warnings.push(format!("{at}{k}: unknown key")),
            }
        }
    };
    for (k, v) in &table {
        match (k.as_str(), v) {
            ("name", toml::Value::String(s)) => file.name = s.clone(),
            ("description", _) => {}
            ("base", toml::Value::String(s)) => file.base = Some(s.trim().to_ascii_lowercase()),
            ("accents", toml::Value::Array(list)) => {
                for (i, a) in list.iter().enumerate() {
                    match accent_def(a) {
                        Some(a) => file.accents.push(a),
                        None => warnings.push(format!("accents[{i}]: needs name, dark, dark_text, light, light_text as #rrggbb")),
                    }
                }
            }
            (name, toml::Value::Table(t)) if LAYERS.contains(&name) => {
                let ix = 1 + LAYERS.iter().position(|l| *l == name).unwrap_or(0);
                let mut layer = std::mem::take(&mut file.layers[ix]);
                read_layer(t, &mut layer, &format!("[{name}] "), &mut warnings);
                file.layers[ix] = layer;
            }
            _ => {
                let mut one = toml::Table::new();
                one.insert(k.clone(), v.clone());
                let mut layer = std::mem::take(&mut file.layers[0]);
                read_layer(&one, &mut layer, "", &mut warnings);
                file.layers[0] = layer;
            }
        }
    }
    Ok((file, warnings))
}

fn accent_def(v: &toml::Value) -> Option<AccentDef> {
    let t = v.as_table()?;
    let rgb = |k: &str| match parse_color(t.get(k)?.as_str()?)? {
        Color::Rgba(c, _) => Some(c),
        _ => None,
    };
    Some(AccentDef { name: t.get("name")?.as_str()?.to_string(), dark: rgb("dark")?, dark_text: rgb("dark_text")?, light: rgb("light")?, light_text: rgb("light_text")? })
}

/// A theme with its mode and visibility applied: one value per key (every key, when the chain starts at a complete theme).
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub values: Layer,
    pub accents: Vec<AccentDef>,
}

/// Lays the files of `chain` (the root first) over each other for one variant.
pub fn resolve(chain: &[&ThemeFile], dark: bool, high: bool) -> Resolved {
    let mut r = Resolved::default();
    let (mode, mode_high) = if dark { (1, 3) } else { (2, 4) };
    for f in chain {
        let mut order = vec![0, mode];
        if high {
            order.push(mode_high);
        }
        for ix in order {
            for (k, v) in &f.layers[ix] {
                r.values.insert(k, v.clone());
            }
        }
        if !f.accents.is_empty() {
            r.accents = f.accents.clone();
        }
    }
    r
}

pub const DEFAULT_THEME: &str = "canopy";

/// The built-in themes, in the order Settings lists them.
pub const BUILTIN_SOURCES: [(&str, &str); 3] =
    [("canopy", include_str!("canopy.toml")), ("stem", include_str!("stem.toml")), ("index", include_str!("index.toml"))];

pub fn builtins() -> &'static [ThemeFile] {
    static B: std::sync::OnceLock<Vec<ThemeFile>> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        BUILTIN_SOURCES
            .iter()
            .filter_map(|(id, text)| match parse(id, text, true) {
                Ok((f, w)) => {
                    for w in w {
                        log::warn!("theme {id} (built in): {w}");
                    }
                    Some(f)
                }
                Err(e) => {
                    log::error!("theme {id} (built in): {e}");
                    None
                }
            })
            .collect()
    })
}

/// The chain a theme resolves through, the root first: Canopy, then the theme's bases, then the theme. Unknown bases and loops end
/// the chain where they are.
pub fn chain<'a>(theme: &'a ThemeFile, all: &'a [ThemeFile]) -> Vec<&'a ThemeFile> {
    let find = |id: &str| all.iter().find(|t| t.id == id);
    let mut out: Vec<&ThemeFile> = vec![theme];
    let mut next = theme.base.clone().or_else(|| (!theme.builtin).then(|| DEFAULT_THEME.to_string()));
    while let Some(id) = next {
        match find(&id) {
            Some(t) if !out.iter().any(|o| o.id == t.id) && out.len() < 8 => {
                out.push(t);
                next = t.base.clone();
            }
            _ => break,
        }
    }
    if out.last().is_none_or(|t| t.id != DEFAULT_THEME)
        && let Some(root) = find(DEFAULT_THEME)
        && !out.iter().any(|o| o.id == DEFAULT_THEME)
    {
        out.push(root);
    }
    out.reverse();
    out
}

/// Where user themes live: `<config>/themes` (honours `LEAF_CONFIG_DIR`).
pub fn user_dir() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("themes"))
}

/// Reads every `*.toml` in `dir`. A file that is not TOML, or that takes a built-in's name, is logged and skipped.
pub fn read_user_themes(dir: &Path) -> Vec<ThemeFile> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml"))).collect();
    paths.sort();
    let mut out = Vec::new();
    for p in paths {
        let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()) else { continue };
        if BUILTIN_SOURCES.iter().any(|(b, _)| *b == id) || out.iter().any(|t: &ThemeFile| t.id == id) {
            log::warn!("theme {}: the name is taken, skipped", p.display());
            continue;
        }
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("theme {}: {e}", p.display());
                continue;
            }
        };
        match parse(&id, &text, false) {
            Ok((f, w)) => {
                for w in w {
                    log::warn!("theme {}: {w} (the base theme's value is used)", p.display());
                }
                out.push(f);
            }
            Err(e) => log::warn!("theme {}: skipped, {e}", p.display()),
        }
    }
    out
}

/// What the user themes folder holds now, cheaply: names, sizes and times. The themes are read again when it changes.
pub fn dir_signature(dir: &Path) -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<_> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("toml")))
        .map(|e| {
            let m = e.metadata().ok();
            (e.path(), m.as_ref().map_or(0, |m| m.len()), m.and_then(|m| m.modified().ok()))
        })
        .collect();
    v.sort();
    v
}

/// Every theme the app knows now: the built-ins, then the user's.
pub fn all_themes() -> Vec<ThemeFile> {
    static USER: parking_lot::Mutex<Option<(Vec<(PathBuf, u64, Option<std::time::SystemTime>)>, Vec<ThemeFile>)>> = parking_lot::Mutex::new(None);
    let mut all = builtins().to_vec();
    if let Some(dir) = user_dir() {
        let sig = dir_signature(&dir);
        let mut cache = USER.lock();
        if cache.as_ref().is_none_or(|(s, _)| *s != sig) {
            *cache = Some((sig, read_user_themes(&dir)));
        }
        if let Some((_, users)) = cache.as_ref() {
            all.extend(users.iter().cloned());
        }
    }
    all
}

/// The file that holds the theme `id`, as Settings lists it: (id, shown name).
pub fn list() -> Vec<(String, String)> {
    all_themes().into_iter().map(|t| (t.id, t.name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(r: &Resolved) -> Vec<&'static str> {
        TOKENS.iter().filter(|t| !r.values.contains_key(t.key)).map(|t| t.key).collect()
    }

    #[test]
    fn colours_parse_in_every_written_form() {
        assert_eq!(parse_color("#101916"), Some(Color::Rgba(0x101916, 1.0)));
        let Some(Color::Rgba(c, a)) = parse_color("#060c0a61") else { panic!() };
        assert_eq!(c, 0x060c0a);
        assert!((a - 0x61 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(parse_color("accent/0.38"), Some(Color::Accent(0.38)));
        assert_eq!(parse_color(" accent_text "), Some(Color::AccentText(1.0)));
        assert_eq!(parse_color("#ffffff/0.5"), Some(Color::Rgba(0xffffff, 0.5)));
        for bad in ["", "red", "#12345", "#gggggg", "accent/2", "accent/x", "#1234567890"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_builtin_parses_cleanly_and_is_complete_in_every_variant() {
        for (id, text) in BUILTIN_SOURCES {
            let (f, warnings) = parse(id, text, true).unwrap();
            assert!(warnings.is_empty(), "{id}: {warnings:?}");
            assert_eq!(f.accents.len(), 6, "{id}: the mockups' six accents");
            // each built-in stands on its own: complete without Canopy under it
            for dark in [true, false] {
                for high in [true, false] {
                    let r = resolve(&[&f], dark, high);
                    assert!(complete(&r).is_empty(), "{id} dark={dark} high={high} lacks {:?}", complete(&r));
                }
            }
        }
        assert_eq!(builtins().len(), 3);
    }

    #[test]
    fn a_builtin_survives_a_round_trip_through_toml() {
        // written back out from what was read and read again, every variant resolves to the same values
        for f in builtins() {
            let mut t = toml::Table::new();
            t.insert("name".into(), toml::Value::String(f.name.clone()));
            let to_toml = |layer: &Layer| {
                let mut tt = toml::Table::new();
                for (k, v) in layer {
                    let tv = match v {
                        Value::Color(Color::Rgba(c, a)) => toml::Value::String(format!("#{c:06x}/{a}")),
                        Value::Color(Color::Accent(a)) => toml::Value::String(format!("accent/{a}")),
                        Value::Color(Color::AccentText(a)) => toml::Value::String(format!("accent_text/{a}")),
                        Value::Number(n) => toml::Value::Float(f64::from(*n)),
                        Value::Text(s) => toml::Value::String(s.clone()),
                        Value::Bool(b) => toml::Value::Boolean(*b),
                    };
                    tt.insert((*k).into(), tv);
                }
                tt
            };
            for (k, v) in to_toml(&f.layers[0]) {
                t.insert(k, v);
            }
            for (i, name) in LAYERS.iter().enumerate() {
                t.insert((*name).into(), toml::Value::Table(to_toml(&f.layers[i + 1])));
            }
            let accents = f
                .accents
                .iter()
                .map(|a| {
                    let mut at = toml::Table::new();
                    at.insert("name".into(), toml::Value::String(a.name.clone()));
                    for (k, c) in [("dark", a.dark), ("dark_text", a.dark_text), ("light", a.light), ("light_text", a.light_text)] {
                        at.insert(k.into(), toml::Value::String(format!("#{c:06x}")));
                    }
                    toml::Value::Table(at)
                })
                .collect();
            t.insert("accents".into(), toml::Value::Array(accents));
            let text = toml::to_string(&t).unwrap();
            let (back, warnings) = parse(&f.id, &text, true).unwrap();
            assert!(warnings.is_empty(), "{}: {warnings:?}", f.id);
            assert_eq!(back.accents, f.accents);
            for dark in [true, false] {
                for high in [true, false] {
                    let (a, b) = (resolve(&[f], dark, high), resolve(&[&back], dark, high));
                    for (k, v) in &a.values {
                        let w = &b.values[k];
                        match (v, w) {
                            (Value::Color(Color::Rgba(c1, a1)), Value::Color(Color::Rgba(c2, a2))) => assert!(c1 == c2 && (a1 - a2).abs() < 1e-4, "{} {k}", f.id),
                            _ => assert_eq!(v, w, "{} {k}", f.id),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_partial_user_theme_keeps_its_base_for_everything_it_leaves_out() {
        let text = "name = \"Mine\"\nbase = \"stem\"\n[dark]\ntext = \"#ff0000\"\nnot_a_key = 3\naccent_text = 5\n[dark_high]\nsidebar = \"nope\"\n";
        let (mine, warnings) = parse("mine", text, false).unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        let mut all = builtins().to_vec();
        all.push(mine.clone());
        let ch = chain(&mine, &all);
        assert_eq!(ch.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["canopy", "stem", "mine"]);
        let r = resolve(&ch, true, true);
        assert!(complete(&r).is_empty());
        // the user's dark text beats the base's High text; what it left out (and the bad sidebar) is Stem's
        assert_eq!(r.values["text"], Value::Color(Color::Rgba(0xff0000, 1.0)));
        let stem = builtins().iter().find(|t| t.id == "stem").unwrap();
        let stem_r = resolve(&[stem], true, true);
        assert_eq!(r.values["sidebar"], stem_r.values["sidebar"]);
        assert_eq!(r.values["content"], stem_r.values["content"]);
        assert_eq!(r.accents, stem.accents);
        // no base: Canopy
        let (plain, _) = parse("plain", "[light]\ntext = \"#000000\"", false).unwrap();
        all.push(plain.clone());
        assert_eq!(chain(&plain, &all).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["canopy", "plain"]);
        // a base that does not exist, or a loop, ends the chain on Canopy
        let (lost, _) = parse("lost", "base = \"nowhere\"", false).unwrap();
        assert_eq!(chain(&lost, &all).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["canopy", "lost"]);
        let (selfish, _) = parse("selfish", "base = \"selfish\"", false).unwrap();
        all.push(selfish.clone());
        assert_eq!(chain(&selfish, &all).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["canopy", "selfish"]);
    }

    #[test]
    fn a_broken_file_is_an_error_and_an_empty_one_is_all_base() {
        assert!(parse("x", "this is = = not toml", false).is_err());
        let (empty, w) = parse("empty", "", false).unwrap();
        assert!(w.is_empty());
        let all = builtins().to_vec();
        let r = resolve(&chain(&empty, &all), false, false);
        let canopy = resolve(&[&builtins()[0]], false, false);
        assert_eq!(r.values, canopy.values);
    }

    #[test]
    fn the_user_folder_skips_broken_files_and_taken_names() {
        let dir = std::env::temp_dir().join(format!("leaf-themes-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.toml"), "name = \"Good\"\n[dark]\ntext = \"#123456\"").unwrap();
        std::fs::write(dir.join("broken.toml"), "[[[").unwrap();
        std::fs::write(dir.join("Stem.toml"), "name = \"Not stem\"").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a theme").unwrap();
        let got = read_user_themes(&dir);
        assert_eq!(got.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["good"]);
        assert_eq!(got[0].name, "Good");
        assert_eq!(dir_signature(&dir).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
