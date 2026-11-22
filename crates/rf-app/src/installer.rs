//! Installing through Velopack (its install / update / uninstall hooks). Update checks are off in this fork: nothing here contacts
//! a release feed, so an update from the upstream project can never be downloaded or applied over this build.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use velopack::VelopackApp;

/// AppUserModelID of the installed app (Velopack names shortcuts "velopack.<pack id>").
pub const APP_ID: &str = "velopack.leaf";

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
    let app = VelopackApp::build().set_auto_apply_on_startup(applies_update_at_start(first.as_deref()));
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
    let journal = leaf_sys::default_fm::explorer_hook::journal_path();
    // the Win+E hook re-points its own verbs only (the default file manager's enable would add the archive registration too)
    if leaf_sys::default_fm::explorer_hook::is_enabled(leaf_sys::default_fm::CLASSES, &journal) {
        let _ = leaf_sys::default_fm::explorer_hook::enable(&me.display().to_string(), leaf_sys::default_fm::CLASSES, &journal);
    } else if leaf_sys::default_fm::is_enabled(leaf_sys::default_fm::CLASSES) {
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
    // the Win+E hook first: it puts back what was there before it (which may be the default file manager's keys, removed next)
    let _ = leaf_sys::default_fm::explorer_hook::disable(classes, &leaf_sys::default_fm::explorer_hook::journal_path());
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

/// Update checks are off in this fork: nothing is fetched (`LEAF_FAKE_UPDATE` still shows the About page's states).
pub fn check() {
    if std::env::var_os("LEAF_FAKE_UPDATE").is_some() {
        return fake_check();
    }
    set(State::NotInstalled);
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

/// Updates are off in this fork: there is never a downloaded update to restart into.
pub fn restart_to_update() {}

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
