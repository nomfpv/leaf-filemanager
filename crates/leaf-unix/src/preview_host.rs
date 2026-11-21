//! Linux backend of rf_win::preview_host. Status: STUB

use crate::geom::RECT;
use leaf_common::stub;
use std::path::Path;

/// Never calls `f` (watch style): no media is played in a preview here.
pub fn set_media_events(_f: impl Fn(MediaEvent) + Send + 'static) {
    stub!("preview_host::set_media_events")
}

pub fn handler_for(_path: &Path) -> Option<u128> {
    stub!("preview_host::handler_for")
}

// TYPE-COPY(rf-win src/preview_host.rs:82)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaEvent {
    Playing(bool),
    DoubleClick,
}

pub fn is_playable(_path: &Path) -> bool {
    stub!("preview_host::is_playable")
}

pub fn mf_decodes(_codec: &str) -> bool {
    stub!("preview_host::mf_decodes")
}

pub fn media_play(_owner: isize, _path: &Path, _rect: Option<RECT>) {
    stub!("preview_host::media_play")
}

pub fn media_pause() {
    stub!("preview_host::media_pause")
}

pub fn media_toggle() -> Option<bool> {
    stub!("preview_host::media_toggle")
}

pub fn media_seek(_fraction: f64) {
    stub!("preview_host::media_seek")
}

// TYPE-COPY(rf-win src/preview_host.rs:394)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaState {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
}

pub fn media_state() -> Option<MediaState> {
    stub!("preview_host::media_state")
}

pub fn media_reposition(_rect: RECT) {
    stub!("preview_host::media_reposition")
}

// TYPE-COPY(rf-win src/preview_host.rs:501)
pub struct Colors {
    pub background: u32,
    pub text: u32,
}

pub fn show(_owner: isize, _path: &Path, _rect: RECT, _colors: &Colors) -> bool {
    stub!("preview_host::show")
}

pub fn hide() {
    stub!("preview_host::hide")
}

pub fn detach_from(_owner: isize) {
    stub!("preview_host::detach_from")
}

pub fn reposition(_rect: RECT) {
    stub!("preview_host::reposition")
}
