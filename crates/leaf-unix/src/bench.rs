//! Linux backend of rf_win::bench. Status: STUB

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;


pub fn machine(_test_dir: &Path) -> Machine {
    stub!("bench::machine")
}

/// Runs `f` (its result is real), but nothing is measured: every number is NaN, not a time of 0 ms.
pub fn measure_in_process<T>(f: impl FnOnce() -> T) -> (T, InProcess) {
    // STUB(linux): InProcess has no Default; NaN says "not measured"
    stub::hit("bench::measure_in_process");
    (
        f(),
        InProcess {
            ms: f64::NAN,
            cpu_ms: f64::NAN,
            extra_mb: f64::NAN,
        },
    )
}

// TYPE-COPY(rf-win src/bench.rs:45)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AppKind {
    Leaf,
    Explorer,
    Files,
    FilePilot,
}

impl AppKind {
    pub fn name(self) -> &'static str {
        stub!("bench::AppKind::name")
    }
}

// TYPE-COPY(rf-win src/bench.rs:65)
#[derive(Debug, Clone)]
pub struct App {
    pub kind: AppKind,
    pub exe: PathBuf,
    pub env: Vec<(String, String)>,
}

pub fn detect() -> Vec<App> {
    stub!("bench::detect")
}

// TYPE-COPY(rf-win src/bench.rs:307)
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OpenResult {
    pub settled_ms: f64,
    pub window_ms: f64,
    pub cpu_ms: f64,
    pub ram: u64,
    pub peak_ram: u64,
    #[serde(default)]
    pub private: u64,
    #[serde(default)]
    pub private_ws: u64,
    pub shared: bool,
    pub timed_out: bool,
    pub changes: Vec<f64>,
    pub process: String,
}

pub fn measure_open(_app: &App, _folder: &Path, _timeout: Duration, _cancel: &AtomicBool) -> anyhow::Result<OpenResult> {
    stub_err!("bench::measure_open")
}

// TYPE-COPY(rf-win src/bench.rs:500)
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Machine {
    pub cpu: String,
    pub cores: u32,
    pub ram_gb: u32,
    pub windows: String,
    pub disk: String,
}

pub fn shell_copy_quiet(_sources: &[PathBuf], _dest_dir: &Path) -> anyhow::Result<()> {
    stub_err!("bench::shell_copy_quiet")
}

// TYPE-COPY(rf-win bench.rs:689)
#[derive(Debug, Clone, Copy)]
pub struct InProcess {
    pub ms: f64,
    pub cpu_ms: f64,
    pub extra_mb: f64,
}
