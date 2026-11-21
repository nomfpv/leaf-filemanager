//! leaf-unix: the Linux backend, the twin of rf-win. Every module rf-app reaches through `leaf-sys` has its file here, with rf-win's
//! public signatures (`HWND` becomes [`Owner`], `windows::core::Result` becomes `anyhow::Result`). A module that does not do its work
//! yet says so in its first line (`Status: STUB`) and answers through the `stub!` / `stub_err!` macros of leaf-common.
//!
//! This list of modules is complete: later work fills the files and never edits it. Unix only: on Windows this crate is empty.
#![cfg(unix)]

pub mod accent;
pub mod activate;
pub mod apppkg;
pub mod apps;
pub mod args;
pub mod bench;
pub mod cert;
pub mod clipboard;
pub mod compat;
pub mod complete;
pub mod ctxmenu;
pub mod datefmt;
pub mod default_fm;
pub mod dialogs;
pub mod diskusage;
pub mod dnd;
pub mod drives;
pub mod editors;
pub mod encoding;
pub mod engine_guard;
pub mod fastcopy;
pub mod fastdelete;
pub mod fileactions;
pub mod fileattr;
pub mod fileinfo;
pub mod fileops;
pub mod folder_custom;
pub mod foldersize;
pub mod ftp;
pub mod geom;
pub mod git;
pub mod glass;
pub mod handover;
pub mod hotkey;
pub mod indexer;
pub mod input;
pub mod jumplist;
pub mod keys;
pub mod kit;
pub mod known;
pub mod libraries;
pub mod listing;
pub mod locale;
pub mod localized;
pub mod lock;
pub mod locks;
pub mod look;
pub mod nameindex;
pub mod network;
pub mod openrules;
pub mod organize;
pub mod osver;
pub mod overlays;
pub mod pe;
pub mod picture_fallback;
pub mod pkgverbs;
pub mod preview_host;
pub mod process;
pub mod propcols;
pub mod props;
pub mod recycle;
pub mod recycle_settings;
pub mod registry;
pub mod removal;
pub mod rotate;
pub mod runcmd;
pub mod runfind;
pub mod scrollbars;
pub mod search;
pub mod security;
pub mod sendto;
pub mod settingscat;
pub mod share;
mod openwith;
pub mod shell;
pub mod shell_window;
pub mod shellfolder;
pub mod shortcut;
pub mod signature;
pub mod single_instance;
pub mod sound;
pub mod sta;
pub mod syncroots;
pub mod sysparams;
pub mod taskbar_progress;
pub mod thumbs;
pub mod toast_post;
pub mod tray;
pub mod vfiles;
pub mod volume;
pub mod window;
pub mod wsearch;

pub use drives::{Drive, DriveKind, list_drives};
pub use known::{KnownFolder, known_folders};
pub use leaf_common::{APP_EXE_STEM, LOOK_EXE_STEM, exe_name};
pub use listing::{ListError, list_dir, stat_entry};

/// The window a system dialog or menu belongs to: `HWND` on Windows, nothing yet on Linux (Wayland gives a client no such handle; a
/// portal's parent window string can come here later).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Owner;

/// Milliseconds since this process was created (start-up timing): its start time in `/proc/self/stat` against `/proc/uptime`, both
/// counted from boot (10 ms steps). 0 when they cannot be read.
pub fn process_age_ms() -> u64 {
    process_age_from(
        &std::fs::read_to_string("/proc/self/stat").unwrap_or_default(),
        &std::fs::read_to_string("/proc/uptime").unwrap_or_default(),
        clock_ticks(),
    )
    .unwrap_or(0)
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf only reads a constant of the system; it takes no pointers.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 { t as u64 } else { 100 }
}

/// The pure part of [`process_age_ms`]: `stat` is `/proc/self/stat`, `uptime` is `/proc/uptime`.
fn process_age_from(stat: &str, uptime: &str, ticks: u64) -> Option<u64> {
    // (the program's name, field 2, is in parentheses and may hold spaces: count from the last ')')
    let rest = &stat[stat.rfind(')')? + 1..];
    // field 22 (starttime) is the 20th after the name
    let start_ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let up_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let start_ms = start_ticks * 1000 / ticks.max(1);
    Some(((up_secs * 1000.0) as u64).saturating_sub(start_ms))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_age_of_a_process_from_its_start_and_the_uptime() {
        let stat = "4242 (my prog (x)) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 12345 1000000 100";
        // started at tick 12345 (123.45 s after boot), now 130.00 s after boot
        assert_eq!(super::process_age_from(stat, "130.00 500.00\n", 100), Some(6550));
        assert_eq!(super::process_age_from("garbage", "1.0 1.0", 100), None);
        let age = super::process_age_ms();
        assert!(age < 24 * 3600 * 1000, "{age}");
    }
}
