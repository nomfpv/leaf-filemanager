//! Linux backend of rf_win::bench. Status: PARTIAL (`machine` is read from /proc and os-release; the comparison with Windows file managers does not apply: `detect` finds none)

use leaf_common::{stub, stub_err};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;


/// This PC for the result card: the CPU's model name (`/proc/cpuinfo`), logical cores, installed memory in GB (`/proc/meminfo`), the
/// distribution (`PRETTY_NAME` of `/etc/os-release`, with the kernel release) and SSD/HDD of the test folder's disk.
pub fn machine(test_dir: &Path) -> Machine {
    let cpu = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| cpu_model(&t)).unwrap_or_default();
    let ram_gb = std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| mem_total_kb(&t)).map_or(0, |kb| (kb as f64 / (1u64 << 20) as f64).round() as u32);
    let os = ["/etc/os-release", "/usr/lib/os-release"].iter().find_map(|p| std::fs::read_to_string(p).ok()).and_then(|t| pretty_name(&t)).unwrap_or_else(|| "Linux".to_string());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
    let disk = match crate::drives::seek_penalty(test_dir) {
        Some(true) => "HDD",
        Some(false) => "SSD",
        None => "",
    };
    Machine {
        cpu,
        cores: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
        ram_gb,
        windows: if kernel.is_empty() { os } else { format!("{os} ({kernel})") },
        disk: disk.into(),
    }
}

/// `model name	: AMD Ryzen 7 ...` (the first one; ARM boards have `Model` or `Hardware` instead).
fn cpu_model(text: &str) -> Option<String> {
    ["model name", "Model", "Hardware"].iter().find_map(|key| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == *key && !v.trim().is_empty()).then(|| v.split_whitespace().collect::<Vec<_>>().join(" "))
        })
    })
}

/// `MemTotal:       16318244 kB`.
fn mem_total_kb(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix("MemTotal:")?.split_whitespace().next()?.parse().ok())
}

fn pretty_name(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim().trim_matches(['"', '\'']).to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod machine_tests {
    use super::*;

    #[test]
    fn machine_text_is_read() {
        assert_eq!(cpu_model("processor\t: 0\nmodel name\t: AMD  Ryzen 7\t5800X\n").as_deref(), Some("AMD Ryzen 7 5800X"));
        assert_eq!(mem_total_kb("MemTotal:       16318244 kB\nMemFree: 1 kB\n"), Some(16318244));
        assert_eq!(pretty_name("NAME=\"Arch\"\nPRETTY_NAME=\"CachyOS Linux\"\n").as_deref(), Some("CachyOS Linux"));
    }
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
        match self {
            AppKind::Leaf => "leaf",
            AppKind::Explorer => "File Explorer",
            AppKind::Files => "Files",
            AppKind::FilePilot => "File Pilot",
        }
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
    Vec::new()
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
