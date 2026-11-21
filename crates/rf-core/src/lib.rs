//! rf-core: platform-independent model for leaf (entries, sorting, filtering, navigation).

pub mod actions;
pub mod addrcmd;
pub mod compare;
pub mod copy_path;
pub mod dupes;
pub mod entry;
pub mod format;
pub mod history;
pub mod hotkey;
pub mod iconpack;
pub mod navtree;
pub mod openrules;
pub mod organize;
pub mod pathkey;
pub mod previewpane;
pub mod recycle_bin;
pub mod propcols;
pub mod rename;
pub mod rename_plan;
pub mod rename_rules;
pub mod search_date;
pub mod content_search;
pub mod basket;
pub mod shelf;
pub mod sort;
pub mod sortkeys;
pub mod thumbcache;
pub mod tags;
pub mod terminal;
pub mod transfer;
pub mod treemap;
pub mod usage;
pub mod tssource;
pub mod vpath;
pub mod hosts;

pub use entry::{Entry, EntryKind, attrs};
pub use history::{History, Location};
pub use sort::{SortDir, SortKey, SortSpec};

/// Renames an unreadable settings/data file to `<name>.bozuk-<unix secs>` so a later save
/// cannot silently destroy what the user had (they can fix and restore it). Best effort.
pub fn backup_corrupt(path: &std::path::Path) {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".corrupt-{ms}"));
    let _ = std::fs::rename(path, path.with_file_name(name));
}

/// Reads a small config file, retrying briefly on transient errors (antivirus / sync tools
/// holding it open) so a passing lock is not mistaken for a broken file.
pub fn read_config(path: &std::path::Path) -> std::io::Result<String> {
    let mut last = None;
    for attempt in 0..3 {
        match std::fs::read_to_string(path) {
            Ok(t) => return Ok(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::InvalidData => return Err(e),
            Err(e) => {
                last = Some(e);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("read failed")))
}
pub mod desktop;
pub mod textload;
