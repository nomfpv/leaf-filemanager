//! Linux backend of rf_win::accent. Status: STUB
//!
//! The worked example of the stub template (A4): every item of `stub-worklist.tsv` for this module, in its order, the signature as
//! rf-win has it, the body by the rules of SEAM-DESIGN section 5.

use leaf_common::stub;

/// Shades of the accent, as 0xRRGGBB: `[light3, light2, light1, base, dark1, dark2, dark3]`.
// TYPE-COPY(rf-win accent.rs:7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentPalette {
    pub shades: [u32; 7],
}

impl AccentPalette {
    pub fn light2(&self) -> u32 {
        stub!("accent::AccentPalette::light2")
    }
    pub fn dark1(&self) -> u32 {
        stub!("accent::AccentPalette::dark1")
    }
    pub fn base(&self) -> u32 {
        stub!("accent::AccentPalette::base")
    }
}

pub fn system_palette() -> Option<AccentPalette> {
    stub!("accent::system_palette")
}

pub fn set_override(_color: Option<u32>) {
    stub!("accent::set_override")
}

pub fn override_color() -> Option<u32> {
    stub!("accent::override_color")
}

// PORTABLE? (pure colour math)
pub fn from_color(_base: u32) -> AccentPalette {
    // STUB(linux): AccentPalette has no Default; all shades black
    stub::hit("accent::from_color");
    AccentPalette { shades: [0; 7] }
}

// PORTABLE? (pure text parsing)
pub fn parse_hex(_text: &str) -> Option<u32> {
    stub!("accent::parse_hex")
}

// PORTABLE? (pure formatting)
pub fn to_hex(_color: u32) -> String {
    stub!("accent::to_hex")
}
