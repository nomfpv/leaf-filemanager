//! One item in the system's trash (the Windows Recycle Bin, the freedesktop Trash). Listing and restoring are the backends' (`recycle`).

/// One item in the Recycle Bin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecycleItem {
    /// Original file name as shown by Explorer.
    pub name: String,
    /// Folder the item was deleted from.
    pub original_location: String,
    /// Deletion time, Unix milliseconds (UTC); 0 when unknown.
    pub deleted_ms: i64,
    pub size: u64,
    /// Real location inside `$Recycle.Bin\<SID>\$R...` (Windows) or `Trash/files/` (freedesktop); also the key used by `restore`.
    pub parsing_path: String,
    pub is_dir: bool,
}
