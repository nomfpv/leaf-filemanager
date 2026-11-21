//! Linux backend of rf_win::compat. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;

// TYPE-COPY(rf-win src/compat.rs:13)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Win95,
    Win98,
    WinXpSp2,
    WinXpSp3,
    Vista,
    VistaSp1,
    VistaSp2,
    Win7,
    Win8,
}

impl Mode {
    pub const ALL: [Mode; 9] = [Mode::Win95, Mode::Win98, Mode::WinXpSp2, Mode::WinXpSp3, Mode::Vista, Mode::VistaSp1, Mode::VistaSp2, Mode::Win7, Mode::Win8];

    pub fn layer(self) -> &'static str {
        stub!("compat::Mode::layer")
    }
    pub fn label(self) -> &'static str {
        stub!("compat::Mode::label")
    }
}

// TYPE-COPY(rf-win src/compat.rs:60)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    C256,
    C16Bit,
}

// TYPE-COPY(rf-win src/compat.rs:68)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dpi {
    Application,
    System,
    SystemEnhanced,
}

// TYPE-COPY(rf-win src/compat.rs:78)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compat {
    pub mode: Option<Mode>,
    pub color: Option<Color>,
    pub res_640: bool,
    pub disable_fso: bool,
    pub run_as_admin: bool,
    pub register_restart: bool,
    pub legacy_icc: bool,
    pub dpi: Option<Dpi>,
    pub other: Vec<String>,
}

pub fn read(_exe: &Path) -> Compat {
    stub!("compat::read")
}

pub fn read_all_users(_exe: &Path) -> Compat {
    stub!("compat::read_all_users")
}

pub fn write(_exe: &Path, _c: &Compat) -> anyhow::Result<()> {
    stub_err!("compat::write")
}
