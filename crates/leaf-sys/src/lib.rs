//! leaf-sys: the one door from the app to the operating system. Only `pub use` lines: on Windows every name here IS rf-win's (the same
//! item, re-exported: no wrapper, no extra call), on Linux it is leaf-unix's, and the shared parts are leaf-common's.
//!
//! The list is explicit on purpose: it is the API surface the app may use. A module that is not listed here cannot be called by the
//! app, so a new Windows-only call cannot slip in unnoticed. Adding a module means adding it to both backends and to this list.

#[cfg(windows)]
use rf_win as os;
#[cfg(unix)]
use leaf_unix as os;

// the same code on every system
pub use leaf_common::stub::Unsupported;
pub use leaf_common::{design, docs, elf, hash, ownfolder, portable, tree_watch, watch};

// one backend per system
pub use os::{
    accent, activate, apppkg, apps, bench, cert, clipboard, compat, complete, ctxmenu, datefmt, default_fm, dialogs, diskusage, dnd,
    drives, editors, encoding, engine_guard, fastcopy, fastdelete, fileactions, fileinfo, fileops, folder_custom, foldersize, ftp, geom, git,
    glass, handover, hotkey, indexer, jumplist, keys, kit, known, libraries, listing, localized, look, nameindex, network, openrules,
    organize, osver, overlays, pe, pkgverbs, preview_host, propcols, props, recycle, recycle_settings, removal, rotate, runcmd, runfind,
    scrollbars, search, security, sendto, settingscat, share, shell, shell_window, shellfolder, shortcut, signature, single_instance,
    sound, sta, syncroots, sysparams, taskbar_progress, thumbs, toast_post, tray, vfiles, volume, window, wsearch,
};

// what rf-app did with Win32 itself until the Linux seam (A3)
pub use os::{args, fileattr, input, lock, process, registry};

// the crate-root items rf-win always had
pub use os::{
    APP_EXE_STEM, Drive, DriveKind, KnownFolder, ListError, LOOK_EXE_STEM, Owner, exe_name, known_folders, list_dir, list_drives,
    process_age_ms, stat_entry,
};
