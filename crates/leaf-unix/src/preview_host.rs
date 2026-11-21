//! Linux backend of rf_win::preview_host. Status: STUB

/// Never calls `f` (watch style): no media is played in a preview here.
pub fn set_media_events(_f: impl Fn(MediaEvent) + Send + 'static) {
    stub!("preview_host::set_media_events")
}
