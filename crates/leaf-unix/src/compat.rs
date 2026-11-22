//! Linux backend of rf_win::compat. Status: N/A (Windows program compatibility settings: reading answers "none set", writing says it is not supported)

use leaf_common::stub_err;
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
        match self {
            Mode::Win95 => "WIN95",
            Mode::Win98 => "WIN98",
            Mode::WinXpSp2 => "WINXPSP2",
            Mode::WinXpSp3 => "WINXPSP3",
            Mode::Vista => "VISTARTM",
            Mode::VistaSp1 => "VISTASP1",
            Mode::VistaSp2 => "VISTASP2",
            Mode::Win7 => "WIN7RTM",
            Mode::Win8 => "WIN8RTM",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Win95 => "Windows 95",
            Mode::Win98 => "Windows 98 / Windows ME",
            Mode::WinXpSp2 => "Windows XP (Service Pack 2)",
            Mode::WinXpSp3 => "Windows XP (Service Pack 3)",
            Mode::Vista => "Windows Vista",
            Mode::VistaSp1 => "Windows Vista (Service Pack 1)",
            Mode::VistaSp2 => "Windows Vista (Service Pack 2)",
            Mode::Win7 => "Windows 7",
            Mode::Win8 => "Windows 8",
        }
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
    Compat::default()
}

pub fn read_all_users(_exe: &Path) -> Compat {
    Compat::default()
}

pub fn write(_exe: &Path, _c: &Compat) -> anyhow::Result<()> {
    stub_err!("compat::write")
}
