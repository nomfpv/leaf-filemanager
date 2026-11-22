//! Network locations: shares of a server (`\\server` is not listable with FindFirstFile) and the
//! user's network shortcuts. Blocking and potentially slow (SMB timeouts): off the UI thread.

use std::path::{Path, PathBuf};

use windows::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
use windows::Win32::Storage::FileSystem::{NetShareEnum, SHARE_INFO_1, STYPE_DISKTREE, STYPE_SPECIAL};
use windows::core::PCWSTR;

use crate::wide::wide;

const MAX_PREFERRED_LENGTH: u32 = u32::MAX;
const ERROR_MORE_DATA: u32 = 234;

/// `\\server` or `\\server\` (no share part) → `Some("server")`.
pub fn server_root(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    let rest = s.strip_prefix(r"\\")?;
    if rest.starts_with('?') || rest.starts_with('.') {
        return None;
    }
    let rest = rest.trim_end_matches('\\');
    (!rest.is_empty() && !rest.contains('\\')).then(|| rest.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub remark: String,
}

/// Disk shares of `server` (hidden `$` and admin shares excluded).
pub fn shares(server: &str) -> std::io::Result<Vec<Share>> {
    let w = wide(format!(r"\\{server}"));
    let mut out = Vec::new();
    let mut resume: u32 = 0;
    loop {
        let mut buf: *mut u8 = std::ptr::null_mut();
        let (mut read, mut total) = (0u32, 0u32);
        let rc = unsafe {
            NetShareEnum(PCWSTR(w.as_ptr()), 1, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total, Some(&mut resume))
        };
        if rc != 0 && rc != ERROR_MORE_DATA {
            if !buf.is_null() {
                unsafe { NetApiBufferFree(Some(buf as *const _)) };
            }
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        if !buf.is_null() {
            let items = unsafe { std::slice::from_raw_parts(buf as *const SHARE_INFO_1, read as usize) };
            for it in items {
                if it.shi1_type.0 & !STYPE_SPECIAL.0 != STYPE_DISKTREE.0 || it.shi1_type.0 & STYPE_SPECIAL.0 != 0 {
                    continue;
                }
                let name = unsafe { it.shi1_netname.to_string() }.unwrap_or_default();
                if name.is_empty() || name.ends_with('$') {
                    continue;
                }
                let remark = if it.shi1_remark.is_null() { String::new() } else { unsafe { it.shi1_remark.to_string() }.unwrap_or_default() };
                out.push(Share { name, remark });
            }
            unsafe { NetApiBufferFree(Some(buf as *const _)) };
        }
        if rc != ERROR_MORE_DATA {
            break;
        }
    }
    out.sort_by(|a, b| rf_core::sort::natural_cmp(&a.name, &b.name));
    Ok(out)
}

/// A machine of the local network that offers files (TYPE-COPY of leaf-unix src/network.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub uri: String,
}

/// Whether [`servers`] looks the network up itself. Windows does not: the shell's Network folder and `\\server` addresses do that.
pub const DISCOVERS: bool = false;

/// Servers found on the local network: none here (see [`DISCOVERS`]).
pub fn servers() -> Vec<Server> {
    Vec::new()
}

/// Folders under `%APPDATA%\Microsoft\Windows\Network Shortcuts` ("Add a network location").
/// Each is a folder holding `target.lnk`; returns (display name, target).
pub fn network_shortcuts() -> Vec<(String, PathBuf)> {
    let Some(dir) = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Network Shortcuts")) else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let lnk = p.join("target.lnk");
            let target = crate::shortcut::resolve(&lnk).ok()?.target;
            if target.as_os_str().is_empty() {
                return None;
            }
            Some((e.file_name().to_string_lossy().into_owned(), target))
        })
        .collect();
    out.sort_by(|a, b| rf_core::sort::natural_cmp(&a.0, &b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_root_parsing() {
        assert_eq!(server_root(Path::new(r"\\nas")), Some("nas".into()));
        assert_eq!(server_root(Path::new(r"\\nas\")), Some("nas".into()));
        assert_eq!(server_root(Path::new(r"\\nas\share")), None);
        assert_eq!(server_root(Path::new(r"\\?\C:\x")), None);
        assert_eq!(server_root(Path::new(r"C:\x")), None);
    }

    #[test]
    fn local_shares() {
        // localhost always answers (may legitimately be empty without user shares)
        let r = shares("localhost");
        assert!(r.is_ok(), "{r:?}");
        eprintln!("{:?}", r.unwrap());
        eprintln!("{:?}", network_shortcuts());
    }
}

/// Windows' own "Map network drive" dialog (drive letter, folder, reconnect at sign-in,
/// other credentials). Modal to `owner`; returns when it closes.
pub fn map_drive_dialog(owner: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::NetworkManagement::WNet::{RESOURCETYPE_DISK, WNetConnectionDialog};
    // SAFETY: plain Win32 dialog call on a valid (or null) owner window
    unsafe {
        let _ = WNetConnectionDialog(HWND(owner as *mut _), RESOURCETYPE_DISK.0);
    }
}

/// The folder of a `smb://` or `nfs://` place: only Linux (gvfs) mounts these; `Unsupported` here.
pub fn mount_path(_uri: &str) -> std::io::Result<PathBuf> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// `smb://` with a password: only Linux (`gio mount`); `Unsupported` here (Windows logs in with [`connect_share`]).
pub fn mount_path_login(_uri: &str, _user: &str, _domain: &str, _password: &str) -> std::io::Result<PathBuf> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Lets go of `smb://host/share` (the session Windows keeps for `\\host\share`, and for `\\host\IPC$`, the server's own list of
/// shares): `WNetCancelConnection2W`, never forced (a file still open there keeps it). `NotConnected` when neither was. Other
/// schemes (`nfs://`): `Unsupported`.
pub fn disconnect(uri: &str) -> std::io::Result<()> {
    let Some(rest) = uri.strip_prefix("smb://") else { return Err(std::io::ErrorKind::Unsupported.into()) };
    let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let (host, share) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(':').next().unwrap_or(host);
    let share = share.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let (mut first, mut any) = (None, false);
    for s in [share, "IPC$"].into_iter().filter(|s| !s.is_empty()) {
        match cancel_share(&format!(r"\\{host}\{s}")) {
            Ok(was) => any |= was,
            Err(e) => {
                any = true;
                first.get_or_insert(e);
            }
        }
    }
    match first {
        Some(e) => Err(e),
        // (a session Explorer or a mapped drive holds is under another name: none of these was connected)
        None if !any => Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "nothing of leaf's was connected to this server")),
        None => Ok(()),
    }
}

/// `WNetCancelConnection2W` on `\\host\share`, not forced. `Ok(false)`: it was not connected (2250).
pub fn cancel_share(remote: &str) -> std::io::Result<bool> {
    use windows::Win32::NetworkManagement::WNet::{NET_CONNECT_FLAGS, WNetCancelConnection2W};
    let w = wide(remote);
    // SAFETY: a NUL-terminated name that lives for the call
    let rc = unsafe { WNetCancelConnection2W(PCWSTR(w.as_ptr()), NET_CONNECT_FLAGS(0), false) };
    match rc.0 {
        0 => Ok(true),
        2250 => Ok(false),
        e => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
}

/// Logs in to `\\host\share` (`remote`) as `user` (`name`, `DOMAIN\name` or `name@domain`; empty: this PC's user) with `password`:
/// `WNetAddConnection2W` without a drive letter, `CONNECT_TEMPORARY` unless `remember`. The password's copy made here is wiped
/// before returning. A refused login reads as [`is_login_refused`].
pub fn connect_share(remote: &str, user: &str, password: &str, remember: bool) -> std::io::Result<()> {
    use windows::Win32::NetworkManagement::WNet::{CONNECT_TEMPORARY, NET_CONNECT_FLAGS, NETRESOURCEW, RESOURCETYPE_DISK, WNetAddConnection2W};
    let mut name = wide(remote);
    let user_w = wide(user);
    let mut pw: Vec<u16> = password.encode_utf16().chain(std::iter::once(0)).collect();
    let res = NETRESOURCEW { dwType: RESOURCETYPE_DISK, lpRemoteName: windows::core::PWSTR(name.as_mut_ptr()), ..Default::default() };
    let flags = if remember { NET_CONNECT_FLAGS(0) } else { CONNECT_TEMPORARY };
    let user_p = if user.is_empty() { PCWSTR::null() } else { PCWSTR(user_w.as_ptr()) };
    // SAFETY: every pointer is to a NUL-terminated buffer that lives for the call
    let rc = unsafe { WNetAddConnection2W(&res, PCWSTR(pw.as_ptr()), user_p, flags) };
    for c in pw.iter_mut() {
        // SAFETY: a plain write into our own buffer; volatile so it is not left out
        unsafe { std::ptr::write_volatile(c, 0) };
    }
    if rc.0 == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(rc.0 as i32)) }
}

/// Whether a share's error says the user name or the password is wrong (or that this user may not log in there): access
/// denied, a wrong password, a logon failure, an unknown user, a login that is needed.
pub fn is_login_refused(e: &std::io::Error) -> bool {
    // ERROR_ACCESS_DENIED, ERROR_INVALID_PASSWORD, ERROR_NOT_AUTHENTICATED, ERROR_LOGON_FAILURE, ERROR_ACCOUNT_RESTRICTION,
    // ERROR_BAD_USERNAME, NERR_BadUsername (2202)
    matches!(e.raw_os_error(), Some(5 | 86 | 1244 | 1326 | 1327 | 2202))
}

/// Whether a share's error says the user name or the password itself is wrong (ERROR_INVALID_PASSWORD, ERROR_LOGON_FAILURE,
/// NERR_BadUsername): only then is a saved password deleted. Access denied (the account lost its rights to the share), an
/// account restriction (logon hours, this PC not allowed) or a login that is needed are refusals too, but the password may
/// be right: asked again, the saved one kept.
pub fn is_bad_password(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(86 | 1326 | 2202))
}

/// Windows' "Disconnect network drive" dialog.
pub fn disconnect_drive_dialog(owner: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::NetworkManagement::WNet::{RESOURCETYPE_DISK, WNetDisconnectDialog};
    // SAFETY: plain Win32 dialog call on a valid (or null) owner window
    unsafe {
        let _ = WNetDisconnectDialog(Some(HWND(owner as *mut _)), RESOURCETYPE_DISK.0);
    }
}


/// What a network link is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkType {
    Wifi,
    Ethernet,
    Other,
}

/// The link this machine reaches the network over: its kind and rate in Mbit/s (Wi-Fi's current transmit rate, Ethernet's link
/// speed). The interface is the routing table's best one toward an outside address; nothing is sent. None without a route or a rate.
pub fn lan_link() -> Option<(LinkType, u32)> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestInterface, GetIfEntry2, MIB_IF_ROW2};
    let mut index = 0u32;
    if unsafe { GetBestInterface(u32::from_ne_bytes([8, 8, 8, 8]), &mut index) } != 0 {
        return None;
    }
    let mut row = MIB_IF_ROW2 { InterfaceIndex: index, ..Default::default() };
    unsafe { GetIfEntry2(&mut row) }.ok().ok()?;
    // IF_TYPE_IEEE80211, IF_TYPE_ETHERNET_CSMACD
    let kind = match row.Type {
        71 => LinkType::Wifi,
        6 => LinkType::Ethernet,
        _ => LinkType::Other,
    };
    let bps = row.TransmitLinkSpeed;
    (bps != u64::MAX && bps >= 1_000_000).then(|| (kind, (bps / 1_000_000).min(u64::from(u32::MAX)) as u32))
}
