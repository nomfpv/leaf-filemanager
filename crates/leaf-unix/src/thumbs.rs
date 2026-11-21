//! Linux backend of rf_win::thumbs. Status: REAL for pictures, type icons from the system icon theme (`crate::icontheme`)
//!
//! A picture file (by extension: PNG, JPEG, GIF, WebP, BMP, ICO, TIFF, TGA, QOI, EXR, HDR, DDS, PNM, JPEG XL, AVIF) gets a
//! thumbnail from [`crate::look::decode`] at the asked size. A `Kind::Icon` request gets the system icon theme's picture for the file's type. Everything else answers `None` / `Err` (the caller
//! draws its generic icon). The pool is the same shape as Windows': newest request first on worker threads, a small
//! lane of its own for the urgent picture (preview pane, Quick Look), a bounded backlog whose oldest are answered `dropped`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use leaf_common::stub;
pub use leaf_common::thumbs::{Bgra, Kind, Request, Response};

/// Nothing to keep alive here (a Windows GDI+ detail).
pub fn keep_gdiplus_alive() {
    stub::hit("thumbs::keep_gdiplus_alive");
}

const PICTURE_EXTS: &[&str] = &[
    "png", "apng", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff", "tga", "qoi", "exr", "hdr", "dds", "pnm", "pbm", "pgm", "ppm", "pam", "avif", "jxl",
];

fn is_picture(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| PICTURE_EXTS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

/// The picture of a picture file at `size` pixels on its long side (never enlarged); `None` for any other file or one that does
/// not decode.
fn picture(path: &Path, size: u32) -> Option<Bgra> {
    if !is_picture(path) {
        return None;
    }
    let d = crate::look::decode::decode_full(path, size.max(1)).ok()?;
    d.frames.into_iter().next().map(|(b, _)| b)
}

/// A thumbnail for a picture file; `Kind::Icon` is the system theme's icon of the file's type; any other file is an `Err`.
pub fn get_image(path: &Path, size: u32, kind: Kind) -> anyhow::Result<Bgra> {
    match kind {
        Kind::Thumbnail | Kind::ThumbnailOnly => picture(path, size).ok_or_else(|| anyhow::anyhow!("no picture for {}", path.display())),
        Kind::Icon => crate::icontheme::icon_for(path, size).ok_or_else(|| anyhow::anyhow!("no system icon for {}", path.display())),
    }
}

/// The file's own picture only, never its type's icon: a picture file's, else `None`. Blocking.
pub fn picture_only(path: &Path, size: u32) -> Option<Bgra> {
    picture(path, size)
}

/// Same rule as Windows: a read-only or system folder may carry its own icon.
pub fn folder_has_own_icon(attrs: u32) -> bool {
    const READONLY: u32 = 0x1;
    const SYSTEM: u32 = 0x4;
    attrs & (READONLY | SYSTEM) != 0
}

/// Extensions whose icon differs per file (must not be cached by extension): none have icons here.
pub fn has_unique_icon(_ext: &str) -> bool {
    false
}

/// How many urgent requests may stand waiting: the pane's picture and Quick Look's; older ones are only arrow-keying.
const URGENT_KEEP: usize = 2;
const LANE_THREADS: usize = 2;
const BACKLOG: usize = 512;

struct Queue {
    stack: Mutex<Vec<Request>>,
    cv: Condvar,
    down: AtomicBool,
}

impl Queue {
    fn new() -> Arc<Self> {
        Arc::new(Queue { stack: Mutex::new(Vec::new()), cv: Condvar::new(), down: AtomicBool::new(false) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Request>> {
        self.stack.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The newest request; waits while there is none. `None` once the pool is gone.
    fn take(&self) -> Option<Request> {
        let mut s = self.lock();
        loop {
            if self.down.load(Ordering::Acquire) {
                return None;
            }
            if let Some(r) = s.pop() {
                return Some(r);
            }
            s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn serve(q: Arc<Queue>, on_done: Arc<dyn Fn(Response) + Send + Sync>, urgent: bool, name: String) {
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        while let Some(req) = q.take() {
            let started = std::time::Instant::now();
            let image = match req.kind {
                Kind::Thumbnail => picture(&req.path, req.size),
                Kind::Icon => crate::icontheme::icon_for(&req.path, req.size),
                Kind::ThumbnailOnly => None,
            }
            .map(Arc::new);
            on_done(Response { key: req.key, image, dropped: false, waited: started.saturating_duration_since(req.queued()), took: started.elapsed(), urgent });
        }
    });
}

/// The thumbnail pool: newest requests first; an `urgent` request goes to a lane of its own that is started by the first one.
pub struct ThumbPool {
    q: Arc<Queue>,
    on_done: Arc<dyn Fn(Response) + Send + Sync>,
    lane: OnceLock<Arc<Queue>>,
}

impl ThumbPool {
    pub fn new(workers: usize, on_done: impl Fn(Response) + Send + Sync + 'static) -> Self {
        let on_done: Arc<dyn Fn(Response) + Send + Sync> = Arc::new(on_done);
        let q = Queue::new();
        // decoding a big picture is heavy: a few workers are plenty, and the UI keeps its cores
        for i in 0..workers.clamp(1, 4) {
            serve(q.clone(), on_done.clone(), false, format!("leaf-thumb-{i}"));
        }
        ThumbPool { q, on_done, lane: OnceLock::new() }
    }

    pub fn request(&self, req: Request) {
        if req.urgent {
            return self.request_urgent(req);
        }
        let dropped: Vec<Request> = {
            let mut s = self.q.lock();
            let d = if s.len() > BACKLOG { s.drain(0..BACKLOG / 2).collect() } else { Vec::new() };
            s.push(req);
            d
        };
        for d in dropped {
            (self.on_done)(Response { key: d.key, image: None, dropped: true, waited: d.queued().elapsed(), took: std::time::Duration::ZERO, urgent: d.urgent });
        }
        self.q.cv.notify_one();
    }

    fn request_urgent(&self, req: Request) {
        let lane = self.lane.get_or_init(|| {
            let q = Queue::new();
            for i in 0..LANE_THREADS {
                serve(q.clone(), self.on_done.clone(), true, format!("leaf-thumb-urgent-{i}"));
            }
            q
        });
        let dropped: Vec<Request> = {
            let mut s = lane.lock();
            let over = (s.len() + 1).saturating_sub(URGENT_KEEP);
            let d = s.drain(0..over).collect();
            s.push(req);
            d
        };
        for d in dropped {
            (self.on_done)(Response { key: d.key, image: None, dropped: true, waited: d.queued().elapsed(), took: std::time::Duration::ZERO, urgent: true });
        }
        lane.cv.notify_one();
    }

    /// Drops queued work (e.g. on navigation).
    pub fn clear(&self) {
        self.q.lock().clear();
        if let Some(l) = self.lane.get() {
            l.lock().clear();
        }
    }
}

impl Drop for ThumbPool {
    fn drop(&mut self) {
        for q in std::iter::once(&self.q).chain(self.lane.get()) {
            q.down.store(true, Ordering::Release);
            q.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_picture_gets_a_thumbnail_and_other_files_none() {
        let dir = std::env::temp_dir().join(format!("leaf-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, txt) = (dir.join("a.png"), dir.join("a.txt"));
        image::RgbaImage::new(64, 32).save(&png).unwrap();
        std::fs::write(&txt, b"hi").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let pool = ThumbPool::new(2, move |r| {
            let _ = tx.lock().unwrap().send((r.key, r.image.map(|i| (i.width, i.height)), r.dropped));
        });
        pool.request(Request::new(1, png, 16, Kind::Thumbnail));
        pool.request(Request::new(2, txt, 16, Kind::Thumbnail));
        let mut got: Vec<_> = (0..2).map(|_| rx.recv_timeout(Duration::from_secs(20)).unwrap()).collect();
        got.sort_by_key(|g| g.0);
        assert_eq!(got, vec![(1, Some((16, 8)), false), (2, None, false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
