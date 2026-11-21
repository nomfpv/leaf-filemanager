//! Device icons for saved hosts (a host's `kind`): a clean line set (the default, drawn in the theme's colours) and a realistic set
//! (Settings > Appearance > "Device icons"). The SVGs are in `assets/host-icons`; a picture is made on the first ask and kept per
//! (kind, set, size, colours).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use rf_core::hosts::HostKind;

const C16: &[(&str, &str)] = &[
    ("server", include_str!("../assets/host-icons/c16/server.svg")),
    ("nas", include_str!("../assets/host-icons/c16/nas.svg")),
    ("router", include_str!("../assets/host-icons/c16/router.svg")),
    ("switch", include_str!("../assets/host-icons/c16/switch.svg")),
    ("ap", include_str!("../assets/host-icons/c16/ap.svg")),
    ("firewall", include_str!("../assets/host-icons/c16/firewall.svg")),
    ("pc", include_str!("../assets/host-icons/c16/pc.svg")),
    ("laptop", include_str!("../assets/host-icons/c16/laptop.svg")),
    ("phone", include_str!("../assets/host-icons/c16/phone.svg")),
    ("printer", include_str!("../assets/host-icons/c16/printer.svg")),
    ("vm", include_str!("../assets/host-icons/c16/vm.svg")),
    ("container", include_str!("../assets/host-icons/c16/container.svg")),
    ("cloud", include_str!("../assets/host-icons/c16/cloud.svg")),
];
const C20: &[(&str, &str)] = &[
    ("server", include_str!("../assets/host-icons/c20/server.svg")),
    ("nas", include_str!("../assets/host-icons/c20/nas.svg")),
    ("router", include_str!("../assets/host-icons/c20/router.svg")),
    ("switch", include_str!("../assets/host-icons/c20/switch.svg")),
    ("ap", include_str!("../assets/host-icons/c20/ap.svg")),
    ("firewall", include_str!("../assets/host-icons/c20/firewall.svg")),
    ("pc", include_str!("../assets/host-icons/c20/pc.svg")),
    ("laptop", include_str!("../assets/host-icons/c20/laptop.svg")),
    ("phone", include_str!("../assets/host-icons/c20/phone.svg")),
    ("printer", include_str!("../assets/host-icons/c20/printer.svg")),
    ("vm", include_str!("../assets/host-icons/c20/vm.svg")),
    ("container", include_str!("../assets/host-icons/c20/container.svg")),
    ("cloud", include_str!("../assets/host-icons/c20/cloud.svg")),
];
const REAL: &[(&str, &str)] = &[
    ("server", include_str!("../assets/host-icons/r/server.svg")),
    ("nas", include_str!("../assets/host-icons/r/nas.svg")),
    ("router", include_str!("../assets/host-icons/r/router.svg")),
    ("switch", include_str!("../assets/host-icons/r/switch.svg")),
    ("ap", include_str!("../assets/host-icons/r/ap.svg")),
    ("firewall", include_str!("../assets/host-icons/r/firewall.svg")),
    ("pc", include_str!("../assets/host-icons/r/pc.svg")),
    ("laptop", include_str!("../assets/host-icons/r/laptop.svg")),
    ("phone", include_str!("../assets/host-icons/r/phone.svg")),
    ("printer", include_str!("../assets/host-icons/r/printer.svg")),
    ("vm", include_str!("../assets/host-icons/r/vm.svg")),
    ("container", include_str!("../assets/host-icons/r/container.svg")),
    ("cloud", include_str!("../assets/host-icons/r/cloud.svg")),
];

/// The accent hex the clean set's one detail is drawn in (the dark theme's); replaced by the theme's own.
const SET_ACCENT: &str = "#86e3b8";

static CACHE: Mutex<Option<HashMap<(u8, bool, u32, u32, u32), Option<Arc<RenderImage>>>>> = Mutex::new(None);

fn hex(c: Hsla) -> (String, u32) {
    let c: Rgba = c.into();
    let v = ((c.r * 255.).round() as u32, (c.g * 255.).round() as u32, (c.b * 255.).round() as u32);
    let n = (v.0 << 16) | (v.1 << 8) | v.2;
    (format!("#{n:06x}"), n)
}

/// The picture of a host kind at `size` logical pixels. `realistic`: the realistic set (it ignores the colours); otherwise the
/// clean set in `color`, its one detail in `accent`.
pub fn image(kind: HostKind, realistic: bool, size: f32, color: Hsla, accent: Hsla) -> Option<Arc<RenderImage>> {
    let (color_hex, color_n) = hex(color);
    let (accent_hex, accent_n) = hex(accent);
    let (color_n, accent_n) = if realistic { (0, 0) } else { (color_n, accent_n) };
    let key = (kind as u8, realistic, size as u32, color_n, accent_n);
    let mut g = CACHE.lock().ok()?;
    if let Some(r) = g.get_or_insert_with(HashMap::new).get(&key) {
        return r.clone();
    }
    let table = if realistic { REAL } else if size >= 18. { C20 } else { C16 };
    let made = table.iter().find(|(k, _)| *k == kind.id()).and_then(|(_, svg)| {
        let svg = if realistic { svg.to_string() } else { svg.replace("currentColor", &color_hex).replace(SET_ACCENT, &accent_hex) };
        // (twice the size: the screen may be scaled)
        let px = (size * 2.).round() as u32;
        crate::icon_pack::render(svg.as_bytes(), "x.svg", px, None).and_then(|b| crate::image_cache::to_render_image(&b))
    });
    g.get_or_insert_with(HashMap::new).insert(key, made.clone());
    made
}

/// A host kind's icon as an element (`None` if the picture cannot be made: the caller draws the line server icon).
pub fn element(kind: HostKind, realistic: bool, size: f32, color: Hsla, accent: Hsla) -> Option<AnyElement> {
    image(kind, realistic, size, color, accent).map(|im| gpui_kit::img(im).size(px(size)).flex_shrink_0().into_any_element())
}
