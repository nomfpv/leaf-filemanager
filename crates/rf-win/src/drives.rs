use std::path::PathBuf;

use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::core::PCWSTR;

use crate::wide::{from_wide, wide};

// (defined once for every system in leaf-common)
pub use leaf_common::drives::{Drive, DriveKind};

// ---------------------------------------------------------------- drives that are not listed

/// The bit of a drive letter in a drive mask ('A' is bit 0), in either case; `None` for what is no drive letter.
pub fn letter_bit(letter: char) -> Option<u32> {
    let l = letter.to_ascii_uppercase();
    l.is_ascii_uppercase().then(|| 1u32 << (l as u8 - b'A'))
}

/// The mask of the letters as the settings keep them or a user writes them: "V", "v:", "V:\\". What is no drive letter is skipped.
pub fn mask_of_letters<S: AsRef<str>>(letters: &[S]) -> u32 {
    letters.iter().filter_map(|s| s.as_ref().trim().chars().next().and_then(letter_bit)).fold(0, |m, b| m | b)
}

/// The letters of a mask, A to Z.
pub fn letters_of_mask(mask: u32) -> Vec<char> {
    (0..26u8).filter(|i| mask & (1 << i) != 0).map(|i| (b'A' + i) as char).collect()
}

/// The drives the user hides in leaf (Settings > Files and folders), as a mask. One value for the process, set from the
/// settings whenever they are applied.
static HIDDEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn set_hidden(mask: u32) {
    HIDDEN.store(mask, std::sync::atomic::Ordering::Relaxed);
}

pub fn hidden() -> u32 {
    HIDDEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// The drives Explorer itself is told not to show: the `NoDrives` value of Explorer's policies, the user's and the machine's (what
/// "Hide these specified drives in My Computer" and the tweak tools write). A drive hidden there is hidden here too.
pub fn policy_hidden() -> u32 {
    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    let read = |root: &windows_registry::Key| root.open(KEY).ok().and_then(|k| k.get_u32("NoDrives").ok()).unwrap_or(0);
    (read(windows_registry::CURRENT_USER) | read(windows_registry::LOCAL_MACHINE)) & 0x03FF_FFFF
}

/// The drives a list of places shows: all of them less the ones the user hides and the ones Explorer hides.
///
/// WHY (issue 23, LokeYourC3PH, with a picture of four drives of which one was a raw encrypted volume: "any encrypted volume which is
/// not the system volume, stills shows up, even though I can hide it in the Windows Explorer ... I would like for an option where we
/// can hide volumes in leaf"). A hidden drive is only not LISTED: its address typed into the address bar opens it as before.
/// It is also not asked for its name and its size, which is where a volume that cannot be read answers slowly or with an error.
pub fn visible_drives() -> Vec<Drive> {
    list_drives_except(hidden() | policy_hidden())
}

/// Lists drive letters. Volume info can stall on disconnected network / empty optical drives,
/// so call this off the UI thread.
pub fn list_drives() -> Vec<Drive> {
    list_drives_except(0)
}

fn list_drives_except(skip: u32) -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() } & !skip;
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let w = wide(&root);
        let kind = match unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            6 => DriveKind::RamDisk,
            _ => DriveKind::Unknown,
        };
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let has_vol = unsafe {
            GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut label), None, None, None, Some(&mut fs))
        }
        .is_ok();
        if !has_vol && kind == DriveKind::Optical {
            // empty DVD tray: still list it, without sizes
        }
        let (mut free, mut total) = (0u64, 0u64);
        if has_vol {
            unsafe {
                let _ = GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), Some(&mut total), None);
            }
        }
        out.push(Drive {
            root: PathBuf::from(root),
            letter: Some(letter),
            label: from_wide(&label),
            kind,
            total,
            free,
            fs: from_wide(&fs),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{letter_bit, letters_of_mask, list_drives_except, mask_of_letters, policy_hidden};

    /// Network paths are slow without touching the network; a file on the system disk is not.
    #[test]
    fn hidden_drives_are_a_mask_of_their_letters() {
        assert_eq!(letter_bit('A'), Some(1));
        assert_eq!(letter_bit('c'), Some(4));
        assert_eq!(letter_bit('Z'), Some(1 << 25));
        assert_eq!(letter_bit('1'), None);
        assert_eq!(letter_bit('ç'), None);
        assert_eq!(mask_of_letters(&["V", "c:", " Z:\\ ", "", "9", "é"]), (1 << 21) | 4 | (1 << 25));
        assert_eq!(mask_of_letters::<&str>(&[]), 0);
        assert_eq!(letters_of_mask((1 << 21) | 4 | 1), ['A', 'C', 'V']);
        assert!(letters_of_mask(0).is_empty());
        // back and forth
        let letters: Vec<String> = letters_of_mask(0b1010_0100).into_iter().map(String::from).collect();
        assert_eq!(mask_of_letters(&letters), 0b1010_0100);
    }

    /// On the PC the tests run on: a drive that is hidden is not listed and not asked anything, and the others are as they were.
    #[test]
    fn a_hidden_drive_is_left_out_of_the_list() {
        let all: Vec<char> = list_drives_except(0).into_iter().filter_map(|d| d.letter).collect();
        let Some(&first) = all.first() else { return };
        let without: Vec<char> = list_drives_except(letter_bit(first).unwrap()).into_iter().filter_map(|d| d.letter).collect();
        assert_eq!(without, all[1..]);
        assert!(list_drives_except(0x03FF_FFFF).is_empty(), "every letter hidden: nothing is listed");
        // (reading Explorer's policy never fails: no value is no drive)
        assert_eq!(policy_hidden() & !0x03FF_FFFF, 0);
    }

    #[test]
    fn slow_places() {
        use std::path::Path;
        assert!(super::slow_to_read(Path::new(r"\\nas\share\film.mkv")));
        assert!(super::slow_to_read(Path::new(r"\\?\UNC\nas\share\film.mkv")));
        assert!(!super::slow_to_read(&std::env::temp_dir()));
        assert!(!super::slow_to_read(Path::new(r"C:\Windows\notepad.exe")));
    }

    #[test]
    fn has_c_drive() {
        let d = super::list_drives();
        let c = d.iter().find(|d| d.letter == Some('C')).expect("C: exists");
        assert!(c.total > 0 && c.free <= c.total);
    }
}

/// Whether `path` is on a removable drive (USB stick, SD card).
pub fn is_removable(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    let b = s.as_bytes();
    if b.len() < 2 || b[1] != b':' {
        return false;
    }
    let root = crate::wide::wide(format!("{}:\\", b[0] as char));
    // 2 = DRIVE_REMOVABLE
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == 2 }
}

/// Whether reading `path` may take seconds: a network share, a removable or optical disk, or a
/// cloud file whose data is not on this PC (reading it downloads it). leaf-look opens such files
/// off the UI thread.
pub fn slow_to_read(path: &std::path::Path) -> bool {
    if crate::localized::is_network(path) {
        return true;
    }
    let s = path.to_string_lossy();
    let letter = match s.as_bytes() {
        [l, b':', ..] => Some(*l),
        [b'\\', b'\\', b'?', b'\\', l, b':', ..] => Some(*l),
        _ => None,
    };
    if let Some(l) = letter {
        let root = crate::wide::wide(format!("{}:\\", l as char));
        // 2 = DRIVE_REMOVABLE, 5 = DRIVE_CDROM
        if matches!(unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }, 2 | 5) {
            return true;
        }
    }
    // FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_OPEN, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    use std::os::windows::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_attributes() & (0x1000 | 0x4_0000 | 0x40_0000) != 0)
}

/// Bitmask of present drive letters (cheap; poll it to notice USB plug/unplug).
pub fn drive_mask() -> u32 {
    unsafe { GetLogicalDrives() }
}

/// Whether the disk holding `path` has a seek penalty (a spinning disk): `None` when it can't
/// be told (network paths, no access). Reads the device's own property; no admin needed.
pub fn seek_penalty(path: &std::path::Path) -> Option<bool> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
        StorageDeviceSeekPenaltyProperty,
    };
    let s = path.to_str()?;
    let b = s.as_bytes();
    if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let dev = crate::wide::wide(format!(r"\\.\{}:", b[0] as char));
    unsafe {
        // no access rights needed for a property query
        let h = CreateFileW(
            windows::core::PCWSTR(dev.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .map_err(|e| log::debug!("seek_penalty open: {e}"))
        .ok()?;
        let q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceSeekPenaltyProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
        let mut d = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut got = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&q as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut d as *mut _ as *mut _),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut got),
            None,
        );
        let _ = CloseHandle(h);
        ok.map_err(|e| log::debug!("seek_penalty ioctl: {e}")).ok()?;
        Some(d.IncursSeekPenalty)
    }
}

/// Bytes read and written since boot on the volume under `path` (counters of the disk driver, `IOCTL_DISK_PERFORMANCE`): `None` when
/// they can't be read (network paths, no access, counters off). Two readings a while apart give the throughput. No admin needed.
pub fn disk_counters(path: &std::path::Path) -> Option<(u64, u64)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
    let s = path.to_str()?;
    let b = s.as_bytes();
    if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return None;
    }
    let dev = crate::wide::wide(format!(r"\\.\{}:", b[0] as char));
    unsafe {
        let h = CreateFileW(
            windows::core::PCWSTR(dev.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .map_err(|e| log::debug!("disk_counters open: {e}"))
        .ok()?;
        let mut d = std::mem::zeroed::<DISK_PERFORMANCE>();
        let mut got = 0u32;
        let ok = DeviceIoControl(
            h,
            IOCTL_DISK_PERFORMANCE,
            None,
            0,
            Some(&mut d as *mut _ as *mut _),
            std::mem::size_of::<DISK_PERFORMANCE>() as u32,
            Some(&mut got),
            None,
        );
        let _ = CloseHandle(h);
        ok.map_err(|e| log::debug!("disk_counters ioctl: {e}")).ok()?;
        Some((d.BytesRead.max(0) as u64, d.BytesWritten.max(0) as u64))
    }
}

// ---------------------------------------------------------------- health and temperature

pub use leaf_common::drives::{DriveHealth, Health, HealthSource};

/// An open volume handle with no access rights (enough for a property query); closed when dropped.
struct Vol(windows::Win32::Foundation::HANDLE);

impl Vol {
    fn open(path: &std::path::Path) -> Option<Self> {
        use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
        let b = path.to_str()?.as_bytes();
        if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
            return None;
        }
        let dev = crate::wide::wide(format!(r"\\.\{}:", b[0] as char));
        unsafe { CreateFileW(PCWSTR(dev.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }
            .map_err(|e| log::debug!("drive health open: {e}"))
            .ok()
            .map(Self)
    }
}

impl Drop for Vol {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// The number of the physical disk under a drive letter (`IOCTL_STORAGE_GET_DEVICE_NUMBER`, no rights needed); `None` for what is
/// not a plain disk volume (network paths, optical drives, a volume spread over several disks).
pub fn disk_number(root: &std::path::Path) -> Option<u32> {
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER};
    let vol = Vol::open(root)?;
    let mut n = STORAGE_DEVICE_NUMBER::default();
    let mut got = 0u32;
    unsafe {
        DeviceIoControl(
            vol.0,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut n as *mut _ as *mut _),
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    // (FILE_DEVICE_DISK)
    (n.DeviceType == 7).then_some(n.DeviceNumber)
}

/// The drive's own temperature in degrees Celsius (`StorageDeviceTemperatureProperty`); `None` when the driver does not answer it
/// without rights (SATA disks often do not).
fn device_temperature(vol: &Vol) -> Option<i16> {
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY, STORAGE_TEMPERATURE_DATA_DESCRIPTOR, StorageDeviceTemperatureProperty};
    let q = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceTemperatureProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
    let mut d = STORAGE_TEMPERATURE_DATA_DESCRIPTOR::default();
    let mut got = 0u32;
    unsafe {
        DeviceIoControl(
            vol.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&q as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut d as *mut _ as *mut _),
            std::mem::size_of::<STORAGE_TEMPERATURE_DATA_DESCRIPTOR>() as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    let t = (d.InfoCount > 0).then(|| d.TemperatureInfo[0].Temperature)?;
    // (the driver answers with a sentinel when there is no sensor)
    (-40..=150).contains(&t).then_some(t)
}

/// A connection to the Storage Management provider (`ROOT\Microsoft\Windows\Storage`) on this thread. Reading is allowed to a
/// normal user for the disks' health; the reliability counters need administrator rights.
struct Storage {
    /// (taken out before COM is ended: an interface must not be released after that)
    svc: Option<windows::Win32::System::Wmi::IWbemServices>,
    /// COM was started by us on this thread and is ended with the connection.
    com: bool,
}

impl Storage {
    fn connect() -> Option<Self> {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE};
        use windows::Win32::System::Wmi::{IWbemLocator, IWbemServices, WbemLocator};
        use windows::core::BSTR;
        unsafe {
            // (S_OK and S_FALSE both count a use that is ended again; another apartment mode on this thread is used as it is)
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let made = (|| -> windows::core::Result<IWbemServices> {
                let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
                let svc = loc.ConnectServer(&BSTR::from(r"ROOT\Microsoft\Windows\Storage"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
                // (10 = RPC_C_AUTHN_WINNT, 0 = RPC_C_AUTHZ_NONE)
                CoSetProxyBlanket(&svc, 10, 0, None, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)?;
                Ok(svc)
            })();
            match made {
                Ok(svc) => Some(Self { svc: Some(svc), com }),
                Err(e) => {
                    log::debug!("storage provider: {e}");
                    if com {
                        CoUninitialize();
                    }
                    None
                }
            }
        }
    }

    /// The objects a query returns (at most 130: more disks than a machine has).
    fn query(&self, wql: &str) -> Vec<windows::Win32::System::Wmi::IWbemClassObject> {
        use windows::Win32::System::Wmi::{WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE};
        use windows::core::BSTR;
        let mut out = Vec::new();
        let Some(svc) = &self.svc else { return out };
        let Ok(rows) = (unsafe { svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(wql), WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None) }) else {
            return out;
        };
        while out.len() < 130 {
            let mut one = [None];
            let mut got = 0u32;
            let hr = unsafe { rows.Next(WBEM_INFINITE, &mut one, &mut got) };
            match one[0].take() {
                Some(o) if hr.is_ok() && got == 1 => out.push(o),
                _ => break,
            }
        }
        out
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.svc = None;
        if self.com {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

fn wmi_u32(o: &windows::Win32::System::Wmi::IWbemClassObject, name: &str) -> Option<u32> {
    let n = crate::wide::wide(name);
    let mut v = windows::Win32::System::Variant::VARIANT::default();
    unsafe { o.Get(PCWSTR(n.as_ptr()), 0, &mut v, None, None) }.ok()?;
    // (a counter the drive does not keep comes back as a null value, not as 0)
    if v.is_empty() || v.vt() == windows::Win32::System::Variant::VT_NULL {
        return None;
    }
    u32::try_from(&v).ok()
}

fn wmi_text(o: &windows::Win32::System::Wmi::IWbemClassObject, name: &str) -> Option<String> {
    let n = crate::wide::wide(name);
    let mut v = windows::Win32::System::Variant::VARIANT::default();
    unsafe { o.Get(PCWSTR(n.as_ptr()), 0, &mut v, None, None) }.ok()?;
    windows::core::BSTR::try_from(&v).ok().map(|b| b.to_string())
}

/// `MSFT_PhysicalDisk.HealthStatus`: 0 healthy, 1 warning, 2 unhealthy, anything else (5 = unknown) is not said to be good.
fn health_of(code: Option<u32>) -> Health {
    match code {
        Some(0) => Health::Good,
        Some(1) => Health::Warning,
        Some(2) => Health::Failing,
        _ => Health::Unknown,
    }
}

/// What can be read about the disk under `root` WITHOUT administrator rights: the Storage Management health status of the physical
/// disk and, where the driver allows it, the drive's own temperature. `None` when `root` is not on a plain disk. Blocking (a
/// provider query takes some tens of milliseconds): off the UI thread, and not more than about once a minute.
///
/// Wear and power-on hours are not given here: Windows hands those out (the reliability counters) only to administrators, see
/// [`disk_health_admin`].
pub fn disk_health(root: &std::path::Path) -> Option<DriveHealth> {
    let vol = Vol::open(root)?;
    let disk = disk_number(root)?;
    let mut h = DriveHealth::unknown(Some(disk));
    if let Some(t) = device_temperature(&vol) {
        h.temperature_c = Some(t);
        h.sources.push(HealthSource::DeviceQuery);
    }
    drop(vol);
    if let Some(st) = Storage::connect() {
        // (the number comes from the system, not from text: it only ever holds digits)
        let rows = st.query(&format!("SELECT DeviceId, HealthStatus FROM MSFT_PhysicalDisk WHERE DeviceId='{disk}'"));
        if let Some(r) = rows.first() {
            h.health = health_of(wmi_u32(r, "HealthStatus"));
            h.sources.push(HealthSource::StorageManagement);
        }
    }
    Some(h)
}

/// Whether reading wear and power-on hours on this system takes the administrator helper (the helper's "drive health" request).
pub const HEALTH_NEEDS_ADMIN: bool = true;

/// For the administrator helper (READ ONLY): health, temperature, wear and power-on hours of the physical disk `disk`, or of all of
/// them, from the Storage Management provider's reliability counters, which only an administrator may read. A disk that says
/// nothing is left out.
pub fn disk_health_admin(disk: Option<u32>) -> Vec<DriveHealth> {
    let Some(st) = Storage::connect() else { return Vec::new() };
    let wql = match disk {
        Some(n) => format!("SELECT * FROM MSFT_PhysicalDisk WHERE DeviceId='{n}'"),
        None => "SELECT * FROM MSFT_PhysicalDisk".to_string(),
    };
    let mut out = Vec::new();
    for d in st.query(&wql) {
        let Some(n) = wmi_text(&d, "DeviceId").and_then(|t| t.trim().parse::<u32>().ok()) else { continue };
        let mut h = DriveHealth::unknown(Some(n));
        h.health = health_of(wmi_u32(&d, "HealthStatus"));
        // the counters hang off the disk (an association, not a class that can be listed alone)
        if let Some(rel) = wmi_text(&d, "__RELPATH") {
            let counters = st.query(&format!("ASSOCIATORS OF {{{rel}}} WHERE AssocClass = MSFT_PhysicalDiskToStorageReliabilityCounter"));
            if let Some(c) = counters.first() {
                h.temperature_c = wmi_u32(c, "Temperature").filter(|t| (1..=150).contains(t)).map(|t| t as i16);
                h.wear_percent = wmi_u32(c, "Wear").filter(|w| *w <= 100).map(|w| w as u8);
                h.power_on_hours = wmi_u32(c, "PowerOnHours");
            }
        }
        h.sources.push(HealthSource::Administrator);
        out.push(h);
    }
    out
}

#[cfg(test)]
mod seek_tests {
    #[test]
    fn system_drive_answers() {
        // the system drive always exists: the query must not panic (virtual disks, as on CI
        // runners, may not answer it, so the result itself isn't asserted)
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let _ = super::seek_penalty(std::path::Path::new(&format!("{sys}\\")));
        assert_eq!(super::seek_penalty(std::path::Path::new(r"\\server\share")), None);
    }

    /// The unelevated health read on the system drive: never panics, never answers "good" without a source, and a network path has
    /// no health at all. Prints the state and the temperature only (no serial numbers are read).
    #[test]
    fn system_drive_health_is_read_without_rights() {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let got = super::disk_health(std::path::Path::new(&format!("{sys}\\")));
        if let Some(h) = &got {
            println!("health {:?}, temperature {:?}, sources {:?}", h.health, h.temperature_c, h.sources);
            assert!(h.health == super::Health::Unknown || h.sources.contains(&super::HealthSource::StorageManagement));
            assert!(h.wear_percent.is_none() && h.power_on_hours.is_none());
        }
        assert!(super::disk_health(std::path::Path::new(r"\\server\share")).is_none());
    }

    /// The administrator reading: one entry per disk, each marked as read by the administrator. (Without rights the counters are
    /// refused and only the state is there.) Prints numbers only.
    #[test]
    fn the_administrator_reading_names_its_source_and_one_disk_stays_one_disk() {
        let all = super::disk_health_admin(None);
        for h in &all {
            println!("disk {:?}: {:?}, {:?} C, wear {:?}, hours {:?}", h.disk, h.health, h.temperature_c, h.wear_percent, h.power_on_hours);
            assert!(h.sources.contains(&super::HealthSource::Administrator));
        }
        if let Some(first) = all.first().and_then(|h| h.disk) {
            let one = super::disk_health_admin(Some(first));
            assert_eq!(one.len(), 1);
            assert_eq!(one[0].disk, Some(first));
        }
    }
}
