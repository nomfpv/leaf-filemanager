//! Linux backend of rf_win::glass (the Windows backdrop has no Linux counterpart; only the items rf-app uses). Status: N/A (Windows glass and backdrop: no Linux counterpart, the app draws solid)

use leaf_common::stub;

// TYPE-COPY(rf-win src/glass/mod.rs:100)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlassState {
    Off,
    Starting,
    Running,
    Failed(String),
}

// TYPE-COPY(rf-win src/glass/mod.rs:115)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub opacity: f32,
    pub dy: f32,
    pub blur: f32,
}

// TYPE-COPY(rf-win src/glass/mod.rs:155)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassOptions {
    pub dark: bool,
    pub radius_px: Option<f32>,
    pub cut_corners: bool,
    pub shape: Option<[f32; 4]>,
    pub shadow: Option<Shadow>,
}

impl GlassOptions {
    pub const fn window(dark: bool) -> GlassOptions {
        GlassOptions {
            dark,
            radius_px: None,
            cut_corners: false,
            shape: None,
            shadow: None,
        }
    }
    pub fn sanitized(self) -> GlassOptions {
        self
    }
}

// TYPE-COPY(rf-win src/glass/mod.rs:210)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    Off,
    Session,
    Affinity,
}

pub fn capture_mode() -> CaptureMode {
    // STUB(linux): no glass
    stub::hit("glass::capture_mode");
    CaptureMode::Off
}

pub const MAX_LENSES: usize = 8;

// TYPE-COPY(rf-win src/glass/mod.rs:250)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookParams {
    pub frost: f32,
    pub tint: f32,
    pub refraction: f32,
    pub saturation: f32,
}

impl LookParams {
    /// The shipped look (the same numbers as rf-win's; a settings test keeps the app's default equal to it).
    pub const DEFAULT: LookParams = LookParams { frost: 52.0, tint: 50.0, refraction: 67.0, saturation: 62.0 };

    pub fn clamped(self) -> LookParams {
        let c = |v: f32, d: f32| if v.is_nan() { d } else { v.clamp(0.0, 100.0) };
        LookParams {
            frost: c(self.frost, Self::DEFAULT.frost),
            tint: c(self.tint, Self::DEFAULT.tint),
            refraction: c(self.refraction, Self::DEFAULT.refraction),
            saturation: c(self.saturation, Self::DEFAULT.saturation),
        }
    }
}

impl Default for LookParams {
    fn default() -> LookParams {
        LookParams::DEFAULT
    }
}

// TYPE-COPY(rf-win src/glass/mod.rs:287)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LensSpec {
    pub key: u64,
    pub rect: [f32; 4],
    pub clip: [f32; 4],
    pub radius: f32,
}

pub fn attach(_hwnd: isize, _dark: bool) -> bool {
    stub!("glass::attach")
}

pub fn attach_with(_hwnd: isize, _opts: GlassOptions) -> bool {
    stub!("glass::attach_with")
}

pub fn set_dark(_hwnd: isize, _dark: bool) {
    stub!("glass::set_dark")
}

pub fn detach(_hwnd: isize) {
    stub!("glass::detach")
}

pub fn set_lens(_hwnd: isize, _slot: u8, _lens: Option<LensSpec>, _animate: bool) {
    stub!("glass::set_lens")
}

pub fn set_look(_params: LookParams) {
    stub!("glass::set_look")
}

pub fn set_freeze_when_behind(_on: bool) {
    stub!("glass::set_freeze_when_behind")
}

pub fn pointer_over(_hwnd: isize) {
    stub!("glass::pointer_over")
}

pub fn set_freezable(_hwnd: isize, _on: bool) {
    stub!("glass::set_freezable")
}

pub fn state(_hwnd: isize) -> GlassState {
    // STUB(linux): no glass
    stub::hit("glass::state");
    GlassState::Off
}

pub fn exclude_from_capture(_hwnd: isize, _on: bool) {
    stub!("glass::exclude_from_capture")
}
