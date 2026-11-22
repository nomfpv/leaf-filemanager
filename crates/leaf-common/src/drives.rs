//! A drive (Windows) or a mounted file system (elsewhere), as the places list shows it. The listing is the backends' (`drives`).

use serde::{Deserialize, Serialize};
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

/// What a drive's own report says about its state. "Unknown" is a state of its own and is never shown as good.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Health {
    Good,
    Warning,
    Failing,
    Unknown,
}

/// Where a number of a [`DriveHealth`] came from (the detail shows it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthSource {
    /// Windows: the Storage Management health status of the physical disk (no rights needed).
    StorageManagement,
    /// Windows: the drive's own temperature property (no rights needed where the driver allows it).
    DeviceQuery,
    /// Linux: the kernel's hardware monitor of the drive (`hwmon`).
    Hwmon,
    /// Linux: the SMART values udisks2 keeps.
    Udisks,
    /// Read by the administrator helper (reliability counters).
    Administrator,
}

/// What is known about one drive's health: a fixed set of numbers, every one of them `None` when nobody could read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveHealth {
    /// The disk number (Windows) the numbers belong to; `None` where disks have no number.
    pub disk: Option<u32>,
    pub health: Health,
    pub temperature_c: Option<i16>,
    /// How much of the rated life is used, in percent (0 = new).
    pub wear_percent: Option<u8>,
    pub power_on_hours: Option<u32>,
    pub sources: Vec<HealthSource>,
}

impl DriveHealth {
    pub fn unknown(disk: Option<u32>) -> Self {
        Self { disk, health: Health::Unknown, temperature_c: None, wear_percent: None, power_on_hours: None, sources: Vec::new() }
    }

    /// Whether any number or state is known at all.
    pub fn is_empty(&self) -> bool {
        self.health == Health::Unknown && self.temperature_c.is_none() && self.wear_percent.is_none() && self.power_on_hours.is_none()
    }

    /// Fills what this one does not know from `other` (the administrator's reading fills what the unelevated read could not give; a
    /// number both have stays the first one's, the state the worse of the two known ones).
    pub fn fill_from(&mut self, other: &DriveHealth) {
        self.health = match (self.health, other.health) {
            (Health::Unknown, h) | (h, Health::Unknown) => h,
            (a, b) => worse(a, b),
        };
        self.temperature_c = self.temperature_c.or(other.temperature_c);
        self.wear_percent = self.wear_percent.or(other.wear_percent);
        self.power_on_hours = self.power_on_hours.or(other.power_on_hours);
        for s in &other.sources {
            if !self.sources.contains(s) {
                self.sources.push(*s);
            }
        }
    }
}

fn worse(a: Health, b: Health) -> Health {
    let rank = |h: Health| match h {
        Health::Good => 0,
        Health::Unknown => 1,
        Health::Warning => 2,
        Health::Failing => 3,
    };
    if rank(a) >= rank(b) { a } else { b }
}

/// The largest disk number the helper accepts in a health request (a number from a drive letter is never near it).
pub const MAX_DISK_NUMBER: u32 = 127;

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
