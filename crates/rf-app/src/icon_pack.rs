//! Icon packs (X4): folders in `<config>/icons/<pack>/` with a `pack.toml` (`rf_core::iconpack`) and SVG or PNG pictures, plus one
//! small example pack that ships inside the program. Packs are read the first time one is asked for; a broken pack is logged once and
//! skipped (the built-in line icons draw instead, as they do for any item the pack has no picture for). The pictures themselves are
//! made by `ImageCache::pack_icon` and live in its cache, per (picture, size, colour).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rf_core::iconpack::Pack;
use rf_core::Entry;

/// The id of the pack that ships with leaf.
pub const BUILTIN: &str = "leaf-canopy";

const BUILTIN_FILES: &[(&str, &[u8])] = &[
    ("folder.svg", include_bytes!("../assets/icon-packs/leaf-canopy/folder.svg")),
    ("file.svg", include_bytes!("../assets/icon-packs/leaf-canopy/file.svg")),
    ("rust.svg", include_bytes!("../assets/icon-packs/leaf-canopy/rust.svg")),
    ("js.svg", include_bytes!("../assets/icon-packs/leaf-canopy/js.svg")),
    ("python.svg", include_bytes!("../assets/icon-packs/leaf-canopy/python.svg")),
    ("data.svg", include_bytes!("../assets/icon-packs/leaf-canopy/data.svg")),
    ("markdown.svg", include_bytes!("../assets/icon-packs/leaf-canopy/markdown.svg")),
    ("image.svg", include_bytes!("../assets/icon-packs/leaf-canopy/image.svg")),
    ("archive.svg", include_bytes!("../assets/icon-packs/leaf-canopy/archive.svg")),
    ("pdf.svg", include_bytes!("../assets/icon-packs/leaf-canopy/pdf.svg")),
    ("lock.svg", include_bytes!("../assets/icon-packs/leaf-canopy/lock.svg")),
    ("git.svg", include_bytes!("../assets/icon-packs/leaf-canopy/git.svg")),
];
const BUILTIN_TOML: &str = include_str!("../assets/icon-packs/leaf-canopy/pack.toml");

pub struct Loaded {
    pub id: String,
    pub pack: Pack,
    dir: Option<PathBuf>,
}

impl Loaded {
    /// The pack's picture for an item (a file name inside the pack), if it has one.
    pub fn file_for(&self, e: &Entry) -> Option<&str> {
        self.pack.resolve(e.is_dir(), &e.name, &e.ext)
    }

    /// The bytes of a picture of the pack.
    pub fn read(&self, file: &str) -> Option<Vec<u8>> {
        match &self.dir {
            Some(d) => std::fs::read(d.join(file)).ok(),
            None => BUILTIN_FILES.iter().find(|(n, _)| *n == file).map(|(_, b)| b.to_vec()),
        }
    }
}

/// `<config>/icons`.
pub fn icons_dir() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("icons"))
}

/// Packs asked for so far (`None`: tried and broken, not tried again until [`forget`]).
static LOADED: Mutex<Option<HashMap<String, Option<Arc<Loaded>>>>> = Mutex::new(None);

/// The pack with this id (its folder's name, or the built-in one), loaded on first use. A folder in the config folder with the
/// built-in's id replaces it.
pub fn get(id: &str) -> Option<Arc<Loaded>> {
    if id.is_empty() || id.contains(['/', '\u{5c}']) || id.starts_with('.') {
        return None;
    }
    let mut g = LOADED.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(p) = map.get(id) {
        return p.clone();
    }
    let loaded = load(id);
    map.insert(id.to_string(), loaded.clone());
    loaded
}

fn load(id: &str) -> Option<Arc<Loaded>> {
    let dir = icons_dir().map(|d| d.join(id)).filter(|d| d.join("pack.toml").is_file());
    if let Some(dir) = dir {
        let parsed = std::fs::read_to_string(dir.join("pack.toml")).map_err(|e| e.to_string()).and_then(|t| rf_core::iconpack::parse(&t, id));
        return match parsed {
            Ok(pack) => Some(Arc::new(Loaded { id: id.to_string(), pack, dir: Some(dir) })),
            Err(e) => {
                log::warn!("icon pack {id}: {e} (skipped)");
                None
            }
        };
    }
    if id == BUILTIN {
        return match rf_core::iconpack::parse(BUILTIN_TOML, id) {
            Ok(pack) => Some(Arc::new(Loaded { id: id.to_string(), pack, dir: None })),
            Err(e) => {
                log::warn!("built-in icon pack: {e}");
                None
            }
        };
    }
    log::warn!("icon pack {id}: not found");
    None
}

/// Drops what was loaded, so the next draw reads the packs again (after the user chose a pack or edited one).
pub fn forget() {
    if let Ok(mut g) = LOADED.lock() {
        *g = None;
    }
}

/// The packs the user can choose: (id, name), the built-in one first, then the folders of `<config>/icons` that hold a readable
/// `pack.toml`.
pub fn installed() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(rd) = icons_dir().and_then(|d| std::fs::read_dir(d).ok()) {
        let mut ids: Vec<String> = rd.flatten().filter(|e| e.path().join("pack.toml").is_file()).filter_map(|e| e.file_name().into_string().ok()).collect();
        ids.sort();
        for id in ids {
            if let Some(p) = get(&id) {
                out.push((id.clone(), p.pack.name.clone()));
            }
        }
    }
    if !out.iter().any(|(id, _)| id == BUILTIN) {
        if let Some(p) = get(BUILTIN) {
            out.insert(0, (BUILTIN.to_string(), p.pack.name.clone()));
        }
    }
    out
}

/// A picture of a pack at `px` square, as the cache holds it: premultiplied BGRA pixels. An SVG is drawn with resvg, a PNG (or other
/// bitmap) is scaled. `tint`: paint every pixel in this colour (r, g, b), keeping the picture's alpha.
pub fn render(bytes: &[u8], file: &str, px: u32, tint: Option<[u8; 3]>) -> Option<leaf_sys::thumbs::Bgra> {
    let mut rgba: Vec<u8> = if file.to_ascii_lowercase().ends_with(".svg") {
        let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        let scale = px as f32 / size.width().max(size.height()).max(1.0);
        let mut pix = resvg::tiny_skia::Pixmap::new(px, px)?;
        let (ox, oy) = ((px as f32 - size.width() * scale) / 2.0, (px as f32 - size.height() * scale) / 2.0);
        resvg::render(&tree, resvg::tiny_skia::Transform::from_translate(ox, oy).pre_scale(scale, scale), &mut pix.as_mut());
        // (premultiplied RGBA, as the cache wants it once r and b are swapped)
        pix.take()
    } else {
        let img = image::load_from_memory(bytes).ok()?.resize(px, px, image::imageops::FilterType::Lanczos3).to_rgba8();
        let (w, h) = img.dimensions();
        let mut canvas = image::RgbaImage::new(px, px);
        image::imageops::overlay(&mut canvas, &img, ((px - w) / 2) as i64, ((px - h) / 2) as i64);
        let mut v = canvas.into_raw();
        for c in v.chunks_exact_mut(4) {
            let a = c[3] as u32;
            for k in &mut c[..3] {
                *k = ((*k as u32 * a + 127) / 255) as u8;
            }
        }
        v
    };
    for c in rgba.chunks_exact_mut(4) {
        if let Some([r, g, b]) = tint {
            let a = c[3] as u32;
            c[0] = ((r as u32 * a + 127) / 255) as u8;
            c[1] = ((g as u32 * a + 127) / 255) as u8;
            c[2] = ((b as u32 * a + 127) / 255) as u8;
        }
        c.swap(0, 2);
    }
    Some(leaf_sys::thumbs::Bgra { width: px, height: px, data: rgba })
}
