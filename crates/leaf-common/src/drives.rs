//! A drive (Windows) or a mounted file system (elsewhere), as the places list shows it. The listing is the backends' (`drives`).

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    RamDisk,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Drive {
    /// "C:\" (Windows), the mount point elsewhere
    pub root: PathBuf,
    /// The drive letter; `None` on systems without them.
    pub letter: Option<char>,
    pub label: String,
    pub kind: DriveKind,
    pub total: u64,
    pub free: u64,
    pub fs: String,
}

impl Drive {
    pub fn display_name(&self) -> String {
        let label = if self.label.is_empty() {
            match self.kind {
                DriveKind::Removable => "USB Drive",
                DriveKind::Network => "Network Drive",
                DriveKind::Optical => "DVD Drive",
                _ => "Local Disk",
            }
            .to_string()
        } else {
            self.label.clone()
        };
        match self.letter {
            Some(l) => format!("{label} ({l}:)"),
            None => format!("{label} ({})", self.root.display()),
        }
    }
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { 1.0 - self.free as f32 / self.total as f32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(letter: Option<char>, root: &str, label: &str) -> Drive {
        Drive { root: PathBuf::from(root), letter, label: label.into(), kind: DriveKind::Removable, total: 0, free: 0, fs: String::new() }
    }

    #[test]
    fn a_drive_is_named_by_its_letter_and_a_mount_by_its_folder() {
        assert_eq!(drive(Some('E'), "E:\\", "").display_name(), "USB Drive (E:)");
        assert_eq!(drive(Some('E'), "E:\\", "Stick").display_name(), "Stick (E:)");
        assert_eq!(drive(None, "/run/media/me/STICK", "STICK").display_name(), "STICK (/run/media/me/STICK)");
        assert_eq!(drive(None, "/", "").used_fraction(), 0.0);
    }
}
