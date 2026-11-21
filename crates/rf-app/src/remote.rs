//! Places that are not local folders (`Location::Remote`), listed through `leaf-vfs` on a worker thread. Local folders never
//! come here: they keep `Location::Path` and the direct listing.
//!
//! Reachable today: `mem://demo/` (the in-memory demo tree; `mem://offline/` and `mem://locked/` show the error banner) in debug
//! builds, or in any build with `LEAF_VFS_DEMO=1`. Schemes without a backend (`ftp://` is still the old WinINet path) are not
//! taken: they stay paths as before. Copy and move between such a place and a local folder are not wired yet.

use rf_core::Location;
use rf_core::vpath::VPath;

/// Marks a listing error that is the server's (offline, login refused): the pane shows it quietly with a Retry button.
const RETRY_MARK: &str = "\u{1}retry\u{1}";

/// Why copy, move, paste and the like are off in a remote folder (the menus show it).
pub const NOT_WIRED: &str = "Copying and moving between a server and this PC is not built yet";

/// The `mem://` demo is on: debug builds, or `LEAF_VFS_DEMO` set to anything but `0`.
pub fn demo_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("LEAF_VFS_DEMO").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether addresses of this scheme open as a remote place.
pub fn usable(scheme: &str) -> bool {
    match scheme {
        // local paths stay local (the fast path)
        "file" => false,
        "mem" => demo_enabled(),
        s => leaf_vfs::has_backend(s),
    }
}

/// Text typed into the address bar (or handed over at start) that names a remote place.
pub fn parse_typed(text: &str) -> Option<Location> {
    let t = text.trim().trim_matches('"');
    if !VPath::looks_like(t) {
        return None;
    }
    let v = VPath::parse(t).ok()?;
    usable(v.scheme()).then_some(Location::Remote(v))
}

/// Lists `dir` (on a worker thread). The error is the text the pane shows.
pub fn list(dir: &VPath, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<rf_core::Entry>)) -> Result<usize, String> {
    let fs = leaf_vfs::fs_for(dir.scheme()).map_err(message)?;
    fs.list(dir, 512, cancelled, on_batch).map_err(message)
}

fn message(e: leaf_vfs::FsError) -> String {
    use leaf_vfs::FsError as E;
    match e {
        E::Offline(_) | E::Auth(_) => format!("{RETRY_MARK}{e}"),
        E::NotFound => "Folder not found".into(),
        E::Denied => "Access denied".into(),
        E::Cancelled => "Cancelled".into(),
        // a saved host whose scheme has no backend yet (SFTP comes in its own lane)
        E::Unsupported => "Connecting is not built yet".into(),
        e => e.to_string(),
    }
}

/// The text of a server error that gets the Retry banner, or `None` for any other error.
pub fn retry_message(err: &str) -> Option<&str> {
    err.strip_prefix(RETRY_MARK)
}

