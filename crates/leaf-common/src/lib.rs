//! leaf-common: what the platform backends share. The neutral data types of the seam (defined once, so the backends cannot drift
//! apart), the stub macros, and the modules that already work on every system.
//!
//! `rf-win` (Windows) and `leaf-unix` (Linux) re-export these at the paths rf-win always had; the app reaches them through `leaf-sys`.

pub mod stub;

// the data types of the seam
pub mod ctxmenu;
pub mod drives;
pub mod engine_guard;
pub mod fastcopy;
pub mod fileops;
pub mod listing;
pub mod locks;
pub mod propcols;
pub mod recycle;
pub mod thumbs;

// modules that work on every system as they are
pub mod design;
pub mod dupes;
pub mod docs;
pub mod elf;
pub mod exif_orient;
pub mod hash;
pub mod kit;
pub mod ownfolder;
pub mod perms;
pub mod photo;
pub mod portable;
pub mod tree_watch;
pub mod watch;

#[cfg(windows)]
mod wide;

/// The file manager's program (its file name without the extension).
pub const APP_EXE_STEM: &str = "leaf";
/// The Preview program, the second binary (its file name without the extension).
pub const LOOK_EXE_STEM: &str = "leaf-look";

/// A program's file name: `leaf.exe` on Windows, `leaf` elsewhere.
pub fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}
