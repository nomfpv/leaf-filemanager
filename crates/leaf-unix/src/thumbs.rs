//! Linux backend of rf_win::thumbs. Status: STUB
//!
//! No icons or thumbnails yet. The pool answers every request at once, on the caller's thread, with no picture (`image: None`,
//! not `dropped`): the cache then marks the slot failed instead of waiting for it forever.

use std::path::Path;
use std::sync::Arc;

use leaf_common::{stub, stub_err};
pub use leaf_common::thumbs::{Bgra, Kind, Request, Response};

pub fn keep_gdiplus_alive() {
    stub!("thumbs::keep_gdiplus_alive")
}

pub fn get_image(_path: &Path, _size: u32, _kind: Kind) -> anyhow::Result<Bgra> {
    stub_err!("thumbs::get_image")
}

pub fn picture_only(_path: &Path, _size: u32) -> Option<Bgra> {
    stub!("thumbs::picture_only")
}

pub fn folder_has_own_icon(_attrs: u32) -> bool {
    stub!("thumbs::folder_has_own_icon")
}

pub fn has_unique_icon(_ext: &str) -> bool {
    stub!("thumbs::has_unique_icon")
}

/// The thumbnail pool: no workers, every request is answered in [`request`](Self::request).
// TYPE-COPY(rf-win thumbs.rs:284): the stack, the workers' wake channel and the urgent lane dropped
pub struct ThumbPool {
    on_done: Arc<dyn Fn(Response) + Send + Sync>,
}

impl ThumbPool {
    pub fn new(_workers: usize, on_done: impl Fn(Response) + Send + Sync + 'static) -> Self {
        stub::hit("thumbs::ThumbPool::new");
        Self { on_done: Arc::new(on_done) }
    }

    /// Request/response: answered exactly once, now, with no picture.
    pub fn request(&self, req: Request) {
        stub::hit("thumbs::ThumbPool::request");
        (self.on_done)(Response {
            key: req.key,
            image: None,
            dropped: false,
            waited: req.queued().elapsed(),
            took: std::time::Duration::ZERO,
            urgent: req.urgent,
        });
    }

    pub fn clear(&self) {
        stub!("thumbs::ThumbPool::clear")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn every_request_is_answered_once_with_no_picture() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let pool = ThumbPool::new(2, move |r| g.lock().unwrap().push((r.key, r.image.is_none(), r.dropped)));
        pool.request(Request::new(7, "a.png".into(), 32, Kind::Thumbnail));
        pool.request(Request::new(8, "b.png".into(), 32, Kind::Icon));
        assert_eq!(*got.lock().unwrap(), vec![(7, true, false), (8, true, false)]);
    }
}
