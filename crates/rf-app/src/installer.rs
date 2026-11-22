//! Installing through Velopack (its install / update / uninstall hooks). Update checks are off unless the user turned them on
//! (Settings: "Check for leaf updates") AND wrote a feed address: with either missing nothing here contacts any server. leaf has no
//! feed address of its own, so an update from the upstream project can never be downloaded or applied over this build. Velopack
//! checks every downloaded package against the SHA1 / SHA256 of the feed's entry before it can be applied; nothing here bypasses that.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use velopack::VelopackApp;

/// AppUserModelID of the installed app (Velopack names shortcuts "velopack.<pack id>").
pub const APP_ID: &str = "velopack.leaf";

/// This copy was put in place by the installer (Velopack's layout: the program in `current`, `Update.exe` one folder up), so its
/// shortcuts carry [`APP_ID`]. A build or a copied folder is not.
pub fn is_installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())).is_some_and(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("current")) && d.parent().is_some_and(|r| r.join("Update.exe").is_file()))
}

/// How this copy of leaf was put on the PC. Decides whether it may update itself and what Settings says about administrator mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// In Program Files (`tools\install-local.ps1 -AllUsers`, or a per-machine Velopack setup): only administrators can change the
    /// files, so the administrator helper may be started; no self-update (the installer is run again).
    AllUsers,
    /// In the user's own folders (`install-local.ps1` to `%LOCALAPPDATA%\Programs\leaf`, or the per-user Velopack setup).
    PerUser,
    /// A build folder, a portable copy or any other folder.
    Other,
}

/// The kind of install for the program file `exe`. `program_dirs`: the Program Files folders of this PC; `velopack`: the
/// updater's layout ([`is_installed`]).
pub fn kind_for(exe: &Path, program_dirs: &[std::path::PathBuf], local_programs: Option<&Path>, velopack: bool) -> InstallKind {
    let inside = |dir: &Path| {
        let (e, d) = (exe.to_string_lossy().to_lowercase().replace('/', "\\"), dir.to_string_lossy().to_lowercase().replace('/', "\\"));
        let d = d.trim_end_matches('\\');
        !d.is_empty() && e.strip_prefix(d).is_some_and(|rest| rest.starts_with('\\'))
    };
    if program_dirs.iter().any(|d| inside(d)) {
        InstallKind::AllUsers
    } else if velopack || local_programs.is_some_and(inside) {
        InstallKind::PerUser
    } else {
        InstallKind::Other
    }
}

/// The kind of this running copy (worked out once).
pub fn install_kind() -> InstallKind {
    static KIND: std::sync::OnceLock<InstallKind> = std::sync::OnceLock::new();
    *KIND.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else { return InstallKind::Other };
        let program_dirs: Vec<std::path::PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|v| std::env::var_os(v)).map(Into::into).collect();
        let local = std::env::var_os("LOCALAPPDATA").map(|l| std::path::PathBuf::from(l).join("Programs"));
        kind_for(&exe, &program_dirs, local.as_deref(), is_installed())
    })
}

/// The kind of install a registration's program file `leaf` belongs to (the same rules; for Velopack's layout the folder above
/// `current`).
fn kind_of_registered(leaf: &Path) -> InstallKind {
    let program_dirs: Vec<std::path::PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"].iter().filter_map(|v| std::env::var_os(v)).map(Into::into).collect();
    let local = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    let velopack = leaf.parent().is_some_and(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("current")) && d.parent().is_some_and(|r| r.join("Update.exe").is_file()));
    // %LOCALAPPDATA%\Programs\leaf (install-local) and %LOCALAPPDATA%\leaf (Velopack) are per-user installs
    let programs = local.as_ref().map(|l| l.join("Programs"));
    kind_for(leaf, &program_dirs, programs.as_deref(), velopack)
}

/// After a switch between a per-user and an all-users install the Windows registrations (default file manager, the browsers'
/// hand-over host) still name the old place. An installed leaf takes them over when they name the OTHER kind of install; a
/// build folder or a portable copy never does, and registrations that name some other place are left alone.
pub fn follow_install() {
    let mine = install_kind();
    if mine == InstallKind::Other {
        return;
    }
    let Ok(me) = std::env::current_exe() else { return };
    let classes = leaf_sys::default_fm::CLASSES;
    if let Some(old) = leaf_sys::default_fm::registered_leaf(classes)
        && !old.as_os_str().eq_ignore_ascii_case(me.as_os_str())
        && kind_of_registered(&old) != InstallKind::Other
        && kind_of_registered(&old) != mine
    {
        match leaf_sys::default_fm::enable(&me.display().to_string(), classes) {
            Ok(()) => log::warn!("default file manager moved from {} to this install ({}): the other kind of install", old.display(), me.display()),
            Err(e) => log::warn!("default file manager still names {}: {e:#}", old.display()),
        }
    }
}

/// Whether a process started with this first argument may apply a waiting update as it starts. Velopack does that before any code
/// of ours runs, and it ends every running leaf process to do it.
///
/// WHY NOT EVERY PROCESS (the updater's log on the user's PC, 2026-11-14: an update from 0.7.0 to 0.7.2 was applied by a process
/// started with `--pick`, that is by another program's Open dialog; the dock, the picker, the quick search and the main app in the
/// background are all started without the user asking for leaf). Only a start the user means as "open leaf" applies it:
/// no argument, a folder or file to open, or a Settings page. Everything that begins with `--` otherwise is a side process (and
/// the updater's own hooks, which never apply anything). The update is not lost: the next such start applies it, and so does the
/// button in Settings and in the status bar.
pub fn applies_update_at_start(first_arg: Option<&str>) -> bool {
    match first_arg {
        None => true,
        Some(a) if a.starts_with("--settings=") => true,
        Some(a) => !a.starts_with("--"),
    }
}

/// First thing in `main`: Velopack's install / update / uninstall hooks run here and exit.
pub fn startup() {
    let first = std::env::args().nth(1);
    // (an all-users install never applies an update by itself: it is replaced by running the installer again)
    let apply = applies_update_at_start(first.as_deref()) && install_kind() != InstallKind::AllUsers;
    let app = VelopackApp::build().set_auto_apply_on_startup(apply);
    // the install / update / uninstall hooks exist in Velopack's Windows build only
    #[cfg(windows)]
    let app = app
        .on_after_install_fast_callback(|_| after_install_or_update())
        .on_after_update_fast_callback(|_| after_install_or_update())
        .on_before_uninstall_fast_callback(|_| before_uninstall());
    let mut app = app;
    app.run();
}

/// Registrations that are on follow this install: the default file manager and leaf-look's start
/// with Windows.
#[cfg_attr(not(windows), allow(dead_code))]
fn after_install_or_update() {
    let Ok(me) = std::env::current_exe() else { return };
    if leaf_sys::default_fm::is_enabled(leaf_sys::default_fm::CLASSES) {
        let _ = leaf_sys::default_fm::enable(&me.display().to_string(), leaf_sys::default_fm::CLASSES);
    }
    if leaf_sys::dialogs::starts_with_windows_as("leaf-look") {
        let _ = leaf_sys::dialogs::set_start_with_windows_as("leaf-look", &me.with_file_name(leaf_sys::exe_name(leaf_sys::LOOK_EXE_STEM)), &["--background"], true);
    }
    // the browsers' download hand-over, when it was connected: to this install's leaf-nmhost
    crate::capture::refresh();
}

/// Everything leaf changed in Windows goes back (settings in %APPDATA% stay).
#[cfg_attr(not(windows), allow(dead_code))]
fn before_uninstall() {
    let classes = leaf_sys::default_fm::CLASSES;
    if leaf_sys::default_fm::is_enabled(classes) {
        let _ = leaf_sys::default_fm::disable(classes);
    }
    let none = Path::new("");
    let _ = leaf_sys::dialogs::set_start_with_windows_as("leaf-look", none, &[], false);
    let _ = leaf_sys::dialogs::set_start_with_windows(none, false);
    crate::capture::unregister();
    // leaf-look holds its exe open
    leaf_sys::single_instance::send_to(&crate::look::ipc::app_name(), &crate::look::ipc::Msg::Quit.encode());
    std::thread::sleep(Duration::from_millis(500));
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Not installed by Velopack: no updates.
    NotInstalled,
    /// Installed for all users (Program Files): no self-update, the installer is run again.
    AllUsers,
    Idle,
    Checking,
    /// Version and percent downloaded.
    Downloading(String, u8),
    /// Downloaded: applied on restart (or at the next start anyway).
    Ready(String),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The downloaded version, when it is an important one (only the fake update sets it now).
static IMPORTANT: Mutex<Option<String>> = Mutex::new(None);

/// Whether the downloaded `version` brings an important fix.
pub fn is_important(version: &str) -> bool {
    IMPORTANT.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(version)
}

pub fn state() -> State {
    if install_kind() == InstallKind::AllUsers && std::env::var_os("LEAF_FAKE_UPDATE").is_none() {
        return State::AllUsers;
    }
    STATE.lock().unwrap().clone().unwrap_or(State::Idle)
}

fn set(s: State) {
    if matches!(s, State::Ready(_)) && !matches!(state(), State::Ready(_)) {
        *READY_SINCE.lock().unwrap() = Some(std::time::Instant::now());
    }
    *STATE.lock().unwrap() = Some(s);
}

static READY_SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// When the downloaded update became ready (the Status Center button pulses for a few seconds).
pub fn ready_since() -> Option<std::time::Instant> {
    *READY_SINCE.lock().unwrap()
}

/// A check or download is running (the About page redraws while this is true).
pub fn busy() -> bool {
    matches!(state(), State::Checking | State::Downloading(..))
}

/// `LEAF_FAKE_UPDATE=1` (test copies): check, download 0→100 % and "ready" without a network,
/// to see the About page's states.
fn fake_check() {
    use std::thread::sleep;
    set(State::Checking);
    sleep(Duration::from_millis(1500));
    for pct in (0..=100).step_by(4) {
        set(State::Downloading("9.9.9".into(), pct));
        sleep(Duration::from_millis(120));
    }
    // (`LEAF_FAKE_UPDATE=important`: the look of an important one)
    if std::env::var("LEAF_FAKE_UPDATE").is_ok_and(|v| v.trim() == "important") {
        *IMPORTANT.lock().unwrap_or_else(|p| p.into_inner()) = Some("9.9.9".into());
    }
    set(State::Ready("9.9.9".into()));
}

/// "0.1.0 · b241ace · 08.11.2026 02:40": the version, the commit and when it was built.
pub fn version_full() -> String {
    let built = env!("LEAF_BUILT").parse::<i64>().ok().and_then(|s| chrono::DateTime::from_timestamp(s, 0));
    let when = built.map(|t| t.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default();
    format!("{} · {} · {when}", version(), env!("LEAF_COMMIT"))
}

/// This build's version (the crate's).
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// What a check found and downloaded: applied by [`restart_to_update`].
static PENDING: Mutex<Option<(velopack::UpdateManager, velopack::VelopackAsset)>> = Mutex::new(None);

/// Why the last check failed (shown under the Updates card); empty when it did not.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

pub fn last_error() -> String {
    LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Whether `feed` is something a check may use: an https address or a folder on this PC (plain http is refused).
pub fn feed_is_valid(feed: &str) -> bool {
    let f = feed.trim();
    if f.contains("://") {
        f.len() > 8 && f.get(..8).is_some_and(|h| h.eq_ignore_ascii_case("https://"))
    } else {
        !f.is_empty()
    }
}

/// The one place an update check starts: `enabled` is the Settings switch (off by default), `feed` the address from Settings
/// (empty by default). With the switch off or no usable address this returns before anything is created: no socket, no request.
/// `LEAF_FAKE_UPDATE` (test copies) shows the About page's states without a network.
pub fn check(enabled: bool, feed: &str) {
    if std::env::var_os("LEAF_FAKE_UPDATE").is_some() {
        return fake_check();
    }
    LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()).clear();
    // no self-update into Program Files, whatever the settings say (nothing below runs: no socket, no request)
    if install_kind() == InstallKind::AllUsers {
        return set(State::AllUsers);
    }
    if !enabled || !feed_is_valid(feed) {
        return set(State::Idle);
    }
    if !is_installed() {
        return set(State::NotInstalled);
    }
    set(State::Checking);
    match run_check(feed.trim()) {
        Ok(Some((v, pending))) => {
            *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some(pending);
            set(State::Ready(v));
        }
        Ok(None) => set(State::Idle),
        Err(e) => {
            log::warn!("update check: {e}");
            *LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = e;
            set(State::Idle);
        }
    }
}

/// Velopack's own check and download (its checksum test included); the version and what to apply when one is ready.
fn run_check(feed: &str) -> Result<Option<(String, (velopack::UpdateManager, velopack::VelopackAsset))>, String> {
    use velopack::{sources::AutoSource, UpdateCheck, UpdateManager};
    let um = UpdateManager::new(AutoSource::new(feed), None, None).map_err(|e| e.to_string())?;
    // a package downloaded earlier and not yet applied
    if let Some(a) = um.get_update_pending_restart() {
        let v = a.Version.to_string();
        return Ok(Some((v, (um, a))));
    }
    let info = match um.check_for_updates().map_err(|e| e.to_string())? {
        UpdateCheck::UpdateAvailable(i) => *i,
        _ => return Ok(None),
    };
    let v = info.TargetFullRelease.Version.to_string();
    set(State::Downloading(v.clone(), 0));
    let (tx, rx) = std::sync::mpsc::channel::<i16>();
    let shown = v.clone();
    let reader = std::thread::spawn(move || {
        for pct in rx {
            set(State::Downloading(shown.clone(), pct.clamp(0, 100) as u8));
        }
    });
    let done = um.download_updates(&info, Some(tx)).map_err(|e| e.to_string());
    let _ = reader.join();
    done?;
    Ok(Some((v, (um, info.TargetFullRelease))))
}

/// Redraws the windows a few times a second while a check or download runs (progress on About).
pub async fn redraw_while_busy(cx: &mut gpui_kit::AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(50)).await;
    while busy() {
        cx.update(|cx| cx.refresh_windows());
        cx.background_executor().timer(Duration::from_millis(150)).await;
    }
    cx.update(|cx| cx.refresh_windows());
}

/// Ends leaf, applies the downloaded update and starts the new version. Does nothing when no update was downloaded by a check
/// (the fake update included).
pub fn restart_to_update() {
    let Some((um, asset)) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).take() else { return };
    if let Err(e) = um.apply_updates_and_restart(&asset) {
        log::warn!("apply update: {e}");
        *LAST_ERROR.lock().unwrap_or_else(|p| p.into_inner()) = e.to_string();
        *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some((um, asset));
    }
}

#[cfg(all(test, windows))]
mod kind_tests {
    use super::{InstallKind::*, kind_for};
    use std::path::{Path, PathBuf};

    #[test]
    fn program_files_is_all_users_and_the_users_own_folders_are_per_user() {
        let pf = [PathBuf::from(r"C:\Program Files"), PathBuf::from(r"C:\Program Files (x86)")];
        let local = Path::new(r"C:\Users\user\AppData\Local\Programs");
        assert_eq!(kind_for(Path::new(r"C:\Program Files\leaf\leaf.exe"), &pf, Some(local), false), AllUsers);
        assert_eq!(kind_for(Path::new(r"c:\program files\leaf\current\leaf.exe"), &pf, Some(local), true), AllUsers);
        assert_eq!(kind_for(Path::new(r"C:\Program Files (x86)\leaf\leaf.exe"), &pf, Some(local), false), AllUsers);
        assert_eq!(kind_for(Path::new(r"C:\Program Files Extra\leaf.exe"), &pf, Some(local), false), Other, "a sibling name is not inside");
        assert_eq!(kind_for(Path::new(r"C:\Users\user\AppData\Local\Programs\leaf\leaf.exe"), &pf, Some(local), false), PerUser);
        assert_eq!(kind_for(Path::new(r"C:\Users\user\AppData\Local\leaf\current\leaf.exe"), &pf, Some(local), true), PerUser);
        assert_eq!(kind_for(Path::new(r"C:\dev\leaf\target\release\leaf.exe"), &pf, Some(local), false), Other);
    }
}

#[cfg(test)]
mod start_apply_tests {
    use super::applies_update_at_start;

    /// A waiting update is applied when the user opens leaf, and never by a process started behind the user's back.
    #[test]
    fn only_a_start_the_user_means_applies_a_waiting_update() {
        for open in [None, Some(r"C:\Users\user\Documents"), Some("D:\\"), Some("report.pdf"), Some("--settings=dock"), Some("--settings=speed")] {
            assert!(applies_update_at_start(open), "{open:?}");
        }
        for side in ["--pick", "--dock", "--shell", "--quick-search", "--no-window", "--update-splash", "--bar-preview", "--shell-probe", "--veloapp-updated", "--veloapp-obsolete"] {
            assert!(!applies_update_at_start(Some(side)), "{side}");
        }
    }
}
