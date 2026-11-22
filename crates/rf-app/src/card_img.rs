//! Pictures leaf draws by itself for the share card and the "About this PC" picture: an SVG made from text is drawn with resvg
//! (which is in the tree already for the icon packs), the leaf logo is laid over it, and the result is a PNG. No window and no GPU
//! take part, so it also works off the UI thread. The first call reads the system's font list (a moment): call from a thread.

use std::sync::LazyLock;

use gpui_kit::Hsla;
use resvg::tiny_skia::{FilterQuality, IntSize, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

const LOGO: &[u8] = include_bytes!("../assets/leaf.png");

/// The UI font and the monospace font of the system, with fall-backs (a name that is not installed is skipped).
pub const SANS: &str = "'Segoe UI', 'Inter', 'DejaVu Sans', 'Noto Sans', sans-serif";
pub const MONO: &str = "'Cascadia Mono', Consolas, 'DejaVu Sans Mono', 'Liberation Mono', monospace";

static OPTIONS: LazyLock<usvg::Options<'static>> = LazyLock::new(|| {
    let mut o = usvg::Options::default();
    let db = o.fontdb_mut();
    db.load_system_fonts();
    if cfg!(windows) {
        db.set_sans_serif_family("Segoe UI");
        db.set_monospace_family("Consolas");
    } else {
        db.set_sans_serif_family("DejaVu Sans");
        db.set_monospace_family("DejaVu Sans Mono");
    }
    o
});

/// "#rrggbb" of a theme colour (its transparency is dropped: a picture has a solid background).
pub fn hex(c: Hsla) -> String {
    let c = c.to_rgb();
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
}

/// Text made safe for an SVG.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Draws an SVG document at `w` x `h` pixels.
pub fn draw_svg(svg: &str, w: u32, h: u32) -> Option<Pixmap> {
    let tree = usvg::Tree::from_str(svg, &OPTIONS).ok()?;
    let mut pix = Pixmap::new(w, h)?;
    resvg::render(&tree, Transform::identity(), &mut pix.as_mut());
    Some(pix)
}

/// Lays the leaf logo over the picture, `size` pixels square with its top left corner at (`x`, `y`).
pub fn put_logo(pix: &mut Pixmap, x: f32, y: f32, size: f32) -> Option<()> {
    let img = image::load_from_memory_with_format(LOGO, image::ImageFormat::Png).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for p in data.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
    let logo = Pixmap::from_vec(data, IntSize::from_wh(w, h)?)?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..PixmapPaint::default() };
    let s = size / w as f32;
    pix.draw_pixmap(0, 0, logo.as_ref(), &paint, Transform::from_translate(x, y).pre_scale(s, s), None);
    Some(())
}

pub fn png(pix: &Pixmap) -> Option<Vec<u8>> {
    pix.encode_png().ok()
}

/// Puts a PNG on the clipboard as a picture.
pub fn copy_png(png: Vec<u8>, cx: &mut gpui_kit::App) {
    use gpui_kit::{ClipboardItem, Image, ImageFormat};
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, png)));
}

/// Asks where to save a PNG and writes it there (the picture is made already).
pub fn save_png(png: Vec<u8>, name: &str, cx: &mut gpui_kit::App) {
    let dir = dirs::picture_dir().or_else(dirs::home_dir).unwrap_or_default();
    let rx = cx.prompt_for_new_path(&dir, Some(name));
    cx.spawn(async move |_| {
        if let Ok(Ok(Some(target))) = rx.await
            && let Err(e) = std::fs::write(&target, png)
        {
            log::warn!("save picture: {e}");
        }
    })
    .detach();
}
