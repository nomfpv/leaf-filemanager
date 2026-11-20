//! Installing through Velopack (its install / update / uninstall hooks). Update checks are off in this fork: nothing here contacts
//! a release feed, so an update from the upstream project can never be downloaded or applied over this build.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use velopack::VelopackApp;

/// AppUserModelID of the installed app (Velopack names shortcuts "velopack.<pack id>").
pub const APP_ID: &str = "velopack.rustyfiles";

/// Whether a process started with this first argument may apply a waiting update as it starts. Velopack does that before any code
/// of ours runs, and it ends every running rustyfiles process to do it.
///
/// WHY NOT EVERY PROCESS (the updater's log on the user's PC, 2026-11-14: an update from 0.7.0 to 0.7.2 was applied by a process
/// started with `--pick`, that is by another program's Open dialog; the dock, the picker, the quick search and the main app in the
/// background are all started without the user asking for rustyfiles). Only a start the user means as "open rustyfiles" applies it:
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
    VelopackApp::build()
        .set_auto_apply_on_startup(applies_update_at_start(first.as_deref()))
        .on_after_install_fast_callback(|_| {
            after_install_or_update();
            dialogs_on_by_default();
        })
        .on_after_update_fast_callback(|_| after_install_or_update())
        .on_before_uninstall_fast_callback(|_| before_uninstall())
        .run();
}

/// Registrations that are on follow this install: the default file manager, DirectLook's start
/// with Windows, and the Open/Save dialog DLL (a per-build copy).
fn after_install_or_update() {
    let Ok(me) = std::env::current_exe() else { return };
    if rf_win::default_fm::is_enabled(rf_win::default_fm::CLASSES) {
        let _ = rf_win::default_fm::enable(&me.display().to_string(), rf_win::default_fm::CLASSES);
    }
    if rf_win::dialogs::starts_with_windows_as("DirectLook") {
        let _ = rf_win::dialogs::set_start_with_windows_as("DirectLook", &me.with_file_name("directlook.exe"), &["--background"], true);
    }
    let _ = crate::dialog_registration(crate::DialogReg::Follow);
}

/// A fresh install turns on rustyfiles' Open/Save dialogs (per user, no admin rights; Settings →
/// Advanced turns them off). Updates don't, so someone who turned them off keeps them off.
fn dialogs_on_by_default() {
    if !rf_win::dialog_reg::is_enabled(rf_win::dialog_reg::CLASSES) {
        let _ = crate::dialog_registration(crate::DialogReg::Enable);
    }
}

/// Everything rustyfiles changed in Windows goes back (settings in %APPDATA% stay).
fn before_uninstall() {
    let classes = rf_win::default_fm::CLASSES;
    if rf_win::default_fm::is_enabled(classes) {
        let _ = rf_win::default_fm::disable(classes);
    }
    let _ = rf_win::dialog_reg::disable(rf_win::dialog_reg::CLASSES);
    let none = Path::new("");
    let _ = rf_win::dialogs::set_start_with_windows_as("DirectLook", none, &[], false);
    let _ = rf_win::dialogs::set_start_with_windows(none, false);
    // DirectLook holds its exe open
    rf_win::single_instance::send_to(&crate::look::ipc::app_name(), &crate::look::ipc::Msg::Quit.encode());
    std::thread::sleep(Duration::from_millis(500));
    // and so does a running dock (a process of its own that outlives rustyfiles' window)
    crate::dock::control::stop_for_update();
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

/// `RUSTYFILES_FAKE_UPDATE=1` (test copies): check, download 0→100 % and "ready" without a network,
/// to see the About page's states.
fn fake_check() {
    use std::thread::sleep;
    set(State::Checking);
    sleep(Duration::from_millis(1500));
    for pct in (0..=100).step_by(4) {
        set(State::Downloading("9.9.9".into(), pct));
        sleep(Duration::from_millis(120));
    }
    // (`RUSTYFILES_FAKE_UPDATE=important`: the look of an important one)
    if std::env::var("RUSTYFILES_FAKE_UPDATE").is_ok_and(|v| v.trim() == "important") {
        *IMPORTANT.lock().unwrap_or_else(|p| p.into_inner()) = Some("9.9.9".into());
    }
    set(State::Ready("9.9.9".into()));
}

/// "0.1.0 · b241ace · 08.11.2026 02:40": the version, the commit and when it was built.
pub fn version_full() -> String {
    let built = env!("RUSTYFILES_BUILT").parse::<i64>().ok().and_then(|s| chrono::DateTime::from_timestamp(s, 0));
    let when = built.map(|t| t.with_timezone(&chrono::Local).format("%d.%m.%Y %H:%M").to_string()).unwrap_or_default();
    format!("{} · {} · {when}", version(), env!("RUSTYFILES_COMMIT"))
}

/// This build's version (the crate's).
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Update checks are off in this fork: nothing is fetched (`RUSTYFILES_FAKE_UPDATE` still shows the About page's states).
pub fn check() {
    if std::env::var_os("RUSTYFILES_FAKE_UPDATE").is_some() {
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

/// Where a coffee is bought (the README's button).
pub const DONATE: &str = "https://www.buymeacoffee.com/ozerguzel";

const DAY_MS: i64 = 24 * 3600 * 1000;

/// The "buy me a coffee" strip: after two weeks and 40 starts, then again some weeks after each
/// answer, until turned off.
pub fn donate_due(s: &crate::settings::Settings) -> bool {
    let now = chrono::Utc::now().timestamp_millis();
    if s.donate_never || s.first_start_ms == 0 || s.starts < 40 {
        return false;
    }
    let first = s.first_start_ms + 14 * DAY_MS;
    now >= first.max(s.donate_next_ms)
}

/// How long the strip stays away after an answer: longer after a coffee than after "later".
pub fn donate_answered(s: &mut crate::settings::Settings, donated: bool) {
    let now = chrono::Utc::now().timestamp_millis();
    s.donate_next_ms = now + if donated { 120 } else { 30 } * DAY_MS;
    s.donate_seen = s.donate_seen.saturating_add(1);
}

/// Counted at each start of the app (not for test copies or a dialog-only start).
pub fn count_start(cx: &mut gpui_kit::App) {
    if std::env::var_os("RUSTYFILES_CONFIG_DIR").is_some() {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    crate::settings::update(cx, |s| {
        if s.first_start_ms == 0 {
            s.first_start_ms = now;
        }
        s.starts = s.starts.saturating_add(1);
    });
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, donate_answered, donate_due};

    #[test]
    fn coffee_strip_now_and_then() {
        let now = chrono::Utc::now().timestamp_millis();
        let mut s = crate::settings::Settings::default();
        s.first_start_ms = now - 15 * DAY_MS;
        s.starts = 39;
        assert!(!donate_due(&s)); // too few starts
        s.starts = 40;
        assert!(donate_due(&s));
        s.first_start_ms = now - 13 * DAY_MS;
        assert!(!donate_due(&s)); // not two weeks yet
        s.first_start_ms = now - 15 * DAY_MS;
        donate_answered(&mut s, false);
        assert!(!donate_due(&s) && s.donate_seen == 1); // "later": a month away
        s.donate_next_ms = now - 1;
        assert!(donate_due(&s));
        donate_answered(&mut s, true);
        assert!(s.donate_next_ms - now >= 119 * DAY_MS); // after a coffee: four months
        s.donate_next_ms = 0;
        s.donate_never = true;
        assert!(!donate_due(&s));
    }

}

#[cfg(test)]
mod start_apply_tests {
    use super::applies_update_at_start;

    /// A waiting update is applied when the user opens rustyfiles, and never by a process started behind the user's back.
    #[test]
    fn only_a_start_the_user_means_applies_a_waiting_update() {
        for open in [None, Some(r"C:\Users\x\Documents"), Some("D:\\"), Some("report.pdf"), Some("--settings=dock"), Some("--settings=speed")] {
            assert!(applies_update_at_start(open), "{open:?}");
        }
        for side in ["--pick", "--dock", "--shell", "--quick-search", "--no-window", "--update-splash", "--bar-preview", "--shell-probe", "--veloapp-updated", "--veloapp-obsolete"] {
            assert!(!applies_update_at_start(Some(side)), "{side}");
        }
    }
}
