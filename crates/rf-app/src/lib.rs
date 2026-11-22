//! leaf ve leaf-look'un ortak kodu; iki program da bu kütüphaneyi kullanır.
#![recursion_limit = "256"]

mod actions;
mod admin;
mod agent_files;
mod aliases;
mod background;
mod cli;
pub mod bench;
mod fast_start;
mod git_share;
mod features_page;
mod whats_new_page;
mod bench_view;
mod card_img;
mod nearby;
mod archive_view;
mod bulk_rename;
mod bluray_ui;
mod command_palette;
mod compare_mode;
mod conflict_dialog;
mod ctx_menu;
mod menu_model;
mod modules;
mod setup;
mod basket;
mod agent_marks;
mod branch_map;
mod agent_usage;
mod agents;
mod agents_view;
mod session_view;
mod lanes;
mod agent_rules;
mod brain_view;
mod mockup_demo;
mod context_copy;
mod custom_actions;
mod custom_actions_page;
mod cut_marks;
mod disk_usage;
mod selection_stats;
mod numbers_page;
mod share_card;
mod speed_hud;
mod this_pc;
mod usage;
mod xfer_history;
mod diff_view;
mod duplicates_view;
mod changes_view;
mod checkpoint_ui;
mod junk_view;
mod fluent_menu;
mod flyout;
mod glass;
mod glass_watch;
mod liquid;
mod layout_panel;
mod git_ui;
mod prompt;
mod prop_chooser;
mod hash_panel;
mod i18n;
mod folder_history;
mod full_path;
mod folder_view;
mod home;
mod icons;
mod keymap_editor;
mod last_window;
mod logfile;
mod layouts;
pub mod look;
mod host_icons;
mod icon_pack;
mod image_cache;
mod installer;
mod open_rules_page;
mod ops;
mod organizer;
mod nav_touch;
mod namespaces;
mod ui_stall;
mod places;
mod platform;
mod settings;
mod settings_merge;
mod shellless;
mod shell_view;
mod select_box;
mod settings_view;
mod share;
mod share_ui;
mod shelf_view;
mod status_center;
mod sync_ui;
mod mcp_host;
mod capture;
mod space_check;
mod download_schedule;
mod downloads;
mod videodl;
mod usenet;
mod vpn;
mod vpn_page;
mod transfer_view;
mod tags;
mod terminal;
mod titlebar_prefs;
mod undo;
mod preview;
mod preview_trace;
mod program_launch;
mod properties_view;
mod quick_commands;
mod quick_search;
mod quicklook;
mod run_names;
mod settings_rows;
mod hotkey_field;
mod recycle_settings_view;
mod recycle_text;
mod remote;
mod hosts_ui;
mod settings_bundle;
mod recycle_view;
mod registry;
mod rename_exec;
mod rename_presets;
mod review_view;
mod rtl;
mod theme;
mod thumb_cache;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::TitleBar;
use gpui_kit::*;
use rf_core::Location;

use crate::workspace::Workspace;

/// Restores the window on its original display, even when displays use different scales.
fn restored_bounds(cx: &App) -> Option<(WindowBounds, Option<DisplayId>)> {
    let prefs = cx.global::<settings::Settings>();
    let values @ [x, y, w, h, max] = prefs.window?;
    if !values.iter().all(|v| v.is_finite()) || w < 500. || h < 350. {
        return None;
    }
    let mut b = Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
    let display = if let Some((key, [dx, dy])) = &prefs.window_display {
        let saved = cx.displays().into_iter().find(|d| d.uuid().is_ok_and(|id| id.to_string() == *key));
        if let Some(d) = saved.as_ref().cloned().or_else(|| cx.primary_display()) {
            let origin = saved.filter(|_| dx.is_finite() && dy.is_finite())
                .map(|d| d.bounds().origin + point(px(*dx), px(*dy)));
            b = fit_window_bounds(b.size, origin, d.visible_bounds());
            Some(d.id())
        } else {
            None
        }
    } else {
        None
    };
    Some((if max > 0.5 { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) }, display))
}

fn fit_window_bounds(size: Size<Pixels>, origin: Option<Point<Pixels>>, area: Bounds<Pixels>) -> Bounds<Pixels> {
    let size = gpui_kit::size(size.width.min(area.size.width), size.height.min(area.size.height));
    let center = area.center() - point(size.width / 2., size.height / 2.);
    let origin = origin.filter(|&p| area.contains(&Bounds::new(p, size).center())).unwrap_or(center);
    Bounds::new(origin, size)
}

#[cfg(test)]
mod placement_tests {
    use super::fit_window_bounds;
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn restores_negative_coordinates_and_centers_on_the_fallback_screen() {
        let secondary = Bounds::new(point(px(-1920.), px(0.)), size(px(1920.), px(1040.)));
        let original = Bounds::new(point(px(-1750.), px(40.)), size(px(1000.), px(700.)));
        assert_eq!(fit_window_bounds(original.size, Some(original.origin), secondary), original);
        let primary = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(720.)));
        let centered = fit_window_bounds(original.size, None, primary);
        assert_eq!(centered.center(), primary.center());
        assert_eq!(centered.size, original.size);
    }
}

/// Panics are written to %APPDATA%\leaf\crash.log (the app has no console in release).
pub(crate) fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leaf_sys::handover::trace("panic (see crash.log)");
        if let Some(dir) = crate::settings::app_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let bt = std::backtrace::Backtrace::force_capture();
            let line = format!(
                "[{}] {info}\n{bt}\n\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            );
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")) {
                let _ = f.write_all(line.as_bytes());
            }
        }
        prev(info);
    }));
}

/// Whether leaf is registered as the default file manager (HKCU).
pub static DEFAULT_FM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Opens a leaf window. `start = None` restores the session (first window) or opens Home.
pub fn open_main_window(
    start: Option<Location>,
    ipc: Option<futures::channel::mpsc::UnboundedReceiver<String>>,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) {
    open_main_window_made(start, ipc, bounds, cx);
}

/// [`open_main_window`], saying whether the window was made (a second launch waits for that answer: see `leaf_sys::handover`).
pub fn open_main_window_made(
    start: Option<Location>,
    ipc: Option<futures::channel::mpsc::UnboundedReceiver<String>>,
    bounds: Option<WindowBounds>,
    cx: &mut App,
) -> bool {
    let (bounds, display_id) = match bounds {
        Some(bounds) => (bounds, None),
        None => {
            let restored = if workspace::any_main_window(cx) || leaf_sys::window::offscreen() { None } else { restored_bounds(cx) };
            restored.unwrap_or_else(|| {
                // the window manager may add a title bar and borders of its own (KWin on Wayland does): leave room for them, or
                // the bottom of a small screen is cut off
                let mut want = size(px(1280.), px(800.));
                if !cfg!(windows) {
                    // (Wayland does not list the outputs this early: then a height that fits the smallest usual screen, 800 px)
                    match cx.primary_display().or_else(|| cx.displays().into_iter().next()) {
                        Some(d) => {
                            let room = d.visible_bounds().size;
                            want = size(want.width.min(room.width - px(16.)), want.height.min(room.height - px(64.)));
                        }
                        None => want.height = px(720.),
                    }
                }
                (WindowBounds::centered(want, cx), None)
            })
        }
    };
    let options = WindowOptions {
        window_bounds: Some(bounds),
        display_id,
        window_min_size: Some(size(px(720.), px(420.))),
        focus: !leaf_sys::window::offscreen(),
        window_background: crate::theme::window_material(WindowBackgroundAppearance::MicaBackdrop),
        // Wayland app id / X11 WM_CLASS: the desktop file leaf.desktop gives the window its icon and name
        app_id: (!cfg!(windows)).then(|| "leaf".to_string()),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, move |window, cx| cx.new(|cx| Workspace::new(start, ipc, window, cx)));
    leaf_sys::window::park_offscreen();
    if let Err(e) = opened {
        log::error!("open window: {e:#}");
        leaf_sys::handover::trace("open window: failed");
        return false;
    }
    true
}

/// Quits now, or once running copy/move/delete operations finish (closing the window must not
/// cut a copy in half). A window opened meanwhile (second launch) cancels the quit.
pub(crate) fn quit_when_idle(cx: &mut App) {
    // (an app or file the quick search is still starting counts as running work)
    let running = |cx: &App| cx.try_global::<status_center::OpsGlobal>().map_or(0, |g| g.0.read(cx).running()) + quick_search::launching();
    if running(cx) == 0 {
        save_and_quit(cx);
        return;
    }
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
        let done = cx.update(|cx| {
            if workspace::any_main_window(cx) {
                return Some(false);
            }
            (running(cx) == 0).then_some(true)
        });
        match done {
            Some(true) => {
                cx.update(save_and_quit);
                break;
            }
            Some(false) => break,
            None => {}
        }
    })
    .detach();
}

/// The session/placement save on window close runs on a helper thread; write once more here,
/// synchronously, so quitting cannot cut it off (a small file, only at exit).
fn save_and_quit(cx: &mut App) {
    // The quit and a second launch's request meet here (GitHub issue 15): a request that was taken and is not acted on yet stops the
    // quit (it opens a window, and that keeps the process); once the quit goes ahead, requests are refused with `leaving` and the
    // second launch shows its folder itself. Without this a folder opened while the app was quitting was forwarded to a process
    // that closed the window it opened, or that never looked at it.
    if !leaf_sys::handover::GATE.try_leave() {
        leaf_sys::handover::trace("quit: held back, a request is in flight");
        return;
    }
    leaf_sys::handover::trace("quit: leaving, requests are refused from now on");
    cx.global::<settings::Settings>().clone().save();
    usage::flush();
    // the search index keeps its place in the change journal (or its changes): the next start
    // then catches up instead of walking the drives; bounded, quitting must not hang on it
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        leaf_sys::indexer::stop();
        let _ = tx.send(());
    });
    // (kept on the UI thread and before the process ends on purpose: the index must be at rest when the next process opens it, and
    // a launch meanwhile is told `leaving` and waits for the mutex; the trace gives the real time of this wait)
    let stopped = rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok();
    leaf_sys::handover::trace(&format!("quit: index stopped {}, ending", if stopped { "in time" } else { "late (2 s limit)" }));
    cx.quit();
}

/// The variable a restarted leaf is started with: the process id of the one that is ending (see [`restart`]).
const AFTER_EXIT_ENV: &str = "LEAF_AFTER_EXIT";

/// Restarts leaf: leaf-look is asked to stop, a new leaf is started that waits for this one to end, and
/// this one quits the usual way (settings and session written). The new one opens its last session and brings
/// leaf-look back as the settings say. WHY (the owner, 2026-11-17, after a morning where the dock came up empty: "bir de bir
/// kısayol ekleyelim. leaf'ı restart etsin"): there was no way to start everything afresh short of ending the processes by
/// hand. Ctrl+Alt+Shift+R and the command palette.
pub(crate) fn restart(cx: &mut App) {
    let Ok(me) = std::env::current_exe() else { return };
    let pid = std::process::id().to_string();
    std::thread::spawn(move || {
        leaf_sys::single_instance::send_to(&look::ipc::app_name(), &look::ipc::Msg::Quit.encode());
        // (no folder: the new process opens the last session; its working directory is never the installation folder)
        let dir = std::env::temp_dir();
        if let Err(e) = std::process::Command::new(&me).env(AFTER_EXIT_ENV, &pid).current_dir(dir).spawn() {
            log::warn!("restart: the new leaf did not start: {e}");
        }
    });
    // (give the stops their moment, then quit; the new process waits for this one to be gone)
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
        cx.update(|cx| {
            leaf_sys::handover::GATE.leave_now();
            workspace::save_all_sessions(cx);
            save_and_quit(cx)
        });
    })
    .detach();
}

/// A restarted leaf: waits (up to ten seconds) for the one that started it to end, so that it does not hand its start to
/// that one as a second launch does.
fn wait_for_the_old_one() {
    let Some(pid) = std::env::var(AFTER_EXIT_ENV).ok().and_then(|p| p.trim().parse::<u32>().ok()) else { return };
    // (not inherited by what this process starts)
    unsafe { std::env::remove_var(AFTER_EXIT_ENV) };
    leaf_sys::process::wait_pid(pid, 10_000);
}

/// `Path::is_dir` with a time limit (it can block for the SMB timeout on an offline share).
pub(crate) fn is_dir_bounded(p: PathBuf, limit: std::time::Duration) -> Option<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(p.is_dir());
    });
    rx.recv_timeout(limit).ok()
}

/// A shell namespace argument resolved to a folder we list ourselves that is not on the file
/// system (see `leaf_sys::shellfolder::browsable`), within `limit`.
fn shell_place_bounded(arg: &str, limit: std::time::Duration) -> Option<Location> {
    match namespaces::resolve(arg, limit) {
        namespaces::Place::Here(loc) => Some(loc),
        namespaces::Place::Explorer => None,
    }
}

/// leaf'ın girişi.
/// Start-up timing (`LEAF_TRACE_START=1`, logged at info): milliseconds since the process
/// was created, at the steps before the first window.
pub(crate) fn start_mark(step: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("LEAF_TRACE_START").is_some()) {
        eprintln!("start: {step} at {} ms", leaf_sys::process_age_ms());
    }
}

pub fn main() {
    start_mark("main");
    wait_for_the_old_one();
    // Not in the installation folder: the updater renames it, and Windows refuses while it is a working directory of this process
    // or of anything this process starts (`leaf_sys::ownfolder`). Before everything else, so that every child inherits the way out.
    leaf_sys::ownfolder::leave();
    // the side processes and previews of the cut taskbar/desktop/snip/download features: started by an old shortcut, autostart entry
    // or browser host, they end here without doing anything
    if std::env::args().nth(1).as_deref().is_some_and(retired_arg) {
        return;
    }
    // `leaf nmhost --register` / `--unregister`: the browser hand-over host (see capture), then out
    if std::env::args().nth(1).as_deref() == Some("nmhost")
        && let Some(a) = std::env::args().nth(2).filter(|a| matches!(a.as_str(), "--register" | "--unregister"))
    {
        std::process::exit(capture::cli(&a));
    }
    // `leaf default-fm status|on|off`: the default file manager switch of Settings, then out
    if std::env::args().nth(1).as_deref() == Some("default-fm") {
        std::process::exit(settings_view::default_fm_cli(std::env::args().nth(2).as_deref()));
    }
    // Velopack (installer / updater): first, it may run an install hook and exit
    installer::startup();
    // the id Velopack gives the Start menu / taskbar shortcuts: a pin survives updates. Only the installed copy takes it: a copy
    // started from a build or a plain folder is pinned by a shortcut without the id, and with the id its window would sit beside
    // that pin as a second taskbar button instead of joining it
    if installer::is_installed() {
        leaf_sys::window::set_app_id(installer::APP_ID);
    }
    install_panic_log();
    leaf_sys::shell::suppress_critical_error_dialogs();
    // before any shell icon/thumbnail work: see keep_gdiplus_alive (loader-lock deadlock)
    leaf_sys::thumbs::keep_gdiplus_alive();
    // Logs go to stderr and to leaf.log in the settings folder; override the level with
    // RUST_LOG. The default filter: `platform::LOG_FILTER`.
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(platform::LOG_FILTER));
    let logger = logger.target(env_logger::Target::Pipe(Box::new(logfile::Tee::new()))).format_timestamp_millis().build();
    // crates that write a password at their most detailed level (the FTP client's PASS line, at trace): their records above
    // warn are dropped before the filter, so no RUST_LOG (not even one naming their modules) brings them back
    struct Quiet(env_logger::Logger);
    impl log::Log for Quiet {
        fn enabled(&self, m: &log::Metadata) -> bool {
            !leaf_remote::is_quiet(m.target(), m.level()) && self.0.enabled(m)
        }
        fn log(&self, r: &log::Record) {
            if !leaf_remote::is_quiet(r.target(), r.level()) {
                self.0.log(r);
            }
        }
        fn flush(&self) {
            self.0.flush();
        }
    }
    let max = logger.filter();
    if log::set_logger(Box::leak(Box::new(Quiet(logger)))).is_ok() {
        log::set_max_level(max);
    }
    start_mark("logging");
    // sftp://, ftp://, ftps:// (cheap: nothing connects until a server is opened; before the command line, which may name one)
    remote::register();
    // a bulk rename that did not end (crash, power loss) left its journal: logged and kept, the files are not touched
    if let Some(dir) = rename_exec::journal_dir() {
        std::thread::spawn(move || rename_exec::report_journals(&dir));
    }

    // As default file manager we also receive shell namespaces (Control Panel = "::{…}") and
    // non-folders: hand those back to Explorer so nothing becomes unreachable.
    // The folder check is bounded: an offline network path must not keep the window from
    // appearing for the whole SMB timeout. Undecided after 2 s = treat as a (slow) folder.
    // `--settings=look`: leaf-look'un simge menüsünden "leaf-look ayarları"
    // `--settings=speed`: straight to Settings → Speed test
    let cli = cli::parse(std::env::args());
    let settings_arg = cli.args.iter().cloned().nth(1).and_then(|a| a.strip_prefix("--settings=").map(str::to_string));
    let open_look_settings = settings_arg.is_some();
    // `--quick-search`: the quick search window, as its hotkey opens it (a shortcut of the
    // user's own can run this)
    let quick_search = cli.args.iter().cloned().nth(1).as_deref() == Some(quick_search::LAUNCH_ARG);
    let arg = cli.args.iter().cloned().nth(1).map(|a| a.trim().trim_matches('"').to_string()).filter(|a| !a.is_empty());
    let arg = arg.filter(|_| !open_look_settings && !quick_search);
    // jump list task "New window"
    let new_window = arg.as_deref() == Some(leaf_sys::jumplist::NEW_WINDOW_ARG);
    let arg = arg.filter(|_| !new_window);
    // several folders on the command line (issue 125): a first one that is gone (moved, renamed) is passed over, not handed to Explorer
    // with its error, and the first one that is there opens instead; `first_taken` is how many arguments that used up
    let mut first_taken = 1;
    let arg = match arg {
        Some(a) if cli.args.iter().cloned().nth(2).is_some() && !a.starts_with("::") && !a.to_ascii_lowercase().starts_with("shell:") && !a.starts_with("/") && !a.starts_with('-') && !std::path::Path::new(&a).exists() => {
            let later = cli.args.iter().cloned()
                .enumerate()
                .skip(2)
                .map(|(i, x)| (i, x.trim().trim_matches('"').to_string()))
                .find(|(_, x)| !x.is_empty() && !x.starts_with("--") && is_dir_bounded(PathBuf::from(x), std::time::Duration::from_secs(2)) == Some(true));
            match later {
                Some((i, x)) => {
                    first_taken = i;
                    Some(x)
                }
                None => Some(a),
            }
        }
        other => other,
    };
    let mut start: Option<Location> = None;
    // Explorer's command line, read as Explorer reads it (rf_core::explorer_args): `/select,<file>` (also what `leaf reveal` becomes)
    // opens its folder with the file selected; on Windows also `/e,`, `/n,`, `/root,<folder>` from programs that start Explorer, and
    // what leaf does not read goes to Explorer as it came. (Callers also write `/select, <file>`: the path then comes as the next
    // argument, which the joined line gives back.)
    let all_args = cli.args.iter().cloned().skip(1).collect::<Vec<_>>().join(" ");
    let explorer_launch = {
        use rf_core::explorer_args as ea;
        let lower = all_args.trim_start().trim_start_matches('"').to_ascii_lowercase();
        // on Linux `/e` or `/root` are folders: only the select line (`leaf reveal`) is Explorer's there
        (ea::starts_with_switch(&all_args) && (cfg!(windows) || lower.starts_with("/select,") || lower.starts_with("-select,"))).then(|| ea::parse(&all_args))
    };
    // a relative path is the caller's: made absolute here, since the window that takes it may run in another folder
    let absolute = |p: &str| if rf_core::explorer_args::is_namespace(p) { p.to_string() } else { std::path::absolute(p).map_or_else(|_| p.to_string(), |a| a.to_string_lossy().into_owned()) };
    if explorer_launch == Some(rf_core::explorer_args::Launch::FallThrough) {
        leaf_sys::handover::trace("main: an Explorer command line leaf does not read: to Explorer as it came, this process ends");
        if !leaf_sys::default_fm::forwarded_recently(&all_args) {
            leaf_sys::default_fm::note_forward(&all_args);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("explorer.exe").raw_arg(&all_args).spawn();
            }
        }
        return;
    }
    let mut select = match &explorer_launch {
        Some(rf_core::explorer_args::Launch::Select { file, .. }) => Some(absolute(file))
            // bounded like the folder check below: an offline share must not hold the window
            .filter(|file| {
                PathBuf::from(file)
                    .parent()
                    .is_some_and(|d| is_dir_bounded(d.to_path_buf(), std::time::Duration::from_secs(2)) != Some(false))
            }),
        _ => None,
    };
    let arg = match &explorer_launch {
        None => arg,
        Some(_) if select.is_some() => None,
        // a folder that is not there (or a file Explorer would open): the checks below hand it to Explorer
        Some(rf_core::explorer_args::Launch::Select { folder, .. }) => Some(absolute(folder)),
        Some(rf_core::explorer_args::Launch::Open(t) | rf_core::explorer_args::Launch::Namespace(t)) => Some(absolute(t)),
        // Win+E, `/e,` alone: leaf's usual start
        Some(_) => None,
    };
    // `leaf download <address> [folder]`: the window opens in that folder (when it is there), the download is started once it is up
    let download = cli.download.clone();
    if let Some((_, Some(folder))) = &download
        && is_dir_bounded(PathBuf::from(folder), std::time::Duration::from_secs(2)) == Some(true)
    {
        start = Some(Location::Path(PathBuf::from(folder)));
    }
    if let Some(file) = &select {
        let p = PathBuf::from(file);
        start = p.parent().map(|d| Location::Path(d.to_path_buf()));
        *workspace::START_SELECT.lock().unwrap() = p.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    // desktop icons (This PC, Recycle Bin) arrive as shell namespaces: those we show ourselves
    if let Some(loc) = arg.as_deref().and_then(Location::from_shell_name) {
        start = Some(loc);
    } else if let Some(arg) = arg {
        let shell_namespace = arg.starts_with("::") || arg.to_ascii_lowercase().starts_with("shell:");
        // a namespace folder we can list ourselves (a phone, Network, a library…) opens here;
        // Control Panel and other applets (and anything that does not enumerate) stay with Explorer.
        // Bounded like the folder check: a device that hangs must not keep the window away.
        let shell_place = if shell_namespace { shell_place_bounded(&arg, std::time::Duration::from_secs(3)) } else { None };
        if let Some(place) = shell_place {
            start = Some(place);
        } else {
            // a network server root (\\nas) isn't a "directory" to Rust, but we browse its shares
            let server = leaf_sys::network::server_root(std::path::Path::new(&arg)).is_some();
            let is_dir = if shell_namespace {
                Some(false)
            } else if server {
                Some(true)
            } else {
                is_dir_bounded(PathBuf::from(&arg), std::time::Duration::from_secs(2))
            };
            // a browsable archive (leaf-look's "leaf'ta aç") opens like a folder, here
            let archive = !shell_namespace && archive_view::is_browsable(std::path::Path::new(&arg));
            if is_dir == Some(false) && !archive {
                // Explorer opens folders through the verb we took over: what we just handed it may
                // come straight back. Never ping-pong; a path is then shown here (with its error).
                leaf_sys::handover::trace("main: not a folder for us (or a namespace we do not list): to Explorer unless it just came from there");
                if !leaf_sys::default_fm::forwarded_recently(&arg) {
                    leaf_sys::default_fm::note_forward(&arg);
                    leaf_sys::handover::trace("main: handed to Explorer, this process ends");
                    // a namespace or a folder goes through Explorer's own handler (HKLM), which our HKCU verb cannot send back; a file
                    // to explorer.exe, which opens it with its program
                    let own = !std::path::Path::new(&arg).is_file() && leaf_sys::default_fm::open_in_explorer(&arg);
                    if !own {
                        let _ = std::process::Command::new("explorer.exe").arg(&arg).spawn();
                    }
                    return;
                }
                if shell_namespace {
                    return;
                }
                // a file that came back (an archive type Windows browses but we don't, e.g. .cpio):
                // its folder, with the file selected
                let p = PathBuf::from(&arg);
                if let (Some(dir), Some(name)) = (p.parent().filter(|_| p.is_file()), p.file_name()) {
                    start = Some(Location::Path(dir.to_path_buf()));
                    *workspace::START_SELECT.lock().unwrap() = Some(name.to_string_lossy().into_owned());
                    select = Some(arg.clone());
                }
            }
            if start.is_none() {
                start = Some(Location::Path(PathBuf::from(arg)));
            }
        }
    }
    // Cache the registry state for the settings page (read once, off the hot path).
    let registers = std::env::var_os("LEAF_CONFIG_DIR").is_none();
    std::thread::spawn(move || {
        // copies opened/copied out of archives days ago
        crate::archive_view::clean_temp();
        // default file manager whose registered exe vanished (moved / rebuilt elsewhere):
        // re-point it here, or Windows can't open any folder. Not from test instances.
        if registers {
            if let Ok(me) = std::env::current_exe() {
                if leaf_sys::default_fm::heal(&me.display().to_string(), leaf_sys::default_fm::CLASSES) {
                    log::warn!("default file manager re-registered to {}", me.display());
                }
                if leaf_sys::default_fm::add_archives(&me.display().to_string(), leaf_sys::default_fm::CLASSES) {
                    log::warn!("default file manager: archive registration updated");
                }
            }
        }
        // the browsers' host registration follows a moved or updated leaf
        if registers {
            crate::capture::refresh();
        }
        DEFAULT_FM.store(
            leaf_sys::default_fm::is_enabled(leaf_sys::default_fm::CLASSES),
            std::sync::atomic::Ordering::Relaxed,
        )
    });

    // more folders on the command line (`leaf.exe "C:\My Pictures" "D:\My Videos"`): each one a tab of the same window, after the
    // first (issue 125). Only plain folders that are there (one that is gone is passed over without a word), after a first argument that
    // was one too.
    let more_folders: Vec<String> = if matches!(start, Some(Location::Path(_))) && select.is_none() && explorer_launch.is_none() {
        cli.args
            .iter()
            .cloned()
            .skip(first_taken + 1)
            .map(|a| a.trim().trim_matches('"').to_string())
            .filter(|a| !a.is_empty() && !a.starts_with("--") && is_dir_bounded(PathBuf::from(a), std::time::Duration::from_secs(2)) == Some(true))
            .collect()
    } else {
        Vec::new()
    };
    // "Show in folder" from other apps (SHOpenFolderAndSelectItems: Telegram, browsers) while we
    // are the default file manager: Windows opens the folder through our verb and names the
    // file afterwards, to a window it finds in its window list (see leaf_sys::shell_window). The
    // folder opens at once; the file, if one comes, is selected in it.
    const SHELL_SELECT_WAIT: std::time::Duration = std::time::Duration::from_secs(4);
    let shell_select = match &start {
        Some(Location::Path(dir)) if select.is_none() && std::env::var_os("LEAF_MULTI_INSTANCE").is_none() && std::env::var_os("LEAF_INSTANCE").is_none() && leaf_sys::default_fm::is_enabled(leaf_sys::default_fm::CLASSES) => {
            let (tx, rx) = std::sync::mpsc::channel();
            let dir = dir.clone();
            std::thread::spawn(move || {
                let _ = tx.send(leaf_sys::shell_window::wait_for_select(&dir, SHELL_SELECT_WAIT));
            });
            Some(rx)
        }
        _ => None,
    };
    let shell_selected = move |rx: std::sync::mpsc::Receiver<Option<PathBuf>>| {
        rx.recv_timeout(SHELL_SELECT_WAIT + std::time::Duration::from_secs(1)).ok().flatten().map(|f| format!("{}{}", workspace::SELECT_HERE_PREFIX, f.display()))
    };

    // Single instance: a second launch opens its folder as a tab in the running window.
    let message = match &start {
        _ if download.is_some() => {
            let (url, folder) = download.clone().unwrap_or_default();
            downloads::message(&url, folder.as_deref())
        }
        _ if open_look_settings => look::ipc::ToApp::OpenSettings.encode(),
        _ if new_window => leaf_sys::jumplist::NEW_WINDOW_ARG.to_string(),
        _ if quick_search => quick_search::LAUNCH_ARG.to_string(),
        _ if select.is_some() => format!("{}{}", workspace::SELECT_PREFIX, select.as_deref().unwrap_or_default()),
        Some(Location::Path(p)) => p.to_string_lossy().into_owned(),
        Some(Location::RecycleBin) => "shell:RecycleBinFolder".to_string(),
        Some(Location::Shell { parsing, .. }) => format!("{}{parsing}", workspace::SHELL_PREFIX),
        // a search link goes on to the running window as a search link
        Some(Location::Search { root, query }) => {
            let enc = |s: &str| s.replace('%', "%25").replace('&', "%26");
            format!("search-ms:query={}&crumb=location:{}", enc(query), enc(&root.to_string_lossy()))
        }
        _ => String::new(),
    };
    // `--new-tab`: a tab of the running window, whatever the setting for folders from other programs says
    let message = if cli.new_tab && !message.is_empty() && !new_window && !quick_search && !open_look_settings { format!("{}{message}", cli::NEW_TAB_PREFIX) } else { message };
    // LEAF_MULTI_INSTANCE=1 skips forwarding (automated tests run several copies).
    let multi = std::env::var_os("LEAF_MULTI_INSTANCE").is_some();
    leaf_sys::handover::trace(&format!("main: launch, request of {} bytes, multi {multi}", message.len()));
    let _instance = if multi {
        None
    } else {
        // (the hand-over waits for the running instance's answer, and shows the folder here when that one is leaving: see leaf_sys::handover)
        match leaf_sys::handover::acquire(&message) {
            leaf_sys::single_instance::Role::Forwarded => {
                // the folder is on its way to the running window; the file to select follows
                if let Some(msg) = shell_select.and_then(shell_selected) {
                    leaf_sys::handover::send_followup(&msg);
                }
                for folder in &more_folders {
                    leaf_sys::handover::send_followup(folder);
                }
                return;
            }
            leaf_sys::single_instance::Role::Primary(guard) => Some(guard),
        }
    };
    let primary_instance = _instance.is_some() && !quick_search;
    let (ipc_tx, ipc_rx) = futures::channel::mpsc::unbounded::<String>();
    // second launches: each comes with the acknowledgement it waits for (the other senders of ipc_tx need none)
    let (launch_tx, launch_rx) = futures::channel::mpsc::unbounded::<(String, leaf_sys::handover::Ack)>();
    // (this process is the first: its own extra folders open as tabs once the window is there, like second launches do)
    for folder in more_folders {
        let _ = ipc_tx.unbounded_send(folder);
    }
    if let Some(rx) = shell_select {
        let tx = ipc_tx.clone();
        std::thread::spawn(move || {
            if let Some(msg) = shell_selected(rx) {
                let _ = tx.unbounded_send(msg);
            }
        });
    }
    if !multi {
        leaf_sys::handover::serve(move |m, ack| {
            let _ = launch_tx.unbounded_send((m, ack));
        });
    }

    // a drag of files that live inside the source (a mail's attachment, a browser's image) is dropped as files (see leaf_sys::vfiles)
    crate::platform::gpui::dnd_hint::set_virtual_reader(leaf_sys::vfiles::drag_paths);
    // (the first window on the CPU renderer or on the card: what this machine was like the last time, see `fast_start`)
    fast_start::before_app(!quick_search && !open_look_settings);
    start_mark("before the app");
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        start_mark("app running");
        gpui_kit::init(cx);
        ui_stall::start(cx);
        cx.set_global(settings::Settings::load());
        start_mark("settings loaded");
        rf_core::sort::set_turkish_collation(leaf_sys::encoding::user_locale_is_turkish());
        // eski kod sayfalı metinlerin tahmini: Türkçe olmayan sistemde Türkçe ipucu verilmez
        if !leaf_sys::encoding::user_locale_is_turkish() {
            rf_core::textload::set_locale_hint(None);
        }
        settings::apply_component_theme(cx);
        cx.set_global(tags::load());
        let ops = cx.new(status_center::Ops::new);
        // the Status Center flyout is its own Acrylic window, opened/closed with `open`
        cx.observe(&ops, |ops, cx| {
            status_center::sync_popup(&ops, cx);
            status_center::sync_taskbar(&ops, cx);
        })
        .detach();
        cx.set_global(status_center::OpsGlobal(ops));
        // the questions of server connections (host key, passphrase, password) become dialogs
        remote::init_ui(cx);
        actions::bind(cx);
        actions::bind_user(cx);
        features_page::init(cx);
        setup::init(cx);
        custom_actions::init(cx);
        // leaf's VPN: the kill switch, the state the windows show, "Connect when leaf starts"
        vpn::init(cx);
        home::init(cx);
        keymap_editor::init(cx);
        if let Some(page) = &settings_arg {
            settings_view::open_page(
                match page.as_str() {
                    "speed" => settings_view::Page::Speed,
                    "general" => settings_view::Page::General,
                    "appearance" => settings_view::Page::Appearance,
                    "filetypes" => settings_view::Page::FileTypes,
                    "hypersearch" => settings_view::Page::Search,
                    "numbers" => settings_view::Page::Numbers,
                    "features" => settings_view::Page::Features,
                    "modules" => settings_view::Page::Modules,
                    "whatsnew" => settings_view::Page::WhatsNew,
                    "advanced" => settings_view::Page::Advanced,
                    _ => settings_view::Page::Look,
                },
                cx,
            );
        }
        // copies and moves keep a crash-safe journal there (see leaf_sys::fastcopy)
        if let Some(d) = settings::app_dir() {
            leaf_sys::fastcopy::set_journal_dir(d.join("jobs"));
        }
        // folder sizes survive a restart (shown at once, measured again when asked for)
        if let Some(d) = settings::local_data_dir() {
            leaf_sys::foldersize::shared_cache().persist(d.join("foldersizes.bin"));
        }
        if quick_search {
            // started for the quick search only: no main window (it quits when that closes,
            // unless running in the background)
            quick_search::toggle(cx);
        } else {
            start_mark("opening the window");
            // the first start (no settings file): the setup guide, in Settings
            let first_run = !cx.global::<settings::Settings>().setup_done && std::env::var_os("LEAF_MULTI_INSTANCE").is_none();
            if first_run {
                setup::begin(cx);
            }
            open_main_window(if open_look_settings || first_run { Some(Location::Settings) } else { start }, None, None, cx);
            start_mark("window opened");
            if let Some((url, folder)) = download {
                downloads::start_cli(url, folder, cx);
            }
            fast_start::remember(cx);
            // downloads that were not finished last time: back in the Status Center, resuming from their part files
            // (a module that is off starts nothing: no download is taken up, no timer runs, no Usenet job is looked at)
            if modules::enabled(cx, modules::Module::Downloads) {
                downloads::restore(cx);
            }
            download_schedule::start(cx);
            downloads::init_ui(cx);
            if modules::enabled(cx, modules::Module::Usenet) {
                usenet::init(cx);
            }
            // a copy or move cut off last time: ask to carry it on
            status_center::offer_resume(cx);
        }
        // sync jobs with a schedule run while leaf runs, also when it was started for the quick search and stays in the background;
        // a test copy (LEAF_MULTI_INSTANCE) never runs the user's jobs
        if std::env::var_os("LEAF_MULTI_INSTANCE").is_none() {
            sync_ui::start_scheduler(cx);
        }
        agents::init(cx);
        lanes::init(cx);
        // leaf-look ayrı süreç: önceden başlatılır ki ilk Boşluk beklemesin (testlerde değil)
        if cx.global::<settings::Settings>().look_enabled && std::env::var_os("LEAF_MULTI_INSTANCE").is_none() {
            look::ipc::send(look::ipc::Msg::Wake, true);
        }
        // "Safely remove" / eject: let go of a drive when Windows asks (see leaf_sys::removal)
        {
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<(char, std::sync::mpsc::Sender<()>)>();
            leaf_sys::removal::start(move |letter, ack| {
                let _ = tx.unbounded_send((letter, ack));
            });
            cx.spawn(async move |cx| {
                use futures::StreamExt;
                while let Some((letter, ack)) = rx.next().await {
                    cx.update(|cx| workspace::release_drive(letter, cx));
                    let _ = ack.send(());
                }
            })
            .detach();
        }
        // Second launches arrive here and go to whichever window is still open.
        let mut ipc_rx = ipc_rx;
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(msg) = ipc_rx.next().await {
                cx.update(|cx| workspace::route_ipc(msg, cx));
            }
        })
        .detach();
        // The launches of second processes, which wait for this answer: told once the tab or window is there. A quit that was held
        // back for the sake of a request (see save_and_quit) is looked at again when the last one is acted on, and goes ahead if the
        // request left nothing open.
        let mut launch_rx = launch_rx;
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some((msg, ack)) = launch_rx.next().await {
                // leaf-mcp's requests have answers of their own (see mcp_host)
                if let Some(req) = msg.strip_prefix(mcp_host::PREFIX) {
                    let req = req.to_string();
                    cx.update(|cx| mcp_host::handle(req, ack, cx));
                    continue;
                }
                // a download the browser started, from leaf-nmhost (see capture); its text may hold the browser's cookies: not traced
                if let Some(req) = msg.strip_prefix(capture::PREFIX) {
                    let req = req.to_string();
                    drop(msg);
                    cx.update(|cx| capture::handle(req, ack, cx));
                    continue;
                }
                leaf_sys::handover::trace("first: acting on a request");
                let shown = cx.update(|cx| workspace::route_ipc_shown(msg, cx));
                // (no window could be made: the second launch is told, and shows the folder itself)
                let quit_held_back = if shown { ack.done() } else { ack.refuse() };
                if quit_held_back {
                    cx.update(|cx| last_window::settle(cx));
                }
            }
        })
        .detach();
        // The last window closing ends the app (GPUI does not on its own: the process would
        // linger invisibly and swallow every later launch through the single-instance pipe).
        // With "run in the background" on, the app stays in the notification area instead.
        background::init(cx);
        quick_search::init(cx);
        // the folder organizer's watchers (only if a rule runs by itself); not in helper processes
        if primary_instance {
            organizer::init(cx);
            // Nearby (off until the user turns it on): only the main process listens
            nearby::init(cx);
            // shared folders with a friend's leaf: only the main process listens, and only while a folder is shared or joined
            share_ui::init(cx);
        }
        // "Your numbers": one launch, and the time a window is in front (not in the helper process of quick search)
        if !quick_search {
            usage::bump(usage::Counter::Launches, 1);
            usage::start_focus_clock(cx);
        }
        // GPUI would quit by itself when the last window closes (QuitMode::Default on Windows):
        // that cut running copies in half and makes background mode impossible. We decide.
        cx.set_quit_mode(gpui_kit::QuitMode::Explicit);
        cx.on_window_closed(move |cx, id| {
            setup::revert_preview(cx);
            workspace::save_closed_placement(id, cx);
            last_window::settle(cx);
        })
        .detach();
        cx.activate(true);
    });
    leaf_sys::handover::trace("main: event loop ended");
}

/// The first arguments of the cut side processes and previews (dock / shell host, snip, taskbar / desktop / switcher previews, the
/// dock's windowless search helper, HyperDownload's window and browser hand-over, the update splash, the Open/Save dialog picker).
fn retired_arg(a: &str) -> bool {
    matches!(a, "--pick" | "--dock" | "--shell" | "--shell-probe" | "--snip" | "--bar-preview" | "--desktop-preview" | "--switcher-preview" | "--no-window" | "--downloads" | "--update-splash")
        || a.starts_with("--hd-follow=")
}
