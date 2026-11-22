//! File-type icons from the system icon theme (the twin of Windows' shell icons), used by `thumbs::get_image(.., Kind::Icon)`.
//!
//! Path: file name -> mime type (`globs2` of `$XDG_DATA_HOME/mime` and `$XDG_DATA_DIRS/mime`) -> icon names (the mime type with
//! `/` as `-`, then its entry in `generic-icons`, then `<media>-x-generic`) -> the current icon theme (KDE: `kdeglobals`
//! `[Icons] Theme`; else gsettings; else `hicolor`) and its `Inherits` chain through each `index.theme`, the directory nearest in
//! size, PNG or SVG. Pictures are cached per (name, size). Nothing here fails loudly: no theme or no icon is a `None`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::thumbs::Bgra;

/// An ini file in order: `(section, [(key, value)])`. Comment and blank lines are skipped; text before the first section is
/// section "".
pub(crate) fn parse_ini(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = vec![(String::new(), Vec::new())];
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.push((name.to_string(), Vec::new()));
        } else if let Some((k, v)) = line.split_once('=') {
            if let Some(last) = out.last_mut() {
                last.1.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

fn ini_get<'a>(sec: &'a [(String, String)], key: &str) -> Option<&'a str> {
    sec.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------- mime

#[derive(Default)]
pub(crate) struct Globs {
    /// `*.ext` (lower case, may hold dots: `tar.gz`) -> (weight, mime)
    exts: HashMap<String, (u32, String)>,
    /// plain names (`Makefile`, lower case)
    names: HashMap<String, (u32, String)>,
}

impl Globs {
    /// Adds the lines of one `globs2` file (`weight:mime:glob[:flags]`); a heavier entry replaces a lighter one.
    pub(crate) fn add(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut p = line.splitn(4, ':');
            let (Some(w), Some(mime), Some(glob)) = (p.next(), p.next(), p.next()) else { continue };
            let w: u32 = w.trim().parse().unwrap_or(50);
            let glob = glob.to_lowercase();
            let (map, key) = if let Some(ext) = glob.strip_prefix("*.") {
                if ext.contains(['*', '?', '[']) {
                    continue;
                }
                (&mut self.exts, ext.to_string())
            } else if glob.contains(['*', '?', '[']) {
                continue;
            } else {
                (&mut self.names, glob)
            };
            if map.get(&key).is_none_or(|(old, _)| *old < w) {
                map.insert(key, (w, mime.to_string()));
            }
        }
    }

    /// The mime type of a file name, the longest extension first (`a.tar.gz` is `tar.gz`, then `gz`).
    pub(crate) fn mime_of(&self, name: &str) -> Option<&str> {
        let n = name.to_lowercase();
        if let Some((_, m)) = self.names.get(&n) {
            return Some(m);
        }
        for (i, _) in n.match_indices('.') {
            if let Some((_, m)) = self.exts.get(&n[i + 1..]) {
                return Some(m);
            }
        }
        None
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::data_dir() {
        v.push(d);
    }
    let dd = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    v.extend(dd.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v
}

fn globs() -> &'static Globs {
    static G: OnceLock<Globs> = OnceLock::new();
    G.get_or_init(|| {
        let mut g = Globs::default();
        for d in data_dirs() {
            if let Ok(t) = std::fs::read_to_string(d.join("mime/globs2")) {
                g.add(&t);
            }
        }
        g
    })
}

fn generic_icons() -> &'static HashMap<String, String> {
    static G: OnceLock<HashMap<String, String>> = OnceLock::new();
    G.get_or_init(|| {
        let mut m = HashMap::new();
        for d in data_dirs() {
            if let Ok(t) = std::fs::read_to_string(d.join("mime/generic-icons")) {
                for l in t.lines().filter(|l| !l.starts_with('#')) {
                    if let Some((k, v)) = l.split_once('=') {
                        m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
                    }
                }
            }
        }
        m
    })
}

/// Icon names to try for a mime type, best first.
pub(crate) fn names_for_mime(mime: &str, generic: &HashMap<String, String>) -> Vec<String> {
    let mut v = vec![mime.replace('/', "-")];
    if let Some(g) = generic.get(mime) {
        v.push(g.clone());
    }
    if let Some((media, _)) = mime.split_once('/') {
        v.push(format!("{media}-x-generic"));
    }
    v.push("application-x-generic".into());
    v.push("unknown".into());
    v.dedup();
    v
}

// ---------------------------------------------------------------- theme

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Dir {
    pub path: String,
    pub size: i32,
    pub kind: DirKind,
    pub min: i32,
    pub max: i32,
    pub threshold: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum DirKind {
    Fixed,
    Scalable,
    Threshold,
}

impl Dir {
    /// The spec's "DirectorySizeDistance".
    pub(crate) fn distance(&self, size: i32) -> i32 {
        match self.kind {
            DirKind::Fixed => (self.size - size).abs(),
            DirKind::Scalable => {
                if size < self.min {
                    self.min - size
                } else if size > self.max {
                    size - self.max
                } else {
                    0
                }
            }
            DirKind::Threshold => {
                if size < self.size - self.threshold {
                    self.size - self.threshold - size
                } else if size > self.size + self.threshold {
                    size - (self.size + self.threshold)
                } else {
                    0
                }
            }
        }
    }
}

/// The `Inherits` list and the (scale 1) directories of an `index.theme`.
pub(crate) fn parse_theme(text: &str) -> (Vec<String>, Vec<Dir>) {
    let ini = parse_ini(text);
    let general = ini.iter().find(|(s, _)| s == "Icon Theme").map(|(_, kv)| kv.as_slice()).unwrap_or(&[]);
    let inherits = ini_get(general, "Inherits").map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let mut dirs = Vec::new();
    for name in ini_get(general, "Directories").unwrap_or("").split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((_, kv)) = ini.iter().find(|(s, _)| s == name) else { continue };
        let num = |k: &str| ini_get(kv, k).and_then(|v| v.parse::<i32>().ok());
        if num("Scale").is_some_and(|s| s > 1) {
            continue;
        }
        let Some(size) = num("Size") else { continue };
        let kind = match ini_get(kv, "Type") {
            Some("Fixed") => DirKind::Fixed,
            Some("Scalable") => DirKind::Scalable,
            _ => DirKind::Threshold,
        };
        dirs.push(Dir { path: name.to_string(), size, kind, min: num("MinSize").unwrap_or(size), max: num("MaxSize").unwrap_or(size), threshold: num("Threshold").unwrap_or(2) });
    }
    (inherits, dirs)
}

struct Theme {
    id: String,
    dirs: Vec<Dir>,
}

fn icon_bases() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = dirs::home_dir() {
        v.push(h.join(".icons"));
    }
    v.extend(data_dirs().into_iter().map(|d| d.join("icons")));
    v
}

/// The chain to search: the current theme, what it inherits (depth first), `hicolor` last.
fn chain() -> &'static Vec<Theme> {
    static C: OnceLock<Vec<Theme>> = OnceLock::new();
    C.get_or_init(|| {
        let bases = icon_bases();
        let mut out: Vec<Theme> = Vec::new();
        let mut todo = vec![current_theme_name()];
        while !todo.is_empty() {
            let id = todo.remove(0);
            if out.iter().any(|t| t.id == id) {
                continue;
            }
            let text = bases.iter().find_map(|b| std::fs::read_to_string(b.join(&id).join("index.theme")).ok());
            let (inherits, dirs) = text.map(|t| parse_theme(&t)).unwrap_or_default();
            // (a theme's parents come right after it, before the siblings queued earlier)
            for (i, p) in inherits.into_iter().enumerate() {
                todo.insert(i, p);
            }
            out.push(Theme { id: id.clone(), dirs });
            if todo.is_empty() && id != "hicolor" && !out.iter().any(|t| t.id == "hicolor") {
                todo.push("hicolor".into());
            }
        }
        out
    })
}

fn current_theme_name() -> String {
    if let Some(t) = dirs::config_dir().and_then(|d| std::fs::read_to_string(d.join("kdeglobals")).ok()).and_then(|t| {
        parse_ini(&t).into_iter().find(|(s, _)| s == "Icons").and_then(|(_, kv)| ini_get(&kv, "Theme").map(str::to_string))
    }) {
        if !t.is_empty() {
            return t;
        }
    }
    if let Ok(o) = leaf_proc::output_helper(std::process::Command::new("gsettings").args(["get", "org.gnome.desktop.interface", "icon-theme"]), Some(std::time::Duration::from_secs(5))) {
        let s = String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string();
        if o.status.success() && !s.is_empty() {
            return s;
        }
    }
    "hicolor".into()
}

/// The icon file for `name` nearest to `size`: the first theme of the chain that has it, the nearest directory in it (scalable on a tie).
fn find_icon(name: &str, size: u32) -> Option<PathBuf> {
    let bases = icon_bases();
    for t in chain() {
        let mut best: Option<(i32, bool, PathBuf)> = None;
        for d in &t.dirs {
            let dist = d.distance(size as i32);
            for b in &bases {
                for (ext, svg) in [("png", false), ("svg", true)] {
                    let p = b.join(&t.id).join(&d.path).join(format!("{name}.{ext}"));
                    let better = best.as_ref().is_none_or(|(bd, bs, _)| dist < *bd || (dist == *bd && svg && !*bs));
                    if better && p.is_file() {
                        best = Some((dist, svg, p));
                    }
                }
            }
        }
        if let Some((_, _, p)) = best {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------- pictures

fn load(path: &Path, size: u32) -> Option<Bgra> {
    let bytes = std::fs::read(path).ok()?;
    let rgba = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        render_svg(&bytes, size)?
    } else {
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        if img.width() == size && img.height() == size {
            img
        } else {
            let (w, h) = img.dimensions();
            let f = size as f32 / w.max(h).max(1) as f32;
            image::imageops::resize(&img, ((w as f32 * f).round() as u32).max(1), ((h as f32 * f).round() as u32).max(1), image::imageops::FilterType::Lanczos3)
        }
    };
    let (width, height) = rgba.dimensions();
    let mut data = rgba.into_raw();
    for px in data.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(Bgra { width, height, data })
}

pub(crate) fn render_svg(bytes: &[u8], size: u32) -> Option<image::RgbaImage> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let s = tree.size();
    let f = size as f32 / s.width().max(s.height()).max(1.0);
    let (w, h) = (((s.width() * f).round() as u32).max(1), ((s.height() * f).round() as u32).max(1));
    let mut pix = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(f, f), &mut pix.as_mut());
    // (tiny-skia is premultiplied, the picture is straight alpha)
    let mut raw = Vec::with_capacity((w * h * 4) as usize);
    for p in pix.pixels() {
        let c = p.demultiply();
        raw.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    image::RgbaImage::from_raw(w, h, raw)
}

fn cache() -> &'static Mutex<HashMap<(String, u32), Option<Bgra>>> {
    static C: OnceLock<Mutex<HashMap<(String, u32), Option<Bgra>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The theme's picture for the first of `names` that has one, cached per (name, size).
fn picture_of(names: &[String], size: u32) -> Option<Bgra> {
    for n in names {
        let key = (n.clone(), size);
        if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if hit.is_some() {
                return hit.clone();
            }
            continue;
        }
        let pic = find_icon(n, size).and_then(|p| load(&p, size));
        cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key, pic.clone());
        if pic.is_some() {
            return pic;
        }
    }
    None
}

/// The theme's own icon name for the XDG user folders (Documents, Downloads, Pictures, ...) and the home folder: `Some` only for those
/// exact folders (the paths `user-dirs.dirs` names).
fn special_folder_icon(path: &Path) -> Option<&'static str> {
    let same = |d: Option<PathBuf>| d.is_some_and(|d| d == path);
    let table: [(Option<PathBuf>, &'static str); 8] = [
        (dirs::document_dir(), "folder-documents"),
        (dirs::download_dir(), "folder-download"),
        (dirs::picture_dir(), "folder-pictures"),
        (dirs::audio_dir(), "folder-music"),
        (dirs::video_dir(), "folder-videos"),
        (dirs::desktop_dir(), "user-desktop"),
        (dirs::public_dir(), "folder-publicshare"),
        (dirs::template_dir(), "folder-templates"),
    ];
    if dirs::home_dir().is_some_and(|h| h == path) {
        return Some("user-home");
    }
    table.into_iter().find(|(d, _)| same(d.clone())).map(|(_, n)| n)
}

/// The system icon of a file or folder at `size` pixels; `None` when the theme has nothing for it.
pub fn icon_for(path: &Path, size: u32) -> Option<Bgra> {
    let names: Vec<String> = if path.is_dir() {
        let mut v: Vec<String> = special_folder_icon(path).map(|n| vec![n.to_string()]).unwrap_or_default();
        v.extend(["inode-directory".into(), "folder".into()]);
        v
    } else {
        let name = path.file_name()?.to_string_lossy();
        let mime = globs().mime_of(&name).unwrap_or("application/octet-stream");
        names_for_mime(mime, generic_icons())
    };
    picture_of(&names, size.max(8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs2_and_index_theme_parse() {
        let mut g = Globs::default();
        g.add("# comment\n50:text/plain:*.txt\n50:application/gzip:*.gz\n50:application/x-compressed-tar:*.tar.gz\n10:text/x-makefile:Makefile\n50:image/png:*.PNG:cs\n50:x/y:foo*\n");
        assert_eq!(g.mime_of("Notes.TXT"), Some("text/plain"));
        assert_eq!(g.mime_of("a.tar.gz"), Some("application/x-compressed-tar"));
        assert_eq!(g.mime_of("b.gz"), Some("application/gzip"));
        assert_eq!(g.mime_of("makefile"), Some("text/x-makefile"));
        assert_eq!(g.mime_of("nothing"), None);
        let generic: HashMap<String, String> = [("text/plain".to_string(), "text-x-generic".to_string())].into();
        assert_eq!(names_for_mime("text/plain", &generic)[..2], ["text-plain", "text-x-generic"]);

        let (inh, dirs) = parse_theme("[Icon Theme]\nName=X\nInherits=breeze, hicolor\nDirectories=mimetypes/16,mimetypes/64,scalable/mimetypes,mimetypes/32@2\n\n[mimetypes/16]\nSize=16\nType=Fixed\n\n[mimetypes/64]\nSize=64\n\n[scalable/mimetypes]\nSize=64\nMinSize=8\nMaxSize=512\nType=Scalable\n\n[mimetypes/32@2]\nSize=32\nScale=2\n");
        assert_eq!(inh, ["breeze", "hicolor"]);
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0].distance(24), 8);
        assert_eq!(dirs[1].distance(63), 0);
        assert_eq!(dirs[2].distance(24), 0);
    }
}
