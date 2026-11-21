//! Linux backend of rf_win::accent. Status: REAL
//!
//! The system accent, from KDE's `kdeglobals` (`[General] AccentColor=r,g,b`), else the settings portal
//! (`org.freedesktop.appearance` `accent-color`, three floats 0..1) over the session bus. `None` when neither has one: the app then
//! uses the accent of its own theme. The portal is asked once, on a thread, and waited for at most a second (a missing portal can
//! take much longer to answer). The dark / light preference (`color-scheme`) comes to the app through GPUI's window appearance, which
//! reads the same portal; [`portal_color_scheme`] is here for what wants it directly.

use std::sync::OnceLock;
use std::time::Duration;

/// Shades of the accent, as 0xRRGGBB: `[light3, light2, light1, base, dark1, dark2, dark3]`.
// TYPE-COPY(rf-win accent.rs:7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentPalette {
    pub shades: [u32; 7],
}

impl AccentPalette {
    pub fn light2(&self) -> u32 {
        self.shades[1]
    }
    pub fn dark1(&self) -> u32 {
        self.shades[4]
    }
    pub fn base(&self) -> u32 {
        self.shades[3]
    }
}

/// The system's accent as a palette; `None` when the desktop has none to give.
pub fn system_palette() -> Option<AccentPalette> {
    static SYSTEM: OnceLock<Option<u32>> = OnceLock::new();
    (*SYSTEM.get_or_init(read_system_accent)).map(from_color)
}

fn read_system_accent() -> Option<u32> {
    kde_accent().or_else(portal_accent)
}

fn kde_accent() -> Option<u32> {
    let dir = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).filter(|p| p.is_absolute()).or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    let text = std::fs::read_to_string(dir.join("kdeglobals")).ok()?;
    parse_kdeglobals_accent(&text)
}

/// `AccentColor=61,174,238` of the `[General]` group.
fn parse_kdeglobals_accent(text: &str) -> Option<u32> {
    let mut in_general = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_general = l == "[General]";
        } else if in_general {
            if let Some(v) = l.strip_prefix("AccentColor=") {
                let c: Vec<u32> = v.split(',').filter_map(|n| n.trim().parse::<u32>().ok()).collect();
                if c.len() >= 3 && c[..3].iter().all(|&n| n <= 255) {
                    return Some(c[0] << 16 | c[1] << 8 | c[2]);
                }
            }
        }
    }
    None
}

/// The portal's accent: three floats of 0..1; anything outside that range means "no accent chosen".
fn accent_from_floats(r: f64, g: f64, b: f64) -> Option<u32> {
    if ![r, g, b].iter().all(|c| (0.0..=1.0).contains(c)) {
        return None;
    }
    let ch = |c: f64| (c * 255.0).round() as u32;
    Some(ch(r) << 16 | ch(g) << 8 | ch(b))
}

fn portal_read(key: &'static str) -> Option<zbus::zvariant::OwnedValue> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| {
            let bus = crate::bus::session()?;
            let msg = bus
                .call_method(
                    Some("org.freedesktop.portal.Desktop"),
                    "/org/freedesktop/portal/desktop",
                    Some("org.freedesktop.portal.Settings"),
                    "Read",
                    &("org.freedesktop.appearance", key),
                )
                .ok()?;
            msg.body().deserialize::<zbus::zvariant::OwnedValue>().ok()
        })();
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(1)).ok().flatten()
}

/// The value inside the portal's variant wrappers (`Read` answers a variant of a variant).
fn unwrap_value<'a>(mut v: &'a zbus::zvariant::Value<'a>) -> &'a zbus::zvariant::Value<'a> {
    while let zbus::zvariant::Value::Value(inner) = v {
        v = inner;
    }
    v
}

fn portal_accent() -> Option<u32> {
    use zbus::zvariant::Value;
    let v = portal_read("accent-color")?;
    let Value::Structure(s) = unwrap_value(&v) else { return None };
    let f: Vec<f64> = s.fields().iter().filter_map(|x| if let Value::F64(n) = unwrap_value(x) { Some(*n) } else { None }).collect();
    if f.len() != 3 {
        return None;
    }
    accent_from_floats(f[0], f[1], f[2])
}

/// The portal's `color-scheme`: `Some(true)` dark, `Some(false)` light, `None` for "no preference" or no portal.
pub fn portal_color_scheme() -> Option<bool> {
    let v = portal_read("color-scheme")?;
    match unwrap_value(&v) {
        zbus::zvariant::Value::U32(n) => scheme_is_dark(*n),
        _ => None,
    }
}

/// 1 is "prefer dark", 2 "prefer light", 0 no preference.
fn scheme_is_dark(n: u32) -> Option<bool> {
    match n {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The colour the user chose for leaf in place of the system's accent (`NO_OVERRIDE`: none).
static OVERRIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(NO_OVERRIDE);
const NO_OVERRIDE: u32 = u32::MAX;

/// Sets or clears the user's own accent colour (0xRRGGBB).
pub fn set_override(color: Option<u32>) {
    OVERRIDE.store(color.map_or(NO_OVERRIDE, |c| c & 0xFF_FFFF), std::sync::atomic::Ordering::Relaxed);
}

/// The user's own accent colour, when they chose one.
pub fn override_color() -> Option<u32> {
    let c = OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
    (c != NO_OVERRIDE).then_some(c)
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32).min(255) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// A palette made from one colour: three lighter shades (towards white), the colour, three darker ones (towards black); the same
/// math as rf-win's.
pub fn from_color(base: u32) -> AccentPalette {
    let base = base & 0xFF_FFFF;
    AccentPalette {
        shades: [mix(base, 0xFF_FFFF, 0.54), mix(base, 0xFF_FFFF, 0.36), mix(base, 0xFF_FFFF, 0.18), base, mix(base, 0, 0.15), mix(base, 0, 0.30), mix(base, 0, 0.45)],
    }
}

/// A colour as the user writes it: "#1A2B3C", "1a2b3c", or the short "#abc". `None` for anything else.
pub fn parse_hex(text: &str) -> Option<u32> {
    let t = text.trim().trim_start_matches('#');
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match t.len() {
        6 => u32::from_str_radix(t, 16).ok(),
        3 => {
            let d: Vec<u32> = t.chars().filter_map(|c| c.to_digit(16)).collect();
            Some((d[0] * 17) << 16 | (d[1] * 17) << 8 | d[2] * 17)
        }
        _ => None,
    }
}

/// A colour as it is kept in the settings: "#RRGGBB".
pub fn to_hex(color: u32) -> String {
    format!("#{:06X}", color & 0xFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_and_portal_values_are_read() {
        let ini = "[Colors:View]\nAccentColor=1,2,3\n\n[General]\nColorScheme=BreezeDark\nAccentColor=61,174,238\n";
        assert_eq!(parse_kdeglobals_accent(ini), Some(0x3DAEEE));
        assert_eq!(parse_kdeglobals_accent("[General]\nAccentColor=300,0,0\n"), None);
        assert_eq!(parse_kdeglobals_accent("[General]\nName=x\n"), None);
        assert_eq!(accent_from_floats(1.0, 0.0, 0.5), Some(0xFF0080));
        assert_eq!(accent_from_floats(-1.0, 0.0, 0.0), None);
        assert_eq!(scheme_is_dark(1), Some(true));
        assert_eq!(scheme_is_dark(2), Some(false));
        assert_eq!(scheme_is_dark(0), None);
        assert_eq!(parse_hex("#abc"), Some(0xAABBCC));
        assert_eq!(from_color(0x102030).base(), 0x102030);
        assert_eq!(to_hex(0x1A2B3C), "#1A2B3C");
    }
}
