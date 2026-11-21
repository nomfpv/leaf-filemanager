//! `LEAF_MOCKUP_DEMO=1`: the window shows the Canopy mockup's content (`leaf-notes/design/mockups/direction-2-canopy.html`) so the
//! app can be compared with it picture by picture, without touching real data: the folders, the tags, the hosts, the lanes, the
//! running copy, the cut row, the rename box and the selection are made up here, in memory. The demo's folders live under
//! `C:\Users\user\`; the demo stays off on a PC where that folder exists, and while it is on, no file operation is started.
//! Off (and free) unless the variable is set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::*;
use rf_core::{Entry, EntryKind};

pub const LEAF_SITE: &str = r"C:\Users\user\Projects\leaf-site";
pub const HOLIDAY: &str = r"C:\Users\user\Pictures\holiday-2026";
pub const DOCUMENTS: &str = r"C:\Users\user\Documents";
pub const DOWNLOADS: &str = r"C:\Users\user\Downloads";
pub const DESKTOP: &str = r"C:\Users\user\Desktop";
pub const PROJECTS: &str = r"C:\Users\user\Projects";
pub const PICTURES: &str = r"C:\Users\user\Pictures";
/// The third tab: a server folder that is still being listed (it never finishes in the demo).
pub const NAS: &str = r"C:\Users\user\Hosts\nas.example.com";

/// The demo is on: the variable is set and `C:\Users\user` is no real folder on this PC.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let asked = std::env::var("LEAF_MOCKUP_DEMO").is_ok_and(|v| !v.is_empty() && v != "0");
        if asked && Path::new(r"C:\Users\user").exists() {
            log::warn!("LEAF_MOCKUP_DEMO: C:\\Users\\user exists on this PC, the demo stays off");
            return false;
        }
        asked
    })
}

/// A demo folder's listing.
pub enum Listing {
    Entries(Vec<Entry>),
    /// Still being listed (the busy tab): the listing thread waits until it is cancelled.
    Busy,
}

fn ms(stamp: &str) -> i64 {
    use chrono::TimeZone as _;
    let t = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M").expect("demo time");
    chrono::Local.from_local_datetime(&t).earliest().map_or(0, |d| d.timestamp_millis())
}

fn dir(name: &str, stamp: &str) -> Entry {
    Entry::new(name, EntryKind::Dir, 0, ms(stamp), ms(stamp), 0x10)
}

fn file(name: &str, size: u64, stamp: &str) -> Entry {
    Entry::new(name, EntryKind::File, size, ms(stamp), ms(stamp), 0x20)
}

/// The holiday pictures: (name, picture of `THUMBS`).
const PHOTOS: [(&str, usize); 12] = [
    ("beach-morning.jpg", 0),
    ("cliff-path.jpg", 1),
    ("harbour-dusk.jpg", 2),
    ("pine-forest.jpg", 4),
    ("lake-mirror.jpg", 5),
    ("dunes-noon.jpg", 6),
    ("old-town.jpg", 7),
    ("snow-ridge.jpg", 10),
    ("field-flowers.jpg", 11),
    ("sunset-pier.jpg", 0),
    ("night-market.jpg", 3),
    ("clip-waves.mp4", 2),
];

/// The listing of a demo folder, `None` for every other path (those are listed as always).
pub fn list(path: &Path) -> Option<Listing> {
    if !on() {
        return None;
    }
    let p = path.to_string_lossy();
    let p = p.trim_end_matches('\\');
    let es = if p.eq_ignore_ascii_case(LEAF_SITE) {
        vec![
            dir("assets", "2026-11-18 14:20"),
            dir("docs", "2026-11-20 18:02"),
            dir("drafts", "2026-11-12 09:40"),
            dir("exports", "2026-11-19 21:15"),
            dir("src", "2026-11-21 10:05"),
            file("report-final.pdf", 2516582, "2026-11-19 16:42"),
            file("holiday-2026.jpg", 7234150, "2026-08-14 09:12"),
            file("main.rs", 14540, "2026-11-21 10:05"),
            file("Cargo.toml", 1212, "2026-11-20 17:31"),
            file("README.md", 6204, "2026-11-20 17:34"),
            file("notes.md", 3388, "2026-11-21 08:50"),
            file("budget-q3.ods", 48128, "2026-10-02 11:20"),
            file("logo-draft.svg", 22016, "2026-11-14 15:03"),
            file("screenshot-1104.png", 1204224, "2026-11-04 20:47"),
            file("backup-2026-10.zip", 412316860, "2026-10-31 23:58"),
            file("meeting-notes.txt", 2048, "2026-11-17 10:30"),
            file("install.sh", 4096, "2026-11-09 13:12"),
            file("video-clip.mp4", 38797312, "2026-09-03 18:22"),
            file("config.toml", 980, "2026-11-20 17:31"),
        ]
    } else if p.eq_ignore_ascii_case(HOLIDAY) {
        PHOTOS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let video = name.ends_with(".mp4");
                let size = if video { 52428800 } else { 4100000 + i as u64 * 377311 };
                file(name, size, &format!("2026-08-{:02} {:02}:{:02}", 10 + i, 8 + i % 10, 5 + i * 4))
            })
            .collect()
    } else if p.eq_ignore_ascii_case(DOCUMENTS) {
        vec![
            dir("Invoices", "2026-11-02 09:10"),
            dir("Letters", "2026-09-21 12:00"),
            dir("University", "2026-10-28 19:44"),
            file("tax-return-2025.pdf", 884736, "2026-05-12 20:15"),
            file("lease-agreement.pdf", 1572864, "2026-03-01 10:02"),
            file("cv-2026.pdf", 412672, "2026-10-30 22:41"),
            file("recipes.md", 9216, "2026-11-15 18:30"),
            file("travel-plan.ods", 30720, "2026-07-02 08:00"),
            file("scan-0031.pdf", 2202009, "2026-11-20 11:11"),
            file("insurance-letter.pdf", 301056, "2026-11-18 15:20"),
        ]
    } else if p.eq_ignore_ascii_case(DOWNLOADS) {
        vec![
            file("installer-x64.msi", 94371840, "2026-11-20 21:02"),
            file("dataset-2026.csv", 6815744, "2026-11-19 13:40"),
            file("wallpapers.zip", 210763776, "2026-11-12 17:05"),
            file("manual-v3.pdf", 5452595, "2026-11-08 09:30"),
            file("image-0921.png", 2936012, "2026-09-21 16:16"),
            file("font-pack.zip", 18874368, "2026-10-19 11:48"),
        ]
    } else if p.eq_ignore_ascii_case(PROJECTS) {
        vec![dir("leaf-site", "2026-11-21 10:05")]
    } else if p.eq_ignore_ascii_case(PICTURES) {
        vec![dir("holiday-2026", "2026-08-21 17:49")]
    } else if p.eq_ignore_ascii_case(NAS) {
        return Some(Listing::Busy);
    } else if p.eq_ignore_ascii_case(DESKTOP) || p.to_ascii_lowercase().starts_with(r"c:\users\user") {
        Vec::new()
    } else {
        return None;
    };
    Some(Listing::Entries(es))
}

/// How many items a demo folder holds ("24 items" in the Size column).
pub fn child_count(path: &Path) -> Option<u32> {
    if !on() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_string();
    let n = match name.as_str() {
        "assets" => 24,
        "docs" => 9,
        "drafts" => 5,
        "exports" => 31,
        "src" => 42,
        "Invoices" => 18,
        "Letters" => 7,
        "University" => 64,
        "leaf-site" => 19,
        "holiday-2026" => 12,
        _ => return path.to_string_lossy().to_ascii_lowercase().starts_with(r"c:\users\user").then_some(0),
    };
    Some(n)
}

/// A demo video's play time (the grid's badge).
pub fn video_length(path: &Path) -> Option<String> {
    if !on() {
        return None;
    }
    match path.file_name()?.to_str()? {
        "clip-waves.mp4" => Some("1:08".into()),
        "video-clip.mp4" => Some("0:42".into()),
        _ => None,
    }
}

/// The free space the status line names for a demo path: (bytes, drive root).
pub fn free_space(path: &Path) -> Option<(u64, PathBuf)> {
    (on() && path.to_string_lossy().to_ascii_lowercase().starts_with(r"c:\users\user")).then(|| (214_000_000_000, PathBuf::from(r"C:\")))
}

/// The pinned folders of the demo sidebar (title, path, icon).
pub fn pinned() -> Vec<crate::places::Pinned> {
    use gpui_kit::assets::IconName as L;
    [("Desktop", DESKTOP, L::Monitor), ("Documents", DOCUMENTS, L::FileText), ("Downloads", DOWNLOADS, L::Download), ("Pictures", PICTURES, L::Image), ("Projects", PROJECTS, L::Folder)]
        .into_iter()
        .map(|(title, path, icon)| crate::places::Pinned { title: title.into(), path: PathBuf::from(path), icon })
        .collect()
}

/// The demo's tags: the mockup's four, on its files, with the mockup's counts (the rest of each count are made-up paths in
/// Documents). Never written to disk.
pub fn tags() -> rf_core::tags::TagStore {
    let mut t = rf_core::tags::TagStore { defs: Vec::new(), items: Default::default(), next_id: 1 };
    let work = t.add("Work", 0x7cb4ff);
    let personal = t.add("Personal", 0x79dd9b);
    let review = t.add("Review", 0xf2c466);
    let urgent = t.add("Urgent", 0xff8a80);
    let site = |n: &str| Path::new(LEAF_SITE).join(n);
    let on_files: [(u32, Vec<PathBuf>); 4] = [
        (work, vec![site("exports"), site("report-final.pdf"), site("budget-q3.ods")]),
        (personal, vec![site("holiday-2026.jpg"), Path::new(HOLIDAY).join("harbour-dusk.jpg")]),
        (review, vec![site("main.rs")]),
        (urgent, vec![site("budget-q3.ods")]),
    ];
    for (id, (tag, paths)) in [(work, 23usize), (personal, 41), (review, 6), (urgent, 2)].into_iter().zip(on_files) {
        let filler = (paths.len()..id.1).map(|i| Path::new(DOCUMENTS).join(format!("tagged-{}-{i}", id.0)));
        let all: Vec<PathBuf> = paths.into_iter().chain(filler).collect();
        t.toggle(&all, tag);
    }
    t
}

/// The demo sidebar's hosts: (name, second line, online).
pub const HOSTS: [(&str, &str, bool); 2] = [("nas.example.com", "SFTP · 2 ms", true), ("192.0.2.10", "SMB · offline", false)];
/// The demo sidebar's lanes: (name, second line, working).
pub const LANES: [(&str, &str, bool); 2] = [("search-index", "agent working", true), ("fix-rename", "idle · 2 files changed", false)];

/// The running copy the demo's status line shows: (words, fraction, bytes a second, seconds left).
pub const COPY: (&str, f32, f64, f64) = ("Copying 214 files to nas.example.com", 0.62, 38.4e6, 67.0);

/// The item the left pane is renaming and what is typed in its box.
pub const RENAMING: (&str, &str) = ("drafts", "drafts-2025");
/// The row the mockup shows under the pointer (its hover fill).
pub const HOVERED: &str = "docs";
/// The cut item (waits for a paste).
pub const CUT: &str = "backup-2026-10.zip";
/// The selection of the left pane (the cursor on the second), and of the right one.
pub const SELECTED_LEFT: [&str; 3] = ["report-final.pdf", "holiday-2026.jpg", "video-clip.mp4"];
pub const SELECTED_RIGHT: [&str; 1] = ["harbour-dusk.jpg"];

// ------------------------------------------------------------------------------------------------ the pictures

/// The picture of a demo photo or video (the mockup's abstract "photos", painted from the same gradients), 4:3.
pub fn thumbnail(path: &Path, size: u32) -> Option<Arc<RenderImage>> {
    if !on() || !path.to_string_lossy().eq_ignore_ascii_case(&Path::new(HOLIDAY).join(path.file_name()?).to_string_lossy()) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let ix = PHOTOS.iter().find(|(n, _)| *n == name)?.1;
    static CACHE: Mutex<Option<HashMap<(usize, u32), Arc<RenderImage>>>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    let map = cache.get_or_insert_with(HashMap::new);
    let w = size.clamp(64, 512);
    if let Some(img) = map.get(&(ix, w)) {
        return Some(img.clone());
    }
    let h = w * 3 / 4;
    let data = paint(ix, w, h);
    let img = crate::image_cache::to_render_image(&leaf_sys::thumbs::Bgra { width: w, height: h, data })?;
    map.insert((ix, w), img.clone());
    Some(img)
}

type Rgba = [f32; 4];

fn hex(c: u32) -> Rgba {
    [((c >> 16) & 255) as f32 / 255., ((c >> 8) & 255) as f32 / 255., (c & 255) as f32 / 255., 1.]
}
const CLEAR: Rgba = [0., 0., 0., 0.];

/// One CSS gradient layer (the first of a picture's layers is on top).
enum Layer {
    /// `linear-gradient(deg, stops)`, the stops at fractions of the gradient line.
    Linear(f32, Vec<(f32, Rgba)>),
    /// `radial-gradient(circle at x y, stops)`, the stops at fractions of the distance to the farthest corner.
    Radial(f32, f32, Vec<(f32, Rgba)>),
    /// `repeating-linear-gradient(deg, stops)`, the stops in CSS pixels, repeating every `period`.
    Repeat(f32, f32, Vec<(f32, Rgba)>),
}

fn stops(list: &[(f32, u32)]) -> Vec<(f32, Rgba)> {
    list.iter().map(|&(p, c)| (p, if c == u32::MAX { CLEAR } else { hex(c) })).collect()
}
const T: u32 = u32::MAX; // transparent

fn layers(ix: usize) -> Vec<Layer> {
    use Layer::*;
    let l = |deg: f32, s: &[(f32, u32)]| Linear(deg, stops(s));
    let r = |x: f32, y: f32, s: &[(f32, u32)]| Radial(x, y, stops(s));
    match ix {
        0 => vec![r(0.70, 0.38, &[(0., 0xffe2a8), (0.07, 0xffe2a8), (0.08, T)]), l(180., &[(0., 0xf3b98a), (0.40, 0xe88d76), (0.41, 0x5a87ad), (1., 0x2c5a80)])],
        1 => vec![
            l(160., &[(0.55, T), (0.555, 0x3d5a3a)]),
            l(200., &[(0.60, T), (0.605, 0x56734a)]),
            l(180., &[(0., 0xb9d3dc), (0.60, 0x8fb3c2), (1., 0x6a8f7a)]),
        ],
        2 => vec![
            r(0.30, 0.70, &[(0., 0xffcf70), (0.02, 0xffcf70), (0.03, T)]),
            r(0.55, 0.72, &[(0., 0xffb85c), (0.015, 0xffb85c), (0.025, T)]),
            l(180., &[(0., 0x2b2f55), (0.45, 0x7b4f74), (0.62, 0xe08a63), (0.63, 0x1d2a3a), (1., 0x0f1824)]),
        ],
        3 => vec![
            r(0.20, 0.30, &[(0., 0xf7d27a), (0.03, 0xf7d27a), (0.04, T)]),
            r(0.65, 0.40, &[(0., 0xe66a6a), (0.03, 0xe66a6a), (0.04, T)]),
            r(0.80, 0.25, &[(0., 0x7ad1f7), (0.025, 0x7ad1f7), (0.035, T)]),
            l(180., &[(0., 0x12152a), (0.60, 0x262a4a), (1., 0x3b3048)]),
        ],
        4 => vec![Repeat(90., 11., stops(&[(0., 0x1f3d2a), (6., 0x1f3d2a), (6., 0x264a33), (11., 0x264a33)]))],
        5 => vec![l(180., &[(0., 0xa9c9e8), (0.48, 0xd8e8f2), (0.50, 0x7d9fb8), (1., 0xa9c9e8)])],
        6 => vec![l(170., &[(0.58, T), (0.585, 0xc98a4e)]), l(190., &[(0.52, T), (0.525, 0xe3a865)]), l(180., &[(0., 0xf2d7a6), (1., 0xe9b77a)])],
        7 => vec![l(
            90.,
            &[
                (0., 0xc7795a),
                (0.18, 0xc7795a),
                (0.18, 0xe0c79a),
                (0.34, 0xe0c79a),
                (0.34, 0x8aa3b5),
                (0.52, 0x8aa3b5),
                (0.52, 0xd9a05b),
                (0.70, 0xd9a05b),
                (0.70, 0xb5685a),
                (0.86, 0xb5685a),
                (0.86, 0xe8dcc2),
            ],
        )],
        10 => vec![l(150., &[(0.50, T), (0.505, 0xe9f0f5)]), l(210., &[(0.46, T), (0.465, 0xcfdbe6)]), l(180., &[(0., 0x5d7fa3), (1., 0x9db6cf)])],
        11 => vec![
            r(0.20, 0.80, &[(0., 0xd94a4a), (0.03, 0xd94a4a), (0.04, T)]),
            r(0.45, 0.70, &[(0., 0xe85b4d), (0.03, 0xe85b4d), (0.04, T)]),
            r(0.72, 0.82, &[(0., 0xd24646), (0.03, 0xd24646), (0.04, T)]),
            l(180., &[(0., 0xbfe0f0), (0.45, 0xbfe0f0), (0.46, 0x7fa65a), (1., 0x4f7a3a)]),
        ],
        _ => vec![l(180., &[(0., 0x1b1f22), (0.10, 0x1b1f22), (0.10, 0x2a3035), (1., 0x2a3035)])],
    }
}

/// The colour at `t` of a stop list (premultiplied while mixing, as CSS does; a repeated position is a hard edge).
fn at(stops: &[(f32, Rgba)], t: f32) -> Rgba {
    let first = stops[0];
    if t <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((p0, c0), (p1, c1)) = (w[0], w[1]);
        if t < p1 {
            let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1. };
            let a = c0[3] + (c1[3] - c0[3]) * f;
            if a <= 0. {
                return CLEAR;
            }
            let ch = |i: usize| (c0[i] * c0[3] + (c1[i] * c1[3] - c0[i] * c0[3]) * f) / a;
            return [ch(0), ch(1), ch(2), a];
        }
    }
    stops[stops.len() - 1].1
}

/// BGRA pixels of picture `ix` at `w` x `h` (the mockup's thumbnails are about 140 CSS pixels wide).
fn paint(ix: usize, w: u32, h: u32) -> Vec<u8> {
    let ls = layers(ix);
    let (fw, fh) = (w as f32, h as f32);
    let css = fw / 140.; // image pixels per CSS pixel
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut col = [0f32; 3];
            let mut cover = 0f32; // how much of the pixel the layers above already cover
            let mut acc = [0f32; 3];
            for l in &ls {
                let c = match l {
                    Layer::Linear(deg, s) | Layer::Repeat(deg, _, s) => {
                        let a = deg.to_radians();
                        let (dx, dy) = (a.sin(), -a.cos());
                        let len = (fw * dx).abs() + (fh * dy).abs();
                        let d = (px - fw / 2.) * dx + (py - fh / 2.) * dy;
                        match l {
                            Layer::Repeat(_, period, _) => at(s, ((d + len / 2.) / css).rem_euclid(*period)),
                            _ => at(s, d / len + 0.5),
                        }
                    }
                    Layer::Radial(cx, cy, s) => {
                        let (ox, oy) = (cx * fw, cy * fh);
                        let far = [(0f32, 0f32), (fw, 0.), (0., fh), (fw, fh)].iter().map(|(x, y)| ((x - ox).powi(2) + (y - oy).powi(2)).sqrt()).fold(0f32, f32::max);
                        at(s, ((px - ox).powi(2) + (py - oy).powi(2)).sqrt() / far)
                    }
                };
                // front to back: what this layer adds under the ones above
                let k = c[3] * (1. - cover);
                for i in 0..3 {
                    acc[i] += c[i] * k;
                }
                cover += k;
                if cover >= 0.999 {
                    break;
                }
            }
            for i in 0..3 {
                col[i] = acc[i];
            }
            let b = |v: f32| (v.clamp(0., 1.) * 255. + 0.5) as u8;
            out.extend_from_slice(&[b(col[2]), b(col[1]), b(col[0]), 255]);
        }
    }
    out
}
