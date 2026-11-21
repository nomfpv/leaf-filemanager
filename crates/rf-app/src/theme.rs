//! leaf palette: the theme in use (Canopy, Stem, Index or a user theme, see `data`) for one mode (dark, light, pure black) and one
//! visibility (Standard, High), as the colours and sizes the views read.

use gpui_kit::*;

/// Themes as data (the TOML files, user themes, resolution).
pub mod data;

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct Palette {
    pub dark: bool,
    /// The window frame behind the title bar, address row and sidebar; over a backdrop it is the veil.
    pub window_bg: Hsla,
    pub titlebar_bg: Hsla,
    /// The address row.
    pub toolbar_bg: Hsla,
    pub sidebar_bg: Hsla,
    /// The file panel (command bar, list, preview, status line).
    pub content_bg: Hsla,
    /// Opaque surface for menus, dialogs and inputs that float over other content.
    pub card_bg: Hsla,
    /// A bar of our own with controls and text in it that no other layer backs (the Open / Save window's name row). Clear on every
    /// backdrop but glass, where it is a wash of its own: the text must not sit straight on the live screen.
    pub bar_bg: Hsla,
    /// Cards that sit on the content layer (Home tiles, preview boxes).
    pub tile_bg: Hsla,
    pub tile_hover: Hsla,
    pub field_bg: Hsla,
    pub field_border: Hsla,
    /// The status line (transparent: the panel's colour) and its text.
    pub status_bg: Hsla,
    pub status_text: Hsla,
    pub status_text_secondary: Hsla,
    /// Every second details row (transparent: no stripes).
    pub stripe: Hsla,
    pub border: Hsla,
    pub divider: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_disabled: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla,
    pub hover: Hsla,
    pub pressed: Hsla,
    pub selection: Hsla,
    pub selection_border: Hsla,
    /// The file list's own selection (Index: the solid accent) and the text on it.
    pub row_selection: Hsla,
    pub row_selection_text: Hsla,
    /// The outline of the focused item in a multiple selection.
    pub cursor: Hsla,
    /// The focused-pane marker.
    pub focus: Hsla,
    pub tab_active: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub success: Hsla,
    /// Icon tints: folder, image, video, document, code, text, archive, app (see `kind_color`).
    pub kinds: [Hsla; 8],
    /// The theme lets the window backdrop show through (Canopy). Off: every surface is opaque.
    pub backdrop: bool,
    pub sheet_opacity: f32,
    pub solid_window: Hsla,
    pub solid_sheet: Hsla,
    pub solid_raised: Hsla,
    pub radius: f32,
    pub control_radius: f32,
    pub row_radius: f32,
    pub row_height: f32,
    pub marker_width: f32,
    pub focus_width: f32,
    pub tab_marker: f32,
    pub tag_dot: f32,
    pub name_weight: f32,
    /// "" = the system font.
    pub font_ui: &'static str,
    pub font_mono: &'static str,
    pub mono_numbers: bool,
    /// The controls of this window are drawn the liquid way (the window shows glass, or it is a popup opened from one): see `liquid`.
    /// Set by `for_window`.
    pub liquid: bool,
}

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

fn hexa(c: u32, a: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = a;
    h
}

/// A font name that lives as long as the program (a palette is `Copy`); each distinct name is kept once.
fn intern(s: &str) -> &'static str {
    static NAMES: parking_lot::Mutex<Vec<&'static str>> = parking_lot::Mutex::new(Vec::new());
    let mut names = NAMES.lock();
    if let Some(n) = names.iter().find(|n| **n == s) {
        return n;
    }
    let n: &'static str = Box::leak(s.to_string().into_boxed_str());
    names.push(n);
    n
}

/// Which accent the app wears: one of the theme's set (by name; unknown or empty = the theme's first), Windows' own, or a colour the
/// user wrote.
#[derive(Clone, Debug, PartialEq)]
pub enum AccentChoice {
    Theme(String),
    Windows,
    Custom(u32),
}

/// What the settings choose: the theme, High or Standard, the accent.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub theme: String,
    pub high: bool,
    pub accent: AccentChoice,
}

impl Default for Selection {
    fn default() -> Self {
        Self { theme: data::DEFAULT_THEME.into(), high: true, accent: AccentChoice::Theme(String::new()) }
    }
}

/// The user's Windows accent palette, read once.
fn windows_accent() -> Option<rf_win::accent::AccentPalette> {
    static WINDOWS: std::sync::OnceLock<Option<rf_win::accent::AccentPalette>> = std::sync::OnceLock::new();
    *WINDOWS.get_or_init(rf_win::accent::system_palette)
}

/// The accent and the text on it for one mode.
fn accent_for(r: &data::Resolved, dark: bool, choice: &AccentChoice) -> (Hsla, Hsla) {
    // Windows' or the user's colour: the Fluent shade for the mode, dark text on a light accent, white on a dark one
    let fluent = |a: rf_win::accent::AccentPalette| if dark { (hex(a.light2()), hexa(0x000000, 0.90)) } else { (hex(a.dark1()), hex(0xFFFFFF)) };
    match choice {
        AccentChoice::Custom(c) => return fluent(rf_win::accent::from_color(*c)),
        AccentChoice::Windows => {
            if let Some(a) = windows_accent() {
                return fluent(a);
            }
        }
        AccentChoice::Theme(_) => {}
    }
    let want = match choice {
        AccentChoice::Theme(n) => n.as_str(),
        _ => "",
    };
    match r.accents.iter().find(|a| a.name.eq_ignore_ascii_case(want)).or_else(|| r.accents.first()) {
        Some(a) if dark => (hex(a.dark), hex(a.dark_text)),
        Some(a) => (hex(a.light), hex(a.light_text)),
        None if dark => (hex(0x60CDFF), hexa(0x000000, 0.90)),
        None => (hex(0x005FB8), hex(0xFFFFFF)),
    }
}

impl Palette {
    /// The palette of one resolved variant with its accent. With a backdrop the opaque panel surfaces take the sheet opacity.
    pub fn from_resolved(r: &data::Resolved, dark: bool, accent: (Hsla, Hsla)) -> Self {
        use data::{Color, Value};
        let (acc, acc_text) = accent;
        let col = |k: &str| -> Hsla {
            match r.values.get(k) {
                Some(Value::Color(Color::Rgba(c, a))) => hexa(*c, *a),
                Some(Value::Color(Color::Accent(a))) => Hsla { a: acc.a * a, ..acc },
                Some(Value::Color(Color::AccentText(a))) => Hsla { a: acc_text.a * a, ..acc_text },
                _ => transparent_black(),
            }
        };
        let num = |k: &str, d: f32| match r.values.get(k) {
            Some(Value::Number(n)) => *n,
            _ => d,
        };
        let flag = |k: &str| matches!(r.values.get(k), Some(Value::Bool(true)));
        let text = |k: &str| match r.values.get(k) {
            Some(Value::Text(s)) => intern(s),
            _ => "",
        };
        let backdrop = flag("backdrop");
        let sheet_opacity = num("sheet_opacity", 1.0).clamp(0.0, 1.0);
        // a sheet over the backdrop: the file's colour at the sheet opacity (a colour the file made see-through itself stays as written)
        let sheet = |k: &str| {
            let mut c = col(k);
            if backdrop && c.a >= 1.0 {
                c.a = sheet_opacity;
            }
            c
        };
        Self {
            dark,
            window_bg: col("window"),
            titlebar_bg: col("titlebar"),
            toolbar_bg: sheet("toolbar"),
            sidebar_bg: sheet("sidebar"),
            content_bg: sheet("content"),
            card_bg: col("raised"),
            bar_bg: transparent_black(),
            tile_bg: col("tile"),
            tile_hover: col("tile_hover"),
            field_bg: col("field"),
            field_border: col("field_border"),
            status_bg: sheet("status"),
            status_text: col("status_text"),
            status_text_secondary: col("status_text_secondary"),
            stripe: col("stripe"),
            border: col("border"),
            divider: col("divider"),
            border_strong: col("border_strong"),
            text: col("text"),
            text_secondary: col("text_secondary"),
            text_tertiary: col("text_tertiary"),
            text_disabled: col("text_disabled"),
            accent: acc,
            accent_text: acc_text,
            hover: col("hover"),
            pressed: col("pressed"),
            selection: col("selection"),
            selection_border: col("selection_border"),
            row_selection: col("row_selection"),
            row_selection_text: col("row_selection_text"),
            cursor: col("cursor"),
            focus: col("focus"),
            tab_active: sheet("tab_active"),
            danger: col("danger"),
            warning: col("warning"),
            success: col("success"),
            kinds: ["kind_folder", "kind_image", "kind_video", "kind_document", "kind_code", "kind_text", "kind_archive", "kind_app"].map(col),
            backdrop,
            sheet_opacity,
            solid_window: col("solid_window"),
            solid_sheet: col("solid_sheet"),
            solid_raised: col("solid_raised"),
            radius: num("radius", 8.0),
            control_radius: num("control_radius", 6.0),
            row_radius: num("row_radius", 6.0),
            row_height: num("row_height", 30.0).clamp(18.0, 64.0),
            marker_width: num("marker_width", 0.0).clamp(0.0, 8.0),
            focus_width: num("focus_width", 2.0).clamp(0.0, 8.0),
            tab_marker: num("tab_marker", 0.0).clamp(0.0, 8.0),
            tag_dot: num("tag_dot", 8.0).clamp(4.0, 14.0),
            name_weight: num("name_weight", 400.0).clamp(100.0, 900.0),
            font_ui: text("font_ui"),
            font_mono: text("font_mono"),
            mono_numbers: flag("mono_numbers"),
            liquid: false,
        }
    }

    /// The backdrop is off or not there: a backdrop theme wears its solid colours (the layout stays, only the see-through goes).
    pub fn apply_solid(&mut self) {
        if !self.backdrop {
            return;
        }
        self.window_bg = self.solid_window;
        let solid = Hsla { a: 1.0, ..self.solid_sheet };
        for c in [&mut self.content_bg, &mut self.tab_active] {
            *c = solid;
        }
        for c in [&mut self.sidebar_bg, &mut self.toolbar_bg, &mut self.status_bg] {
            if c.a > 0.0 {
                *c = solid;
            }
        }
        self.card_bg = self.solid_raised;
        self.backdrop = false;
    }

    /// Pure black for OLED screens (issue 45): the theme's dark variant with every surface black and opaque, the text white, and the
    /// lines and fills just bright enough to be seen on black.
    fn blacken(dark: Self) -> Self {
        Self {
            window_bg: hex(0x000000),
            titlebar_bg: hexa(0x000000, 0.0),
            toolbar_bg: hexa(0x000000, 0.0),
            sidebar_bg: hexa(0x000000, 0.0),
            content_bg: hex(0x000000),
            card_bg: hex(0x0A0A0A),
            bar_bg: hexa(0x000000, 0.0),
            tile_bg: hexa(0xFFFFFF, 0.04),
            tile_hover: hexa(0xFFFFFF, 0.08),
            field_bg: hex(0x000000),
            field_border: hexa(0xFFFFFF, 0.22),
            status_bg: hexa(0x000000, 0.0),
            status_text: hex(0xFFFFFF),
            status_text_secondary: hexa(0xFFFFFF, 0.72),
            stripe: if dark.stripe.a > 0.0 { hexa(0xFFFFFF, 0.04) } else { hexa(0x000000, 0.0) },
            border: hexa(0xFFFFFF, 0.14),
            divider: hexa(0xFFFFFF, 0.12),
            border_strong: hexa(0xFFFFFF, 0.24),
            text: hex(0xFFFFFF),
            text_secondary: hexa(0xFFFFFF, 0.72),
            text_tertiary: hexa(0xFFFFFF, 0.50),
            text_disabled: hexa(0xFFFFFF, 0.32),
            hover: hexa(0xFFFFFF, 0.08),
            pressed: hexa(0xFFFFFF, 0.12),
            tab_active: hexa(0xFFFFFF, 0.08),
            solid_window: hex(0x000000),
            solid_sheet: hex(0x000000),
            solid_raised: hex(0x0A0A0A),
            backdrop: false,
            ..dark
        }
    }

    /// The theme in use, dark, for windows that do not ask `for_window` (they follow the theme too).
    pub fn dark() -> Self {
        current().dark
    }

    pub fn for_window(window: &Window) -> Self {
        let cur = current();
        let pref = crate::settings::theme_override();
        let mut p = match pref {
            crate::settings::ThemePref::Light => cur.light,
            crate::settings::ThemePref::Dark => cur.dark,
            crate::settings::ThemePref::Black => cur.black,
            crate::settings::ThemePref::System => match window.appearance() {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => cur.dark,
                _ => cur.light,
            },
        };
        if pref == crate::settings::ThemePref::Black {
            // (opaque black whatever the material: no glass, no washes)
            return p;
        }
        if crate::settings::solid_backdrop() {
            // "Solid" material, or a Windows without materials: the theme's solid colours
            p.apply_solid();
        }
        let hwnd = crate::look::window::hwnd_of(window);
        // one place decides the look of the controls: every control asks the palette
        p.liquid = crate::glass::liquid(hwnd);
        if crate::glass::shown(hwnd) {
            // Over the glass the two washes are the veil: Mica is already a quiet, nearly opaque tone, the glass is the live screen.
            // At the strength made for Mica they hid two thirds of it in the dark theme (the user: too black), so the default is half
            // of that; the HyperGlass "surface wash" setting moves it either way. A solid theme's frame surfaces thin out with it.
            (p.window_bg.a, p.content_bg.a) = glass_washes(p.dark, crate::settings::hg_surface());
            p.sidebar_bg.a = p.sidebar_bg.a.min(p.window_bg.a);
            p.toolbar_bg.a = p.toolbar_bg.a.min(p.window_bg.a);
            p.bar_bg = Hsla { a: glass_bar_wash(crate::settings::hg_surface()), ..p.card_bg };
        }
        p
    }

    /// The palette of the status line: its own text colours (Index: an inverted readout).
    pub fn for_status(&self) -> Self {
        if self.status_bg.a <= 0.0 {
            return *self;
        }
        Self { text: self.status_text, text_secondary: self.status_text_secondary, text_tertiary: self.status_text_secondary, ..*self }
    }

    /// The palette inside a selected row of the file list: its text colours are the selection's (Index: text on the solid accent).
    pub fn on_row_selection(&self) -> Self {
        if self.row_selection_text == self.text {
            return *self;
        }
        let t = self.row_selection_text;
        Self { text: t, text_secondary: Hsla { a: t.a * 0.86, ..t }, text_tertiary: Hsla { a: t.a * 0.86, ..t }, ..*self }
    }
}

/// The resolved palettes of the theme in use, rebuilt by [`select`].
#[derive(Clone, Copy)]
struct Current {
    light: Palette,
    dark: Palette,
    black: Palette,
}

static CURRENT: parking_lot::RwLock<Option<Current>> = parking_lot::RwLock::new(None);
/// The details row height of the theme, as f32 bits (read where no palette is at hand: the list's scroll arithmetic).
static THEME_ROW_H: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn build(sel: &Selection) -> Current {
    let all = data::all_themes();
    let theme = all.iter().find(|t| t.id == sel.theme).or_else(|| {
        log::warn!("theme '{}' not found, Canopy is used", sel.theme);
        all.iter().find(|t| t.id == data::DEFAULT_THEME)
    });
    let ch = theme.map(|t| data::chain(t, &all)).unwrap_or_default();
    let variant = |dark: bool| {
        let r = data::resolve(&ch, dark, sel.high);
        Palette::from_resolved(&r, dark, accent_for(&r, dark, &sel.accent))
    };
    let (light, dark) = (variant(false), variant(true));
    Current { light, dark, black: Palette::blacken(dark) }
}

/// Makes `sel` the theme in use (Settings call it whenever they change; a user theme file that changed is read again).
pub fn select(sel: &Selection) {
    let cur = build(sel);
    THEME_ROW_H.store(cur.dark.row_height.to_bits(), std::sync::atomic::Ordering::Relaxed);
    *CURRENT.write() = Some(cur);
}

fn current() -> Current {
    if let Some(c) = *CURRENT.read() {
        return c;
    }
    // (a process or a test that never loaded the settings: the defaults)
    select(&Selection::default());
    CURRENT.read().unwrap_or_else(|| build(&Selection::default()))
}

/// The details list's row height at the default size, from the theme in use.
pub fn default_row_h() -> f32 {
    let bits = THEME_ROW_H.load(std::sync::atomic::Ordering::Relaxed);
    if bits == 0 { current().dark.row_height } else { f32::from_bits(bits) }
}

/// The material a window asks Windows for: the wanted one where Windows has materials, none where it has not (`rf_win::osver`,
/// issue 24). There the palette paints the window itself (`settings::solid_backdrop`), and asking for "none" also switches off a
/// blur-behind that an earlier choice may have left on the window. A see-through window (glass, an overlay) stays what it is.
pub fn window_material(wanted: WindowBackgroundAppearance) -> WindowBackgroundAppearance {
    match wanted {
        WindowBackgroundAppearance::Transparent | WindowBackgroundAppearance::Opaque => wanted,
        _ if crate::settings::materials_available() => wanted,
        _ => WindowBackgroundAppearance::Opaque,
    }
}

/// [`window_material`] for a MAIN window: on a Windows without materials it may still have Acrylic, the old blur behind it, when that
/// is the chosen material (`settings::main_window_only`, issue 78). Popups never ask this.
pub fn main_window_material(wanted: WindowBackgroundAppearance) -> WindowBackgroundAppearance {
    match wanted {
        WindowBackgroundAppearance::Blurred if !crate::settings::materials_available() => WindowBackgroundAppearance::Blurred,
        _ => window_material(wanted),
    }
}

/// The material of a popup (a menu, a flyout, a drop-down list): real glass when it was opened from a window that shows glass,
/// else Windows' Acrylic, else none (and then [`popup_fill`] is its surface).
pub fn popup_material(liquid: bool) -> WindowBackgroundAppearance {
    if liquid { WindowBackgroundAppearance::Transparent } else { window_material(WindowBackgroundAppearance::Blurred) }
}

/// What a popup paints behind its content on a Windows that gives it no Acrylic: the plain colour Windows 11 itself uses for a
/// flyout without transparency. `None` where the Acrylic is the surface and the popup paints nothing.
pub fn popup_fill(p: &Palette) -> Option<Hsla> {
    // (a solid theme, Stem or Index, paints its menus too: their raised colour, not Acrylic)
    (!crate::settings::materials_available() || !p.backdrop).then(|| Hsla { a: 1.0, ..p.card_bg })
}

/// The alpha of `window_bg` and `content_bg` over a window that shows glass, for the surface value (0..=100) of the HyperGlass
/// settings. 50 is the veil the app shipped with (dark 0.18 / 0.22, light 0.20 / 0.35), 0 is no veil at all and 100 is the strength made
/// for Mica (dark 0.40 / 0.45, light 0.30 / 0.50: the values in `Palette::dark` and `light`); each side of 50 is a straight line. A
/// value above 100 counts as 100.
pub fn glass_washes(dark: bool, surface: u8) -> (f32, f32) {
    let ((w50, c50), (w100, c100)) = if dark { ((0.18, 0.22), (0.40, 0.45)) } else { ((0.20, 0.35), (0.30, 0.50)) };
    let v = f32::from(surface.min(100));
    let lerp = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
    if v <= 50.0 {
        let t = v / 50.0;
        (lerp(0.0, w50, t), lerp(0.0, c50, t))
    } else {
        let t = (v - 50.0) / 50.0;
        (lerp(w50, w100, t), lerp(c50, c100, t))
    }
}

/// The alpha of `Palette::bar_bg` over glass. It follows the surface value too, but never gets thin: 0.40 at 0, 0.55 at the default 50
/// and 0.70 at 100, so a bar's text is readable whatever the picture behind the glass is.
pub fn glass_bar_wash(surface: u8) -> f32 {
    0.40 + 0.30 * f32::from(surface.min(100)) / 100.0
}

/// Icon tint per file category, from the theme in use (`kind_*` tokens).
pub fn kind_color(cat: crate::icons::Category, dark: bool) -> Hsla {
    use crate::icons::Category::*;
    let ix = match cat {
        Folder => 0,
        Image => 1,
        Video | Audio => 2,
        Document | Pdf => 3,
        Code => 4,
        Text | Other => 5,
        Archive => 6,
        Spreadsheet | Executable => 7,
    };
    let read = |c: &Current| if dark { c.dark.kinds[ix] } else { c.light.kinds[ix] };
    let quick = CURRENT.read().as_ref().map(read);
    quick.unwrap_or_else(|| read(&current()))
}

/// The selection marker shared by every list (sidebar, details, columns, menus): a short
/// rounded accent bar on the left edge of a selected row. The row must be `relative()`.
pub fn selection_pill(p: &Palette) -> Div {
    selection_pill_joined(p, false, false)
}

/// The pill of a selected row whose neighbour above or below is selected too: it reaches nearly to that edge, so that a run of
/// selected rows reads as one block while each row keeps a pill of its own to count by (a small gap between them). The user,
/// 2026-11-20: "ortadakiler birleşik gibi mi olsa ... hem bir görünür hem de sayılabilir çizgiler ile".
pub fn selection_pill_joined(p: &Palette, above: bool, below: bool) -> Div {
    // (the theme's marker width: Stem's stem; 0 in a theme that marks a selection otherwise)
    if p.marker_width <= 0.0 {
        return div();
    }
    let (top, bottom) = pill_insets(above, below);
    div().absolute().left(px(0.)).top(px(top)).bottom(px(bottom)).w(px(p.marker_width)).rounded_full().bg(p.accent)
}

/// How far the pill stays from the row's top and bottom: 8 px on a side with no selected neighbour, 2 px towards one.
pub fn pill_insets(above: bool, below: bool) -> (f32, f32) {
    (if above { 2. } else { 8. }, if below { 2. } else { 8. })
}

pub const RADIUS: Pixels = px(8.);
pub const RADIUS_SM: Pixels = px(6.);
pub const ROW_H: Pixels = px(30.);
pub const TITLEBAR_H: Pixels = px(40.);

#[cfg(test)]
mod tests {
    use super::{glass_bar_wash, glass_washes};

    #[test]
    fn a_run_of_selected_rows_has_long_pills_with_small_gaps_and_a_lone_row_its_short_one() {
        assert_eq!(super::pill_insets(false, false), (8., 8.));
        assert_eq!(super::pill_insets(true, true), (2., 2.));
        // the gap between two pills in a run: one row's bottom and the next one's top
        let gap = super::pill_insets(false, true).1 + super::pill_insets(true, false).0;
        assert_eq!(gap, 4.);
    }

    fn palette(id: &str, dark: bool, high: bool) -> super::Palette {
        let all = super::data::builtins();
        let t = all.iter().find(|t| t.id == id).unwrap();
        let r = super::data::resolve(&super::data::chain(t, all), dark, high);
        super::Palette::from_resolved(&r, dark, super::accent_for(&r, dark, &super::AccentChoice::Theme(String::new())))
    }

    #[test]
    fn canopy_sheets_follow_the_visibility_and_go_solid_without_a_backdrop() {
        let std_ = palette("canopy", true, false);
        let high = palette("canopy", true, true);
        assert!(std_.backdrop && high.backdrop);
        assert!((std_.content_bg.a - 0.62).abs() < 1e-6, "{}", std_.content_bg.a);
        assert!((high.content_bg.a - 0.88).abs() < 1e-6);
        assert!((palette("canopy", false, true).content_bg.a - 0.90).abs() < 1e-6);
        assert!(std_.window_bg.a < 0.5, "the window is a veil over the backdrop");
        let mut solid = high;
        solid.apply_solid();
        assert!(!solid.backdrop && solid.window_bg.a == 1.0 && solid.content_bg.a == 1.0 && solid.card_bg.a == 1.0);
        // the accent is the theme's first: Mint
        assert_eq!(high.accent, super::hex(0x86e3b8));
        assert_eq!(palette("canopy", false, true).accent, super::hex(0x1b7550));
    }

    #[test]
    fn stem_and_index_are_opaque_and_mark_selection_their_own_way() {
        for id in ["stem", "index"] {
            for dark in [true, false] {
                let p = palette(id, dark, true);
                assert!(!p.backdrop, "{id}");
                for c in [p.window_bg, p.content_bg, p.card_bg, p.tab_active, p.toolbar_bg, p.sidebar_bg] {
                    assert_eq!(c.a, 1.0, "{id} dark={dark}");
                }
            }
        }
        let stem = palette("stem", true, true);
        assert_eq!((stem.marker_width, stem.tab_marker), (3.0, 2.0));
        assert_eq!(palette("stem", true, false).marker_width, 2.0);
        let index = palette("index", false, true);
        assert_eq!(index.row_selection, index.accent, "a solid accent selection");
        assert_eq!(index.row_selection_text, index.accent_text);
        assert_eq!(index.on_row_selection().text, index.accent_text);
        assert!(index.mono_numbers && index.font_ui == "Bahnschrift" && index.row_height == 28.0);
        assert!(index.stripe.a > 0.0 && index.status_bg.a == 1.0);
        assert_eq!(index.for_status().text, index.status_text);
        // the other two keep the text on their selection
        assert_eq!(palette("canopy", true, true).on_row_selection().text, palette("canopy", true, true).text);
    }

    #[test]
    fn black_keeps_the_accent_and_paints_everything_black() {
        let b = super::Palette::blacken(palette("canopy", true, true));
        assert_eq!(b.window_bg, super::hex(0x000000));
        assert_eq!(b.content_bg, super::hex(0x000000));
        assert!(!b.backdrop);
        assert_eq!(b.accent, super::hex(0x86e3b8));
    }

    #[test]
    fn windows_or_own_accent_overrides_the_theme_set() {
        let all = super::data::builtins();
        let r = super::data::resolve(&super::data::chain(&all[1], all), false, true);
        let (a, t) = super::accent_for(&r, false, &super::AccentChoice::Custom(0xF7630C));
        assert_eq!(a, super::hex(rf_win::accent::from_color(0xF7630C).dark1()));
        assert_eq!(t, super::hex(0xFFFFFF));
        let (a, _) = super::accent_for(&r, false, &super::AccentChoice::Theme("lake".into()));
        assert_eq!(a, super::hex(0x2b64a8), "by name, any case");
        let (a, _) = super::accent_for(&r, false, &super::AccentChoice::Theme("nope".into()));
        assert_eq!(a, super::hex(0x3d7a2c), "unknown: the first");
    }

    #[test]
    fn a_bar_over_glass_always_keeps_a_wash_of_its_own() {
        assert!((glass_bar_wash(0) - 0.40).abs() < 1e-6);
        assert!((glass_bar_wash(50) - 0.55).abs() < 1e-6);
        assert!((glass_bar_wash(100) - 0.70).abs() < 1e-6);
        assert_eq!(glass_bar_wash(200), glass_bar_wash(100));
        assert!((0..=100u8).all(|v| glass_bar_wash(v) >= 0.40), "readable at every setting");
    }

    #[test]
    fn the_default_surface_is_the_shipped_veil_exactly() {
        assert_eq!(glass_washes(true, 50), (0.18, 0.22));
        assert_eq!(glass_washes(false, 50), (0.20, 0.35));
        assert_eq!(glass_washes(true, crate::settings::HG_DEFAULT[4]), (0.18, 0.22), "and 50 is what the settings default to");
    }

    #[test]
    fn the_ends_are_no_veil_and_the_mica_strength() {
        assert_eq!(glass_washes(true, 0), (0.0, 0.0));
        assert_eq!(glass_washes(false, 0), (0.0, 0.0));
        assert_eq!(glass_washes(true, 100), (0.40, 0.45));
        assert_eq!(glass_washes(false, 100), (0.30, 0.50));
        assert_eq!(glass_washes(true, 255), glass_washes(true, 100), "a value out of range counts as the end");
    }

    #[test]
    fn the_veil_only_grows_with_the_value_and_stays_a_valid_alpha() {
        for dark in [true, false] {
            let mut last = (-1.0f32, -1.0f32);
            for v in 0..=100u8 {
                let (w, c) = glass_washes(dark, v);
                assert!(w >= last.0 && c >= last.1, "dark={dark} v={v}: {w} {c} after {last:?}");
                assert!((0.0..=1.0).contains(&w) && (0.0..=1.0).contains(&c));
                last = (w, c);
            }
        }
        // a quarter of the way is half of the shipped veil, three quarters is halfway to the Mica strength
        let (w, c) = glass_washes(true, 25);
        assert!((w - 0.09).abs() < 1e-6 && (c - 0.11).abs() < 1e-6, "{w} {c}");
        let (w, c) = glass_washes(false, 75);
        assert!((w - 0.25).abs() < 1e-6 && (c - 0.425).abs() < 1e-6, "{w} {c}");
    }
}

/// The display a popup of `owner` (a menu level, a flyout, a dropdown) is opened on: the owner's own.
///
/// WHY IT IS NAMED (issue 47, a 1920x1080 monitor at 100% beside a 3440x1440 one at 125%): positions are logical pixels, and on
/// Windows a monitor's logical pixels are its device pixels divided by its own scale, so the two monitors' logical ranges overlap. A
/// popup that names no display is put on the first display whose scale makes its position land on it, and a submenu meant for the
/// 100% monitor landed on the 125% one, a screen away and a quarter larger ("the menu breaks and shifts down"). The owner's
/// coordinates are its display's, so that display is the one that reads them right.
pub fn popup_display(owner: &Window, cx: &App) -> Option<DisplayId> {
    owner.display(cx).map(|d| d.id())
}
