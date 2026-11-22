//! Shell namespaces leaf shows itself (This PC, Network, Libraries, the Recycle Bin, a phone, a folder named by `shell:` or its
//! CLSID) and the ones it leaves to Explorer (Control Panel, Settings pages, printers, Bluetooth, unknown CLSIDs). One decision for
//! every way in: the address field, a launch by another program, a saved session.
//!
//! Reading the namespace blocks (a dead share, a locked phone): [`resolve`] runs it on a thread of its own and gives up after `limit`.

use std::time::Duration;

use rf_core::history::Location;

/// Network (the computers found, their shares).
const NETWORK: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// Where a namespace leads.
pub enum Place {
    /// Listed in leaf.
    Here(Location),
    /// Not a place leaf lists (or one that did not answer in time): Explorer's.
    Explorer,
}

/// Whether text typed in the address field names a namespace (and not a path): `shell:`, `::{CLSID}`, `This PC`, a `search-ms:` link
/// or a Settings page, and `\` alone (Explorer's way to Network).
pub fn is_typed(text: &str) -> bool {
    let t = text.trim().trim_matches('"');
    t == r"\\" || rf_core::explorer_args::is_namespace(t) || matches!(t.to_ascii_lowercase().as_str(), "this pc" | "my computer")
}

/// The place a namespace name stands for. Blocks up to `limit`: call it off the UI thread (or from the start-up code, which bounds
/// the wait itself).
pub fn resolve(name: &str, limit: Duration) -> Place {
    let name = name.trim().trim_matches('"');
    if let Some(loc) = Location::from_shell_name(name) {
        return Place::Here(loc);
    }
    let asked = if name == r"\\" { NETWORK.to_string() } else { name.to_string() };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(leaf_sys::shellfolder::browsable(&asked));
    });
    match rx.recv_timeout(limit).ok().flatten() {
        Some(place) => Place::Here(match place.fs_path {
            Some(file) if file.extension().is_some_and(|x| x.eq_ignore_ascii_case("library-ms")) => Location::Library { file, name: place.name },
            // a folder of the file system (`shell:Downloads`, `shell:Desktop`, a user folder's CLSID) is an ordinary folder
            Some(dir) => Location::Path(dir),
            None => Location::Shell { parsing: place.parsing, name: place.name },
        }),
        None => Place::Explorer,
    }
}
