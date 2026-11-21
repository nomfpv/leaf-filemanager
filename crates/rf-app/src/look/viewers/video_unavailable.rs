//! The video viewer where it is not available yet (off Windows: video.rs plays through the Windows-only presenter and audio sink of
//! rf-player and the preview host). It takes no file, so a video gets the next viewer that takes it, or the info card.

use gpui_kit::{AnyElement, App, IntoElement, Window, div};

use crate::look::viewer::{Prepared, Probe, Viewer};

#[derive(Default)]
pub struct VideoViewer;

impl Viewer for VideoViewer {
    fn name(&self) -> &'static str {
        "video"
    }
    fn priority(&self) -> i32 {
        -3
    }
    fn can_handle(&self, _f: &Probe) -> bool {
        false
    }
    fn prepare(&mut self, f: &Probe, _cx: &mut App) -> Prepared {
        Prepared::new((480.0, 270.0), super::file_name(&f.path))
    }
    fn render(&mut self, _window: &mut Window, _cx: &mut App) -> AnyElement {
        div().into_any_element()
    }
}
