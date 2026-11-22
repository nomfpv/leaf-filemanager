//! Resim görüntüleyici: önce kabuk küçük resmi, sonra tam görüntü (arka planda çözülür);
//! tekerlekle %10 yakınlaştırma (sığdır ve %100'de yapışır), sürükle-kaydır, çift tık sığdır,
//! düzey göstergesi, saydamlıkta damalı zemin, animasyon, EXIF katmanı, kopyala, PNG kaydet,
//! renkleri ters çevir, koyu/açık zemin.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_sys::thumbs::Bgra;

use super::zoom;
use crate::look::viewer::{MoreItem, Prepared, Probe, ThemeChoice, Viewer};
use gpui_kit::Modifiers;
use crate::look::config::LookSettings;
use crate::settings::LookImageTheme;
use crate::t;

/// QuickLook'un resim görüntüleyicisinin tanıdığı uzantılar.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jfif", "jpe", "png", "apng", "gif", "bmp", "dib", "ico", "cur", "webp", "avif", "heic", "heif", "tif", "tiff", "svg",
    "svgz", "tga", "psd", "dds", "exr", "hdr", "jxl", "qoi", "pbm", "pgm", "ppm", "pnm", "wdp", "jxr", "arw", "cr2", "cr3", "crw", "dng",
    "erf", "kdc", "mef", "mos", "mrw", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f",
];

/// Tam görüntünün en uzun kenarı (GPU doku sınırının altında).
const MAX_SIDE: u32 = 8192;

/// The slideshow goes on across files: each step makes leaf show the next file (a new view), which picks the slideshow up if
/// the step it asked for came in time.
static RESUME_UNTIL: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
const SLIDE_EVERY: Duration = Duration::from_secs(4);
/// How long after a step the next file may take to arrive before the slideshow gives up.
const SLIDE_WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct ImageViewer {
    view: Option<Entity<ImageView>>,
    /// The slideshow needs leaf's list to step through (it is not offered when look was opened from Explorer).
    can_step: bool,
    /// Whether the slideshow runs: the "More" menu reads it (it gets no `App`).
    sliding: Rc<Cell<bool>>,
}

impl Viewer for ImageViewer {
    fn name(&self) -> &'static str {
        "image"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn can_handle(&self, f: &Probe) -> bool {
        // design files (CorelDRAW, EPS, Krita…): the picture saved inside them
        f.is_file() && (EXTENSIONS.contains(&f.ext.as_str()) || leaf_sys::design::sniff(&f.ext, &f.head))
    }
    fn prepare(&mut self, f: &Probe, cx: &mut App) -> Prepared {
        let name = super::file_name(&f.path);
        let dims = leaf_sys::look::decode::dimensions(&f.path);
        let path = f.path.clone();
        self.can_step = crate::look::system::opened_from_app(cx);
        self.sliding = Rc::new(Cell::new(false));
        let (can_step, sliding) = (self.can_step, self.sliding.clone());
        self.view = Some(cx.new(|cx| ImageView::new(path, dims, can_step, sliding, cx)));
        let (w, h) = dims.map_or((800.0, 600.0), |(w, h)| (w as f32, h as f32));
        let title = match dims {
            // (a design file's picture is its preview, not the design's size)
            _ if leaf_sys::design::is_design_ext(&f.ext) => name,
            Some((w, h)) => format!("{w}×{h}: {name}"),
            None => name,
        };
        let mut p = Prepared::new((w.max(240.0), h.max(160.0)), title);
        p.overlay_titlebar = true;
        p.drag_by_body = false;
        p.theme = Some(match cx.global::<LookSettings>().look_image_theme {
            LookImageTheme::Dark => ThemeChoice::Dark,
            LookImageTheme::Light => ThemeChoice::Light,
        });
        p
    }
    fn render(&mut self, _window: &mut Window, _cx: &mut App) -> AnyElement {
        self.view.as_ref().map_or_else(|| div().into_any_element(), |v| v.clone().into_any_element())
    }
    fn more_menu(&self) -> Vec<MoreItem> {
        let Some(v) = self.view.clone() else { return vec![] };
        let item = |label: &'static str, f: fn(&mut ImageView, &mut Window, &mut Context<ImageView>)| {
            let v = v.clone();
            MoreItem { label: label.into(), action: Rc::new(move |window: &mut Window, cx: &mut App| v.update(cx, |iv, cx| f(iv, window, cx))) }
        };
        let mut items = vec![
            item(t!("Rotate right (R)"), |v, _, cx| v.rotate(true, cx)),
            item(t!("Rotate left (Shift+R)"), |v, _, cx| v.rotate(false, cx)),
            item(t!("Copy to clipboard"), |v, _, cx| v.copy(cx)),
            item(t!("Save as PNG…"), |v, _, cx| v.save_png(cx)),
            item(t!("Invert colors"), |v, _, cx| v.toggle_invert(cx)),
            item(t!("EXIF info"), |v, _, cx| {
                v.show_exif = !v.show_exif;
                cx.notify();
            }),
            item(t!("Background: dark/light"), |_, _, cx| {
                crate::look::config::update(cx, |s| {
                    s.look_image_theme = match s.look_image_theme {
                        LookImageTheme::Dark => LookImageTheme::Light,
                        LookImageTheme::Light => LookImageTheme::Dark,
                    }
                });
                cx.notify();
            }),
        ];
        if self.can_step {
            let label = if self.sliding.get() { t!("Stop slideshow (S)") } else { t!("Slideshow (S)") };
            items.insert(2, item(label, |v, _, cx| v.toggle_slideshow(cx)));
        }
        items
    }
    fn on_key(&mut self, key: &str, mods: &Modifiers, _window: &mut Window, cx: &mut App) -> bool {
        let Some(view) = self.view.clone() else { return false };
        if mods.control || mods.alt || mods.platform {
            return false;
        }
        match key {
            "r" => view.update(cx, |v, cx| v.rotate(!mods.shift, cx)),
            "s" if !mods.shift && self.can_step => view.update(cx, |v, cx| v.toggle_slideshow(cx)),
            // any other key ends the slideshow (an arrow then also steps, as it does outside one)
            _ => {
                view.update(cx, |v, cx| v.stop_slideshow(cx));
                return false;
            }
        }
        true
    }
    fn cleanup(&mut self) {
        self.view = None;
    }
}

enum State {
    Loading,
    Ready,
    Failed(String),
}

pub struct ImageView {
    path: PathBuf,
    /// Asıl boyut (döndürülmüş); yakınlaştırma buna göre.
    dims: Option<(f32, f32)>,
    thumb: Option<Arc<RenderImage>>,
    /// Each frame's delay (ms); the pixels live once, in `frames`.
    delays: Vec<u32>,
    frames: Vec<Arc<RenderImage>>,
    frame: usize,
    has_alpha: bool,
    exif: Vec<(String, String)>,
    state: State,
    /// None = sığdır.
    zoom: Option<f32>,
    offset: (f32, f32),
    drag: Option<((f32, f32), (f32, f32))>,
    label_until: Option<Instant>,
    inverted: bool,
    show_exif: bool,
    /// Quarter turns clockwise the view should show, and how many the pixels in `frames` / `thumb` have (they are turned off
    /// the UI thread; `inverted_frames` is the same for the colour inversion).
    turns: u8,
    frames_turns: u8,
    thumb_turns: u8,
    inverted_frames: bool,
    working: bool,
    slideshow: bool,
    can_step: bool,
    sliding: Rc<Cell<bool>>,
    _show: Option<Task<()>>,
    _tasks: Vec<Task<()>>,
}

fn render_image(b: &Bgra) -> Option<Arc<RenderImage>> {
    render_image_owned(Bgra { width: b.width, height: b.height, data: b.data.clone() })
}

/// [`render_image`] taking the pixels over (no copy: a 24 MP photo is a 96 MB buffer).
fn render_image_owned(b: Bgra) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(b.width, b.height, b.data)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

/// A frame's pixels back out of the picture the GPU draws.
fn pixels_of(img: &RenderImage) -> Option<Bgra> {
    let s = img.size(0);
    Some(Bgra { width: s.width.0 as u32, height: s.height.0 as u32, data: img.as_bytes(0)?.to_vec() })
}

/// The decoded image as the view keeps it, made off the UI thread.
struct Ready {
    width: u32,
    height: u32,
    exif: Vec<(String, String)>,
    has_alpha: bool,
    delays: Vec<u32>,
    frames: Vec<Arc<RenderImage>>,
}

fn ready(d: leaf_sys::look::decode::Decoded) -> Ready {
    let has_alpha = d.frames.first().is_some_and(|(b, _)| b.data.chunks_exact(4).any(|p| p[3] < 255));
    let delays = d.frames.iter().map(|(_, ms)| *ms).collect();
    let frames = d.frames.into_iter().filter_map(|(b, _)| render_image_owned(b)).collect();
    Ready { width: d.width, height: d.height, exif: d.exif, has_alpha, delays, frames }
}

/// The picture turned by `turns` quarter turns clockwise.
fn turned(b: Bgra, turns: u8) -> Bgra {
    let (w, h) = (b.width as usize, b.height as usize);
    let turns = turns % 4;
    if turns == 0 || w == 0 || h == 0 {
        return b;
    }
    let (nw, nh) = if turns == 2 { (w, h) } else { (h, w) };
    let mut data = vec![0u8; b.data.len()];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (src, dst) = ((y * w + x) * 4, (dy * nw + dx) * 4);
            data[dst..dst + 4].copy_from_slice(&b.data[src..src + 4]);
        }
    }
    Bgra { width: nw as u32, height: nh as u32, data }
}

fn inverted(b: &Bgra) -> Bgra {
    let mut data = b.data.clone();
    for px in data.chunks_exact_mut(4) {
        px[0] = 255 - px[0];
        px[1] = 255 - px[1];
        px[2] = 255 - px[2];
    }
    Bgra { width: b.width, height: b.height, data }
}

/// Damalı zemin dokusu (bir kez üretilir; pencere iş parçacığında).
fn checker(dark: bool) -> Arc<RenderImage> {
    thread_local! {
        static C: std::cell::RefCell<[Option<Arc<RenderImage>>; 2]> = const { std::cell::RefCell::new([None, None]) };
    }
    C.with(|c| {
        let mut c = c.borrow_mut();
        let slot = &mut c[dark as usize];
        slot.get_or_insert_with(|| {
            let (n, cell) = (1024u32, 8u32);
            let (a, b) = if dark { (0x2a, 0x36) } else { (0xff, 0xe6) };
            let mut data = Vec::with_capacity((n * n * 4) as usize);
            for y in 0..n {
                for x in 0..n {
                    let v = if ((x / cell) + (y / cell)) % 2 == 0 { a } else { b };
                    data.extend_from_slice(&[v, v, v, 255]);
                }
            }
            render_image(&Bgra { width: n, height: n, data }).expect("checker")
        })
        .clone()
    })
}

impl ImageView {
    fn new(path: PathBuf, dims: Option<(u32, u32)>, can_step: bool, sliding: Rc<Cell<bool>>, cx: &mut Context<Self>) -> Self {
        let mut v = Self {
            path: path.clone(),
            dims: dims.map(|(w, h)| (w as f32, h as f32)),
            thumb: None,
            delays: Vec::new(),
            frames: Vec::new(),
            frame: 0,
            has_alpha: false,
            exif: Vec::new(),
            state: State::Loading,
            zoom: None,
            offset: (0.0, 0.0),
            drag: None,
            label_until: None,
            inverted: false,
            show_exif: false,
            turns: 0,
            frames_turns: 0,
            thumb_turns: 0,
            inverted_frames: false,
            working: false,
            slideshow: false,
            can_step,
            sliding,
            _show: None,
            _tasks: Vec::new(),
        };
        // the slideshow's step brought us here: it goes on
        let resume = RESUME_UNTIL.lock().ok().and_then(|mut r| r.take()).is_some_and(|t| Instant::now() < t);
        if resume && can_step {
            v.start_slideshow(cx);
        }
        // pencere kapanınca ya da başka dosyaya geçilince GPU dokuları bırakılır
        cx.on_release(|v: &mut Self, cx| {
            for img in v.frames.drain(..).chain(v.thumb.take()) {
                cx.drop_image(img, None);
            }
        })
        .detach();
        // önce kabuk küçük resmi (çoğunlukla önbellekte, anında)
        let p = path.clone();
        let thumb = cx.background_spawn(async move { leaf_sys::thumbs::get_image(&p, 256, leaf_sys::thumbs::Kind::Thumbnail).ok() });
        v._tasks.push(cx.spawn(async move |this, cx| {
            if let Some(t) = thumb.await {
                let _ = this.update(cx, |v, cx| {
                    if v.frames.is_empty() {
                        v.thumb = render_image(&t);
                        cx.notify();
                    }
                });
            }
        }));
        let raw = path.extension().and_then(|e| e.to_str()).is_some_and(leaf_decode::is_raw_ext);
        let dev_path = path.clone();
        let full = cx.background_spawn(async move { leaf_sys::look::decode::decode_full(&path, MAX_SIDE).map(ready) });
        v._tasks.push(cx.spawn(async move |this, cx| {
            let r = full.await;
            let _ = this.update(cx, |v, cx| v.loaded(r, cx));
            // a camera RAW file whose embedded picture is small: developed from the sensor data in the background and swapped in
            // (a failure leaves the embedded picture as it is)
            if raw {
                let dev = cx.background_spawn(async move { leaf_sys::look::decode::decode_raw_developed(&dev_path, MAX_SIDE).map(|d| d.map(ready)) });
                if let Ok(Some(d)) = dev.await {
                    let _ = this.update(cx, |v, cx| {
                        if !matches!(v.state, State::Failed(_)) {
                            v.loaded(Ok(d), cx)
                        }
                    });
                }
            }
        }));
        v
    }

    fn loaded(&mut self, r: anyhow::Result<Ready>, cx: &mut Context<Self>) {
        crate::look::trace::mark("full image");
        match r {
            Ok(d) => {
                self.dims = Some((d.width as f32, d.height as f32));
                self.exif = d.exif;
                self.has_alpha = d.has_alpha;
                self.delays = d.delays;
                for img in std::mem::replace(&mut self.frames, d.frames) {
                    cx.drop_image(img, None);
                }
                // (inverted or turned before it arrived: done now)
                self.frames_turns = 0;
                self.inverted_frames = false;
                self.pump(cx);
                if let Some(t) = self.thumb.take() {
                    cx.drop_image(t, None);
                }
                self.state = State::Ready;
                if self.frames.len() > 1 {
                    self.animate(cx);
                }
            }
            Err(e) => {
                log::warn!("look image {}: {e:#}", self.path.display());
                self.state = State::Failed(e.to_string());
                // (a HEIC the system cannot read: the shell's thumbnail was only its generic icon; the reason is shown instead)
                if leaf_sys::picture_fallback::is_heif(&self.path) && leaf_sys::picture_fallback::heif_known_missing() {
                    if let Some(t) = self.thumb.take() {
                        cx.drop_image(t, None);
                    }
                }
            }
        }
        cx.notify();
    }

    fn animate(&mut self, cx: &mut Context<Self>) {
        let delays: Vec<u32> = self.delays.clone();
        self._tasks.push(cx.spawn(async move |this, cx| {
            let mut i = 0usize;
            loop {
                cx.background_executor().timer(Duration::from_millis(delays[i % delays.len()].max(20) as u64)).await;
                i += 1;
                if this
                    .update(cx, |v, cx| {
                        v.frame = i % v.frames.len().max(1);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn toggle_invert(&mut self, cx: &mut Context<Self>) {
        self.inverted = !self.inverted;
        cx.notify();
        self.pump(cx);
    }

    /// R / Shift+R: the view turns a quarter (the file is not changed).
    fn rotate(&mut self, clockwise: bool, cx: &mut Context<Self>) {
        self.turns = (self.turns + if clockwise { 1 } else { 3 }) % 4;
        self.zoom = None;
        self.offset = (0.0, 0.0);
        // (the small shell picture is turned at once)
        if self.thumb_turns != self.turns {
            if let Some(t) = self.thumb.take() {
                if let Some(b) = pixels_of(&t) {
                    self.thumb = render_image_owned(turned(b, (self.turns + 4 - self.thumb_turns) % 4));
                }
                cx.drop_image(t, None);
            }
            self.thumb_turns = self.turns;
        }
        cx.notify();
        self.pump(cx);
    }

    /// The pixels in `frames` are brought to the turn and the colours the view asks for, on a helper thread; one job at a time
    /// (when the view asked for more meanwhile, the next job follows).
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.working || self.frames.is_empty() {
            return;
        }
        let quarter = (self.turns + 4 - self.frames_turns) % 4;
        let flip = self.inverted != self.inverted_frames;
        if quarter == 0 && !flip {
            return;
        }
        self.working = true;
        let frames = self.frames.clone();
        let (turns, inv) = (self.turns, self.inverted);
        let work = cx.background_spawn(async move {
            frames
                .iter()
                .filter_map(|f| pixels_of(f))
                .filter_map(|b| render_image_owned(turned(if flip { inverted(&b) } else { b }, quarter)))
                .collect::<Vec<_>>()
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |v, cx| {
                v.working = false;
                if !done.is_empty() {
                    for img in std::mem::replace(&mut v.frames, done) {
                        cx.drop_image(img, None);
                    }
                }
                v.frames_turns = turns;
                v.inverted_frames = inv;
                cx.notify();
                v.pump(cx);
            });
        }));
    }

    fn toggle_slideshow(&mut self, cx: &mut Context<Self>) {
        if self.slideshow {
            self.stop_slideshow(cx);
        } else if self.can_step {
            self.start_slideshow(cx);
        }
    }

    fn start_slideshow(&mut self, cx: &mut Context<Self>) {
        self.slideshow = true;
        self.sliding.set(true);
        self._show = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SLIDE_EVERY).await;
                if !this.update(cx, |v, _| v.slideshow).unwrap_or(false) {
                    break;
                }
                // leaf shows the next file in its list (a new view that carries the slideshow on); nothing came: it ends here
                let sent = cx
                    .background_spawn(async { leaf_sys::single_instance::send_to(&leaf_sys::single_instance::instance_name(), &crate::look::ipc::ToApp::Step(true).encode()) })
                    .await;
                if sent {
                    if let Ok(mut r) = RESUME_UNTIL.lock() {
                        *r = Some(Instant::now() + SLIDE_WAIT);
                    }
                    cx.background_executor().timer(SLIDE_WAIT).await;
                }
                let _ = this.update(cx, |v, cx| v.stop_slideshow(cx));
                break;
            }
        }));
        cx.notify();
    }

    fn stop_slideshow(&mut self, cx: &mut Context<Self>) {
        if !self.slideshow {
            return;
        }
        self.slideshow = false;
        self.sliding.set(false);
        self._show = None;
        if let Ok(mut r) = RESUME_UNTIL.lock() {
            *r = None;
        }
        cx.notify();
    }

    fn png_bytes(&self) -> Option<Vec<u8>> {
        let f = self.frames.get(self.frame.min(self.frames.len().saturating_sub(1)))?;
        // (the shown frame: already inverted when that is on)
        let b = pixels_of(f)?;
        let mut rgba = b.data;
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let img = image::RgbaImage::from_raw(b.width, b.height, rgba)?;
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(out.into_inner())
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(png) = self.png_bytes() {
            cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, png)));
        }
    }

    fn save_png(&mut self, cx: &mut Context<Self>) {
        let Some(png) = self.png_bytes() else { return };
        let dir = self.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
        let rx = cx.prompt_for_new_path(&dir, Some(&format!("{stem}.png")));
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(target))) = rx.await {
                if let Err(e) = std::fs::write(&target, png) {
                    log::warn!("look save png: {e}");
                }
            }
        })
        .detach();
    }

    fn show_label(&mut self, cx: &mut Context<Self>) {
        self.label_until = Some(Instant::now() + Duration::from_millis(900));
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(950)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        if self._tasks.len() > 16 {
            self._tasks.drain(2..self._tasks.len() - 4);
        }
    }

    /// Şu anki ölçek, sığdırma ölçeği ve gösterilen boyut.
    fn layout(&self, area: (f32, f32)) -> Option<(f32, f32, (f32, f32))> {
        let mut dims = self.dims?;
        // (the size of the pixels on the screen: a picture still being turned keeps its old shape)
        let shown_turns = if self.frames.is_empty() { self.thumb_turns } else { self.frames_turns };
        if shown_turns % 2 == 1 {
            dims = (dims.1, dims.0);
        }
        let fit = zoom::fit_zoom(dims, area);
        let z = self.zoom.unwrap_or(fit);
        Some((z, fit, (dims.0 * z, dims.1 * z)))
    }
}

fn area_of(window: &Window) -> (f32, f32) {
    let s = window.viewport_size();
    (f32::from(s.width), f32::from(s.height))
}

impl Render for ImageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let light = cx.global::<LookSettings>().look_image_theme == LookImageTheme::Light;
        let (bg, fg) = if light { (rgb(0xf3f3f3), rgb(0x1a1a1a)) } else { (rgb(0x1c1c1c), rgb(0xf0f0f0)) };
        let area = area_of(window);
        let layout = self.layout(area);
        if let Some((_, _, shown)) = layout {
            self.offset = zoom::clamp_offset(self.offset, shown, area);
        }
        let (ox, oy) = self.offset;
        let image = self.frames.get(self.frame).cloned().or_else(|| self.thumb.clone());
        let label = self.label_until.filter(|t| Instant::now() < *t).and(layout).map(|(z, ..)| format!("{:.0}%", z * 100.0));

        let mut root = div()
            .id("look-image")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(bg)
            .on_scroll(cx.listener(|v, ev: &ScrollEvent, window, cx| {
                // çentik: Windows'ta 3 satır (yumuşak kaydırma bunu parçalara bölerek gönderir);
                // dokunmatik yüzeyin piksel kaydırmasında ~100 piksel
                let dy = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y) / 100.0,
                    ScrollDelta::Lines(l) => l.y / 3.0,
                };
                if dy == 0.0 {
                    return;
                }
                let area = area_of(window);
                let Some((z, fit, _)) = v.layout(area) else { return };
                let nz = zoom::step(z, fit, dy);
                let cursor = (f32::from(ev.position.x), f32::from(ev.position.y));
                v.offset = zoom::zoom_offset(v.offset, cursor, z, nz);
                v.zoom = if (nz - fit).abs() < 1e-6 { None } else { Some(nz) };
                v.show_label(cx);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, _, cx| {
                v.stop_slideshow(cx);
                if ev.click_count == 2 {
                    v.zoom = None;
                    v.show_label(cx);
                    cx.notify();
                    return;
                }
                v.drag = Some(((f32::from(ev.position.x), f32::from(ev.position.y)), v.offset));
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, _: &MouseUpEvent, _, _| v.drag = None))
            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                if let Some((start, from)) = v.drag {
                    if ev.pressed_button != Some(MouseButton::Left) {
                        v.drag = None;
                        return;
                    }
                    v.offset = (from.0 + f32::from(ev.position.x) - start.0, from.1 + f32::from(ev.position.y) - start.1);
                    cx.notify();
                }
            }));

        if let (Some(img_src), Some((_, _, shown))) = (image, layout) {
            if self.has_alpha && !self.inverted {
                root = root.child(
                    div()
                        .absolute()
                        .left(px(ox))
                        .top(px(oy))
                        .w(px(shown.0))
                        .h(px(shown.1))
                        .overflow_hidden()
                        .child(gpui_kit::img(checker(!light)).w(px(1024.)).h(px(1024.))),
                );
            }
            root = root.child(gpui_kit::img(img_src).absolute().left(px(ox)).top(px(oy)).w(px(shown.0)).h(px(shown.1)).object_fit(ObjectFit::Fill));
        } else if let State::Failed(e) = &self.state {
            root = root.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(fg)
                    .when(e != leaf_sys::picture_fallback::HEIF_NEEDS_EXTENSION, |d| d.child(t!("This image could not be opened")))
                    .child(div().text_xs().opacity(0.7).child(e.clone())),
            );
        }
        if let Some(l) = label {
            root = root.child(
                div().absolute().inset_0().flex().items_center().justify_center().child(
                    div().px_4().py_2().rounded(px(8.)).bg(hsla(0., 0., 0., 0.6)).text_color(white()).text_lg().child(l),
                ),
            );
        }
        if self.show_exif && !self.exif.is_empty() {
            root = root.child(
                v_flex()
                    .id("look-exif")
                    .absolute()
                    .left(px(12.))
                    .bottom(px(12.))
                    .max_h(px((area.1 - 80.0).max(100.0)))
                    .overflow_y_scroll()
                    .p_3()
                    .gap_0p5()
                    .rounded(px(8.))
                    .bg(hsla(0., 0., 0., 0.65))
                    .text_color(white())
                    .text_xs()
                    .children(self.exif.iter().map(|(k, v)| h_flex().gap_2().child(div().opacity(0.7).child(k.clone())).child(v.clone()))),
            );
        }
        root.when(matches!(self.state, State::Loading) && self.thumb.is_none(), |d| {
            d.child(div().absolute().inset_0().flex().items_center().justify_center().text_color(fg).opacity(0.6).child(t!("Loading…")))
        })
    }
}
