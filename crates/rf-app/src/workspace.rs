//! Window chrome: title bar with tabs, toolbar (nav + breadcrumb/address + search),
//! sidebar, active tab content, status bar.

use crate::t;
use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::{Location, format};

use crate::actions::*;
use crate::folder_view::{FolderEvent, FolderView, ViewMode};
use crate::image_cache::ImageCache;
use crate::preview::Preview;
use crate::places::{self, Places};
use crate::theme::{self, Palette};

// the sidebar's folder tree (setting `sidebar_tree`): the view and the window's hooks for it
#[path = "sidebar_tree.rs"]
mod sidebar_tree;
// the preview pane under the file list (setting `preview_position` = bottom)
#[path = "preview_dock.rs"]
mod preview_dock;
// commands typed into the address field (setting `address_commands`): what the Enter handler does with text that is not a place
#[path = "address_cmd.rs"]
mod address_cmd;
// the scripted test driver (`LEAF_CHECK_SCRIPT`, tools/TESTING.md)
#[path = "check_script.rs"]
mod check_script;
#[path = "check_demo.rs"]
mod check_demo;

/// Payload while a pinned sidebar entry is dragged (reordering).
#[derive(Clone)]
struct PinDrag {
    ix: usize,
    /// Identity of the dragged pin (indices of the drawn list can be stale).
    path: PathBuf,
    title: SharedString,
}

impl Render for PinDrag {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        div()
            .px_3()
            .py_1p5()
            .rounded(theme::RADIUS)
            .bg(p.card_bg)
            .border_1()
            .border_color(p.accent)
            .text_sm()
            .text_color(p.text)
            .shadow_md()
            .child(self.title.clone())
    }
}

/// Payload while a tab is dragged (reordering).
#[derive(Clone)]
struct TabDrag {
    ix: usize,
    title: SharedString,
}

impl Render for TabDrag {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        div()
            .px_3()
            .py_1p5()
            .rounded(theme::RADIUS)
            .bg(p.tab_active)
            .border_1()
            .border_color(p.accent)
            .text_sm()
            .text_color(p.text)
            .shadow_md()
            .child(self.title.clone())
    }
}
#[derive(Clone)]
/// Window placement to go back to when leaving the compact overlay.
struct CompactRestore {
    rect: [i32; 4],
    maximized: bool,
    topmost: bool,
}

/// Compact overlay size and minimum (logical px).
const COMPACT_W: f32 = 420.;
const COMPACT_H: f32 = 560.;
const COMPACT_MIN: (u16, u16) = (340, 300);

/// A tab holds one folder view, or two side by side (dual pane).
#[derive(Clone)]
struct Tab {
    left: Entity<FolderView>,
    right: Option<Entity<FolderView>>,
    right_active: bool,
}

impl Tab {
    /// The pane that has focus (toolbar, sidebar and status bar act on it).
    fn view(&self) -> &Entity<FolderView> {
        match (&self.right, self.right_active) {
            (Some(r), true) => r,
            _ => &self.left,
        }
    }
}

mod lanes;
mod worktrees;

pub struct Workspace {
    tabs: Vec<Tab>,
    /// Compact overlay (small, always-on-top window): the placement to restore on exit.
    compact: Option<CompactRestore>,
    active: usize,
    closed: Vec<Location>,
    places: Entity<Places>,
    images: Entity<ImageCache>,
    ops: Entity<crate::status_center::Ops>,
    preview: Entity<Preview>,
    preview_open: bool,
    /// Space: big preview of the focused item over the file list.
    quick_look: Option<Entity<Preview>>,
    shelf: Entity<crate::shelf_view::ShelfView>,
    shelf_open: bool,
    address: Entity<InputState>,
    editing_address: bool,
    /// The Omnibar is in search mode (Ctrl+F / its search button) instead of showing the path.
    search_mode: bool,
    /// (dark, system appearance) last pushed to the window frame (see render).
    frame_dark: Option<(bool, WindowAppearance)>,
    /// Background material last applied to the window.
    applied_backdrop: Option<crate::settings::BackdropPref>,
    /// The backdrop setting last seen: a change of it lets Glass try again after a failure.
    backdrop_pref_seen: Option<crate::settings::BackdropPref>,
    /// This window's Glass: the HWND the engine was given (0: none), the tint it was given, and whether it failed (the window is on Mica).
    glass_hwnd: isize,
    glass_dark: Option<bool>,
    glass_failed: bool,
    /// The newest asked-for measure of what is behind the sheets (a move or resize asks many times; only the last one runs).
    behind_gen: u64,
    /// The engine has drawn this window's glass and the window is see-through now. Until then it keeps its Mica: a transparent window
    /// with nothing behind it shows the desktop through the palette's translucent layers.
    glass_shown: bool,
    /// How the engine last said it keeps the windows out of the capture: the settings page's note depends on it, and the engine can
    /// change it later (a session that fails makes it fall back).
    glass_capture: leaf_sys::glass::CaptureMode,
    /// Counts the attach attempts; a watcher of an older attempt stops.
    glass_gen: u32,
    /// Set by `render` around the call that builds the sidebar: the selected row of that sidebar is a glass lens (the engine draws the
    /// pill behind the scene) instead of the Fluent fill. False for the rail and for the rail's peek over the content, which is opaque.
    sidebar_lens: bool,
    /// The selected row of this frame's sidebar has claimed the lens (only the first of several selected rows does). A frame that ends
    /// without a claim takes the lens out.
    sidebar_lens_claimed: std::cell::Cell<bool>,
    /// Set just before `sidebar_item` for a saved host: the device icon to draw instead of the plain row icon (taken by that call).
    sidebar_host_kind: std::cell::Cell<Option<rf_core::hosts::HostKind>>,
    /// Set by `render` around the call that builds the tab strip: the selected tab and the "+" button are glass lenses (the engine draws
    /// them behind the scene) instead of the Fluent fill.
    tab_lens: bool,
    /// This frame's strip gave the selected tab its lens / the "+" button its lens. A frame that ends without a claim takes the lens out.
    tab_lens_claimed: bool,
    plus_lens_claimed: bool,
    /// Address-bar folder completions for the typed text, and the highlighted one.
    addr_suggest: Vec<PathBuf>,
    addr_sel: Option<usize>,
    _addr_task: Option<Task<()>>,
    search: Entity<InputState>,
    /// Files' "Filter by" strip under the command bar (name filter for this folder).
    filter_bar: bool,
    /// Folder compare of one tab's two panes (`compare_mode`).
    pub(crate) compare: Option<crate::compare_mode::State>,
    filter_input: Entity<InputState>,
    sidebar_open: bool,
    sidebar_w: f32,
    /// Icons only (RAIL_W wide); file dialogs start this way.
    sidebar_rail: bool,
    /// The full sidebar shown over the content while the pointer is on the rail (or on it).
    rail_peek: [bool; 2],
    /// The colour the window shows behind the rail (Mica's wallpaper tint), read off the screen
    /// when the full sidebar opens over the content: painted with it, the sidebar looks like the
    /// docked one instead of a neutral grey slab over a tinted window.
    rail_peek_bg: Option<Hsla>,
    /// The full sidebar is out over the content (the state the last frame drew), it is on its way back in, and a number that
    /// changes with every turn (the motions are keyed by it, and a way back in that was overtaken by a new opening ends nothing).
    rail_peek_open: bool,
    rail_peek_closing: bool,
    rail_peek_serial: u64,
    preview_w: f32,
    /// The preview pane at the bottom: its height, the measured row and the debounced write (idle while the pane is at the right).
    preview_dock: preview_dock::Dock,
    /// Debounced settings write after a pane resize.
    _width_save: Option<Task<()>>,
    /// The saved session this window holds (opened from it, or saved under it): its name stands at the far left of the tabs, so the
    /// workspace is known at a glance (a Reddit suggestion, 2026-11-18). Shown only while a session of that name is saved.
    session_name: Option<String>,
    palette: Option<Entity<crate::command_palette::CommandPalette>>,
    bulk: Option<Entity<crate::bulk_rename::BulkRename>>,
    /// Last window title / session snapshot we pushed (avoid redundant work every frame).
    last_title: String,
    /// Tab strip scrolling (many tabs): keeps the active tab in view.
    tab_scroll: ScrollHandle,
    tab_scrolled_to: Option<usize>,
    /// Tab being opened with an animation: (its view, a serial so each opening animates anew).
    tab_anim: Option<(Entity<FolderView>, u64)>,
    /// LEAF_MOCKUP_DEMO: 0 nothing done, 1 tabs open, 2 panes dressed (`demo_setup`).
    demo_stage: u8,
    tab_anim_serial: u64,
    /// Files dragged over an inactive tab: (tab, serial). Held still there, the tab opens.
    drag_hover_tab: Option<(usize, u64)>,
    /// Files are being dragged over the window: the sidebar shows its "drop here to pin" row.
    sidebar_ext_drag: bool,
    /// The Recycle Bin's item count for the sidebar, and when it was last asked for (the shell is asked again after a while).
    recycle_count: Option<u64>,
    recycle_asked: Option<std::time::Instant>,
    recycle_epoch: u64,
    /// The free space under the status line: (the folder it is for, when it was read, the bytes).
    free_cache: Option<(PathBuf, std::time::Instant, Option<u64>)>,
    /// The sidebar's folder tree (empty unless the setting is on).
    side_tree: sidebar_tree::Holder,
    /// Window width from the last frame (breadcrumb collapsing).
    viewport_w: f32,
    /// Window height from the last frame (the sidebar folds its drives when the tags would not fit).
    viewport_h: f32,
    /// The sidebar's Drives section opened or folded by a click on its heading; None: folded only when the sidebar is too short.
    drives_open: Option<bool>,
    /// The git worktree panel's state (`worktrees.rs`).
    wt: worktrees::WtState,
    /// The address box's width for the path as last laid out (logical px; 0 before the first layout): what the breadcrumb folds its
    /// leading folders to fit (issue 164: a guess from the window's width folded them with room to spare).
    crumb_room: std::rc::Rc<std::cell::Cell<f32>>,
    last_session: Vec<String>,
    _bulk_sub: Option<Subscription>,
    _palette_sub: Option<Subscription>,
    _subs: Vec<Subscription>,
}

/// Every open main window's workspace, so app-level events (a second launch) can reach one.
#[derive(Default)]
pub struct OpenWorkspaces(Vec<(AnyWindowHandle, WeakEntity<Workspace>)>);
impl Global for OpenWorkspaces {}

#[derive(Clone)]
struct MainPlacement {
    bounds: [f32; 5],
    display: Option<(String, [f32; 2])>,
}

impl MainPlacement {
    fn capture(window: &Window, cx: &App) -> Option<Self> {
        if leaf_sys::window::offscreen() {
            return None; // a parked test window must not become the saved placement
        }
        let (b, max) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, 0.),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, 1.),
        };
        if b.size.width < px(500.) || b.size.height < px(350.) {
            return None;
        }
        let bounds = [f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height), max];
        let current_display = window.display(cx);
        let display = Workspace::hwnd(window)
            .and_then(leaf_sys::kit::monitor_of_window)
            .and_then(|h| {
                let on_monitor = |id| u64::from(id) == h as u64;
                current_display.as_ref().filter(|d| on_monitor(d.id())).cloned()
                    .or_else(|| cx.displays().into_iter().find(|d| on_monitor(d.id())))
            })
            .or(current_display)
            .and_then(|d| {
                let offset = b.origin - d.bounds().origin;
                Some((d.uuid().ok()?.to_string(), [f32::from(offset.x), f32::from(offset.y)]))
            });
        Some(Self { bounds, display })
    }

    fn apply(self, settings: &mut crate::settings::Settings) {
        settings.window = Some(self.bounds);
        settings.window_display = self.display;
    }
}

#[derive(Default)]
struct MainPlacements(std::collections::HashMap<WindowId, MainPlacement>);
impl Global for MainPlacements {}

fn remember_placement(window: &Window, cx: &mut App) {
    if let Some(placement) = MainPlacement::capture(window, cx) {
        cx.default_global::<MainPlacements>().0.insert(window.window_handle().window_id(), placement);
    }
}

pub(crate) fn save_closed_placement(id: WindowId, cx: &mut App) {
    if let Some(placement) = cx.default_global::<MainPlacements>().0.remove(&id) {
        cx.update_global::<crate::settings::Settings, _>(|s, _| placement.apply(s));
        cx.global::<crate::settings::Settings>().save();
    }
}

/// A second launch forwarded its argument: open it as a tab in the most recent live window
/// and bring that window up; with no window left, open a new one.
/// Taskbar jump list: "New window" / "New tab" and the pinned folders. Rewritten only when the
/// pins or the language change; test runs (several instances) leave the user's list alone.
fn update_jump_list(places: &crate::places::Places) {
    // taskbar jump lists are a Windows thing
    if !cfg!(windows) {
        return;
    }
    static LAST: std::sync::Mutex<Option<(Vec<(String, PathBuf)>, Vec<(String, PathBuf)>, [&'static str; 4])>> = std::sync::Mutex::new(None);
    if !places.loaded || !places.extras_loaded || std::env::var_os("LEAF_MULTI_INSTANCE").is_some() || std::env::var_os("LEAF_INSTANCE").is_some() {
        return;
    }
    // in the UI language, as the sidebar shows them (a default pin's title is its Turkish name)
    let pins: Vec<(String, PathBuf)> = places.pinned.iter().map(|p| (crate::i18n::td(&p.title), p.path.clone())).collect();
    // the folders used last (Windows' recent items), without the pinned ones
    let recent: Vec<(String, PathBuf)> = places
        .recent
        .iter()
        .filter(|r| r.is_dir && !pins.iter().any(|(_, p)| *p == r.target))
        .take(6)
        .map(|r| (leaf_sys::localized::label_if_known(&r.target).map_or_else(|| r.name.clone(), |l| l.to_string()), r.target.clone()))
        .collect();
    let labels = [t!("New window"), t!("New tab"), t!("Pinned"), t!("Recent folders")];
    {
        let mut last = LAST.lock().unwrap();
        if last.as_ref() == Some(&(pins.clone(), recent.clone(), labels)) {
            return;
        }
        *last = Some((pins.clone(), recent.clone(), labels));
    }
    let Ok(exe) = std::env::current_exe() else { return };
    // A run of changes is ONE write: of the last of them, a moment after it (`TURN` says which change is the last). WHY: the recent
    // folders can change several times within a second (nine times in five seconds in a user's log), every change started a write
    // on a thread of its own, and the writes collided ("unable to remove the file to be replaced", see `leaf_sys::jumplist::set`).
    use std::sync::atomic::Ordering::Relaxed;
    static TURN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let turn = TURN.fetch_add(1, Relaxed) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(JUMP_LIST_SETTLE);
        if TURN.load(Relaxed) != turn {
            return;
        }
        let tasks = [(labels[0], leaf_sys::jumplist::NEW_WINDOW_ARG), (labels[1], "")];
        let cats = [(labels[2].to_string(), pins), (labels[3].to_string(), recent)];
        if let Err(e) = leaf_sys::jumplist::set(&exe, &tasks, &cats) {
            // (not written: the next look at the places writes it, even when nothing has changed by then)
            if TURN.load(Relaxed) == turn {
                *LAST.lock().unwrap() = None;
            }
            // one warning in a run of the app: where the list cannot be written at all (a policy), every try would add a line
            if WARNED.swap(true, Relaxed) {
                log::debug!("jump list: {e:#}");
            } else {
                log::warn!("jump list: {e:#}");
            }
        }
    });
}

/// How long the jump list waits after a change of its content before it is written (see `update_jump_list`).
const JUMP_LIST_SETTLE: std::time::Duration = std::time::Duration::from_millis(400);

/// Settings, asked for from outside a window (leaf-look's tray icon, the tray menu, a start with `--settings=<page>`): the window
/// that has Settings open comes forward, on that tab; a new window is opened only when none has. WHY (issue 33: "... opens the
/// Settings window multiple times (up to 5)"): every such request opened another window, so the five messages a tray sends for a
/// double click made five windows. 0.8.2 made those five one request; with this a second request is harmless whoever sends it.
/// Says whether the request is taken care of (see `route_ipc_shown`).
pub fn show_settings(cx: &mut App) -> bool {
    let list = cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default();
    for (handle, ws) in list.into_iter().rev() {
        let Some(ws) = ws.upgrade() else { continue };
        let tab = {
            let w = ws.read(cx);
            w.tabs.iter().position(|t| matches!(t.view().read(cx).location(), Location::Settings))
        };
        let Some(tab) = tab else { continue };
        let shown = handle
            .update(cx, |_, window, cx| {
                ws.update(cx, |w, cx| w.activate(tab, window, cx));
                window.activate_window();
            })
            .is_ok();
        if shown {
            return true;
        }
    }
    crate::open_main_window_made(Some(Location::Settings), None, None, cx)
}

/// Whether `path` lives on drive `letter` (`E:\…`, `\\?\E:\…`).
pub fn on_drive(path: &std::path::Path, letter: char) -> bool {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let mut c = s.chars();
    matches!((c.next(), c.next()), (Some(l), Some(':')) if l.eq_ignore_ascii_case(&letter))
}

/// Windows asks to eject `letter` ("Safely remove", eject from our sidebar or Explorer): every
/// window lets go of it — panes on that drive go Home (their folder watchers, size walks and
/// listings stop), its previews close, queued thumbnails are dropped.
pub fn release_drive(letter: char, cx: &mut App) {
    let list = cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default();
    for (handle, ws) in list {
        let Some(ws) = ws.upgrade() else { continue };
        let _ = handle.update(cx, |_, _, cx| ws.update(cx, |w, cx| w.release_drive(letter, cx)));
    }
}

/// Whether any app window (not a menu / flyout popup) is still open.
/// The open main windows (not Settings, leaf-look or popups).
pub fn main_windows(cx: &App) -> Vec<AnyWindowHandle> {
    let open = cx.windows();
    cx.try_global::<OpenWorkspaces>()
        .map(|g| open.into_iter().filter(|w| g.0.iter().any(|(h, ws)| ws.upgrade().is_some() && w.window_id() == h.window_id())).collect())
        .unwrap_or_default()
}

pub fn any_main_window(cx: &App) -> bool {
    let open = cx.windows();
    cx.try_global::<OpenWorkspaces>()
        .is_some_and(|g| g.0.iter().any(|(h, ws)| ws.upgrade().is_some() && open.iter().any(|w| w.window_id() == h.window_id())))
}

/// A second launch's message that asks for a file to be shown selected in its folder
/// (`leaf /select,<file>`, like Explorer).
pub const SELECT_PREFIX: &str = "\u{1}select:";

/// A file to select in the window that just opened its folder ("Show in folder" from another app,
/// see leaf_sys::shell_window): selected in place, or its folder opened when no window shows it.
pub const SELECT_HERE_PREFIX: &str = "\u{1}select-here:";

/// A second launch on a shell folder (a phone, Network…) reaches the running window as this
/// prefix plus the folder's parsing name (and, in a saved session, a tab and its name).
pub const SHELL_PREFIX: &str = "leaf-shell:";

/// How a tab is kept in the saved session: a folder's path, a search's folder, the Recycle Bin
/// and shell folders (a phone…) by name; other places come back as Home ("").
fn session_text(loc: &Location) -> String {
    match loc {
        Location::Path(p) => rf_core::history::redact_credentials(&p.to_string_lossy()),
        Location::Search { root, .. } => root.to_string_lossy().into_owned(),
        Location::RecycleBin => "shell:RecycleBinFolder".into(),
        Location::Downloads => "leaf:downloads".into(),
        Location::SyncJobs => "leaf:sync".into(),
        Location::Agents => "leaf:agents".into(),
        Location::Shell { parsing, name } => format!("{SHELL_PREFIX}{parsing}\t{name}"),
        // (an address never holds a password: VPath refuses one)
        Location::Remote(v) if crate::remote::usable(v.scheme()) => v.to_string(),
        _ => String::new(),
    }
}

/// What the session keeps of the tabs: the left pane of each, and for the tabs that are split the right pane with whether it had
/// the focus. `tabs` is each tab's left place and, when it is split, its right place and that flag.
fn session_record(tabs: &[(Location, Option<(Location, bool)>)]) -> (Vec<String>, Vec<crate::settings::SessionPane>) {
    let left = tabs.iter().map(|(l, _)| session_text(l)).collect();
    let panes = tabs
        .iter()
        .enumerate()
        .filter_map(|(tab, (_, right))| right.as_ref().map(|(loc, active)| crate::settings::SessionPane { tab, location: session_text(loc), active: *active }))
        .collect();
    (left, panes)
}

/// Where a place written by [`session_text`] leads back to.
fn session_place(text: &str) -> Location {
    if text.is_empty() { Location::Home } else { parse_launch(text).0 }
}

/// The second panes to open again when `tabs` tabs came back: for each tab that exists the first pane the record names for it (a
/// record from a session with more tabs, or one edited by hand, must not open a pane for a tab that is not there or two for one).
fn panes_to_restore(panes: &[crate::settings::SessionPane], tabs: usize) -> Vec<(usize, Location, bool)> {
    let mut seen = std::collections::BTreeSet::new();
    panes.iter().filter(|p| p.tab < tabs && seen.insert(p.tab)).map(|p| (p.tab, session_place(&p.location), p.active)).collect()
}

/// What tells that the session has to be written again: the places of all panes (not which pane has the focus: a click from one
/// pane to the other is not worth a write of the settings file; the focus is written when the places change and at exit).
fn session_key(left: &[String], panes: &[crate::settings::SessionPane]) -> Vec<String> {
    left.iter().cloned().chain(panes.iter().map(|p| format!("\u{1}{}\u{1}{}", p.tab, p.location))).collect()
}

/// What a launch message opens: a folder (or Home), and maybe a name to select there.
pub fn parse_launch(msg: &str) -> (Location, Option<String>) {
    let m = msg.trim();
    if let Some(rest) = m.strip_prefix(SHELL_PREFIX) {
        let (parsing, name) = rest.split_once('\t').unwrap_or((rest, ""));
        return (Location::Shell { parsing: parsing.to_string(), name: name.to_string() }, None);
    }
    if let Some(loc) = Location::from_shell_name(m) {
        return (loc, None);
    }
    if let Some(loc) = crate::remote::parse_typed(m) {
        return (loc, None);
    }
    if crate::remote::refused_address(m).is_some() {
        return (Location::Home, None);
    }
    if let Some(file) = m.strip_prefix(SELECT_PREFIX) {
        let p = PathBuf::from(file);
        if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
            return (Location::Path(dir.to_path_buf()), Some(name.to_string_lossy().into_owned()));
        }
    }
    if m.is_empty() { (Location::Home, None) } else { (Location::Path(PathBuf::from(m)), None) }
}

/// leaf-look asked for the next or the previous item (an arrow key in its window): the list of the newest window moves its
/// selection by one and leaf-look is sent the item now selected. Only while leaf-look was opened from our list.
fn step_quick_look(forward: bool, cx: &mut App) {
    if !crate::folder_view::QUICK_LOOK_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let list = cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default();
    for (handle, ws) in list.into_iter().rev() {
        let Some(ws) = ws.upgrade() else { continue };
        let done = handle
            .update(cx, |_, _, cx| {
                let view = ws.read(cx).active_tab().clone();
                view.update(cx, |v, cx| v.step_selection(forward, cx));
                view.read(cx).preview_target()
            })
            .ok();
        if let Some(target) = done {
            if let Some((path, e)) = target {
                *LOOK_FOLLOWED.lock().unwrap() = Some(path.clone());
                crate::archive_view::look_send(path, e.size, true);
            }
            return;
        }
    }
}

/// Writes every open main window's session and placement into the settings, as closing a window does (a restart quits without
/// closing them, and the new process opened at the place the window had at its LAST close: seen in a test copy).
pub fn save_all_sessions(cx: &mut App) {
    let list = cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default();
    for (handle, ws) in list {
        let Some(ws) = ws.upgrade() else { continue };
        let _ = handle.update(cx, |_, window, cx| {
            ws.update(cx, |w, cx| w.save_session(window, cx));
        });
    }
}

/// A saved session waiting for the window that opens it (`open_named_session` -> `Workspace::new`).
static PENDING_NAMED: std::sync::Mutex<Option<crate::settings::NamedSession>> = std::sync::Mutex::new(None);

/// Opens the session saved as `name` in a new window.
pub fn open_named_session(name: &str, cx: &mut App) {
    let Some(session) = cx.global::<crate::settings::Settings>().saved_sessions.iter().find(|s| s.name == name).cloned() else { return };
    *PENDING_NAMED.lock().unwrap() = Some(session);
    crate::open_main_window(None, None, None, cx);
    // (a window that could not open leaves nothing waiting for the next one)
    PENDING_NAMED.lock().unwrap().take();
}

/// The saved sessions after saving `session`: one of the same name is replaced where it stood, a new name goes at the end.
fn remember_named(list: &mut Vec<crate::settings::NamedSession>, session: crate::settings::NamedSession) {
    match list.iter_mut().find(|s| s.name.eq_ignore_ascii_case(&session.name)) {
        Some(old) => *old = session,
        None => list.push(session),
    }
}

/// Selects `file` in the newest window whose current tab shows its folder (and brings that window
/// forward); false when none does.
pub(crate) fn select_in_open_folder(file: &std::path::Path, cx: &mut App) -> bool {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else { return false };
    let key = |p: &std::path::Path| rf_core::pathkey::path_key_trimmed(p);
    let want = key(dir);
    let name = name.to_string_lossy().into_owned();
    let list = cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default();
    for (handle, ws) in list.into_iter().rev() {
        let Some(ws) = ws.upgrade() else { continue };
        let view = ws.read(cx).active_view();
        let here = matches!(view.read(cx).location(), Location::Path(p) if key(p) == want);
        if !here {
            continue;
        }
        let name = name.clone();
        if handle
            .update(cx, |_, window, cx| {
                view.update(cx, |v, cx| v.select_now(name, cx));
                window.activate_window();
            })
            .is_ok()
        {
            return true;
        }
    }
    false
}

/// The `/select,` file of the first launch, picked up by the first window.
pub static START_SELECT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// leaf-look'a en son gönderilen dosya (seçim değişince yalnız yenisi gider).
static LOOK_FOLLOWED: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Whether any main browser window is currently open.
fn has_browser_window(cx: &App) -> bool {
    let open = cx.windows();
    cx.try_global::<OpenWorkspaces>().is_some_and(|g| {
        g.0.iter().any(|(h, ws)| {
            ws.upgrade().is_some() && open.iter().any(|w| w.window_id() == h.window_id())
        })
    })
}

/// Start location for a new window opened from an IPC launch when no existing window took it:
/// `None` restores the session (or startup pages) when no browser window is open and no target
/// was given, so reopening the app from background mode behaves like a fresh start.
fn launch_window_start(msg: &str, has_browser_window: bool, loc: Location) -> Option<Location> {
    if msg.trim().is_empty() && !has_browser_window { None } else { Some(loc) }
}

pub fn route_ipc(msg: String, cx: &mut App) {
    route_ipc_shown(msg, cx);
}

/// [`route_ipc`], saying whether the request is taken care of: false only when it needed a window and none could be made. The
/// launch of a second process waits for this answer (see `leaf_sys::handover`) and shows the folder itself on false.
pub fn route_ipc_shown(msg: String, cx: &mut App) -> bool {
    let (msg, force_tab) = match msg.strip_prefix(crate::cli::NEW_TAB_PREFIX) {
        Some(rest) => (rest.to_string(), true),
        None => (msg, false),
    };
    match crate::look::ipc::ToApp::decode(&msg) {
        Some(crate::look::ipc::ToApp::LookClosed) => {
            crate::folder_view::QUICK_LOOK_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
            *LOOK_FOLLOWED.lock().unwrap() = None;
            return true;
        }
        Some(crate::look::ipc::ToApp::Step(forward)) => {
            step_quick_look(forward, cx);
            return true;
        }
        Some(crate::look::ipc::ToApp::OpenSettings) => {
            crate::settings_view::open_page(crate::settings_view::Page::Look, cx);
            return show_settings(cx);
        }
        Some(crate::look::ipc::ToApp::OpenGuide) => {
            crate::guide_page::set_focus("viewer", cx);
            crate::settings_view::open_page(crate::settings_view::Page::Guide, cx);
            return show_settings(cx);
        }
        None => {}
    }
    // `leaf download <address> [folder]`
    if let Some(rest) = msg.strip_prefix(crate::downloads::MESSAGE_PREFIX) {
        return crate::downloads::from_message(rest, cx);
    }
    if let Some(file) = msg.strip_prefix(SELECT_HERE_PREFIX) {
        if !select_in_open_folder(std::path::Path::new(file), cx) {
            return route_ipc_shown(format!("{SELECT_PREFIX}{file}"), cx);
        }
        return true;
    }
    if msg.trim() == leaf_sys::jumplist::NEW_WINDOW_ARG {
        return crate::open_main_window_made(Some(Location::Home), None, None, cx);
    }
    if msg.trim() == crate::quick_search::LAUNCH_ARG {
        crate::quick_search::toggle(cx);
        return true;
    }
    let (loc, select) = parse_launch(&msg);
    // (with "open folders from other programs in a new window" on, no window takes it as a tab: issue 77)
    let list = if cx.global::<crate::settings::Settings>().launch_in_new_window && !force_tab {
        Vec::new()
    } else {
        cx.try_global::<OpenWorkspaces>().map(|g| g.0.clone()).unwrap_or_default()
    };
    for (handle, ws) in list.into_iter().rev() {
        let Some(ws) = ws.upgrade() else { continue };
        let (target, sel) = (loc.clone(), select.clone());
        let ok = handle
            .update(cx, |_, window, cx| {
                ws.update(cx, |w, cx| w.open_tab_selecting(target, sel, window, cx));
                window.activate_window();
            })
            .is_ok();
        if ok {
            return true;
        }
    }
    *START_SELECT.lock().unwrap() = select;
    let start = launch_window_start(&msg, has_browser_window(cx), loc);
    crate::open_main_window_made(start, None, None, cx)
}

/// Puts a search at the front of the remembered ones (once, whatever its case; ten are kept).
fn remember_search(history: &mut Vec<String>, q: &str) {
    let q = q.trim();
    if q.is_empty() {
        return;
    }
    history.retain(|h| !h.eq_ignore_ascii_case(q) && rf_core::entry::fold_case(h) != rf_core::entry::fold_case(q));
    history.insert(0, q.to_string());
    history.truncate(10);
}

/// The folder a tab's terminal button would open, if the tab has one: a real disk folder,
/// not an archive, not FTP or a network share. Called for every tab on every frame, so it
/// looks at the path only and never asks the disk (a stat of a share that went away blocks
/// the UI thread for seconds); a folder merely named like an archive goes without the button.
pub(crate) fn tab_terminal_dir(loc: &Location) -> Option<PathBuf> {
    let Location::Path(p) = loc else { return None };
    (!crate::remote::is_address_path(p) && leaf_sys::network::server_root(p).is_none() && !crate::nav_touch::may_hold_archive(p))
        .then(|| p.clone())
}

impl Workspace {
    pub fn new(
        start: Option<Location>,
        ipc: Option<futures::channel::mpsc::UnboundedReceiver<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if let Some(mut rx) = ipc {
            cx.spawn_in(window, async move |this, cx| {
                use futures::StreamExt;
                while let Some(msg) = rx.next().await {
                    let (loc, select) = parse_launch(&msg);
                    let ok = this
                        .update_in(cx, |ws, window, cx| {
                            ws.open_tab_selecting(loc, select, window, cx);
                            window.activate_window();
                        })
                        .is_ok();
                    if !ok {
                        break;
                    }
                }
            })
            .detach();
        }
        check_script::start(window, cx);
        let places = cx.new(Places::new);
        let images = cx.new(ImageCache::new);
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        let ops = cx.global::<crate::status_center::OpsGlobal>().0.clone();
        cx.observe(&ops, |_, _, cx| cx.notify()).detach();
        let preview = {
            let images = images.clone();
            cx.new(|cx| Preview::new(images, cx))
        };
        cx.observe(&places, |_, places, cx| {
            update_jump_list(places.read(cx));
            cx.notify()
        })
        .detach();
        let address = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Enter a path")));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search")));
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("File name")));

        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&address, window, |this, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let from_completion = this.addr_sel.and_then(|i| this.addr_suggest.get(i)).is_some();
                let text = match this.addr_sel.and_then(|i| this.addr_suggest.get(i)) {
                    Some(p) => p.to_string_lossy().into_owned(),
                    None => input.read(cx).value().to_string(),
                };
                this.clear_suggestions();
                this.editing_address = false;
                if text.trim().to_lowercase().starts_with("shell:") {
                    // resolve in the background (shell namespace), then navigate
                    let t = text.trim().to_string();
                    let work = cx.background_spawn(async move { leaf_sys::shell::resolve_shell_path(&t) });
                    cx.spawn_in(window, async move |this, cx| {
                        if let Some(p) = work.await {
                            let _ = this.update_in(cx, |ws, window, cx| ws.navigate(Location::Path(p), window, cx));
                        }
                    })
                    .detach();
                    this.focus_active(window, cx);
                    cx.notify();
                    return;
                }
                // a place as always; with the setting on, text that names no place is tried as a program (address_cmd.rs)
                this.enter_address_text(text, from_completion, window, cx);
            }
            InputEvent::Blur => {
                this.editing_address = false;
                this.clear_suggestions();
                cx.notify();
            }
            InputEvent::Change => {
                // the highlight belongs to the previous text
                this.addr_sel = None;
                if this.editing_address {
                    let text = input.read(cx).value().to_string();
                    this.update_suggestions(text, cx);
                }
            }
            _ => {}
        }));
        subs.push(cx.subscribe_in(&filter_input, window, |this, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                let q = input.read(cx).value().to_string();
                if let Some(tab) = this.tabs.get(this.active).map(|t| t.view().clone()) {
                    tab.update(cx, |v, cx| v.set_query(q, cx));
                }
            }
            InputEvent::PressEnter { .. } => this.focus_active(window, cx),
            _ => {}
        }));
        subs.push(cx.subscribe_in(&search, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                // Enter: search this folder and all subfolders.
                let q = input.read(cx).value().trim().to_string();
                this.run_search(q, window, cx);
                return;
            }
            if let InputEvent::Change = ev {
                let q = input.read(cx).value().to_string();
                if let Some(tab) = this.tabs.get(this.active).map(|t| t.view().clone()) {
                    tab.update(cx, |v, cx| v.set_query(q, cx));
                }
            }
            // clicking elsewhere: the Omnibar shows the path again (the query stays as a chip)
            if let InputEvent::Blur = ev {
                if this.search_mode {
                    this.search_mode = false;
                    cx.notify();
                }
            }
        }));

        // Repaint the status-bar renderer badge when the hybrid renderer switches devices.
        cx.spawn(async move |this, cx| {
            let mut last = (crate::platform::gpui::hybrid::switch_count(), crate::platform::gpui::hybrid::mode());
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(800)).await;
                let now = (crate::platform::gpui::hybrid::switch_count(), crate::platform::gpui::hybrid::mode());
                // (the speed overlay, while it is on, is drawn again with each beat)
                let hud = this.update(cx, |_, cx| crate::speed_hud::on(cx)).unwrap_or(false);
                if now != last || hud {
                    last = now;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }
        })
        .detach();

        cx.observe_global::<crate::settings::Settings>(|this, cx| {
            // the language may have changed: the jump list follows
            update_jump_list(this.places.read(cx));
            // another window may have dragged the preview pane's height
            this.sync_preview_height(cx);
            let s = cx.global::<crate::settings::Settings>();
            this.preview_open = s.preview_open;
            this.sidebar_rail = s.sidebar_rail;
            this.sidebar_open = s.sidebar_open;
            cx.notify();
        })
        .detach();
        cx.observe_global::<crate::tags::Tags>(|_, cx| cx.notify()).detach();
        // The system's light / dark switch reaches the window as an event only; nothing else redraws it. The palette and the glass's tint
        // (`set_dark`) are decided in `render`, so a window left alone would keep the old theme until the next click.
        cx.observe_window_appearance(window, |_: &mut Self, window, cx| {
            window.refresh();
            cx.notify();
        })
        .detach();
        // Keep the latest placement while the HWND is alive. Closing the native frame can
        // destroy it before GPUI is able to update the workspace from its should-close callback.
        remember_placement(window, cx);
        cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
            remember_placement(window, cx);
            this.preview.update(cx, |pv, _| pv.invalidate_host());
            this.schedule_behind_sample(window, cx);
            cx.notify();
        })
        .detach();
        // the system backdrop shows only while the window is active: measure what is behind the sheets when it becomes so
        cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            if window.is_window_active() {
                this.schedule_behind_sample(window, cx);
            }
        })
        .detach();
        let ws = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            if let Some(ws) = ws.upgrade() {
                ws.update(cx, |w, cx| w.save_session(window, cx));
            }
            Self::release_preview_host(window);
            true
        });
        // the system's own close button would show as a dim x above the title bar's over a see-through window
        leaf_sys::window::hide_system_caption(crate::look::window::hwnd_of(window));
        let prefs = cx.global::<crate::settings::Settings>().clone();
        {
            let entry = (window.window_handle(), cx.entity().downgrade());
            let list = &mut cx.default_global::<OpenWorkspaces>().0;
            list.retain(|(_, w)| w.upgrade().is_some());
            list.push(entry);
        }
        let shelf = cx.new(crate::shelf_view::ShelfView::new);
        let shelf_sub = cx.subscribe_in(&shelf, window, |this, _, ev, window, cx| this.on_shelf_event(ev, window, cx));

        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            closed: Vec::new(),
            places,
            images,
            ops,
            preview,
            quick_look: None,
            preview_open: prefs.preview_open,
            shelf,
            shelf_open: false,
            address,
            editing_address: false,
            search_mode: false,
            frame_dark: None,
            applied_backdrop: None,
            backdrop_pref_seen: None,
            glass_hwnd: 0,
            glass_dark: None,
            glass_failed: false,
            behind_gen: 0,
            glass_shown: false,
            glass_capture: leaf_sys::glass::CaptureMode::Off,
            glass_gen: 0,
            sidebar_lens: false,
            sidebar_lens_claimed: std::cell::Cell::new(false),
            sidebar_host_kind: std::cell::Cell::new(None),
            tab_lens: false,
            tab_lens_claimed: false,
            plus_lens_claimed: false,
            addr_suggest: Vec::new(),
            addr_sel: None,
            _addr_task: None,
            search,
            filter_bar: false,
            compare: None,
            filter_input,
            sidebar_open: prefs.sidebar_open,
            compact: None,
            sidebar_w: prefs.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX),
            sidebar_rail: prefs.sidebar_rail,
            rail_peek: [false; 2],
            rail_peek_bg: None,
            rail_peek_open: false,
            rail_peek_closing: false,
            rail_peek_serial: 0,
            preview_w: prefs.preview_width.clamp(PREVIEW_MIN, PREVIEW_MAX),
            preview_dock: preview_dock::Dock::new(prefs.preview_height),
            _width_save: None,
            session_name: None,
            palette: None,
            bulk: None,
            last_title: String::new(),
            tab_scroll: ScrollHandle::new(),
            tab_scrolled_to: None,
            tab_anim: None,
            demo_stage: 0,
            tab_anim_serial: 0,
            drag_hover_tab: None,
            sidebar_ext_drag: false,
            recycle_count: None,
            recycle_asked: None,
            recycle_epoch: 0,
            free_cache: None,
            side_tree: Default::default(),
            viewport_w: 1300.,
            viewport_h: 800.,
            drives_open: None,
            wt: Default::default(),
            crumb_room: Default::default(),
            last_session: Vec::new(),
            _bulk_sub: None,
            _palette_sub: None,
            _subs: { let mut s = subs; s.push(shelf_sub); s },
        };
        match start {
            Some(loc) => {
                let select = START_SELECT.lock().unwrap().take();
                this.open_tab_selecting(loc, select, window, cx)
            }
            // a saved session asked for by name (`open_named_session`): its tabs, its split panes, its active tab
            None if PENDING_NAMED.lock().unwrap().is_some() => {
                let named = PENDING_NAMED.lock().unwrap().take().unwrap_or_default();
                this.session_name = Some(named.name.clone());
                for t in &named.tabs {
                    this.open_tab(session_place(t), window, cx);
                }
                if this.tabs.is_empty() {
                    this.open_tab(Location::Home, window, cx);
                }
                for (ix, loc, active) in panes_to_restore(&named.panes, this.tabs.len()) {
                    this.open_second_pane(ix, loc, active, window, cx);
                }
                let a = named.active.min(this.tabs.len() - 1);
                this.activate(a, window, cx);
            }
            None if prefs.startup == crate::settings::StartupPref::Continue && !prefs.session_tabs.is_empty() => {
                for t in &prefs.session_tabs {
                    this.open_tab(session_place(t), window, cx);
                }
                // the tabs that were split in two get their second pane back, at its own folder and with the focus where it was
                for (ix, loc, active) in panes_to_restore(&prefs.session_panes, this.tabs.len()) {
                    this.open_second_pane(ix, loc, active, window, cx);
                }
                let a = prefs.session_active.min(this.tabs.len() - 1);
                this.activate(a, window, cx);
            }
            None if prefs.startup == crate::settings::StartupPref::Pages && !prefs.startup_pages.is_empty() => {
                for t in &prefs.startup_pages {
                    let loc = parse_location(t).unwrap_or(Location::Home);
                    this.open_tab(loc, window, cx);
                }
                this.activate(0, window, cx);
            }
            None => this.open_tab(Location::Home, window, cx),
        }
        // The tabs a window opens WITH are simply there: the opening animation is for a tab that is opened in a window one is looking
        // at. WHY: `open_tab` started it for the first tab too, so every new window spent its first third of a second growing its
        // tab and sliding the "+" in (seen frame by frame, 2026-11-16: the folder is listed in the first picture, the tab strip moves
        // for 280 ms more), and the speed test, which waits for the picture to stand still, counted that as "not open yet".
        this.tab_anim = None;
        this
    }

    /// The preview pane on/off (the saved setting).
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        let v = !self.preview_open;
        crate::settings::update(cx, |s| s.preview_open = v);
    }

    pub(crate) fn active_view(&self) -> Entity<FolderView> {
        self.active_tab().clone()
    }

    fn save_session(&mut self, window: &Window, cx: &mut Context<Self>) {
        let placement = MainPlacement::capture(window, cx);
        if let Some(placement) = &placement {
            cx.default_global::<MainPlacements>().0.insert(window.window_handle().window_id(), placement.clone());
        }
        let (tabs, panes) = self.session_now(cx);
        let active = self.active;
        cx.update_global::<crate::settings::Settings, _>(|s, _| {
            s.session_tabs = tabs;
            s.session_panes = panes;
            s.session_active = active;
            if let Some(placement) = placement {
                placement.apply(s);
            }
        });
        // synchronous: the process is about to exit
        cx.global::<crate::settings::Settings>().save();
    }

    /// The tabs as the session keeps them, now ([`session_record`]).
    fn session_now(&self, cx: &App) -> (Vec<String>, Vec<crate::settings::SessionPane>) {
        let tabs: Vec<(Location, Option<(Location, bool)>)> =
            self.tabs.iter().map(|t| (t.left.read(cx).location().clone(), t.right.as_ref().map(|r| (r.read(cx).location().clone(), t.right_active)))).collect();
        session_record(&tabs)
    }

    /// Gives tab `ix` its second pane at `loc` (a tab of the last session that was split); `active`: that pane has the focus.
    fn open_second_pane(&mut self, ix: usize, loc: Location, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() || self.tabs[ix].right.is_some() {
            return;
        }
        let view = self.new_view(loc, window, cx);
        view.update(cx, |v, _| v.dual = true);
        self.tabs[ix].left.update(cx, |v, _| v.dual = true);
        self.tabs[ix].right = Some(view);
        self.tabs[ix].right_active = active;
        self.sync_shown(cx);
    }

    /// LEAF_MOCKUP_DEMO: opens the mockup's three tabs (the first split, its right pane a grid of the holiday pictures), then
    /// dresses the panes once they are listed. Called on every frame while the demo runs; does its work once.
    fn demo_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::mockup_demo as demo;
        if self.demo_stage == 0 {
            self.demo_stage = 1;
            let at = |s: &str| Location::Path(PathBuf::from(s));
            self.tabs[0].left.update(cx, |v, cx| v.navigate(at(demo::LEAF_SITE), cx));
            self.open_second_pane(0, at(demo::HOLIDAY), false, window, cx);
            if let Some(r) = self.tabs[0].right.clone() {
                r.update(cx, |v, cx| v.set_view_mode(crate::folder_view::ViewMode::Grid, cx));
            }
            self.open_tab(at(demo::DOCUMENTS), window, cx);
            self.open_tab(at(demo::NAS), window, cx);
            self.tab_anim = None;
            self.activate(0, window, cx);
            crate::cut_marks::set(&[Path::new(demo::LEAF_SITE).join(demo::CUT)]);
        }
        if self.demo_stage == 1 {
            let left = self.tabs[0].left.clone();
            let Some(right) = self.tabs[0].right.clone() else { return };
            let l = left.update(cx, |v, cx| v.demo_dress(&demo::SELECTED_LEFT, Some(demo::SELECTED_LEFT[1]), Some(demo::RENAMING), window, cx));
            let r = l && right.update(cx, |v, cx| v.demo_dress(&demo::SELECTED_RIGHT, Some(demo::SELECTED_RIGHT[0]), None, window, cx));
            if r {
                self.demo_stage = 2;
            } else {
                // (asked again on the next frames until both panes are listed)
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
    }

    fn active_tab(&self) -> &Entity<FolderView> {
        self.tabs[self.active].view()
    }

    /// Tab (`forward`) and Shift+Tab: the keyboard goes round the window's panes, the address bar, the folder tree (when it is on
    /// screen) and the file list, as Explorer's Tab does (issue 38: "TAB does not work anywhere"). A rename in the list keeps it.
    fn cycle_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tab().read(cx).renaming.is_some() {
            return;
        }
        let tree = self.tree_focus(cx);
        let here = if self.editing_address {
            Pane::Address
        } else if tree.as_ref().is_some_and(|f| f.is_focused(window)) {
            Pane::Tree
        } else {
            Pane::List
        };
        match next_pane(here, forward, tree.is_some()) {
            Pane::Address => self.begin_address_edit(window, cx),
            to => {
                if self.editing_address {
                    self.editing_address = false;
                    self.clear_suggestions();
                }
                // (the pane the keyboard comes to shows where it is: its row in view, the first one when none was chosen; issue 38)
                match (to, tree) {
                    (Pane::Tree, Some(f)) => {
                        window.focus(&f, cx);
                        self.tree_keyboard_entered(cx);
                    }
                    _ => {
                        self.focus_active(window, cx);
                        self.active_tab().clone().update(cx, |v, cx| v.keyboard_entered(cx));
                    }
                }
                cx.notify();
            }
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let fh = self.active_tab().read(cx).focus.clone();
        window.focus(&fh, cx);
    }

    fn new_view(&mut self, loc: Location, window: &mut Window, cx: &mut Context<Self>) -> Entity<FolderView> {
        let places = self.places.clone();
        let images = self.images.clone();
        let tab = cx.new(|cx| FolderView::new(loc, places, images, cx));
        let sub = cx.subscribe_in(&tab, window, |this, emitter, ev: &FolderEvent, window, cx| match ev {
            FolderEvent::ToOtherPane(kind, paths) => {
                if let Some(other) = this.other_pane(emitter) {
                    let dest = match other.read(cx).location() {
                        Location::Path(p) if !crate::remote::is_address_path(p) && leaf_sys::network::server_root(p).is_none() => Some(p.clone()),
                        _ => None,
                    };
                    if let Some(dest) = dest {
                        crate::status_center::transfer(cx, *kind, paths.clone(), dest);
                    } else if let Some(dir) = other.read(cx).remote_writable_dir() {
                        // into a folder on a server: uploaded (a move would delete here what was sent; it is not offered)
                        if *kind == leaf_sys::fileops::Transfer::Move {
                            crate::prompt::notice(t!("Move to other pane"), t!("Moving onto a server is not offered: copy the items, then delete them here.").to_string(), window, cx);
                        } else {
                            let after = crate::remote::refresh_when_done(other.downgrade());
                            crate::remote::upload(cx, paths.clone(), dir, Some(after));
                        }
                    }
                }
            }
            FolderEvent::DownloadToOtherPane(items, mv) => {
                if let Some(other) = this.other_pane(emitter) {
                    let remote = other.read(cx).remote_writable_dir();
                    match (other.read(cx).download_dir(), remote) {
                        (Some(dest), _) if !*mv => {
                            let after = other.update(cx, |v, cx| v.select_arrived(dest.clone(), cx));
                            crate::remote::download(cx, items.clone(), dest, Some(after));
                        }
                        (_, Some(dir)) if !*mv || items.iter().all(|(v, _)| v.scheme() == dir.scheme() && v.authority() == dir.authority()) => {
                            let after = crate::remote::refresh_when_done(other.downgrade());
                            let me = crate::remote::refresh_when_done(emitter.downgrade());
                            let both: crate::status_center::OnDone = Box::new(move |ok, cx| {
                                after(ok, cx);
                                me(ok, cx);
                            });
                            crate::remote::transfer(cx, items.clone(), dir, *mv, Some(both));
                        }
                        _ if *mv => crate::prompt::notice(t!("Move to other pane"), t!("Moving works within one server; copy the items instead.").to_string(), window, cx),
                        _ => crate::prompt::notice(t!("Copy to other pane"), t!("The other pane shows no folder leaf can copy into.").to_string(), window, cx),
                    }
                }
            }
            FolderEvent::CopyRemoteTo(items) => {
                let initial = this.other_pane(emitter).and_then(|o| o.read(cx).download_dir());
                crate::remote::copy_to_dialog(items.clone(), initial, window, cx);
            }
            FolderEvent::OpenInOtherPane(loc) => {
                if let Some(other) = this.other_pane(emitter) {
                    other.update(cx, |v, cx| v.navigate(loc.clone(), cx));
                }
            }
            FolderEvent::OpenInNewTab(loc) => {
                let keep = this.active;
                this.open_tab(loc.clone(), window, cx);
                // a view asked for by name (disk usage) is what the user wants to see now
                let wanted = matches!(loc, Location::DiskUsage(_) | Location::Duplicates(_) | Location::BuildJunk(_) | Location::Changes(_) | Location::Brain(_) | Location::SyncJobs | Location::Agents);
                if !wanted && !cx.global::<crate::settings::Settings>().focus_new_tab {
                    this.activate(keep, window, cx);
                }
            }
            FolderEvent::OpenInNewWindow(loc) => {
                let loc = loc.clone();
                let mut b = window.bounds();
                b.origin.x += px(36.);
                b.origin.y += px(36.);
                cx.defer(move |cx| crate::open_main_window(Some(loc), None, Some(WindowBounds::Windowed(b)), cx));
            }
            FolderEvent::QuickLook => this.toggle_quick_look(cx),
            FolderEvent::StartFilter(text) => {
                // the first letter opens the bar with it; the next ones are typed into the bar, which has the focus now
                let q = format!("{}{text}", emitter.read(cx).filter.query);
                this.filter_bar = true;
                this.filter_input.update(cx, |s, cx| {
                    s.set_value(q.clone(), window, cx);
                    s.focus(window, cx);
                });
                emitter.update(cx, |v, cx| v.set_query(q, cx));
                cx.notify();
            }
            FolderEvent::NeedPassword(archive, wrong) => {
                let view = emitter.downgrade();
                crate::archive_view::ask_password(archive.clone(), *wrong, window, cx, move |cx| {
                    let _ = view.update(cx, |v, cx| v.refresh(cx));
                });
            }
            FolderEvent::BulkRename(paths) => this.open_bulk_rename(paths.clone(), window, cx),
            FolderEvent::AddToShelf(paths) => {
                let paths = paths.clone();
                this.shelf.update(cx, |s, cx| {
                    s.add(paths, cx);
                });
                this.shelf_open = true;
                cx.notify();
            }
        });
        self._subs.push(sub);
        cx.observe(&tab, |_, _, cx| cx.notify()).detach();
        tab
    }

    /// `open_tab`, then selects `select` (a name in that folder) once it is listed.
    pub fn open_tab_selecting(&mut self, loc: Location, select: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab(loc, window, cx);
        if let Some(name) = select {
            self.active_tab().update(cx, |v, _| v.set_reselect(name));
        }
    }

    pub fn open_tab(&mut self, loc: Location, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.new_view(loc, window, cx);
        crate::usage::bump(crate::usage::Counter::TabsOpened, 1);
        self.tabs.push(Tab { left: view.clone(), right: None, right_active: false });
        let ix = self.tabs.len() - 1;
        // Settings → Appearance → Animations: the "+" grows into the new tab, then a new "+"
        // slides out from behind it. Purely visual: the tab is already open and usable.
        let animate = crate::settings::animations();
        cx.set_reduce_motion(!animate);
        if animate {
            self.tab_anim_serial += 1;
            let serial = self.tab_anim_serial;
            self.tab_anim = Some((view, serial));
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(TAB_ANIM_MS + PLUS_ANIM_MS + 100)).await;
                let _ = this.update(cx, |ws, cx| {
                    if ws.tab_anim.as_ref().is_some_and(|(_, s)| *s == serial) {
                        ws.tab_anim = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.activate(ix, window, cx);
        // Settings → "Open new tabs in dual pane"
        if cx.global::<crate::settings::Settings>().dual_pane_new_tabs && self.tabs[ix].left.read(cx).location().path().is_some() {
            self.toggle_dual_pane(window, cx);
        }
    }

    /// Files are being dragged; `over` tells whether the pointer is on tab `i`. Held on an
    /// inactive tab for a moment, that tab comes forward so the files can be dropped into it.
    fn drag_over_tab(&mut self, i: usize, over: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.drag_hover_tab.map(|(t, _)| t);
        if !over {
            if current == Some(i) {
                self.drag_hover_tab = None;
            }
            return;
        }
        if i == self.active || current == Some(i) {
            return;
        }
        self.tab_anim_serial += 1;
        let serial = self.tab_anim_serial;
        self.drag_hover_tab = Some((i, serial));
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(DRAG_HOVER_MS)).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                // still on the same tab and still dragging
                if ws.drag_hover_tab == Some((i, serial)) && cx.has_active_drag() && i < ws.tabs.len() {
                    ws.drag_hover_tab = None;
                    ws.activate(i, window, cx);
                }
            });
        })
        .detach();
    }

    /// Folder compare: on for the shown tab's two panes (a single pane is split first); off when it is on already.
    fn toggle_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compare.is_some() {
            self.compare_stop(cx);
            return;
        }
        if self.tabs[self.active].right.is_none() {
            self.toggle_dual_pane(window, cx);
        }
        let t = &self.tabs[self.active];
        if let Some(r) = &t.right {
            self.compare = Some(crate::compare_mode::State::new(t.left.clone(), r.clone()));
        }
        cx.notify();
    }

    pub(crate) fn compare_stop(&mut self, cx: &mut Context<Self>) {
        if let Some(st) = self.compare.take() {
            st.clear(cx);
        }
        cx.notify();
    }

    pub(crate) fn compare_set_chip(&mut self, chip: crate::compare_mode::Chip, cx: &mut Context<Self>) {
        if let Some(st) = &mut self.compare {
            st.chip = if st.chip == chip { crate::compare_mode::Chip::All } else { chip };
        }
        cx.notify();
    }

    pub(crate) fn compare_contents(&mut self, cx: &mut Context<Self>) {
        crate::compare_mode::start_content(self, cx);
    }

    pub(crate) fn compare_contents_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(st) = &mut self.compare {
            st.stop_content();
        }
        cx.notify();
    }

    pub(crate) fn compare_contents_all_toggle(&mut self, cx: &mut Context<Self>) {
        if let Some(st) = &mut self.compare {
            st.content_all = !st.content_all;
        }
        cx.notify();
    }

    /// "Select differing": the differing items of the focused pane are selected, so the normal copy to the other pane works.
    pub(crate) fn compare_select_differing(&mut self, cx: &mut Context<Self>) {
        self.active_tab().clone().update(cx, |v, cx| v.select_differing(cx));
    }

    /// Keeps the compare right: it ends when its tab is no longer split into those two panes, else it compares again if a
    /// listing or place changed.
    fn compare_sync(&mut self, cx: &mut Context<Self>) {
        let Some(mut st) = self.compare.take() else { return };
        let alive = self.tabs.iter().any(|t| t.left == st.left && t.right.as_ref() == Some(&st.right));
        if alive {
            st.sync(cx);
            self.compare = Some(st);
        } else {
            st.clear(cx);
        }
    }

    /// Opens/closes the second pane of the current tab (it starts at the same folder).
    fn toggle_dual_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active;
        if self.tabs[ix].right.is_some() {
            self.tabs[ix].right = None;
            self.tabs[ix].right_active = false;
            self.tabs[ix].left.update(cx, |v, _| v.dual = false);
        } else {
            let loc = self.tabs[ix].left.read(cx).location().clone();
            let view = self.new_view(loc, window, cx);
            view.update(cx, |v, _| v.dual = true);
            self.tabs[ix].left.update(cx, |v, _| v.dual = true);
            self.tabs[ix].right = Some(view);
            self.tabs[ix].right_active = true;
        }
        self.sync_shown(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Names the pane that owns the settings page's lens: the focused pane of the shown tab when `owner`, else none.
    fn assign_settings_lens(&self, owner: bool, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        let focused = tab.view().clone();
        for v in std::iter::once(&tab.left).chain(tab.right.as_ref()) {
            let on = owner && *v == focused;
            if v.read(cx).lens_owner != on {
                v.update(cx, |v, _| v.lens_owner = on);
            }
        }
    }

    /// The pane next to `view` when its tab is split.
    fn other_pane(&self, view: &Entity<FolderView>) -> Option<Entity<FolderView>> {
        self.tabs.iter().find_map(|t| match &t.right {
            Some(r) if r == view => Some(t.left.clone()),
            Some(r) if &t.left == view => Some(r.clone()),
            _ => None,
        })
    }

    fn set_active_pane(&mut self, right: bool, cx: &mut Context<Self>) {
        let t = &mut self.tabs[self.active];
        if t.right.is_some() && t.right_active != right {
            t.right_active = right;
            cx.notify();
        }
    }

    /// Tells every folder view whether it is on screen (the panes of the active tab): the others do not run git for file changes.
    fn sync_shown(&self, cx: &mut Context<Self>) {
        let views: Vec<(Entity<FolderView>, bool)> =
            self.tabs.iter().enumerate().flat_map(|(i, t)| std::iter::once(&t.left).chain(t.right.as_ref()).map(move |v| (v.clone(), i == self.active))).collect();
        // (deferred: a view being updated right now cannot be updated from here)
        cx.defer(move |cx| {
            for (v, on) in views {
                v.update(cx, |v, cx| v.set_shown(on, cx));
            }
        });
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        self.sync_shown(cx);
        let q = self.active_tab().read(cx).filter.query.clone();
        self.search.update(cx, |s, cx| s.set_value(q, window, cx));
        self.editing_address = false;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // the active index may stay the same while the tab under it changes: re-scroll
        self.tab_scrolled_to = None;
        if self.tabs.len() <= 1 {
            self.save_session(window, cx);
            Self::release_preview_host(window);
            window.remove_window();
            return;
        }
        let tab = self.tabs.remove(ix);
        self.closed.push(tab.view().read(cx).location().clone());
        let next = if self.active > ix || self.active >= self.tabs.len() {
            self.active.saturating_sub(1)
        } else {
            self.active
        };
        self.activate(next.min(self.tabs.len() - 1), window, cx);
    }

    /// Opens the saved session `name` in this window: its tabs (with their split panes and its active tab) take the place of the ones
    /// that were open, which go to the closed tabs (Ctrl+Shift+T brings them back).
    fn take_named_session(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(named) = cx.global::<crate::settings::Settings>().saved_sessions.iter().find(|s| s.name == name).cloned() else { return };
        let old = self.tabs.len();
        for t in &named.tabs {
            self.open_tab(session_place(t), window, cx);
        }
        if self.tabs.len() == old {
            // (a session with no tab leaves the window as it is)
            return;
        }
        for (ix, loc, active) in panes_to_restore(&named.panes, self.tabs.len() - old) {
            self.open_second_pane(old + ix, loc, active, window, cx);
        }
        for i in (0..old).rev() {
            let tab = self.tabs.remove(i);
            self.closed.push(tab.view().read(cx).location().clone());
        }
        self.tab_scrolled_to = None;
        self.session_name = Some(named.name.clone());
        let a = named.active.min(self.tabs.len() - 1);
        self.activate(a, window, cx);
    }

    fn close_other_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keep = self.active;
        for i in (0..self.tabs.len()).rev() {
            if i != keep {
                let tab = self.tabs.remove(i);
                self.closed.push(tab.view().read(cx).location().clone());
            }
        }
        self.tab_scrolled_to = None;
        self.activate(0, window, cx);
    }

    /// A new tab asked for by the user (Ctrl+T, "+", the tab menu): Home, or the folder of the tab in front when the setting says so.
    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = (cx.global::<crate::settings::Settings>().new_tab_opens == crate::settings::NewTabOpens::Current)
            .then(|| self.tabs.get(self.active).map(|t| t.view().read(cx).location().clone()))
            .flatten();
        self.open_tab(here.unwrap_or(Location::Home), window, cx);
    }

    /// Closes every tab and leaves one at Home (the closed ones can be reopened, Ctrl+Shift+T).
    fn close_all_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.tabs.len();
        self.open_tab(Location::Home, window, cx);
        // (the new tab is the last one)
        for i in (0..old.min(self.tabs.len().saturating_sub(1))).rev() {
            let tab = self.tabs.remove(i);
            self.closed.push(tab.view().read(cx).location().clone());
        }
        self.tab_scrolled_to = None;
        self.activate(0, window, cx);
    }

    fn close_tabs_to_side(&mut self, ix: usize, right: bool, window: &mut Window, cx: &mut Context<Self>) {
        let range: Vec<usize> = if right { (ix + 1..self.tabs.len()).collect() } else { (0..ix).collect() };
        for i in range.into_iter().rev() {
            let tab = self.tabs.remove(i);
            self.closed.push(tab.view().read(cx).location().clone());
        }
        let ix = if right { ix } else { 0 };
        self.tab_scrolled_to = None;
        self.activate(ix.min(self.tabs.len() - 1), window, cx);
    }

    fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active_left = self.tabs[self.active].left.clone();
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        self.active = self.tabs.iter().position(|t| t.left == active_left).unwrap_or(0);
        self.sync_shown(cx);
        let _ = window;
        cx.notify();
    }

    /// Asks for a name and saves this window's tabs under it (`settings::NamedSession`); a name already taken is replaced.
    fn save_named_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tabs, panes) = self.session_now(cx);
        let active = self.active;
        let count = cx.global::<crate::settings::Settings>().saved_sessions.len();
        let first = self.session_name.clone().unwrap_or_else(|| format!("{} {}", t!("Session"), count + 1));
        let me = cx.entity().downgrade();
        crate::prompt::show(
            t!("Save session"),
            Some(t!("This window's tabs are saved under this name; Open session in the tab menu opens them in a new window.").to_string()),
            Some((first, String::new())),
            vec![
                crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, move |text, _, cx| {
                    let name = text.unwrap_or_default().trim().to_string();
                    if name.is_empty() {
                        return Err(t!("Type a name.").to_string());
                    }
                    let session = crate::settings::NamedSession { name: name.clone(), tabs: tabs.clone(), panes: panes.clone(), active };
                    crate::settings::update(cx, move |s| remember_named(&mut s.saved_sessions, session));
                    // (the window now holds that session: its name goes beside the tabs)
                    let _ = me.update(cx, |ws, cx| {
                        ws.session_name = Some(name.clone());
                        cx.notify();
                    });
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }

    fn tab_menu(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let me = cx.entity().downgrade();
        let act = |f: fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>)| {
            let me = me.clone();
            move |w: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |ws, cx| f(ws, ix, w, cx));
            }
        };
        let n = self.tabs.len();
        let items = vec![
            Item::action(t!("New tab"), MenuIcon::Lucide(L::Plus), act(|ws, _, w, cx| ws.open_new_tab(w, cx))).hint("Ctrl+T"),
            Item::action(t!("Duplicate tab"), MenuIcon::Lucide(L::Copy), act(|ws, ix, w, cx| {
                if let Some(loc) = ws.tabs.get(ix).map(|t| t.view().read(cx).location().clone()) {
                    ws.open_tab(loc, w, cx);
                }
            }))
            .hint("Ctrl+Shift+K"),
            Item::action(t!("Move to new window"), MenuIcon::Lucide(L::AppWindow), act(|ws, ix, w, cx| ws.detach_tab(ix, w, cx))).enabled(n > 1),
            Item::Separator,
            Item::action(t!("Close other tabs"), MenuIcon::None, act(|ws, ix, w, cx| {
                ws.active = ix;
                ws.close_other_tabs(w, cx)
            }))
            .enabled(n > 1),
            Item::action(t!("Close tabs to the left"), MenuIcon::None, act(|ws, ix, w, cx| ws.close_tabs_to_side(ix, false, w, cx))).enabled(ix > 0),
            Item::action(t!("Close tabs to the right"), MenuIcon::None, act(|ws, ix, w, cx| ws.close_tabs_to_side(ix, true, w, cx))).enabled(ix + 1 < n),
            Item::action(t!("Close all tabs"), MenuIcon::None, act(|ws, _, w, cx| ws.close_all_tabs(w, cx))),
            Item::action(t!("Close tab"), MenuIcon::Lucide(L::X), act(|ws, ix, w, cx| ws.close_tab(ix, w, cx))).hint("Ctrl+W"),
            Item::Separator,
            Item::action(t!("Reopen closed tab"), MenuIcon::Lucide(L::RotateCcw), act(|ws, _, w, cx| {
                if let Some(loc) = ws.closed.pop() {
                    ws.open_tab(loc, w, cx);
                }
            }))
            .hint("Ctrl+Shift+T")
            .enabled(!self.closed.is_empty()),
            Item::Separator,
            Item::action(t!("Save session…"), MenuIcon::Lucide(L::Save), act(|ws, _, w, cx| ws.save_named_session(w, cx))),
        ];
        // the saved sessions: each opens in a new window; a second list removes one
        let saved: Vec<String> = cx.global::<crate::settings::Settings>().saved_sessions.iter().map(|s| s.name.clone()).collect();
        let mut items = items;
        if !saved.is_empty() {
            let open: Vec<Item> = saved
                .iter()
                .map(|n| {
                    let n = n.clone();
                    let me = me.clone();
                    Item::action(n.clone(), MenuIcon::None, move |w, cx| {
                        // (in this window when the setting says so: issue 41)
                        if cx.global::<crate::settings::Settings>().sessions_in_this_window
                            && let Some(ws) = me.upgrade()
                        {
                            ws.update(cx, |ws, cx| ws.take_named_session(&n, w, cx));
                        } else {
                            open_named_session(&n, cx);
                        }
                    })
                })
                .collect();
            let remove: Vec<Item> = saved
                .iter()
                .map(|n| {
                    let n = n.clone();
                    Item::action(n.clone(), MenuIcon::None, move |_, cx| {
                        let n = n.clone();
                        crate::settings::update(cx, move |s| s.saved_sessions.retain(|x| x.name != n));
                    })
                })
                .collect();
            items.push(Item::sub(t!("Open session"), MenuIcon::Lucide(L::FolderOpen), open));
            items.push(Item::sub(t!("Delete session"), MenuIcon::Lucide(L::Trash), remove));
        }
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    /// Breadcrumb chevron: pick a subfolder of `dir` (or a drive when `dir` is None).
    fn crumb_menu(&mut self, dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let is_drives = dir.is_none();
        // List on a worker (can stall on network/optical drives); the shell thread only shows the menu.
        let listing = cx.background_spawn(async move {
            let mut entries: Vec<(String, PathBuf)> = match &dir {
                None => leaf_sys::drives::visible_drives().into_iter().map(|d| (crate::places::drive_name(&d), d.root)).collect(),
                Some(d) => {
                    let mut v = Vec::new();
                    let _ = leaf_sys::list_dir(d, 4096, &|| false, &mut |b| {
                        for e in b {
                            if e.is_dir() && !e.is_hidden() {
                                v.push((e.name.to_string(), d.join(&*e.name)));
                            }
                        }
                    });
                    v.sort_by(|a, b| rf_core::sort::natural_cmp(&a.0, &b.0));
                    v
                }
            };
            entries.truncate(80);
            entries
        });
        let pos = window.mouse_position() + point(px(-12.), px(14.));
        cx.spawn_in(window, async move |this, cx| {
            use crate::fluent_menu::{Item, Menu, MenuIcon};
            let entries = listing.await;
            let _ = this.update_in(cx, |_, window, cx| {
                let me = cx.entity().downgrade();
                let folder = MenuIcon::Tinted(L::Folder, crate::theme::kind_color(crate::icons::Category::Folder, true));
                let items: Vec<Item> = if entries.is_empty() {
                    vec![Item::action(t!("(no subfolders)"), MenuIcon::None, |_, _| {}).enabled(false)]
                } else {
                    entries
                        .into_iter()
                        .map(|(n, path)| {
                            let me = me.clone();
                            let icon = if is_drives { MenuIcon::Lucide(L::HardDrive) } else { folder.clone() };
                            Item::action(n, icon, move |w, cx| {
                                let p = path.clone();
                                let _ = me.update(cx, |ws, cx| ws.navigate(Location::Path(p), w, cx));
                            })
                        })
                        .collect()
                };
                crate::fluent_menu::show(Menu { quick: Vec::new(), items }, pos, window, cx);
            });
        })
        .detach();
    }

    /// Boşluk: leaf-look'a (ayrı süreç) aç/kapat iletisi. leaf-look kapalıyken bir şey yapmaz.
    fn toggle_quick_look(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::Ordering;
        // a file on a server: a copy is fetched (up to 64 MB), then leaf-look shows that
        if cx.global::<crate::settings::Settings>().look_enabled
            && !crate::folder_view::QUICK_LOOK_OPEN.load(Ordering::Relaxed)
            && let Some((v, size)) = self.active_tab().read(cx).remote_look_target()
        {
            crate::remote::look(v, size);
            crate::folder_view::QUICK_LOOK_OPEN.store(true, Ordering::Relaxed);
            cx.notify();
            return;
        }
        // Settings › leaf-look › "Space opens QuickLook": the file goes to QuickLook (a file inside an archive has no path it can open)
        if cx.global::<crate::settings::Settings>().space_quicklook {
            match self.active_tab().read(cx).preview_target() {
                Some((path, _)) if crate::archive_view::split_inside(&path).is_none() => crate::quicklook::space(path),
                target => log::info!("quicklook: Space with nothing QuickLook can open ({:?})", target.map(|(p, _)| p)),
            }
            return;
        }
        if !cx.global::<crate::settings::Settings>().look_enabled {
            return;
        }
        let open = crate::folder_view::QUICK_LOOK_OPEN.load(Ordering::Relaxed);
        let target = self.active_tab().read(cx).preview_target().map(|(p, e)| (p, e.size));
        match (open, target) {
            (true, _) => {
                crate::look::ipc::send(crate::look::ipc::Msg::Close, false);
                crate::folder_view::QUICK_LOOK_OPEN.store(false, Ordering::Relaxed);
                *LOOK_FOLLOWED.lock().unwrap() = None;
            }
            (false, Some((path, size))) => {
                *LOOK_FOLLOWED.lock().unwrap() = Some(path.clone());
                // an item inside an archive is extracted first
                crate::archive_view::look_send(path, size, false);
                crate::folder_view::QUICK_LOOK_OPEN.store(true, Ordering::Relaxed);
            }
            (false, None) => {}
        }
        cx.notify();
    }

    fn release_drive(&mut self, letter: char, cx: &mut Context<Self>) {
        let views: Vec<Entity<FolderView>> =
            self.tabs.iter().flat_map(|t| std::iter::once(t.left.clone()).chain(t.right.clone())).collect();
        let mut moved = false;
        for v in views {
            let here = v.read(cx).location().path().is_some_and(|p| on_drive(p, letter));
            if here {
                v.update(cx, |v, cx| v.navigate(Location::Home, cx));
                moved = true;
            }
        }
        self.close_quick_look(cx);
        self.preview.update(cx, |pv, cx| pv.release_drive(letter, cx));
        self.images.update(cx, |c, _| c.cancel_pending());
        if moved {
            log::info!("{letter}: is being removed; its panes went Home");
        }
        cx.notify();
    }

    fn close_quick_look(&mut self, cx: &mut Context<Self>) {
        if let Some(ql) = self.quick_look.take() {
            // release the hosted preview window before the pane takes it back
            ql.update(cx, |pv, _| pv.set_occluded(true));
        }
        crate::folder_view::QUICK_LOOK_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
        cx.notify();
    }

    fn open_bulk_rename(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::bulk_rename::{BulkRename, BulkRenameEvent};
        if paths.len() < 2 {
            return;
        }
        let br = cx.new(|cx| BulkRename::new(paths, window, cx));
        self._bulk_sub = Some(cx.subscribe_in(&br, window, |this, _, ev: &BulkRenameEvent, window, cx| match ev {
            BulkRenameEvent::Close => {
                this.bulk = None;
                this._bulk_sub = None;
                this.with_tab(cx, |v, cx| v.soft_reload(cx));
                this.focus_active(window, cx);
                cx.notify();
            }
        }));
        self.bulk = Some(br);
        cx.notify();
    }

    /// The command palette (Ctrl+Shift+P), with past folders among the commands unless the setting turned that off.
    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let with_folders = cx.global::<crate::settings::Settings>().palette_folder_history;
        self.toggle_palette(crate::command_palette::PaletteMode::Commands { with_folders }, window, cx)
    }

    /// Opens the palette listing what `mode` says, or closes it when it is open.
    fn toggle_palette(&mut self, mode: crate::command_palette::PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        use crate::command_palette::{CommandPalette, PaletteEvent, PaletteMode};
        if self.palette.take().is_some() {
            self._palette_sub = None;
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        let folders = match mode {
            PaletteMode::Commands { with_folders: false } => Vec::new(),
            _ => crate::folder_history::snapshot(),
        };
        // the tab search: this window's open tabs, then the closed ones, the last closed first
        let tabs = if mode == PaletteMode::Tabs {
            use crate::command_palette::TabEntry;
            let open = self.tabs.iter().enumerate().map(|(i, t)| {
                let v = t.view().read(cx);
                let detail = v.location().path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                TabEntry { label: v.title(cx), detail, closed: false, id: i }
            });
            let closed = self.closed.iter().enumerate().rev().take(30).map(|(i, loc)| TabEntry {
                label: loc.title(),
                detail: loc.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                closed: true,
                id: i,
            });
            open.chain(closed).collect()
        } else {
            Vec::new()
        };
        let tabs = if mode == PaletteMode::Worktrees { self.wt_entries() } else { tabs };
        let pal = cx.new(|cx| CommandPalette::new(mode, folders, tabs, window, cx));
        self._palette_sub = Some(cx.subscribe_in(&pal, window, |this, _, ev: &PaletteEvent, window, cx| {
            this.palette = None;
            this._palette_sub = None;
            this.focus_active(window, cx);
            match ev {
                PaletteEvent::Dismiss => {}
                PaletteEvent::Run(action) => window.dispatch_action(action.boxed_clone(), cx),
                PaletteEvent::Navigate(p) => {
                    if let Some(loc) = parse_location(&p.to_string_lossy()) {
                        this.navigate(loc, window, cx);
                    }
                }
                PaletteEvent::Tab { closed: false, id } => {
                    if *id < this.tabs.len() {
                        this.activate(*id, window, cx);
                    }
                }
                PaletteEvent::Tab { closed: true, id } => {
                    if *id < this.closed.len() {
                        let loc = this.closed.remove(*id);
                        this.open_tab(loc, window, cx);
                    }
                }
            }
            cx.notify();
        }));
        self.palette = Some(pal);
        cx.notify();
    }

    /// The "Filter by" strip: typing filters this folder by name; closing clears the filter.
    fn toggle_filter_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_bar = !self.filter_bar;
        if self.filter_bar {
            self.filter_input.update(cx, |s, cx| s.focus(window, cx));
        } else {
            self.filter_input.update(cx, |s, cx| s.set_value("", window, cx));
            self.with_tab(cx, |v, cx| v.set_query(String::new(), cx));
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn render_filter_bar(&mut self, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.filter_bar {
            return None;
        }
        // follow the folder: entering another folder clears its filter
        let q = self.active_tab().read(cx).filter.query.clone();
        if self.filter_input.read(cx).value().as_ref() != q.as_str() {
            self.filter_input.update(cx, |s, cx| s.set_value(q, window, cx));
        }
        Some(
            h_flex()
                .px_4()
                .py_2()
                .gap_3()
                .items_center()
                .border_b_1()
                .border_color(p.divider)
                .child(Icon::new(L::Folder).size(px(16.)).text_color(crate::theme::kind_color(crate::icons::Category::Folder, true)))
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(t!("Filter by")))
                .child(
                    div()
                        .w(px(200.))
                        .h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(theme::RADIUS_SM)
                        .bg(p.hover)
                        .border_1()
                        .border_color(p.border)
                        .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                            if ev.keystroke.key == "escape" {
                                this.toggle_filter_bar(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(Input::new(&self.filter_input).appearance(false).cleanable(true)),
                )
                .into_any_element(),
        )
    }

    fn navigate(&mut self, loc: Location, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab().clone().update(cx, |v, cx| v.navigate(loc, cx));
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.focus_active(window, cx);
    }

    fn toggle_shelf(&mut self, cx: &mut Context<Self>) {
        self.shelf_open = !self.shelf_open;
        if self.shelf_open {
            self.shelf.update(cx, |s, cx| s.prune(cx));
        }
        cx.notify();
    }

    fn on_shelf_event(&mut self, ev: &crate::shelf_view::ShelfEvent, window: &mut Window, cx: &mut Context<Self>) {
        use crate::shelf_view::ShelfEvent;
        match ev {
            ShelfEvent::Transfer(kind, items) => {
                let dest = match self.active_tab().read(cx).location() {
                    Location::Path(p)
                        if !crate::remote::is_address_path(p) && leaf_sys::network::server_root(p).is_none() =>
                    {
                        Some(p.clone())
                    }
                    _ => None,
                };
                if let Some(dest) = dest {
                    crate::status_center::transfer(cx, *kind, items.clone(), dest);
                    if *kind == leaf_sys::fileops::Transfer::Move {
                        // moved items leave their old place; the shelf is done with them
                        self.shelf.update(cx, |s, cx| s.clear(cx));
                    }
                }
            }
            ShelfEvent::Reveal(p) => {
                if let Some(parent) = p.parent() {
                    self.navigate(Location::Path(parent.to_path_buf()), window, cx);
                }
            }
            ShelfEvent::Close => {
                self.shelf_open = false;
                cx.notify();
            }
        }
    }

    /// The corner button's menu, like Files' "Tab actions": new window, compact overlay,
    /// split / arrange / close panes — plus the sidebar toggle that used to live there.
    fn tab_actions_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let me = cx.entity().downgrade();
        let with = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
            let me = me.clone();
            move |w: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |ws, cx| f(ws, w, cx));
            }
        };
        let split = self.tabs[self.active].right.is_some();
        let vertical = cx.global::<crate::settings::Settings>().dual_pane_vertical;
        let mut items = vec![
            Item::action(t!("New window"), MenuIcon::Lucide(L::AppWindow), |w, cx| w.dispatch_action(Box::new(NewWindow), cx)).hint("Ctrl+N"),
            if self.compact.is_some() {
                Item::action(t!("Exit compact overlay"), MenuIcon::Lucide(L::Maximize2), with(|ws, w, cx| ws.toggle_compact(w, cx)))
            } else {
                Item::action(t!("Enter compact overlay"), MenuIcon::Lucide(L::PictureInPicture2), with(|ws, w, cx| ws.toggle_compact(w, cx)))
            },
        ];
        if split {
            items.push(Item::sub(
                t!("Arrange panes"),
                MenuIcon::Lucide(L::LayoutPanelLeft),
                vec![
                    Item::action(t!("Side by side"), MenuIcon::Lucide(L::Columns2), with(|ws, w, cx| ws.arrange_panes(true, w, cx))).checked(vertical),
                    Item::action(t!("Stacked"), MenuIcon::Lucide(L::Rows2), with(|ws, w, cx| ws.arrange_panes(false, w, cx))).checked(!vertical),
                ],
            ));
            items.push(Item::action(t!("Close active pane"), MenuIcon::Lucide(L::X), with(|ws, w, cx| ws.close_active_pane(w, cx))));
        } else {
            items.push(Item::sub(
                t!("Split pane"),
                MenuIcon::Lucide(L::LayoutPanelLeft),
                vec![
                    Item::action(t!("Side by side"), MenuIcon::Lucide(L::Columns2), with(|ws, w, cx| ws.split_pane(true, w, cx))).hint("Ctrl+Shift+D"),
                    Item::action(t!("Stacked"), MenuIcon::Lucide(L::Rows2), with(|ws, w, cx| ws.split_pane(false, w, cx))),
                ],
            ));
        }
        items.push(Item::Separator);
        items.push(
            Item::action(t!("Sidebar"), MenuIcon::Lucide(L::PanelLeft), |w, cx| w.dispatch_action(Box::new(ToggleSidebar), cx))
                .checked(self.sidebar_open)
                .hint("Ctrl+B"),
        );
        let tree_on = cx.global::<crate::settings::Settings>().sidebar_tree;
        items.push(Item::action(t!("Folder tree"), MenuIcon::Lucide(L::FolderTree), |_, cx| crate::settings::update(cx, |s| s.sidebar_tree = !s.sidebar_tree)).checked(tree_on));
        // where the preview pane sits (Alt+P shows and hides it)
        {
            let preview_pos = cx.global::<crate::settings::Settings>().preview_position;
            for (pos, label, icon) in [(rf_core::previewpane::PreviewPos::Right, t!("Preview on the right"), L::PanelRight), (rf_core::previewpane::PreviewPos::Bottom, t!("Preview at the bottom"), L::PanelBottom)] {
                items.push(Item::action(label, MenuIcon::Lucide(icon), move |_, cx| preview_dock::set_preview_position(cx, pos)).checked(preview_pos == pos));
            }
        }
        let pos = point(px(6.), theme::TITLEBAR_H);
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, pos, window, cx);
    }

    fn split_pane(&mut self, side_by_side: bool, window: &mut Window, cx: &mut Context<Self>) {
        crate::settings::update(cx, |s| s.dual_pane_vertical = side_by_side);
        if self.tabs[self.active].right.is_none() {
            self.toggle_dual_pane(window, cx);
        }
    }

    fn arrange_panes(&mut self, side_by_side: bool, _: &mut Window, cx: &mut Context<Self>) {
        crate::settings::update(cx, |s| s.dual_pane_vertical = side_by_side);
        cx.notify();
    }

    /// Closes the focused pane of a split tab; the other one stays.
    fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let t = &mut self.tabs[self.active];
        let Some(right) = t.right.take() else { return };
        if !t.right_active {
            t.left = right;
        }
        t.right_active = false;
        t.left.update(cx, |v, _| v.dual = false);
        self.sync_shown(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The preview pane shows documents in a window of the shell thread that this window owns. Destroying this window would wait
    /// for that thread to destroy it: the picker of an app's Open dialog hung on the screen with the PDF it previewed (issue 146).
    /// So the host is let go first, whenever this window is about to close.
    pub(crate) fn release_preview_host(window: &Window) {
        if let Some(owner) = Self::hwnd(window) {
            // (without waiting for the shell thread: it may be the one that is busy, see `detach_from`)
            leaf_sys::preview_host::detach_from(owner);
        }
    }

    fn hwnd(window: &Window) -> Option<isize> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
            _ => None,
        }
    }

    /// Files' "compact overlay": a small always-on-top window with just the file list (sidebar
    /// and preview hidden); leaving restores the previous size, position and z-order.
    fn toggle_compact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = Self::hwnd(window) else { return };
        // The window is moved and resized from a TASK, never from inside this update. These calls deliver the new size and place at
        // once, while the app is still borrowed, and GPUI then drops them ("handling resize ...: RefCell already borrowed" in the
        // log): the window changes, but the content, and with it every place a click is tested against, stays at the old size
        // until the next resize. That is one way a click on the close button could land on something else (issue 31: "after ...
        // toggling window states ... the first click forces it to exit fullscreen / resize down"). `look/window.rs` found the same
        // with its own window.
        if let Some(r) = self.compact.take() {
            let (rect, maximized, topmost) = (r.rect, r.maximized, r.topmost);
            cx.spawn(async move |_, _| {
                leaf_sys::window::set_min_size(hwnd, None);
                leaf_sys::window::place(hwnd, rect);
                if maximized {
                    leaf_sys::window::maximize(hwnd);
                }
                if !topmost {
                    let _ = leaf_sys::window::set_topmost(hwnd, false);
                }
            })
            .detach();
        } else {
            let Some((rect, maximized)) = leaf_sys::window::rect(hwnd) else { return };
            // restore to the normal (not maximized) rectangle later
            let normal = if maximized { None } else { Some(rect) };
            let topmost = leaf_sys::window::is_topmost(hwnd);
            let scale = window.scale_factor();
            let (w, h) = ((COMPACT_W * scale) as i32, (COMPACT_H * scale) as i32);
            // keep the top-right corner where it was: the overlay hugs the side it came from
            let (right, top) = (rect[2], rect[1].max(0));
            let restore = normal.unwrap_or_else(|| {
                let b = window.bounds();
                let o = |v: Pixels| (f32::from(v) * scale) as i32;
                [o(b.origin.x), o(b.origin.y), o(b.origin.x + b.size.width), o(b.origin.y + b.size.height)]
            });
            let small = [right - w, top, right, top + h];
            cx.spawn(async move |_, _| {
                leaf_sys::window::set_min_size(hwnd, Some((COMPACT_MIN.0, COMPACT_MIN.1)));
                leaf_sys::window::place(hwnd, small);
                let _ = leaf_sys::window::set_topmost(hwnd, true);
            })
            .detach();
            self.compact = Some(CompactRestore { rect: restore, maximized, topmost });
        }
        cx.notify();
    }

    fn toggle_always_on_top(&mut self, window: &mut Window) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = window.window_handle() {
            if let RawWindowHandle::Win32(w) = h.as_raw() {
                let hwnd = w.hwnd.get();
                let on = !leaf_sys::window::is_topmost(hwnd);
                if let Err(e) = leaf_sys::window::set_topmost(hwnd, on) {
                    log::warn!("always on top: {e}");
                }
            }
        }
    }

    fn with_tab(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut FolderView, &mut Context<FolderView>)) {
        self.active_tab().clone().update(cx, f);
    }

    fn clear_suggestions(&mut self) {
        self.addr_suggest.clear();
        self.addr_sel = None;
        self._addr_task = None;
    }

    /// Folder completions for the address text, computed off the UI thread (network paths can
    /// be slow); stale answers are dropped with the task.
    fn update_suggestions(&mut self, text: String, cx: &mut Context<Self>) {
        // "@pro": the matching folder aliases
        if text.trim_start().starts_with(crate::aliases::PREFIX) && !text.contains(['\\', '/']) {
            self._addr_task = None;
            self.addr_suggest = crate::aliases::matches(&text).into_iter().map(|(_, p)| p).take(8).collect();
            self.addr_sel = None;
            cx.notify();
            return;
        }
        let expanded = expand_env(text.trim());
        if !leaf_sys::complete::completable(&expanded) {
            self.clear_suggestions();
            cx.notify();
            return;
        }
        let hidden = cx.global::<crate::settings::Settings>().show_hidden;
        let work = cx.background_spawn(async move { leaf_sys::complete::complete_dirs(&expanded, 8, hidden) });
        self._addr_task = Some(cx.spawn(async move |this, cx| {
            let list = work.await;
            let _ = this.update(cx, |ws, cx| {
                if ws.editing_address {
                    ws.addr_suggest = list;
                    ws.addr_sel = None;
                    cx.notify();
                }
            });
        }));
    }

    /// Tab (arrives as the input's unhandled `IndentInline`): takes the highlighted (or first)
    /// completion into the text and lists its children.
    fn accept_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_address && self.addr_suggest.is_empty() {
            // no suggestion to take: Tab goes on to the next pane, as it does from the list (issue 38)
            self.cycle_focus(true, window, cx);
            return;
        }
        if !self.editing_address || self.addr_suggest.is_empty() {
            cx.propagate();
            return;
        }
        let pick = self.addr_sel.unwrap_or(0);
        if let Some(p) = self.addr_suggest.get(pick).cloned() {
            let text = format!("{}{}", p.to_string_lossy().trim_end_matches(rf_core::pathkey::SEP), rf_core::pathkey::SEP);
            self.address.update(cx, |s, cx| s.set_value(text.clone(), window, cx));
            self.update_suggestions(text, cx);
        }
    }

    /// Up/Down move the highlight; Escape closes the list.
    fn address_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing_address || self.addr_suggest.is_empty() {
            return;
        }
        let n = self.addr_suggest.len();
        match ev.keystroke.key.as_str() {
            "down" => {
                self.addr_sel = Some(self.addr_sel.map_or(0, |i| (i + 1) % n));
                cx.stop_propagation();
                cx.notify();
            }
            "up" => {
                self.addr_sel = Some(self.addr_sel.map_or(n - 1, |i| (i + n - 1) % n));
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                self.clear_suggestions();
                cx.notify();
            }
            _ => {}
        }
    }

    /// Searches the active tab's folder and its subfolders for `q`, and remembers the search.
    fn run_search(&mut self, q: String, window: &mut Window, cx: &mut Context<Self>) {
        let root = match self.active_tab().read(cx).location() {
            Location::Search { root, .. } => Some(root.clone()),
            l => l.path().map(Path::to_path_buf),
        };
        let (false, Some(root)) = (q.is_empty(), root) else { return };
        crate::settings::update(cx, |s| remember_search(&mut s.search_history, &q));
        self.active_tab().clone().update(cx, |v, cx| v.navigate(Location::Search { root, query: q }, cx));
        self.focus_active(window, cx);
    }

    /// Under the empty search box: the searches run before (a click runs one again).
    fn render_search_history(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.search_mode {
            return None;
        }
        let value = self.search.read(cx).value().to_string();
        let history = if value.trim().is_empty() { cx.global::<crate::settings::Settings>().search_history.clone() } else { Vec::new() };
        let mut list = v_flex().absolute().top(px(38.)).left_0().right_0().py_1().rounded(theme::RADIUS).bg(p.card_bg).border_1().border_color(p.border).shadow_lg();
        // the "Modified" chips: they write (or take out) the query's `modified:` word
        let now = chrono::Local::now().date_naive();
        let chosen = rf_core::search_date::current(&value, now);
        let mut chips = h_flex().px_3().py_1().gap_2().items_center().text_sm().child(div().text_color(p.text_secondary).child(t!("Modified")));
        for (i, (word, label)) in [(None, t!("Any")), (Some("today"), t!("Today")), (Some("week"), t!("This week")), (Some("month"), t!("This month"))].into_iter().enumerate() {
            let on = chosen.as_deref() == word;
            let value = value.clone();
            chips = chips.child(
                div()
                    .id(("search-modified", i))
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.selection))
                    .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            let q = rf_core::search_date::with_modified(&value, word);
                            this.search.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
                            this.run_search(q, window, cx);
                            cx.notify();
                        }),
                    ),
            );
        }
        list = list.child(chips);
        // "Look in": names, file contents (plain text) or file contents (regular expression): writes the query's `content:` / `regex:` word
        {
            use rf_core::content_search::{Mode, mode_of, with_mode};
            let mode = mode_of(&value).0;
            let mut modes = h_flex().px_3().py_1().gap_2().items_center().text_sm().child(div().text_color(p.text_secondary).child(t!("Look in")));
            for (i, (m, label)) in [(Mode::Names, t!("Names")), (Mode::Text, t!("Contents")), (Mode::Regex, t!("Contents (regex)"))].into_iter().enumerate() {
                let on = mode == m;
                let value = value.clone();
                modes = modes.child(
                    div()
                        .id(("search-mode", i))
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(theme::RADIUS_SM)
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|s| s.bg(p.hover)))
                        .child(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let q = with_mode(&value, m);
                                this.search.update(cx, |s, cx| s.set_value(q.clone(), window, cx));
                                this.run_search(q, window, cx);
                                cx.notify();
                            }),
                        ),
                );
            }
            list = list.child(modes);
        }
        let row = |id: ElementId| h_flex().id(id).mx_1().px_2().h(px(28.)).gap_2().items_center().rounded(theme::RADIUS_SM).text_sm().hover(|s| s.bg(p.hover)).cursor_pointer();
        for (i, q) in history.into_iter().enumerate() {
            let query = q.clone();
            list = list.child(
                row(("search-hist", i).into())
                    .child(Icon::new(L::Clock).size(px(14.)).text_color(p.text_secondary))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(q))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.search.update(cx, |s, cx| s.set_value(query.clone(), window, cx));
                            this.run_search(query.clone(), window, cx);
                            cx.notify();
                        }),
                    ),
            );
        }
        list = list.child(
            row("search-hist-clear".into())
                .text_color(p.text_secondary)
                .child(Icon::new(L::Trash).size(px(14.)))
                .child(t!("Clear previous searches"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| {
                        cx.stop_propagation();
                        crate::settings::update(cx, |s| s.search_history.clear());
                        cx.notify();
                    }),
                ),
        );
        Some(deferred(list).with_priority(2).into_any_element())
    }

    fn render_suggestions(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.editing_address || self.addr_suggest.is_empty() {
            return None;
        }
        let alias_mode = self.address.read(cx).value().trim_start().starts_with(crate::aliases::PREFIX);
        let mut list = v_flex()
            .absolute()
            .top(px(38.))
            .left_0()
            .right_0()
            .py_1()
            .rounded(theme::RADIUS)
            .bg(p.card_bg)
            .border_1()
            .border_color(p.border)
            .shadow_lg();
        for (i, path) in self.addr_suggest.iter().enumerate() {
            let sel = self.addr_sel == Some(i);
            let target = path.clone();
            list = list.child(
                h_flex()
                    .id(("addr-sug", i))
                    .mx_1()
                    .px_2()
                    .h(px(28.))
                    .gap_2()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .when(sel, |d| d.bg(p.selection))
                    .hover(|s| s.bg(p.hover))
                    .cursor_pointer()
                    .child(Icon::new(L::Folder).size(px(14.)).text_color(p.accent))
                    .children(alias_mode.then(|| crate::aliases::name_of(path)).flatten().map(|n| {
                        div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(format!("@{n}"))
                    }))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().when(alias_mode, |d| d.text_color(p.text_secondary)).child(path.to_string_lossy().into_owned()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.clear_suggestions();
                            this.editing_address = false;
                            this.navigate(Location::Path(target.clone()), window, cx);
                            this.focus_active(window, cx);
                            cx.notify();
                        }),
                    ),
            );
        }
        Some(deferred(list).with_priority(2).into_any_element())
    }

    fn begin_address_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = match self.active_tab().read(cx).location() {
            Location::Path(p) => p.to_string_lossy().into_owned(),
            Location::Search { root, .. } => root.to_string_lossy().into_owned(),
            _ => String::new(),
        };
        self.editing_address = true;
        self.address.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        cx.notify();
    }

    // ---------- rendering ----------

    /// A menu, flyout or the Status Center is open: buttons under it show no tooltips (one
    /// would be painted over the popup).
    fn popups_open(&self, cx: &App) -> bool {
        crate::fluent_menu::is_open(cx) || crate::flyout::is_open(cx) || self.ops.read(cx).open
    }

    fn render_titlebar(&mut self, p: &Palette, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let popups_open = self.popups_open(cx);
        // (pill tabs float in the middle of the strip, the others stand on its bottom edge)
        let pill = p.tab_shape == "pill";
        let at_home = *self.active_tab().read(cx).location() == Location::Home;
        // the saved session this window holds, at the far left of the tabs
        let session_label = self.session_name.clone().filter(|n| cx.global::<crate::settings::Settings>().saved_sessions.iter().any(|s| &s.name == n)).map(|name| {
            h_flex()
                .id("session-name")
                .flex_shrink_0()
                .max_w(px(180.))
                .h(px(26.))
                .mb(px(3.))
                .ml_1()
                .px_2()
                .gap_1p5()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .bg(p.hover)
                .border_1()
                .border_color(p.border)
                .text_xs()
                .text_color(p.text_secondary)
                .when(!popups_open, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Saved session")).build(w, cx)))
                .child(Icon::new(L::Layers).size(px(13.)).text_color(p.accent))
                .child(div().min_w_0().truncate().child(name))
        });
        let mut tabs = h_flex()
            .id("tabs")
            // content-sized, shrinks (and scrolls) only when there is not enough room, so the
            // "+" stays right after the last tab
            .flex_shrink(1.)
            .min_w_0()
            .h_full()
            .when(pill, |d| d.items_center())
            .when(!pill, |d| d.items_end())
            .when(p.tab_shape != "card", |d| d.gap_1())
            .pl_1()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        if self.tab_scrolled_to != Some(self.active) {
            // only when the active tab changes, so manual scrolling is not fought every frame
            self.tab_scrolled_to = Some(self.active);
            self.tab_scroll.scroll_to_item(self.active);
        }
        // Equal-width tabs that share the strip (like Files / WinUI TabView "Equal"): wide
        // while there are few, shrinking as more open, scrolling only below the minimum.
        // (the right-hand controls take their room first, in a window wide enough for them)
        let prefs_shown = self.compact.is_none() && self.viewport_w >= TITLEBAR_PREFS_MIN_W;
        let prefs_w = if prefs_shown { crate::titlebar_prefs::width(cx) } else { 0. };
        let strip = (self.viewport_w - 150. - if pill { TITLEBAR_LEAD_PILL } else { TITLEBAR_LEAD } - 40. - 40. - prefs_w).max(120.);
        let n = self.tabs.len().max(1) as f32;
        let tab_max = p.tab_w.max(TAB_MIN_W);
        let tab_w = ((strip - 4.) / n - 4.).clamp(TAB_MIN_W, tab_max);
        // Settings > Appearance > "Tabs as wide as their names" (issue 119): each tab its measured name plus its icon and buttons, within
        // the same limits; the buttons are in that width, so they always show
        let fit_names = cx.global::<crate::settings::Settings>().tabs_fit_names;
        // Settings > Appearance > "Terminal button on tabs" (issue 119: off, nothing is in the way of a click on a narrow tab)
        let tab_terminal = cx.global::<crate::settings::Settings>().tab_terminal_button;
        let (name_px, name_style) = (rems(0.875).to_pixels(window.rem_size()), window.text_style());
        let roomy = fit_names || tab_w >= 110.;
        let mut tab_claimed = false;
        for (i, tab) in self.tabs.iter().enumerate() {
            let v = tab.view().read(cx);
            let name = v.title(cx);
            let tab_full = cx.global::<crate::settings::Settings>().tab_full_path;
            // (the room in characters for a full path, about 7 px each; the tab's own width is only known further down)
            let room_w = if fit_names { tab_max } else { tab_w };
            let title = crate::full_path::tab_text(v.location(), &name, tab_full, ((room_w - TAB_CHROME_W - if tab_terminal { TAB_TERMINAL_W } else { 0. }) / 7.6).max(8.) as usize);
            let tab_tip = crate::full_path::path_text(v.location()).unwrap_or_else(|| name.clone());
            let icon = v.tab_icon();
            // a tab whose folder is still being listed shows a spinner ring in place of its icon (the mockups' busy tab)
            let busy = v.busy();
            let tab_dir = v.location().path().map(Path::to_path_buf);
            // (D2) a coding agent session works in this tab's folder or one inside it: a calm dot after the name
            let tab_agent = tab_dir.as_deref().and_then(|d| crate::agents::tab_state(cx, d)).map(|st| crate::agents::state_color(st, &p));
            // (D6) a tab in a lane wears the lane's colour as a short line along its foot
            let tab_lane = tab_dir.as_deref().and_then(|d| crate::lanes::color_of_dir(cx, d, &p));
            // (D9) the session ribbon: one segment per agent session of this folder, along the head of the tab
            let tab_ribbon = tab_dir.as_deref().and_then(|d| crate::agent_usage::ribbon(cx, d, &p));
            let term_dir = tab_terminal.then(|| tab_terminal_dir(v.location())).flatten();
            let tab_w = if fit_names {
                let name = window.text_system().layout_line(&title, name_px, &[name_style.to_run(title.len())], None).width;
                (f32::from(name) + TAB_CHROME_W + if term_dir.is_some() { TAB_TERMINAL_W } else { 0. }).clamp(TAB_MIN_W, tab_max)
            } else {
                tab_w
            };
            let active = i == self.active;
            let drag = TabDrag { ix: i, title: title.clone().into() };
            let accent = p.accent;
            let drop_bg = p.selection;
            // Over glass the selected tab is a lens the engine draws behind the scene (the tab's rectangle made taller downwards by its
            // corner, cut flat by the strip) instead of the Fluent fill. A stable key per tab, so another selection slides the lens.
            let tab_lens = (active && self.tab_lens && !tab_claimed).then(|| crate::glass::key_of(&tab.left.entity_id().as_u64()));
            tab_claimed |= tab_lens.is_some();
            let lens_tab = tab_lens.is_some();
            let anim = self.tab_anim.as_ref().filter(|(v, _)| *v == tab.left).map(|(_, s)| *s);
            let el = h_flex()
                    .id(("tab", i))
                    .when(!popups_open, |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tab_tip.clone()).build(w, cx)))
                    .group("tab")
                    .flex_shrink_0()
                    .h(px(p.tab_h))
                    .w(px(tab_w))
                    .pl_3()
                    .pr_1()
                    .gap_2()
                    .items_center()
                    // the theme's tab: a pill floating on the backdrop (Canopy), lifted into the address row (Stem), an index card (Index)
                    .map(|d| match p.tab_shape {
                        "pill" => d.rounded(px(p.control_radius)).when(active, |d| d.shadow(p.lifted_shadows())),
                        "card" => d.when(active, |d| d.border_1().border_b_0().border_color(p.sheet_border)),
                        _ => d.rounded_t(px(p.control_radius + 2.)),
                    })
                    .text_size(px(p.fs))
                    .when_some(tab_ribbon, |d, r| d.relative().child(div().absolute().top(px(1.)).left(px(10.)).right(px(10.)).child(r)))
                    .when_some(tab_lane, |d, c| d.relative().child(div().absolute().bottom(px(2.)).left(px(10.)).right(px(10.)).h(px(2.)).rounded_full().bg(c)))
                    // Stem: the accent line on top of the tab in front
                    .when(active && p.tab_marker > 0., |d| {
                        d.relative().child(div().absolute().top_0().left(px(12.)).right(px(12.)).h(px(p.tab_marker)).rounded_b(px(2.)).bg(p.accent))
                    })
                    // while the tab moves (tabs open or close beside it, the window is resized) the scene paints the fill itself: the
                    // engine's lens would trail the tab (see glass::LensPhase)
                    .when(active && (tab_lens.is_none() || crate::glass::lens_phase(self.glass_hwnd, crate::glass::SLOT_TAB) != crate::glass::LensPhase::Rest), |d| d.bg(p.tab_active))
                    .when_some(tab_lens, |d, key| {
                        d.relative().child(crate::glass::lens_probe_shaped(
                            crate::glass::SLOT_TAB,
                            key,
                            crate::glass::LensShape::Tab { radius: f32::from(theme::RADIUS) },
                        ))
                    })
                    .when(!active, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
                    // Index: the tab's number (Ctrl+1..9) in a small box in place of its icon, the box in the accent on the tab in front
                    .when(!(p.numbered_tabs && i < 9) && !busy, |d| {
                        // (16 px, the mockups' tab icon)
                        d.child(Icon::new(icon).size(px(16.)).flex_shrink_0().text_color(if active { p.accent } else { p.text_secondary }))
                    })
                    .when(!(p.numbered_tabs && i < 9) && busy, |d| {
                        d.child(
                            div()
                                .size(px(16.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .relative()
                                        .size(px(12.))
                                        .rounded_full()
                                        .border(px(1.5))
                                        .border_color(p.border_strong)
                                        .child(div().absolute().top(px(-1.5)).left(px(-1.5)).size(px(12.)).rounded_full().border_t(px(1.5)).border_color(p.accent)),
                                ),
                        )
                    })
                    .when(p.numbered_tabs && i < 9, |d| {
                        d.child(
                            div()
                                .size(px(18.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .border_1()
                                .border_color(if active { p.accent } else { p.border_strong })
                                .when(active, |d| d.bg(p.accent).text_color(p.accent_text))
                                .when(!active, |d| d.text_color(p.text_tertiary))
                                .when(!p.font_mono.is_empty(), |d| d.font_family(p.font_mono))
                                .text_size(px(10.))
                                .child(format!("{}", i + 1)),
                        )
                    })
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                    .children(tab_agent.map(|c| div().size(px(7.)).flex_shrink_0().rounded_full().bg(c)))
                    .when_some(term_dir, |d, dir| {
                        d.child(
                            div()
                                .id(("tab-term", i))
                                .size(px(22.))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(theme::RADIUS_SM)
                                .text_color(if active { p.text_secondary } else { p.text_tertiary })
                                // (shown on hover only: the mockups' tab is its name and its close button)
                                .invisible()
                                .when(!roomy, |d| d.hidden())
                                .group_hover("tab", |s| s.visible())
                                .hover(|s| s.bg(p.pressed).text_color(p.text))
                                .when(!popups_open, |d| {
                                    d.tooltip(|w, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(t!("Open in terminal")).build(w, cx)
                                    })
                                })
                                .child(Icon::new(L::SquareTerminal).size(px(12.)))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |_this, _, _, cx| crate::terminal::open(dir.clone(), cx))),
                        )
                    })
                    .child(
                        div()
                            .id(("tab-close", i))
                            .size(px(22.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme::RADIUS_SM)
                            // (the mockups' quieter x, brighter under the pointer)
                            .text_color(p.text_secondary)
                            // like Files: every tab shows its close button while there is room
                            // (pill tabs, the mockups': a tab in the back shows its close button on hover only)
                            .when((!roomy || pill) && !active, |d| d.invisible())
                            .group_hover("tab", |s| s.visible())
                            .hover(|s| s.bg(p.pressed).text_color(p.text))
                            .child(Icon::new(L::X).size(px(12.)))
                            // the press stays with the x: the tab's own press would bring the tab forward first, so closing a tab in the
                            // back showed it for a moment (#145)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, window, cx);
                            })),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            // Keep the title bar from turning a tab drag into a window move.
                            cx.stop_propagation();
                            this.activate(i, window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| this.close_tab(i, window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| this.tab_menu(i, window, cx)),
                    )
                    // files dragged over a tab and held there switch to it (Explorer / Files)
                    .on_drag_move::<ExternalPaths>(cx.listener(move |this, ev: &DragMoveEvent<ExternalPaths>, window, cx| {
                        let over = ev.bounds.contains(&ev.event.position);
                        this.drag_over_tab(i, over, window, cx);
                    }))
                    .on_drag(drag, |d: &TabDrag, _, _, cx| cx.new(|_| d.clone()))
                    // The drop marker of a tab that carries the glass lens must not change its box: the lens is measured from the box, and a
                    // 2 px left border made the pill jump and narrow while a dragged tab hovered the selected one. It gets a tint instead.
                    .drag_over::<TabDrag>(move |s, _, _, _| if lens_tab { s.bg(accent.opacity(0.22)) } else { s.border_l_2().border_color(accent) })
                    .on_drop(cx.listener(move |this, d: &TabDrag, window, cx| this.move_tab(d.ix, i, window, cx)))
                    .when_some(tab_dir, |d, dir| {
                        d.drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
                            .on_drag_move::<ExternalPaths>(crate::folder_view::drop_effect_over(dir.clone()))
                            .on_drop(move |paths: &ExternalPaths, _, cx| drop_files(paths, dir.clone(), cx))
                    });
            tabs = match anim {
                // grows from the "+" box (28 px) to its width and height, contents fading in
                Some(serial) => tabs.child(el.overflow_hidden().with_animation(
                    ("tab-open", serial),
                    Animation::new(std::time::Duration::from_millis(TAB_ANIM_MS)).with_easing(ease_out_quint()),
                    move |el, d| el.w(px(28. + (tab_w - 28.) * d)).h(px(28. + 4. * d)).opacity(0.25 + 0.75 * d),
                )),
                None => tabs.child(el),
            };
        }
        self.tab_lens_claimed = tab_claimed;
        // over glass the "+" sits in a round lens (a constant key: it never slides)
        let plus_lens = self.tab_lens;
        self.plus_lens_claimed = plus_lens;
        // outside the scrolling strip: always reachable
        let new_tab = div()
                .id("new-tab")
                .flex_shrink_0()
                .ml_1()
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("New tab  (Ctrl+T)")).build(w, cx))
                .size(px(if pill { 32. } else { 28. }))
                .when(!pill, |d| d.mb(px(2.)))
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.text_secondary)
                .when(!plus_lens, |d| d.rounded(px(p.control_radius)))
                .when(plus_lens, |d| d.relative().rounded_full().child(crate::glass::lens_probe_shaped(crate::glass::SLOT_NEW_TAB, crate::glass::key_of("new-tab"), crate::glass::LensShape::Circle)))
                // the button moves when a tab opens or closes: a plain round fill stands in for the lens meanwhile
                .when(plus_lens && crate::glass::lens_phase(self.glass_hwnd, crate::glass::SLOT_NEW_TAB) != crate::glass::LensPhase::Rest, |d| d.bg(p.tab_active))
                .hover(|s| s.bg(p.hover))
                .child(Icon::new(L::Plus).size(px(14.)))
                // the title bar arms a window move on mouse down: the slightest jitter then
                // swallows the mouse up and the click never happens
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| this.open_new_tab(window, cx)));
        // after the new tab has grown, the next "+" slides out from behind it
        let new_tab: AnyElement = match self.tab_anim.as_ref().map(|(_, s)| *s) {
            Some(serial) => {
                let total = (TAB_ANIM_MS + PLUS_ANIM_MS) as f32;
                let start = TAB_ANIM_MS as f32 / total;
                new_tab
                    .relative()
                    .with_animation(("plus-in", serial), Animation::new(std::time::Duration::from_millis(TAB_ANIM_MS + PLUS_ANIM_MS)), move |el, d| {
                        let t = ((d - start) / (1. - start)).clamp(0., 1.);
                        let e = ease_out_quint()(t);
                        el.left(px(-26. * (1. - e))).opacity(e)
                    })
                    .into_any_element()
            }
            None => new_tab.into_any_element(),
        };

        TitleBar::new()
            .h(px(p.titlebar_h))
            .bg(p.titlebar_bg)
            .border_color(gpui_kit::transparent_black())
            // (Index: the strip is ruled off from the address row; the tab in front stands over the line)
            .when(p.tab_shape == "card", |d| d.border_b_1().border_color(p.sheet_border))
            .child(
                h_flex()
                    .h_full()
                    // explicit width: the component's title bar never shrinks its content, so
                    // many tabs would push the window buttons off-screen (min/max/close ~140 px)
                    .w(px((self.viewport_w - 150.).max(200.)))
                    .min_w_0()
                    .overflow_hidden()
                    // (pill tabs sit in the middle of the strip below the window's own gap)
                    .when(pill, |d| d.items_center().pt(px(p.gap)))
                    .when(!pill, |d| d.items_end())
                    .child(
                        // sidebar toggle in the corner (Files has the pane button here, no app name)
                        // same size and baseline as the "+" button; centered over the Back
                        // button below (title bar pads 12 px, the nav row 8 px)
                        // (pill tabs: the mockups' 44 px brand box with the mark in its middle)
                        h_flex().flex_shrink_0().w(px(if pill { TITLEBAR_LEAD_PILL } else { TITLEBAR_LEAD })).h_full().when(pill, |d| d.items_center().justify_center()).when(!pill, |d| d.items_end()).child(
                            div()
                                .id("tab-actions")
                                .when(!pill, |d| d.ml(px(-2.)))
                                .when(!pill, |d| d.mb(px(2.)))
                                .size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(theme::RADIUS_SM)
                                .text_color(p.text_secondary)
                                .hover(|s| s.bg(p.hover))
                                .when(!popups_open, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Tab actions")).build(w, cx)))
                                // the leaf: the app's mark, as in the mockups (it opens the window menu)
                                // (the app's own icon at 16 px, as the mockups' brand button; the compact overlay keeps its line icon)
                                .map(|d| match self.compact {
                                    Some(_) => d.child(Icon::new(L::PictureInPicture2).size(px(16.)).text_color(p.accent)),
                                    None => d.child(img(leaf_mark()).w(px(16.)).h(px(16.))),
                                })
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(|this, _, window, cx| this.tab_actions_menu(window, cx))),
                        ),
                    )
                    // Home, left of the tabs (the mockups'): a tab-like button, lit while Home is shown
                    .child(
                        div()
                            .id("tb-home")
                            .flex_shrink_0()
                            .size(px(if pill { 36. } else { 28. }))
                            .when(!pill, |d| d.mb(px(2.)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(p.control_radius))
                            .text_color(if at_home { p.text } else { p.text_secondary })
                            .when(at_home, |d| d.bg(p.tab_active).when(pill, |d| d.shadow(p.lifted_shadows())))
                            .when(!at_home, |d| d.hover(|s| s.bg(p.hover).text_color(p.text)))
                            .when(!popups_open, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Home")).build(w, cx)))
                            .child(Icon::new(L::House).size(px(16.)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| this.navigate(Location::Home, window, cx))),
                    )
                    .children(session_label)
                    .child(tabs)
                    .child(new_tab)
                    // the right-hand controls (the mockup's), at the end of the strip before the window buttons
                    .when(prefs_shown, |d| {
                        d.child(div().flex_1().min_w(px(8.)))
                            .child(div().flex_shrink_0().h_full().when(!pill, |d| d.flex().items_end().pb(px(2.))).child(crate::titlebar_prefs::render(p, &self.ops, popups_open, cx)))
                    }),
            )
    }

    /// Live pane resize; the width is kept in settings (written with the next session save).
    fn set_pane_width(&mut self, sidebar: bool, w: f32, cx: &mut Context<Self>) {
        if sidebar {
            let rail = if self.sidebar_rail { w < RAIL_SNAP + 30. } else { w < RAIL_SNAP };
            if rail != self.sidebar_rail {
                self.sidebar_rail = rail;
                cx.update_global::<crate::settings::Settings, _>(|s, _| s.sidebar_rail = rail);
                cx.notify();
            }
            if rail {
                return;
            }
        }
        let w = if sidebar {
            w.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
        } else {
            // (the list keeps its minimum width beside the pane and the sidebar)
            let side = if !self.sidebar_open { 0. } else if self.sidebar_rail { RAIL_W } else { self.sidebar_w };
            let room = (self.viewport_w - side - rf_core::previewpane::LIST_MIN_W).max(PREVIEW_MIN);
            w.clamp(PREVIEW_MIN, room.min(PREVIEW_MAX))
        };
        let cur = if sidebar { &mut self.sidebar_w } else { &mut self.preview_w };
        if (*cur - w).abs() < 0.5 {
            return;
        }
        *cur = w;
        cx.update_global::<crate::settings::Settings, _>(|s, _| {
            if sidebar {
                s.sidebar_width = w;
            } else {
                s.preview_width = w;
            }
        });
        // write once the drag has settled
        self._width_save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
            let _ = cx.update(|cx| {
                let snapshot = cx.global::<crate::settings::Settings>().clone();
                std::thread::spawn(move || snapshot.save());
            });
        }));
        cx.notify();
    }

    /// The pin list as it is right now: the user's list from settings (updated synchronously),
    /// else the defaults shown by `Places`. `Places` itself refreshes asynchronously, so reading it
    /// right after a change would undo that change.
    fn current_pins(&self, cx: &App) -> Vec<PathBuf> {
        cx.global::<crate::settings::Settings>()
            .pinned
            .clone()
            .unwrap_or_else(|| self.places.read(cx).pinned.iter().map(|f| f.path.clone()).collect())
    }

    /// Reorders pinned entries (drag & drop in the sidebar); persisted as the user's pin list.
    /// By path, not index: the drawn list may still show the order from before a previous drag.
    fn move_pin(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let mut list = self.current_pins(cx);
        let (Some(from), Some(to)) = (list.iter().position(|p| p == from), list.iter().position(|p| p == to)) else {
            return;
        };
        if from == to {
            return;
        }
        let item = list.remove(from);
        list.insert(to, item);
        crate::settings::update(cx, |s| s.pinned = Some(list));
        self.places.update(cx, |p, cx| p.refresh(cx));
    }

    /// Right-click on a mounted share in the sidebar's Network list: open it, copy its address, disconnect.
    fn net_menu(&mut self, v: rf_core::vpath::VPath, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let me = cx.entity().downgrade();
        let (a, b, c) = (v.clone(), v.clone(), v.clone());
        let items = vec![
            Item::action(t!("Open"), MenuIcon::Lucide(L::SquareArrowOutUpRight), {
                let me = me.clone();
                move |w: &mut Window, cx: &mut App| {
                    let loc = Location::Remote(a.clone());
                    let _ = me.update(cx, |ws, cx| ws.navigate(loc, w, cx));
                }
            }),
            Item::action(t!("Open in new tab"), MenuIcon::Lucide(L::SquarePlus), move |w: &mut Window, cx: &mut App| {
                let loc = Location::Remote(b.clone());
                let _ = me.update(cx, |ws, cx| ws.open_tab(loc, w, cx));
            }),
            Item::action(t!("Copy address"), MenuIcon::Lucide(L::ClipboardCopy), {
                let s = v.to_string();
                move |_: &mut Window, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(s.clone()))
            }),
            Item::Separator,
            Item::action(t!("Disconnect"), MenuIcon::Lucide(L::Unplug), move |_: &mut Window, cx: &mut App| crate::remote::disconnect(&c, cx)),
        ];
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    /// Right-click on a saved host in the sidebar: open it, copy its address, forget its saved password.
    fn host_menu(&mut self, v: rf_core::vpath::VPath, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let me = cx.entity().downgrade();
        let (a, b, c) = (v.clone(), v.clone(), v.clone());
        let mut items = vec![
            Item::action(t!("Open"), MenuIcon::Lucide(L::SquareArrowOutUpRight), {
                let me = me.clone();
                move |w: &mut Window, cx: &mut App| {
                    let loc = crate::remote::place_of(a.clone());
                    let _ = me.update(cx, |ws, cx| ws.navigate(loc, w, cx));
                }
            }),
            Item::action(t!("Open in new tab"), MenuIcon::Lucide(L::SquarePlus), move |w: &mut Window, cx: &mut App| {
                let loc = crate::remote::place_of(b.clone());
                let _ = me.update(cx, |ws, cx| ws.open_tab(loc, w, cx));
            }),
            Item::action(t!("Copy address"), MenuIcon::Lucide(L::ClipboardCopy), {
                let s = v.to_string();
                move |_: &mut Window, cx: &mut App| cx.write_to_clipboard(ClipboardItem::new_string(s.clone()))
            }),
        ];
        if v.scheme() == "smb" {
            // the system's session with the server (Windows) or the mount (Linux), and the passwords saved for its shares
            let d = c.clone();
            items.push(Item::Separator);
            items.push(Item::action(t!("Disconnect"), MenuIcon::Lucide(L::Unplug), move |_: &mut Window, cx: &mut App| crate::remote::disconnect(&d, cx)));
            let f = c.clone();
            items.push(Item::action(t!("Forget saved password"), MenuIcon::Lucide(L::Lock), move |_: &mut Window, cx: &mut App| crate::hosts_ui::forget_for(&f, cx)));
        }
        if matches!(v.scheme(), "sftp" | "ftp" | "ftps" | "dav" | "davs") {
            items.push(Item::Separator);
            let d = c.clone();
            items.push(Item::action(t!("Forget saved password"), MenuIcon::Lucide(L::Lock), move |_: &mut Window, cx: &mut App| crate::hosts_ui::forget_for(&c, cx)));
            items.push(Item::action(t!("Empty server trash"), MenuIcon::Lucide(L::Trash), move |_: &mut Window, cx: &mut App| crate::remote::empty_trash(d.clone(), cx)));
        }
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    /// Right-click on a sidebar folder/drive: the Explorer menu (eject, properties, format…)
    /// plus open in new tab/window and pin/unpin.
    fn sidebar_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let shift = leaf_sys::input::shift_down();
        self.sidebar_menu_at(path, shift, window.mouse_position(), window, cx);
    }

    /// The sidebar's folder menu at `at`; `classic`: Windows' own menu (extended verbs) instead of ours.
    fn sidebar_menu_at(&mut self, path: PathBuf, classic: bool, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon, Sub};
        use crate::menu_model::{Entry, MenuContext, Slot, Target, plan};
        if classic {
            // the classic Explorer menu, extended verbs included
            let p2 = path.clone();
            leaf_sys::sta::run(move |owner| {
                let pt = leaf_sys::input::cursor_pos();
                if let Err(e) = leaf_sys::ctxmenu::show_for_one(owner, &p2, (pt.x, pt.y), true, &[]) {
                    log::warn!("sidebar menu: {e:#}");
                }
            });
            return;
        }
        let me = cx.entity().downgrade();
        let is_pinned = self.current_pins(cx).contains(&path);
        let root = path.parent().is_none();
        let with = |f: fn(&mut Workspace, PathBuf, &mut Window, &mut Context<Workspace>)| {
            let (me, path) = (me.clone(), path.clone());
            move |w: &mut Window, cx: &mut App| {
                let p = path.clone();
                let _ = me.update(cx, |ws, cx| f(ws, p, w, cx));
            }
        };
        // the rows come from `menu_model`; what each does is here
        let ctx = {
            let s = cx.global::<crate::settings::Settings>();
            MenuContext {
                target: Target::Sidebar,
                pinned: is_pinned,
                sidebar_root: root,
                tags_defined: !cx.global::<crate::tags::Tags>().0.defs.is_empty(),
                cloud_place: self.places.read(cx).cloud.iter().any(|c| c.path == path),
                drive: crate::settings_view::drive_letter(&path).is_some(),
                bluray: crate::bluray_ui::offers(std::slice::from_ref(&path)),
                prefs: s.menu.clone(),
                ..Default::default()
            }
        };
        let items: Vec<Item> = plan(&ctx)
            .into_iter()
            .map(|e| match e {
                Entry::Separator => Item::Separator,
                Entry::Row(p) => {
                    let (label, ic) = (p.label(&ctx), MenuIcon::Lucide(p.icon));
                    let item = match p.slot {
                        Slot::Open => Item::action(label, ic, with(|ws, p, w, cx| ws.navigate(Location::Path(p), w, cx))),
                        Slot::OpenNewTab => Item::action(label, ic, with(|ws, p, w, cx| {
                            let keep = ws.active;
                            ws.open_tab(Location::Path(p), w, cx);
                            if !cx.global::<crate::settings::Settings>().focus_new_tab {
                                ws.activate(keep, w, cx);
                            }
                        })),
                        Slot::OpenNewWindow => Item::action(label, ic, with(|_, p, w, cx| {
                            let mut b = w.bounds();
                            b.origin.x += px(36.);
                            b.origin.y += px(36.);
                            cx.defer(move |cx| crate::open_main_window(Some(Location::Path(p)), None, Some(WindowBounds::Windowed(b)), cx));
                        })),
                        Slot::Unpin => Item::action(label, ic, with(|ws, p, _, cx| ws.set_pinned(p, false, cx))),
                        Slot::ConvertVideo => Item::action(label, ic, with(|_, p, w, cx| crate::bluray_ui::convert(vec![p], w, cx))),
                        Slot::Pin => Item::action(label, ic, with(|ws, p, _, cx| ws.set_pinned(p, true, cx))),
                        Slot::Tags => Item::sub(label, ic, crate::tags::menu_items(std::slice::from_ref(&path), cx)),
                        Slot::CopyPath => Item::action(label, ic, with(|_, p, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string())))),
                        Slot::Terminal => Item::action(label, ic, with(|_, p, _, cx| crate::terminal::open(p, cx))),
                        // our Properties window (a drive gets its own page there)
                        Slot::Properties => Item::action(label, ic, with(|ws, p, _, cx| {
                            crate::properties_view::open(vec![p], ws.images.clone(), cx);
                        })),
                        // a cloud or WSL place can be taken off the sidebar on its own (it comes back from Settings > Files and folders)
                        Slot::HidePlace => {
                            let key = path.to_string_lossy().into_owned();
                            Item::action(label, ic, move |_, cx| {
                                let key = key.clone();
                                crate::settings::update(cx, move |s| {
                                    if !s.hidden_places.iter().any(|h| rf_core::pathkey::name_eq(h, &key)) {
                                        s.hidden_places.push(key);
                                    }
                                })
                            })
                        }
                        // a drive can be taken off the lists of places (it comes back from Settings > Files and folders)
                        Slot::HideDrive => {
                            let letter = crate::settings_view::drive_letter(&path);
                            Item::action(label, ic, move |_, cx| {
                                if let Some(letter) = letter {
                                    crate::settings::update(cx, move |s| crate::settings_view::set_drive_hidden(s, letter, true))
                                }
                            })
                        }
                        _ => Item::Sub { label: label.into(), icon: ic, items: Sub::Pending { key: "more", fallback: None }, click: None },
                    };
                    if p.hint.is_empty() { item } else { item.hint(p.hint) }
                }
            })
            .collect();
        // the row of quick buttons the file list's menu has (issue 53: "Can't do basic functions like copy, cut, delete, rename for
        // the folders at the left pane"). A drive's root can be pasted into, not cut, renamed or deleted.
        let quick = {
            use crate::fluent_menu::Quick;
            let clip = |kind: leaf_sys::fileops::Transfer| {
                let p = path.clone();
                move |_: &mut Window, _: &mut App| {
                    let p = p.clone();
                    leaf_sys::sta::run(move |owner| {
                        if let Err(e) = leaf_sys::clipboard::set_files(owner, &[p], kind) {
                            log::warn!("clipboard: {e:#}");
                        }
                    });
                }
            };
            vec![
                Quick::new(L::Scissors, t!("Cut  (Ctrl+X)"), !root, clip(leaf_sys::fileops::Transfer::Move)),
                Quick::new(L::Copy, t!("Copy  (Ctrl+C)"), true, clip(leaf_sys::fileops::Transfer::Copy)),
                Quick::new(L::ClipboardPaste, t!("Paste  (Ctrl+V)"), leaf_sys::clipboard::can_paste(), with(|ws, p, w, cx| {
                    ws.active_tab().clone().update(cx, |v, cx| v.paste_to(p, w, cx))
                })),
                Quick::new(L::TextCursorInput, t!("Rename  (F2)"), !root, with(|ws, p, w, cx| ws.rename_place(p, w, cx))),
                Quick::new(
                    L::Share2,
                    t!("Share"),
                    true,
                    with(|_, p, w, cx| {
                        crate::share::open(crate::look::window::hwnd_of(w), vec![p], cx);
                    }),
                ),
                Quick::new(L::Trash, t!("Delete  (Del)"), !root, with(|ws, p, w, cx| {
                    ws.active_tab().clone().update(cx, |v, cx| v.delete_paths(vec![p], false, w, cx))
                })),
                Quick::new(
                    L::Wrench,
                    t!("Properties  (Alt+Enter)"),
                    true,
                    with(|ws, p, _, cx| {
                        crate::properties_view::open(vec![p], ws.images.clone(), cx)
                    }),
                ),
            ]
        };
        let menu = crate::fluent_menu::show(Menu { quick, items }, at, window, cx);
        let view = self.active_tab().downgrade();
        crate::ctx_menu::load_shell_more(menu.downgrade(), leaf_sys::ctxmenu::ShellTarget::One(path), view, cx);
    }

    /// Renames a folder of the sidebar or the folder tree (its menu's Rename, issue 53): asks the new name, renames it off the
    /// interface's thread, and keeps what pointed at it (a pin, its tags, a tab inside it) pointing at it.
    fn rename_place(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(old) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
        let me = cx.entity().downgrade();
        crate::prompt::show(
            t!("Rename"),
            None,
            Some((old.clone(), String::new())),
            vec![
                crate::prompt::Button::new(t!("Rename"), crate::prompt::Kind::Primary, move |text, _, cx| {
                    let name = text.unwrap_or_default().trim().to_string();
                    const INVALID: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
                    if name.is_empty() || name.contains(INVALID) || name.ends_with('.') {
                        return Err(t!("A file name can't contain any of these characters: \\ / : * ? \" < > | and can't end with a period.").to_string());
                    }
                    if name == old {
                        return Ok(());
                    }
                    let to = path.with_file_name(&name);
                    let case_only = name.to_lowercase() == old.to_lowercase();
                    if !case_only && std::fs::symlink_metadata(&to).is_ok() {
                        return Err(t!("An item with this name already exists. Try another name.").to_string());
                    }
                    let (from, me) = (path.clone(), me.clone());
                    let done = cx.background_spawn({
                        let (from, name) = (from.clone(), name.clone());
                        async move { leaf_sys::fileops::rename(&from, &name) }
                    });
                    cx.spawn(async move |cx| match done.await {
                        Ok(true) => {
                            let _ = cx.update(|cx| {
                                crate::undo::push(crate::undo::UndoOp::Renamed { from: from.clone(), to: to.clone() });
                                crate::tags::follow(cx, &[(from.clone(), to.clone())]);
                                let _ = me.update(cx, |ws, cx| ws.place_renamed(&from, &to, cx));
                            });
                        }
                        Ok(false) => {}
                        Err(e) => log::warn!("rename: {e:#}"),
                    })
                    .detach();
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }

    /// After a sidebar folder was renamed: its pin follows it, and so does every tab that showed it or a folder inside it.
    fn place_renamed(&mut self, from: &std::path::Path, to: &std::path::Path, cx: &mut Context<Self>) {
        let pins = self.current_pins(cx);
        if let Some(list) = crate::places::pins_after(&pins, from, Some(to)) {
            crate::settings::update(cx, |s| s.pinned = Some(list));
        }
        self.places.update(cx, |p, cx| p.refresh(cx));
        for t in &self.tabs {
            for v in std::iter::once(&t.left).chain(t.right.as_ref()) {
                let moved = match v.read(cx).location() {
                    Location::Path(p) => p.strip_prefix(from).ok().map(|rest| to.join(rest)),
                    _ => None,
                };
                if let Some(now) = moved {
                    v.update(cx, |v, cx| v.navigate(Location::Path(now), cx));
                }
            }
        }
    }

    /// Folders dropped on the sidebar's pin area get pinned (files are ignored, as in Explorer).
    fn pin_dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        self.sidebar_ext_drag = false;
        let dirs: Vec<PathBuf> = paths.paths().iter().filter(|p| p.is_dir()).cloned().collect();
        if dirs.is_empty() {
            return;
        }
        let mut list = self.current_pins(cx);
        for d in dirs {
            if !list.contains(&d) {
                list.push(d);
            }
        }
        crate::settings::update(cx, |s| s.pinned = Some(list));
        self.places.update(cx, |p, cx| p.refresh(cx));
    }

    fn set_pinned(&mut self, path: PathBuf, pin: bool, cx: &mut Context<Self>) {
        let mut list = self.current_pins(cx);
        if !pin {
            list.retain(|p| *p != path);
        } else if !list.contains(&path) {
            list.push(path);
        }
        crate::settings::update(cx, |s| s.pinned = Some(list));
        self.places.update(cx, |p, cx| p.refresh(cx));
    }

    /// Moves tab `ix` into a new window (tab dragged onto the content area).
    fn detach_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 || ix >= self.tabs.len() {
            return;
        }
        let loc = self.tabs[ix].left.read(cx).location().clone();
        self.close_tab(ix, window, cx);
        let mut b = window.bounds();
        b.origin.x += px(48.);
        b.origin.y += px(48.);
        cx.defer(move |cx| crate::open_main_window(Some(loc), None, Some(WindowBounds::Windowed(b)), cx));
    }

    /// Right-click on Back/Forward: the history as a menu.
    fn show_history_menu(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab().clone();
        let titles: Vec<String> = {
            let v = tab.read(cx);
            let it: Box<dyn Iterator<Item = &Location>> =
                if back { Box::new(v.history.back_entries()) } else { Box::new(v.history.forward_entries()) };
            it.take(16)
                .map(|l| match l {
                    Location::Path(p) => p.to_string_lossy().into_owned(),
                    Location::DiskUsage(p) => crate::disk_usage::title(p),
                    Location::Duplicates(r) => crate::duplicates_view::title(r),
                    Location::BuildJunk(p) => crate::junk_view::title(p),
                    Location::Changes(p) => crate::changes_view::title(p),
                    Location::Brain(p) => crate::brain_view::title(p),
                    other => crate::i18n::td(&other.title()),
                })
                .collect()
        };
        if titles.is_empty() {
            return;
        }
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let weak = tab.downgrade();
        let items: Vec<Item> = titles
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                let weak = weak.clone();
                Item::action(t, MenuIcon::Lucide(L::Folder), move |_, cx| {
                    let _ = weak.update(cx, |v, cx| v.jump(back, i + 1, cx));
                })
            })
            .collect();
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    fn nav_button(
        &self,
        id: &'static str,
        icon: L,
        enabled: bool,
        p: &Palette,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip_text(id)).build(w, cx))
            .size(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme::RADIUS_SM)
            .text_color(if enabled { p.text } else { p.text_tertiary })
            .when(enabled, |d| {
                d.hover(|s| s.bg(p.hover))
                    .active(|s| s.bg(p.pressed))
                    .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
            })
            .child(Icon::new(icon).size(px(16.)))
            .into_any_element()
    }

    /// Navigation row: back / forward / up / refresh, the Omnibar (one box for path, search and
    /// commands, like Files) and the Status Center button.
    fn render_toolbar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let popups_open = self.popups_open(cx);
        let tab = self.active_tab().read(cx);
        let can_back = tab.history.can_back();
        let can_fwd = tab.history.can_forward();
        let loc = tab.location().clone();
        let can_up = loc.parent().is_some();
        // The Omnibar shows the search box only while it is being typed in; afterwards the path
        // comes back and a small chip keeps the active query visible (and clearable).
        let searching = self.search_mode;
        let live_query = self.search.read(cx).value().to_string();
        let active_query: Option<String> = match &loc {
            Location::Search { query, .. } => Some(query.clone()),
            _ => (!live_query.is_empty()).then_some(live_query),
        };

        // (the search field is the tool bar's where it is shown; elsewhere the box takes the search, as before)
        let bar_search = cx.global::<crate::settings::Settings>().show_toolbar
            && matches!(loc, Location::Path(_) | Location::Search { .. } | Location::Library { .. } | Location::Tag(_) | Location::Shell { .. });
        let searching = searching && !bar_search;
        let active_query = active_query.filter(|_| !bar_search);
        let suggestions = if searching { self.render_search_history(p, cx) } else if self.search_mode { None } else { self.render_suggestions(p, cx) };
        let full_text = if cx.global::<crate::settings::Settings>().address_full_path { crate::full_path::path_text(&loc) } else { None };
        let field: AnyElement = if searching {
            Input::new(&self.search).appearance(false).cleanable(true).into_any_element()
        } else if self.editing_address {
            Input::new(&self.address).appearance(false).into_any_element()
        } else if let Some(text) = full_text {
            // the full path as plain text; the omnibar's click edits it
            div().id("address-text").flex_1().min_w_0().px_1p5().py_0p5().text_size(px(p.fs)).whitespace_nowrap().truncate().child(crate::rtl::display_text(&text)).into_any_element()
        } else {
            self.render_breadcrumb(&loc, p, cx).into_any_element()
        };
        let focused = self.editing_address || searching;
        let chip = active_query.filter(|_| !searching).map(|q| {
            h_flex()
                .id("search-chip")
                .flex_shrink_0()
                .max_w(px(220.))
                .h(px(24.))
                .pl_2()
                .gap_1()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .bg(p.selection)
                .text_xs()
                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Edit search")).build(w, cx))
                .child(Icon::new(L::Search).size(px(12.)).text_color(p.accent))
                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(q))
                .child(
                    div()
                        .id("search-chip-x")
                        .size(px(22.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme::RADIUS_SM)
                        .hover(|s| s.bg(p.hover))
                        .child(Icon::new(L::X).size(px(11.)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, window, cx| this.exit_search(window, cx))),
                )
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, window, cx| this.enter_search(window, cx)))
        });
        let mode_btn = |id: &'static str, icon: L, on: bool, tip: &'static str, cx: &mut Context<Self>, f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            div()
                .id(id)
                .size(px(28.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme::RADIUS_SM)
                .text_color(if on { p.accent } else { p.text_secondary })
                .when(on, |d| d.bg(p.selection))
                .hover(|s| s.bg(p.hover))
                .when(!popups_open, |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(tip)).build(w, cx)))
                .child(Icon::new(icon).size(px(15.)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
        };
        let omnibar = h_flex()
            .id("omnibar")
            .flex_1()
            .min_w(px(160.))
            .h(px(32.))
            .mx_1()
            .pl(px(6.))
            .pr_1()
            .gap_1()
            .items_center()
            // a capsule, like Files' Omnibar
            .rounded(px(p.control_radius + 2.))
            .bg(p.field_bg)
            .border_1()
            .border_color(if focused { p.accent } else { p.field_border })
            .relative()
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.search_mode && ev.keystroke.key == "escape" {
                    this.exit_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if this.search_mode {
                    crate::rtl::handle_key_down(&this.search, ev, cx);
                } else if this.editing_address {
                    crate::rtl::handle_key_down(&this.address, ev, cx);
                }
                if !this.search_mode {
                    this.address_key(ev, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::IndentInline, window, cx| this.accept_suggestion(window, cx)))
            .on_action(cx.listener(|this, _: &gpui_kit::component::input::OutdentInline, window, cx| {
                if this.editing_address {
                    this.cycle_focus(false, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .when(searching, |d| d.child(Icon::new(L::Search).size(px(14.)).text_color(p.text_secondary)))
            .child(div().flex_1().min_w_0().overflow_hidden().child(field))
            .children(suggestions)
            .children(chip)
            // (the search field is the tool bar's; the box keeps the path and a pencil that edits it, as the mockups)
            .child(mode_btn("omni-edit", L::Pencil, self.editing_address, "Edit path  (Ctrl+L)", cx, |this, window, cx| {
                if !this.editing_address {
                    this.begin_address_edit(window, cx)
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.editing_address && !this.search_mode {
                    this.begin_address_edit(window, cx);
                }
            }));
        // (the terminal button where a terminal can open there: main's `terminal::offered`)
        let term_offered = crate::terminal::offered(&loc);
        // (with the type bar under it the row sits a little higher, as one sheet with it)
        // (every page keeps the row: Home, Settings and the result pages reserve its space, so the frame does not jump)
        let typebar = p.typebar_h > 0.;

        h_flex()
            .h(px(if typebar { 42. } else { 48. }))
            .when(typebar, |d| d.pt(px(8.)).pb(px(2.)))
            .px(px(12.))
            .text_size(px(p.fs))
            .gap_1()
            .items_center()
            .child(self.nav_button("sidebar", L::PanelLeft, true, p, cx, |_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)))
            .child(
                div()
                    .flex_shrink_0()
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| this.show_history_menu(true, window, cx)))
                    .child(self.nav_button("back", L::ArrowLeft, can_back, p, cx, |this, _, cx| this.with_tab(cx, |v, cx| v.back(cx)))),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| this.show_history_menu(false, window, cx)))
                    .child(self.nav_button("fwd", L::ArrowRight, can_fwd, p, cx, |this, _, cx| this.with_tab(cx, |v, cx| v.forward(cx)))),
            )
            .child(self.nav_button("up", L::ArrowUp, can_up, p, cx, |this, _, cx| this.with_tab(cx, |v, cx| v.up(cx))))
            .child(omnibar)
            .child(self.nav_button("terminal", L::SquareTerminal, term_offered, p, cx, |_, window, cx| window.dispatch_action(Box::new(OpenTerminal), cx)))
            .child(self.nav_button("refresh", L::RotateCw, true, p, cx, |this, _, cx| this.with_tab(cx, |v, cx| v.refresh(cx))))
            .child(crate::status_center::render_button(&self.ops, p, cx))
    }

    /// The type bar under the address row (the mockups'): a thin bar of the folder's types by count, a chip per type with its count
    /// (a click lists only that type, again lists all), and the item count and total size at the right. None where the theme has no
    /// type bar or the page is no folder.
    fn render_typebar(&mut self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if p.typebar_h <= 0. {
            return None;
        }
        let view = self.active_tab().clone();
        let v = view.read(cx);
        v.location().path()?;
        let sums = v.type_summary();
        let on = v.type_filter();
        let total: u32 = sums.iter().map(|s| s.0).sum();
        let bytes: u64 = sums.iter().map(|s| s.1).sum();
        const LABELS: [&str; FolderView::TYPE_BUCKETS] = ["Folders", "Images", "Video", "Audio", "Documents", "Code", "Text", "Archives", "Apps"];
        use crate::icons::Category as C;
        const CATS: [C; FolderView::TYPE_BUCKETS] = [C::Folder, C::Image, C::Video, C::Audio, C::Document, C::Code, C::Text, C::Archive, C::Executable];
        let dot = if p.tag_dot >= 8. || p.square_marks { 8. } else { 6. };
        // (Index: a ruler of square blocks, Canopy and Stem a thin rounded bar)
        let sq = p.square_marks;
        let mut bar = h_flex()
            .w(px(if sq { 240. } else { 144. }))
            .h(px(if sq { 8. } else { 4. }))
            .flex_shrink_0()
            .gap(px(if sq { 2. } else { 1. }))
            .when(!sq, |d| d.rounded(px(3.)))
            .overflow_hidden();
        let mut chips = h_flex().gap(px(2.)).min_w_0().overflow_hidden().flex_shrink(1.);
        for (b, &(n, _)) in sums.iter().enumerate().filter(|(_, s)| s.0 > 0) {
            let c = theme::kind_color(CATS[b], p.dark);
            bar = bar.child(div().h_full().flex_grow(n as f32).flex_basis(px(0.)).bg(c));
            let chosen = on == Some(b);
            let view = view.clone();
            chips = chips.child(
                h_flex()
                    .id(("type-chip", b))
                    .h(px(20.))
                    .px(px(8.))
                    .gap(px(6.))
                    .flex_shrink_0()
                    .items_center()
                    .when(!sq, |d| d.rounded(px(10.)))
                    .whitespace_nowrap()
                    .text_color(if chosen { p.text } else { p.text_secondary })
                    .when(chosen, |d| d.bg(p.selection).border_1().border_color(p.accent.opacity(0.45)))
                    .when(!chosen, |d| d.hover(|s| s.bg(p.hover).text_color(p.text)))
                    .child(div().size(px(dot)).when(!sq, |d| d.rounded_full()).flex_shrink_0().bg(c))
                    .child(crate::i18n::t(LABELS[b]))
                    .child(div().text_color(if chosen { p.text_secondary } else { p.text_tertiary }).child(n.to_string()))
                    .on_click(move |_, _, cx| view.update(cx, |v, cx| v.set_type_filter(Some(b), cx))),
            );
        }
        let clear = on.map(|_| {
            let view = view.clone();
            div()
                .id("type-clear")
                .h(px(20.))
                .px(px(8.))
                .flex()
                .items_center()
                .flex_shrink_0()
                .rounded(px(6.))
                .text_color(p.accent)
                .hover(|s| s.bg(p.hover))
                .child(t!("Clear filter"))
                .on_click(move |_, _, cx| view.update(cx, |v, cx| v.set_type_filter(None, cx)))
        });
        Some(
            h_flex()
                .h(px(p.typebar_h))
                .flex_shrink_0()
                // (the chips start under the address box, past the navigation buttons)
                .pl(px(165.))
                .pr(px(16.))
                .gap(px(12.))
                .items_center()
                .text_size(px(p.fs_xs))
                .text_color(p.text_secondary)
                .when(total > 0, |d| d.child(bar).child(chips).children(clear))
                .when(total == 0, |d| d.child(div().text_color(p.text_tertiary).child(t!("No files to summarise"))))
                .child(div().flex_1())
                .when(total > 0, |d| {
                    d.child(div().flex_shrink_0().whitespace_nowrap().text_color(p.text_tertiary).when(p.mono_chrome && !p.font_mono.is_empty(), |d| d.font_family(p.font_mono)).child(format!("{} · {}", crate::i18n::items(total as usize), format::size(bytes))))
                })
                .into_any_element(),
        )
    }

    /// "Search in file contents": the search box with `content:` already in it, for the text to be typed after it.
    fn enter_content_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_search(window, cx);
        self.search.update(cx, |s, cx| s.set_value("content:", window, cx));
    }

    fn enter_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_mode = true;
        self.editing_address = false;
        self.clear_suggestions();
        self.search.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    fn exit_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_mode = false;
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        if let Some(tab) = self.tabs.get(self.active).map(|t| t.view().clone()) {
            tab.update(cx, |v, cx| {
                v.set_query(String::new(), cx);
                // leave a recursive search result back to its folder
                if let Location::Search { root, .. } = v.location().clone() {
                    v.navigate(Location::Path(root), cx);
                }
            });
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Command bar above the file list (Files' toolbar): create, clipboard and item commands on
    /// the left; sort, view and the preview pane on the right.
    fn render_commandbar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        use crate::fluent_menu::{Item, Menu, MenuIcon};

        let tab = self.active_tab().read(cx);
        let in_folder = tab.location().path().is_some();
        // a tag's or a library's list: real paths from many folders, no folder of its own (no New, Paste, terminal or hidden-files here, as in
        // its right-click menu), but sort, view, filter and every item command work
        let listed = matches!(tab.location(), Location::Tag(_) | Location::Library { .. });
        let (sel, _, _) = tab.selection_summary();
        let has_sel = sel > 0;
        // a phone's folder is read-only here: changing commands are greyed out, not refused later
        let writable_sel = has_sel && !matches!(tab.location(), Location::Shell { .. });
        let split = self.tabs.get(self.active).is_some_and(|t| t.right.is_some());
        let mode = tab.view_mode;
        let sort_asc = tab.sort.dir == rf_core::SortDir::Asc;
        let sort_label: &'static str = match tab.sort.key {
            rf_core::SortKey::Name => t!("Name"),
            rf_core::SortKey::Size => t!("Size"),
            rf_core::SortKey::Modified => t!("Modified"),
            rf_core::SortKey::Created => t!("Created"),
            rf_core::SortKey::Type => t!("Type"),
            _ => t!("Sort"),
        };
        let folder_name = tab.title(cx);
        let preview_open = self.preview_open;
        let filter_on = self.filter_bar;

        // a command: its icon and, as in the mockups, its name (Share, Properties and the like stay icons); in a window of 1200 px or
        // less the commands are their icons, the mockups' narrow bar
        let narrow_bar = self.viewport_w <= 1200.;
        let cmd = |id: &'static str, icon: L, label: Option<&'static str>, tip: &'static str, enabled: bool, cx: &mut Context<Self>, run: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let label = label.filter(|_| !narrow_bar);
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(if label.is_some() { 8. } else { 6. }))
                .gap(px(6.))
                .flex_shrink_0()
                .items_center()
                .rounded(px(p.control_radius))
                .whitespace_nowrap()
                .text_color(if enabled { p.text_secondary } else { p.text_disabled })
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(tip)).build(w, cx))
                .when(enabled, |d| {
                    d.hover(|s| s.bg(p.hover).text_color(p.text))
                        .active(|s| s.bg(p.pressed))
                        .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
                })
                .child(Icon::new(icon).size(px(16.)).when(!enabled, |d| d.opacity(0.55)))
                .children(label.map(|l| div().child(crate::i18n::t(l))))
        };
        let popups_open = self.popups_open(cx);
        let drop = |id: &'static str, icon: L, label: Option<&'static str>, tip: &'static str, enabled: bool, cx: &mut Context<Self>, which: fn(&Self, &App) -> crate::ctx_menu::BarMenu| {
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(6.))
                .gap(px(4.))
                .flex_shrink_0()
                .items_center()
                .rounded(px(p.control_radius))
                .whitespace_nowrap()
                .text_color(if enabled { p.text_secondary } else { p.text_disabled })
                .when(!popups_open, |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(tip)).build(w, cx)))
                .child(Icon::new(icon).size(px(16.)))
                .children(label.map(|l| div().child(crate::i18n::t(l))))
                .child(Icon::new(L::ChevronDown).size(px(12.)).text_color(p.text_tertiary))
                .when(enabled, |d| {
                    d.hover(|s| s.bg(p.hover).text_color(p.text)).active(|s| s.bg(p.pressed)).on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        let menu = which(this, cx);
                        // drop down from under the button (the pointer is on it; the button is 28 px high)
                        let pos = point(ev.position().x - px(14.), ev.position().y + px(18.));
                        this.active_tab().clone().update(cx, |v, cx| v.show_bar_menu(menu, pos, window, cx));
                    }))
                })
        };
        let sep = || div().w(px(1.)).h(px(20.)).mx_1().bg(p.divider);
        // which buttons the user wants (Settings > Appearance > Toolbar buttons)
        let (hidden, extras, commands) = {
            let st = cx.global::<crate::settings::Settings>();
            (st.toolbar_hidden.clone(), st.toolbar_extra.clone(), st.toolbar_commands.clone())
        };
        let on = |k: &str| !hidden.iter().any(|h| h == k);
        let extra = |k: &str| extras.iter().any(|h| h == k);
        let mut inline_commands = Vec::new();
        let mut menu_commands = Vec::new();
        for (i, command) in commands.into_iter().enumerate() {
            let picture = command.icon_path()
                .and_then(|path| self.images.update(cx, |cache, cx| cache.shell_icon(&path, 32, cx)));
            if command.in_menu {
                let label = command.menu_label().to_owned();
                let icon = picture.map_or(MenuIcon::Lucide(L::SquareTerminal), MenuIcon::Image);
                let view = self.active_tab().downgrade();
                menu_commands.push(Item::action(label, icon, move |window, cx| {
                    if let Some(view) = view.upgrade() {
                        let view = view.read(cx);
                        crate::quick_commands::run(command.clone(), view.current_dir(), view.selection_paths(), window, cx);
                    }
                }));
            } else {
                inline_commands.push((i, command, picture));
            }
        }
        let menu_name = crate::i18n::tx("Custom commands");
        let bar_actions = crate::custom_actions::toolbar_actions(cx);
        let bar_fit = crate::custom_actions::toolbar_fit(self.viewport_w);

        // The search field at the bar's right end (the mockups'): typing in it filters this folder, Enter searches the folders inside;
        // a search that is on shows its words here with an x, and the history drops down under it.
        let searching = self.search_mode;
        let live_query = self.search.read(cx).value().to_string();
        let active_query: Option<String> = match self.active_tab().read(cx).location() {
            Location::Search { query, .. } => Some(query.clone()),
            _ => (!live_query.is_empty()).then_some(live_query),
        };
        let history = if searching { self.render_search_history(p, cx) } else { None };
        let search_field = h_flex()
            .id("cb-search")
            .flex_shrink(1.)
            .min_w(px(140.))
            .w(px(240.))
            .h(px(28.))
            .px(px(8.))
            .gap(px(8.))
            .items_center()
            .relative()
            .rounded(px(p.control_radius + 2.))
            .bg(p.field_bg)
            .border_1()
            .border_color(if searching { p.accent } else { p.field_border })
            .text_color(p.text_tertiary)
            .child(Icon::new(L::Search).size(px(16.)).flex_shrink_0())
            .map(|d| {
                if searching {
                    d.child(div().flex_1().min_w_0().text_color(p.text).child(Input::new(&self.search).appearance(false).small().px_0()))
                } else if let Some(q) = active_query.clone() {
                    d.child(div().flex_1().min_w_0().truncate().text_color(p.text).child(q)).child(
                        div()
                            .id("cb-search-x")
                            .size(px(20.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|s| s.bg(p.hover).text_color(p.text))
                            .child(Icon::new(L::X).size(px(12.)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, window, cx| this.exit_search(window, cx))),
                    )
                } else {
                    d.child(div().flex_1().min_w_0().truncate().child(format!("{} {}", t!("Search in"), folder_name)))
                        .child(div().flex_shrink_0().text_size(px(p.fs_xs)).when(!p.font_mono.is_empty(), |d| d.font_family(p.font_mono)).child("Ctrl F"))
                }
            })
            .children(history)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.search_mode && ev.keystroke.key == "escape" {
                    this.exit_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                if this.search_mode {
                    crate::rtl::handle_key_down(&this.search, ev, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.search_mode {
                    this.enter_search(window, cx)
                }
            }));

        h_flex()
            .h(px(p.toolbar_h))
            .px(px(10.))
            .gap(px(2.))
            .items_center()
            .text_size(px(p.fs))
            // New folder, with the other new items (file types, shortcut, ...) under the arrow beside it
            .children((on("new") && !listed).then(|| {
                cmd("cb-newfolder", L::FolderPlus, Some("New folder"), "New folder  (Ctrl+Shift+N)", in_folder, cx, |_, w, cx| w.dispatch_action(Box::new(NewFolder), cx))
            }))
            .children((on("new") && !listed).then(|| {
                div()
                    .id("cb-new")
                    .h(px(28.))
                    .w(px(18.))
                    .ml(px(-2.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(p.control_radius))
                    .text_color(if in_folder { p.text_tertiary } else { p.text_disabled })
                    .when(!popups_open, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("New item")).build(w, cx)))
                    .child(Icon::new(L::ChevronDown).size(px(12.)))
                    .when(in_folder, |d| {
                        d.hover(|s| s.bg(p.hover).text_color(p.text)).on_click(cx.listener(|this, ev: &ClickEvent, window, cx| {
                            let pos = point(ev.position().x - px(14.), ev.position().y + px(18.));
                            this.active_tab().clone().update(cx, |v, cx| v.show_bar_menu(crate::ctx_menu::BarMenu::New, pos, window, cx));
                        }))
                    })
            }))
            .children(on("copy").then(|| {
                cmd("cb-copy", L::Copy, Some("Copy"), "Copy  (Ctrl+C)", has_sel, cx, |this, w, cx| {
                    this.with_tab(cx, |v, cx| v.copy_to_clipboard(leaf_sys::fileops::Transfer::Copy, w, cx))
                })
            }))
            // Move (the mockups' name for cut): the items wait greyed until they are pasted where they go; Paste is under "More"
            .children(on("cut").then(|| {
                cmd("cb-cut", L::Scissors, Some("Move"), "Move: cut, then paste where it goes  (Ctrl+X)", writable_sel, cx, |this, w, cx| {
                    this.with_tab(cx, |v, cx| v.copy_to_clipboard(leaf_sys::fileops::Transfer::Move, w, cx))
                })
            }))
            // the mockups' "Move": straight into the other pane's folder; only while the tab is split
            .children((on("cut") && split).then(|| {
                // (its name from 1400 px on: in a narrower window the bar keeps the search field whole, the mockups', and this button
                // its arrow and tooltip)
                cmd("cb-to-pane", L::MoveRight, (self.viewport_w >= 1400.).then_some("To other pane"), "Move to other pane  (Shift+F6)", writable_sel, cx, |_, w, cx| {
                    w.dispatch_action(Box::new(crate::actions::MoveToOtherPane), cx)
                })
            }))
            .children(on("rename").then(|| {
                cmd("cb-rename", L::TextCursorInput, Some("Rename"), "Rename  (F2)", writable_sel, cx, |this, w, cx| this.with_tab(cx, |v, cx| v.rename_selection(w, cx)))
            }))
            .children(on("delete").then(|| {
                cmd("cb-delete", L::Trash, Some("Delete"), "Delete  (Del)", writable_sel, cx, |this, w, cx| this.with_tab(cx, |v, cx| v.delete_selected(false, w, cx)))
            }))
            .children(on("share").then(|| cmd("cb-share", L::Share2, None, "Share", writable_sel, cx, |this, w, cx| this.with_tab(cx, |v, cx| v.share(w, cx)))))
            .children(on("props").then(|| {
                cmd("cb-props", L::Wrench, None, "Properties  (Alt+Enter)", in_folder || (listed && has_sel), cx, |this, _, cx| {
                    this.with_tab(cx, |v, cx| if v.selection_paths().is_empty() { v.folder_properties(cx) } else { v.properties(cx) })
                })
            }))
            .children((extra("terminal") && !listed).then(|| cmd("cb-terminal", L::SquareTerminal, None, "Open in Terminal  (Ctrl+Shift+`)", in_folder, cx, |_, w, cx| w.dispatch_action(Box::new(OpenTerminal), cx))))
            .children((extra("hidden") && !listed).then(|| cmd("cb-hidden", L::Eye, None, "Show/hide hidden items  (Ctrl+H)", in_folder, cx, |_, w, cx| w.dispatch_action(Box::new(ToggleHidden), cx))))
            .children(extra("refresh").then(|| cmd("cb-refresh", L::RotateCw, None, "Refresh  (F5)", true, cx, |_, w, cx| w.dispatch_action(Box::new(Refresh), cx))))
            // undo, the selection commands, properties, Settings (issue 55, Explorer's "See more")
            .children((on("more") && !listed).then(|| drop("cb-more", L::Ellipsis, None, "More", in_folder, cx, |_, _| crate::ctx_menu::BarMenu::More)))
            .children((!inline_commands.is_empty() || !menu_commands.is_empty()).then(sep))
            .when(!inline_commands.is_empty(), |d| {
                d.child(
                    h_flex().id("cb-quick-commands").min_w_0().overflow_x_scroll().gap_1().children(
                        inline_commands.into_iter().map(|(i, command, picture)| {
                            let tip = command.tooltip_text().to_owned();
                            let label = command.button_label().map(str::to_owned);
                            let icon = if let Some(picture) = picture {
                                img(picture).size(px(18.)).object_fit(ObjectFit::Contain).flex_shrink_0().into_any_element()
                            } else {
                                Icon::new(L::SquareTerminal).size(px(18.)).flex_shrink_0().into_any_element()
                            };
                            div()
                                .id(("cb-command", i))
                                .role(Role::Button)
                                .aria_label(tip.clone())
                                .h(px(32.))
                                .max_w(px(160.))
                                .flex_shrink_0()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .rounded(theme::RADIUS_SM)
                                .text_sm()
                                .tab_index(0)
                                .cursor_pointer()
                                .hover(|s| s.bg(p.hover))
                                .active(|s| s.bg(p.pressed))
                                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                                .when(label.is_none(), |d| d.w(px(36.)).justify_center())
                                .child(icon)
                                .children(label.map(|label| div().truncate().child(label)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let view = this.active_tab().read(cx);
                                    crate::quick_commands::run(command.clone(), view.current_dir(), view.selection_paths(), window, cx);
                                }))
                        }),
                    ),
                )
            })
            .when(!menu_commands.is_empty(), |d| {
                d.child(
                    h_flex().id("cb-quick-menu")
                        .role(Role::Button).aria_label(menu_name)
                        .h(px(32.)).px_2().gap_1().flex_shrink_0().items_center()
                        .rounded(theme::RADIUS_SM).tab_index(0).cursor_pointer()
                        .hover(|s| s.bg(p.hover)).active(|s| s.bg(p.pressed))
                        .when(!popups_open, |d| d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(menu_name).build(w, cx)))
                        .child(Icon::new(L::Layers).size(px(16.)))
                        .child(Icon::new(L::ChevronDown).size(px(12.)).text_color(p.text_secondary))
                        .on_click(move |ev, window, cx| {
                            let pos = point(ev.position().x - px(14.), ev.position().y + px(18.));
                            crate::fluent_menu::show(Menu { quick: Vec::new(), items: menu_commands.clone() }, pos, window, cx);
                        }),
                )
            })
            // the user's own actions with `where = toolbar` (Settings > Your actions); the ones without room are in More
            .children((!bar_actions.is_empty()).then(sep))
            .children(bar_actions.into_iter().take(bar_fit).enumerate().map(|(i, a)| {
                let tip = a.name.clone();
                div()
                    .id(("cb-action", i))
                    .role(Role::Button)
                    .aria_label(tip.clone())
                    .h(px(32.))
                    .max_w(px(160.))
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .rounded(theme::RADIUS_SM)
                    .text_sm()
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .active(|s| s.bg(p.pressed))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                    .child(Icon::new(crate::custom_actions::icon_of(&a)).size(px(18.)).flex_shrink_0())
                    .child(div().truncate().child(a.name.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let view = this.active_tab().read(cx);
                        crate::custom_actions::run(a.clone(), view.selection_paths(), view.current_dir(), window, cx);
                    }))
            }))
            .children(on("view").then(sep))
            // details or grid, as a two-button switch (the other layouts are in the Appearance menu)
            .children(on("view").then(|| {
                let seg = |id: &'static str, icon: L, chosen: bool, run: fn(&mut Window, &mut App)| {
                    div()
                        .id(id)
                        .w(px(28.))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px((p.control_radius).max(2.)))
                        .text_color(if chosen { p.text } else { p.text_tertiary })
                        .when(chosen, |d| d.bg(p.seg_on))
                        .when(!chosen, |d| d.hover(|s| s.text_color(p.text)))
                        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip_text(id)).build(w, cx))
                        .child(Icon::new(icon).size(px(16.)))
                        .on_click(move |_, w, cx| run(w, cx))
                };
                h_flex()
                    .flex_shrink_0()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(p.control_radius + 2.))
                    .bg(p.field_bg)
                    .border_1()
                    .border_color(p.field_border)
                    .child(seg("v-details", L::List, mode == ViewMode::Details, |w, cx| w.dispatch_action(Box::new(ViewDetails), cx)))
                    .child(seg("v-grid", L::LayoutGrid, mode == ViewMode::Grid, |w, cx| w.dispatch_action(Box::new(ViewGrid), cx)))
            }))
            // the sort key by name, with its direction
            .children(on("sort").then(|| {
                h_flex()
                    .id("cb-sort")
                    .h(px(28.))
                    .px(px(8.))
                    .gap(px(6.))
                    .flex_shrink_0()
                    .items_center()
                    .rounded(px(p.control_radius))
                    .whitespace_nowrap()
                    .text_color(if in_folder || listed { p.text_secondary } else { p.text_disabled })
                    .when(!popups_open, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Sort and group")).build(w, cx)))
                    .child(Icon::new(L::ArrowDownUp).size(px(16.)))
                    .child(sort_label)
                    .child(Icon::new(if sort_asc { L::ChevronUp } else { L::ChevronDown }).size(px(12.)))
                    .when(in_folder || listed, |d| {
                        d.hover(|s| s.bg(p.hover).text_color(p.text)).on_click(cx.listener(|this, ev: &ClickEvent, window, cx| {
                            let pos = point(ev.position().x - px(14.), ev.position().y + px(18.));
                            this.active_tab().clone().update(cx, |v, cx| v.show_bar_menu(crate::ctx_menu::BarMenu::Sort, pos, window, cx));
                        }))
                    })
            }))
            .children(on("preview").then(|| {
                h_flex()
                    .id("cb-preview")
                    .h(px(28.))
                    .px(px(8.))
                    .gap(px(6.))
                    .flex_shrink_0()
                    .items_center()
                    .rounded(px(p.control_radius))
                    .whitespace_nowrap()
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Preview pane  (Alt+P)")).build(w, cx))
                    .text_color(if preview_open { p.text } else { p.text_secondary })
                    .when(preview_open, |d| d.bg(p.selection))
                    .when(!preview_open, |d| d.hover(|s| s.bg(p.hover).text_color(p.text)))
                    .child(Icon::new(L::Eye).size(px(16.)))
                    .when(!narrow_bar, |d| d.child(t!("Preview")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_preview(cx);
                    }))
            }))
            .children(on("view").then(|| {
                drop("cb-view", L::LayoutPanelLeft, None, "Appearance", true, cx, move |this, _| crate::ctx_menu::BarMenu::View { dual: this.tabs[this.active].right.is_some(), shelf: this.shelf_open })
            }))
            .child(div().flex_1().min_w(px(8.)))
            .children(on("filter").then(|| {
                div()
                    .id("cb-filter")
                    .h(px(28.))
                    .w(px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(px(p.control_radius))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Filter")).build(w, cx))
                    .text_color(p.text_secondary)
                    .when(filter_on, |d| d.bg(p.selection).text_color(p.accent))
                    .when(!filter_on, |d| d.hover(|s| s.bg(p.hover)))
                    .when(in_folder || listed, |d| d.on_click(cx.listener(|this, _, window, cx| this.toggle_filter_bar(window, cx))))
                    .when(!(in_folder || listed), |d| d.text_color(p.text_disabled))
                    .child(Icon::new(L::ListFilter).size(px(16.)))
            }))
            .child(search_field)
    }

    /// The tool bar row on the pages that have no folder of their own (Home, Settings, Sync jobs, Disk usage, the result pages): the row keeps
    /// its height there (so opening a folder does not move the frame) and holds what works on that page. Home: Customize Home, Refresh and a
    /// search of the indexed drives (the quick search window, the only search that needs no folder). The others: the page's title, quietly.
    fn render_page_bar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let loc = self.active_tab().read(cx).location().clone();
        let narrow_bar = self.viewport_w <= 1200.;
        let btn = |id: &'static str, icon: L, label: Option<&'static str>, tip: &'static str, cx: &mut Context<Self>, run: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let label = label.filter(|_| !narrow_bar);
            h_flex()
                .id(id)
                .h(px(28.))
                .px(px(if label.is_some() { 8. } else { 6. }))
                .gap(px(6.))
                .flex_shrink_0()
                .items_center()
                .rounded(px(p.control_radius))
                .whitespace_nowrap()
                .text_color(p.text_secondary)
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(crate::i18n::t(tip)).build(w, cx))
                .hover(|s| s.bg(p.hover).text_color(p.text))
                .active(|s| s.bg(p.pressed))
                .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
                .child(Icon::new(icon).size(px(16.)))
                .children(label.map(|l| div().child(crate::i18n::t(l))))
        };
        let row = h_flex().h(px(p.toolbar_h)).px(px(10.)).gap(px(2.)).items_center().text_size(px(p.fs));
        match loc {
            Location::Home => row
                .child(btn("pb-customize", L::LayoutGrid, Some("Customize Home"), "Customize Home", cx, |_, w, cx| w.dispatch_action(Box::new(CustomizeHome), cx)))
                .child(btn("pb-refresh", L::RotateCw, None, "Refresh  (F5)", cx, |_, w, cx| w.dispatch_action(Box::new(Refresh), cx)))
                .child(div().flex_1().min_w(px(8.)))
                .child(
                    h_flex()
                        .id("pb-search")
                        .flex_shrink(1.)
                        .min_w(px(140.))
                        .w(px(240.))
                        .h(px(28.))
                        .px(px(8.))
                        .gap(px(8.))
                        .items_center()
                        .rounded(px(p.control_radius + 2.))
                        .bg(p.field_bg)
                        .border_1()
                        .border_color(p.field_border)
                        .text_color(p.text_tertiary)
                        .hover(|s| s.border_color(p.accent))
                        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Search the indexed drives")).build(w, cx))
                        .child(Icon::new(L::Search).size(px(16.)).flex_shrink_0())
                        .child(div().flex_1().min_w_0().truncate().child(t!("Search everywhere")))
                        .on_click(|_, _, cx| crate::quick_search::toggle(cx)),
                ),
            other => row.child(div().min_w_0().truncate().px(px(6.)).text_color(p.text_secondary).child(crate::i18n::td(&other.title()))),
        }
    }

    fn render_breadcrumb(&mut self, loc: &Location, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let drop_bg = p.selection;
        // (the row fills the box; its own width, measured as it is laid out, is the room the segments have)
        let room = self.crumb_room.clone();
        // (the mockups' path: the theme's text size (13 px; it was 14), crumbs padded 6 px, the arrows 12 px with 1 px either side,
        // nothing between)
        let mut row = h_flex()
            .flex_1()
            .min_w_0()
            .relative()
            .items_center()
            .text_size(px(p.fs))
            // Index: the path in the monospace face, 12 px (about the width the estimate below counts with)
            .when(p.mono_chrome && !p.font_mono.is_empty(), |d| d.font_family(p.font_mono).text_size(px(12.)))
            .overflow_hidden()
            .child(
            canvas(
                move |bounds, window, _| {
                    let w = f32::from(bounds.size.width);
                    if (room.get() - w).abs() > 1.0 {
                        room.set(w);
                        window.refresh();
                    }
                },
                |_, _, _, _| (),
            )
            .absolute()
            .size_full(),
        );
        let crumbs: Vec<(String, Location)> = match loc {
            Location::Path(path) | Location::Search { root: path, .. } => breadcrumbs(path),
            // the shell's own name for the folder (the location may carry none)
            Location::Shell { .. } => vec![(self.active_tab().read(cx).title(cx), loc.clone())],
            Location::DiskUsage(path) => vec![(crate::disk_usage::title(path), loc.clone())],
            Location::Duplicates(roots) => vec![(crate::duplicates_view::title(roots), loc.clone())],
            Location::BuildJunk(path) => vec![(crate::junk_view::title(path), loc.clone())],
            Location::Changes(path) => vec![(crate::changes_view::title(path), loc.clone())],
            Location::Brain(path) => vec![(crate::brain_view::title(path), loc.clone())],
            // `server`, then one crumb per folder on it
            Location::Remote(v) => {
                let root = v.root();
                let top = if root.authority().is_empty() { root.scheme().to_string() } else { root.authority().to_string() };
                let mut out = vec![(top, Location::Remote(root.clone()))];
                let mut cur = root;
                for seg in v.segments() {
                    cur = cur.join(seg);
                    out.push((seg.to_string(), Location::Remote(cur.clone())));
                }
                out
            }
            other => vec![(crate::i18n::td(&other.title()), other.clone())],
        };
        // Too long for the address box: hide leading segments after the drive behind "…"
        // (the current folder must stay visible). Widths are estimated from text length.
        let measured = self.crumb_room.get();
        let budget = if measured > 0. { measured + 30. } else { self.viewport_w - 260. - 4. * 36. - 9. * 36. - 90. };
        let est = |l: &str| l.chars().count() as f32 * 7.4 + 30.;
        let mut crumbs = crumbs;
        let mut hidden: Option<Location> = None;
        while crumbs.len() > 2 && crumbs.iter().map(|(l, _)| est(l)).sum::<f32>() + 60. > budget {
            let (_, loc) = crumbs.remove(1);
            hidden = Some(loc);
        }
        let n = crumbs.len();
        // a folder on a drive starts at the drive, with the drive's icon in its crumb (the mockups': no house and no arrow before
        // "C:"; Home is the title bar's button)
        let drive_root = crumbs.first().and_then(|(_, t)| t.path()).is_some_and(|p| {
            let b = p.as_os_str().as_encoded_bytes();
            (2..=3).contains(&b.len()) && b[1] == b':'
        });
        let home_icon = div()
            .id("crumb-home")
            .px_1()
            .py_0p5()
            .rounded(theme::RADIUS_SM)
            .hover(|s| s.bg(p.hover))
            .child(Icon::new(L::House).size(px(14.)).text_color(p.text_secondary))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.navigate(Location::Home, window, cx);
            }));
        if !drive_root {
            row = row.child(home_icon);
        }
        let chevron = |id: (&'static str, usize), dir: Option<PathBuf>, p: &Palette, cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(1.))
                .py_1()
                .rounded(theme::RADIUS_SM)
                .hover(|s| s.bg(p.hover))
                .child(Icon::new(L::ChevronRight).size(px(12.)).text_color(p.text_tertiary))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.crumb_menu(dir.clone(), window, cx);
                }))
        };
        let mut prev_dir: Option<PathBuf> = None;
        let last_dir = crumbs.last().and_then(|(_, t)| t.path().map(Path::to_path_buf));
        for (i, (label, target)) in crumbs.into_iter().enumerate() {
            if i == 1 {
                if let Some(h) = hidden.clone() {
                    // "…" stands for the hidden segments and opens the deepest of them
                    row = row.child(chevron(("crumb-chev-h", 0), prev_dir.clone(), p, cx));
                    prev_dir = h.path().map(Path::to_path_buf);
                    row = row.child(
                        div()
                            .id("crumb-ellipsis")
                            .px_1p5()
                            .py_0p5()
                            .rounded(theme::RADIUS_SM)
                            .text_color(p.text_secondary)
                            .hover(|s| s.bg(p.hover).text_color(p.text))
                            .child("…")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.navigate(h.clone(), window, cx);
                            })),
                    );
                }
            }
            if !(drive_root && i == 0) {
                row = row.child(chevron(("crumb-chev", i), prev_dir.clone(), p, cx));
            }
            prev_dir = target.path().map(Path::to_path_buf);
            let last = i + 1 == n;
            row = row.child(
                h_flex()
                    .id(("crumb", i))
                    .gap(px(6.))
                    .items_center()
                    .px_1p5()
                    .py_0p5()
                    .rounded(theme::RADIUS_SM)
                    .whitespace_nowrap()
                    .when(drive_root && i == 0, |d| d.child(Icon::new(L::HardDrive).size(px(16.)).text_color(p.text_secondary)))
                    // (the mockups' current folder: 600)
                    .when(last, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when(!last, |d| d.text_color(p.text_secondary))
                    .hover(|s| s.bg(p.hover).text_color(p.text))
                    .child(crate::rtl::display_text(&label))
                    .when_some(target.path().map(Path::to_path_buf), |d, dir| {
                        d.drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
                            .on_drag_move::<ExternalPaths>(crate::folder_view::drop_effect_over(dir.clone()))
                            .on_drop(move |paths: &ExternalPaths, _, cx| drop_files(paths, dir.clone(), cx))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.navigate(target.clone(), window, cx);
                    })),
            );
        }
        // (the arrow to the folders inside the current one shows while the pointer is over the path: the mockups' path ends at its name)
        if let Some(d) = last_dir {
            row = row.child(chevron(("crumb-chev", 999), Some(d), p, cx).invisible().group_hover("crumbs", |s| s.visible()));
        }
        row.group("crumbs")
    }

    fn sidebar_item(
        &self,
        id: ElementId,
        icon: L,
        icon_color: Hsla,
        label: String,
        target: Location,
        active: bool,
        extra: Option<AnyElement>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // (the mockups: quiet icons, the current place's in the accent; a tag keeps its colour, as a dot)
        let wt_id = id.clone();
        let host_kind = self.sidebar_host_kind.take();
        let tag = matches!(icon, L::Tag);
        let icon_color = if active && !tag { p.accent } else if icon_color == p.accent || icon_color == p.text_secondary { p.text_tertiary } else { icon_color };
        let target2 = target.clone();
        let target3 = target.clone();
        let drop_dir = target.path().map(Path::to_path_buf);
        let drop_bg = p.selection;
        let drop_border = p.accent;
        // on the rail only the icon; hovering the rail opens the full sidebar over the content
        let rail = self.sidebar_rail;
        // Over glass the selected row is a lens the engine draws behind the scene: no fill and no accent bar, but a probe that reports
        // the row's place on every paint (a scroll or a resize moves the lens with it). Two rows can be "selected" at once (a folder
        // that is pinned and also a library): the first gets the lens, the other keeps the Fluent selection.
        let lens_key = (active && self.sidebar_lens && !self.sidebar_lens_claimed.replace(true)).then(|| crate::glass::key_of(&target));
        let fluent = active && lens_key.is_none();
        // drives look alike as bare icons: on the rail their letter sits in the icon's corner
        let letter = target3.path().filter(|_| rail).and_then(|p| {
            let s = p.to_str()?;
            let b = s.as_bytes();
            (b.len() <= 3 && b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()).then(|| (b[0] as char).to_ascii_uppercase())
        });
        let is_wt = matches!(&wt_id, ElementId::NamedInteger(n, _) if &**n == "sb-wt");
        let is_lane = matches!(&wt_id, ElementId::NamedInteger(n, _) if &**n == "sb-lane");
        let two_lines = extra.is_some();
        // a mounted share of the Network list: its own menu (Disconnect); a saved host: its own menu (Forget saved password...)
        let net_share = match &target3 {
            Location::Remote(v) if matches!(&wt_id, ElementId::NamedInteger(n, _) if &**n == "sb-net") => Some((v.clone(), false)),
            Location::Remote(v) if matches!(&wt_id, ElementId::NamedInteger(n, _) if &**n == "sb-host") => Some((v.clone(), true)),
            // an SMB host on Windows opens as `\\host\share`: its menu is the host's (Disconnect), not a folder's
            Location::Path(p) if matches!(&wt_id, ElementId::NamedInteger(n, _) if &**n == "sb-host") => leaf_remote::smb::unc_share(p).and_then(|(h, s)| {
                let a = if s.eq_ignore_ascii_case("ipc$") { format!("smb://{h}") } else { format!("smb://{h}/{s}") };
                rf_core::vpath::VPath::parse(&a).ok().map(|v| (v, true))
            }),
            _ => None,
        };
        let tag_dot = target3.path().filter(|p| p.parent().is_some()).and_then(|p| crate::tags::first_color(p, cx));
        h_flex()
            .id(id)
            .flex_shrink_0()
            .min_h(px(if rail { 36. } else { p.sidebar_row }))
            .px_2()
            .py(px(2.))
            .gap(px(10.))
            .items_center()
            .when(rail, |d| d.justify_center())
            .rounded(px(p.control_radius))
            .text_size(px(p.fs))
            .when(p.name_weight != 400., |d| d.font_weight(FontWeight(p.name_weight)))
            .text_color(if active { p.place_text } else { p.text_secondary })
            .relative()
            // the current place: the theme's fill and outline (the selection pill where the theme has a marker)
            .when(fluent, |d| d.bg(p.place).child(theme::selection_pill(p)))
            // (the outline's pixel comes off the padding: the icon and the label stay where the other rows have theirs, as under the
            // mockups' inset ring)
            .when(fluent && p.place_border.a > 0., |d| d.border_1().border_color(p.place_border).px(px(7.)))
            // while the row moves (the sidebar scrolls) the scene paints the pill itself: the engine's lens would trail the row
            .when(lens_key.is_some() && crate::glass::lens_phase(self.glass_hwnd, crate::glass::SLOT_SIDEBAR) != crate::glass::LensPhase::Rest, |d| {
                d.child(crate::glass::lens_stand_in(p.dark, 32.))
            })
            .when_some(lens_key, |d, key| d.child(crate::glass::lens_probe(crate::glass::SLOT_SIDEBAR, key)))
            .when(!active, |d| d.hover(|s| s.bg(p.hover).text_color(p.text)))
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .when(tag && !p.square_marks, |d| {
                        // a tag: a dot in its colour with a soft ring (16 px wide like the icons)
                        d.size(px(16.)).flex().items_center().justify_center().rounded_full().bg(icon_color.opacity(0.22)).child(div().size(px(10.)).rounded_full().bg(icon_color))
                    })
                    // (Index: a plain square)
                    .when(tag && p.square_marks, |d| d.size(px(16.)).flex().items_center().justify_center().child(div().size(px(9.)).bg(icon_color)))
                    .when(!tag, |d| {
                        // a saved host: its device icon
                        let size = if rail { 18. } else { 16. };
                        match host_kind.and_then(|k| crate::host_icons::element(k, size, icon_color, p.accent)) {
                            Some(el) => d.child(el),
                            None => d.child(Icon::new(icon).size(px(size)).text_color(icon_color)),
                        }
                    })
                    .when_some(tag_dot, |d, c| d.child(crate::tags::corner_dot(c, p.card_bg)))
                    .when_some(letter, |d, l| {
                        d.child(
                            div()
                                .absolute()
                                .right(px(-9.))
                                .bottom(px(-7.))
                                .min_w(px(15.))
                                .h(px(15.))
                                .px(px(2.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .bg(p.sidebar_bg)
                                .border_1()
                                .border_color(p.border)
                                .text_size(px(11.))
                                .line_height(px(13.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(p.text)
                                .child(l.to_string()),
                        )
                    }),
            )
            .when(!rail, |d| {
                d.child(
                    v_flex()
                        .flex_1()
                        .overflow_hidden()
                        // (a row with a second line: both lines at 1.25, the mockups' two-line rows of 34 px)
                        .child(div().when(two_lines, |d| d.line_height(px(p.fs * 1.25))).whitespace_nowrap().overflow_hidden().text_ellipsis().child(label))
                        .children(extra),
                )
            })

            .on_click(cx.listener(move |this, _, window, cx| this.navigate(target.clone(), window, cx)))
            .when_some(target3.path().map(Path::to_path_buf).filter(|_| net_share.is_none()), |d, path| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if is_wt {
                            this.wt_menu(path.clone(), window, cx);
                        } else if is_lane {
                            this.lane_menu(path.clone(), window, cx);
                        } else {
                            this.sidebar_menu(path.clone(), window, cx);
                        }
                    }),
                )
            })
            .when_some(net_share, |d, (v, host)| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if host {
                            this.host_menu(v.clone(), window, cx);
                        } else {
                            this.net_menu(v.clone(), window, cx);
                        }
                    }),
                )
            })
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    let keep = this.active;
                    this.open_tab(target2.clone(), window, cx);
                    this.activate(keep, window, cx);
                }),
            )
            .when_some(drop_dir, |d, dir| {
                d.drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg).border_1().border_color(drop_border))
                    .on_drag_move::<ExternalPaths>(crate::folder_view::drop_effect_over(dir.clone()))
                    .on_drop(move |paths: &ExternalPaths, _, cx| drop_files(paths, dir.clone(), cx))
            })
    }

    fn render_sidebar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cur = self.active_tab().read(cx).location().clone();
        let places = self.places.read(cx);
        let pinned = places.pinned.clone();
        let pinned_n = pinned.len();
        let drives = places.drives.clone();
        // (the pins and drives are read off the UI thread and are there a moment after the window: see the tags below)
        let places_loaded = places.loaded;
        let rail = self.sidebar_rail;
        let header = |t: &'static str| {
            if rail {
                return div().mx_2().my_2().h(px(1.)).bg(p.divider);
            }
            div()
                .h(px(28.))
                // (never squeezed: the sidebar scrolls when its sections are taller than the window, and its rows and headings used to
                // shrink first, the headings a few pixels each)
                .flex_shrink_0()
                // (the mockups' 12 px between sections)
                .mt(px(12.))
                .px_2()
                .flex()
                .items_center()
                .text_size(px(p.fs_xs))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text_tertiary)
                .child(t)
                // Index: the label ruled off to the edge
                .when(p.section_rule, |d| d.gap_2().child(div().flex_1().h(px(1.)).bg(p.border)))
        };
        // the "drop here to pin" row lives as long as a file drag does
        if !cx.has_active_drag() {
            self.sidebar_ext_drag = false;
        }
        let ext_drag = self.sidebar_ext_drag;
        let (drop_bg, drop_border) = (p.selection, p.accent);
        let mut col = v_flex()
            .id("sidebar")
            .w_full()
            .flex_1()
            .min_h_0()
            .p(px(8.))
            .overflow_y_scroll()
            .on_drag_move::<ExternalPaths>(cx.listener(|this, _: &DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.sidebar_ext_drag {
                    this.sidebar_ext_drag = true;
                    cx.notify();
                }
            }));
        // Places (the mockups'): Home, the pinned folders and the Recycle Bin under one heading; dropping folders on it pins them
        col = col.child(
            div()
                .id("sb-pin-header")
                .rounded(theme::RADIUS_SM)
                .child(header(t!("Places")).mt_0())
                .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.pin_dropped(paths, cx))),
        );
        col = col.child(self.sidebar_item(
            "sb-home".into(),
            L::House,
            p.accent,
            t!("Home").into(),
            Location::Home,
            cur == Location::Home,
            None,
            p,
            cx,
        ));
        let accent = p.accent;
        // the open folder is already a selected row of the sidebar (a pinned folder or a drive): the tree draws no second selection
        let mut shown_elsewhere = pinned.iter().any(|f| cur == Location::Path(f.path.clone()));
        // the place the open folder is in stays lit (the mockups': "Projects" while in Projects\leaf-site): the closest pinned folder
        // that holds it, else the drive it is on (with the folder tree on, the tree marks the folder itself)
        let lit: Option<PathBuf> = (!self.shows_tree(rail)).then(|| cur.path().map(Path::to_path_buf)).flatten().and_then(|here| {
            let best = pinned.iter().map(|f| &f.path).filter(|p| here.starts_with(p)).max_by_key(|p| p.as_os_str().len()).cloned();
            best.or_else(|| drives.iter().map(|d| &d.root).filter(|r| here.starts_with(r)).max_by_key(|r| r.as_os_str().len()).cloned())
        });
        for (i, f) in pinned.into_iter().enumerate() {
            let loc = Location::Path(f.path.clone());
            let active = cur == loc || lit.as_ref() == Some(&f.path);
            let title = crate::i18n::td(&f.title);
            let drag = PinDrag { ix: i, path: f.path.clone(), title: title.clone().into() };
            let target_path = f.path.clone();
            let item = self.sidebar_item(("sb-pin", i).into(), f.icon, p.accent, title, loc, active, None, p, cx);
            // drag a pin onto another to reorder
            col = col.child(
                div()
                    .id(("sb-pin-wrap", i))
                    .flex_shrink_0()
                    .on_drag(drag, |d: &PinDrag, _, _, cx| cx.new(|_| d.clone()))
                    .drag_over::<PinDrag>(move |s, d: &PinDrag, _, _| {
                        // moving down inserts after the target, moving up before it
                        if d.ix < i { s.border_b_2().border_color(accent) } else { s.border_t_2().border_color(accent) }
                    })
                    .on_drop(cx.listener(move |this, d: &PinDrag, _, cx| this.move_pin(&d.path, &target_path, cx)))
                    .child(item),
            );
        }
        if ext_drag {
            col = col.child(
                h_flex()
                    .id("sb-pin-drop")
                    .min_h(px(32.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .border_1()
                    .border_dashed()
                    .border_color(p.divider)
                    .text_sm()
                    .text_color(p.text_tertiary)
                    .child(Icon::new(L::Pin).size(px(16.)).flex_shrink_0())
                    .when(!rail, |d| d.child(t!("Drop here to pin")))
                    .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_bg).border_color(drop_border))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.pin_dropped(paths, cx))),
            );
        }
        // the Recycle Bin with how many items it holds, at the row's right (the mockups' Trash)
        let binned = self.recycle_items(cx);
        let bin = self.sidebar_item(
            "sb-recycle".into(),
            L::Trash,
            p.text_secondary,
            t!("Trash").into(),
            Location::RecycleBin,
            cur == Location::RecycleBin,
            None,
            p,
            cx,
        );
        col = col.child(bin.when(!rail, |d| {
            d.children(binned.map(|n| div().flex_shrink_0().text_size(px(p.fs_xs)).text_color(p.text_tertiary).child(n.to_string())))
        }));
        // the folder tree stands right under the pinned folders, above the drives and the tags (where the user asked for it: the
        // tree is what one works in, the drives and tags are looked at less often)
        let hidden = cx.global::<crate::settings::Settings>().sidebar_hidden.clone();
        let shown = |k: &str| !hidden.iter().any(|h| h == k);
        // (with the folder tree on, the drives are the tree's roots: the user found the second list of them needless)
        let drives = if shown("drives") && !self.shows_tree(rail) { drives } else { Vec::new() };
        shown_elsewhere |= drives.iter().any(|d| cur == Location::Path(d.root.clone()));
        col = self.append_tree(col, &cur, shown_elsewhere, rail, p, cx);
        // The drives fold under their heading when the sidebar is too short for the sections under them (at 800 px the tags were cut
        // off): a click on the heading opens or folds them for good
        let drives_folded = !rail && self.drives_open.map(|o| !o).unwrap_or_else(|| {
            let n_tags = if shown("tags") { cx.global::<crate::tags::Tags>().0.defs.len() } else { 0 };
            let n_cloud = if shown("cloud") { self.places.read(cx).cloud.len() } else { 0 };
            let drive_h = p.sidebar_row + 7. + if self.sidebar_w >= DRIVE_SPACE_MIN_W { p.fs_xs + 5. } else { 0. };
            let rows = (pinned_n + 2 + n_cloud + n_tags) as f32 * p.sidebar_row + drives.len() as f32 * drive_h;
            let heads = (2 + usize::from(n_cloud > 0) + usize::from(n_tags > 0)) as f32 * 38.;
            let room = self.viewport_h - p.titlebar_h - 48. - p.typebar_h - p.status_h - 56.;
            !drives.is_empty() && rows + heads > room
        });
        if !drives.is_empty() && rail {
            col = col.child(header(t!("Drives")));
        } else if !drives.is_empty() {
            let n = drives.len();
            col = col.child(
                header(t!("Drives"))
                    .id("sb-drives-head")
                    .cursor_pointer()
                    .hover(|s| s.text_color(p.text_secondary))
                    .child(Icon::new(if drives_folded { L::ChevronRight } else { L::ChevronDown }).size(px(12.)).flex_shrink_0())
                    .when(drives_folded, |d| d.child(div().text_color(p.text_tertiary).child(n.to_string())))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.drives_open = Some(drives_folded);
                        cx.notify();
                    })),
            );
        }
        let drives = if drives_folded { Vec::new() } else { drives };
        for (i, d) in drives.into_iter().enumerate() {
            let loc = Location::Path(d.root.clone());
            let active = cur == loc || lit.as_ref() == Some(&d.root);
            let frac = d.used_fraction();
            // the bar, and under it "129 GB free of 932 GB" as on the Home page's drive tiles (asked by the first Linux tester); the
            // line only where the sidebar has room for it (not on the rail, not near its narrowest)
            let words = (self.sidebar_w >= DRIVE_SPACE_MIN_W).then(|| format!("{} {} {}", format::size(d.free), t!("free of"), format::size(d.total)));
            let bar = (d.total > 0).then(|| {
                v_flex()
                    .w_full()
                    .child(
                        div()
                            .mt(px(5.))
                            .h(px(2.))
                            .w_full()
                            .rounded_full()
                            .bg(p.pressed)
                            .child(div().h_full().rounded_full().bg(if frac > 0.9 { p.danger } else { p.accent }).w(relative(frac))),
                    )
                    .children(words.map(|w| {
                        div().mt(px(2.)).text_size(px(p.fs_xs)).line_height(px(p.fs_xs + 3.)).font_weight(FontWeight::NORMAL).text_color(p.text_tertiary).whitespace_nowrap().overflow_hidden().text_ellipsis().child(w)
                    }))
                    .into_any_element()
            });
            col = col.child(self.sidebar_item(
                ("sb-drive", i).into(),
                places::drive_icon(&d),
                p.text_secondary,
                crate::places::drive_name(&d),
                loc,
                active,
                bar,
                p,
                cx,
            ));
        }
        // phones and cameras under This PC (issue 49: "The phone also doesn't show up as a location in sidebar"); they are no roots of
        // the folder tree, so they are listed with the tree on too
        let devices = if shown("drives") { self.places.read(cx).devices.clone() } else { Vec::new() };
        if !devices.is_empty() {
            col = col.child(header(t!("Devices")));
            for (i, d) in devices.into_iter().enumerate() {
                let loc = Location::Shell { parsing: d.parsing.clone(), name: d.name.clone() };
                let active = cur == loc;
                col = col.child(self.sidebar_item(("sb-dev", i).into(), L::Usb, p.text_secondary, d.name.clone(), loc, active, None, p, cx));
            }
        }
        col = self.wt_sidebar(col, &cur, rail, p, cx);
        let cloud = visible_places(self.places.read(cx).cloud.clone(), &cx.global::<crate::settings::Settings>().hidden_places);
        if !cloud.is_empty() && shown("cloud") {
            col = col.child(header(t!("Cloud and Linux")));
            for (i, c) in cloud.into_iter().enumerate() {
                let loc = Location::Path(c.path.clone());
                let active = cur == loc;
                col = col.child(self.sidebar_item(("sb-cloud", i).into(), c.icon, p.accent, crate::i18n::td(&c.title), loc, active, None, p, cx));
            }
        }
        let libraries = self.places.read(cx).libraries.clone();
        // Libraries duplicate the pinned folders: off unless turned on in Settings
        if !libraries.is_empty() && cx.global::<crate::settings::Settings>().show_libraries {
            col = col.child(header(t!("Libraries")));
            for (i, l) in libraries.into_iter().enumerate() {
                let loc = Location::Library { file: l.file.clone(), name: l.name.clone() };
                let active = cur == loc;
                col = col.child(self.sidebar_item(("sb-lib", i).into(), L::Library, p.accent, l.name, loc, active, None, p, cx));
            }
        }
        let network = self.places.read(cx).network.clone();
        // (the servers the network announces, found once on a worker: none of them is no error, a calm line says so)
        let servers = self.places.read(cx).servers.clone();
        let servers: Vec<(String, PathBuf)> = servers.unwrap_or_default().into_iter().filter(|(_, u)| !network.iter().any(|(_, t)| t == u)).collect();
        let nothing_found = leaf_sys::network::DISCOVERS && network.is_empty() && servers.is_empty() && self.places.read(cx).servers.is_some();
        if (!network.is_empty() || !servers.is_empty() || nothing_found) && shown("network") {
            col = col.child(header(t!("Network")));
            for (i, (name, target)) in network.into_iter().enumerate() {
                let loc = crate::remote::network_place(target);
                let active = cur == loc;
                col = col.child(self.sidebar_item(("sb-net", i).into(), L::Network, p.text_secondary, name, loc, active, None, p, cx));
            }
            for (i, (name, target)) in servers.into_iter().enumerate() {
                let loc = crate::remote::network_place(target);
                let active = cur == loc;
                col = col.child(self.sidebar_item(("sb-net-srv", i).into(), L::Server, p.text_secondary, name, loc, active, None, p, cx));
            }
            if nothing_found && !rail {
                col = col.child(div().px_2().h(px(24.)).flex().items_center().text_size(px(p.fs_xs)).text_color(p.text_tertiary).child(t!("No servers found on the network")));
            }
        }
        // saved hosts (Settings > Hosts), only while there are any
        let hosts = if shown("hosts") && places_loaded && crate::modules::enabled(cx, crate::modules::Module::Servers) { crate::hosts_ui::all(cx) } else { Vec::new() };
        if !hosts.is_empty() {
            col = col.child(header(t!("Hosts")));
        }
        for (i, h) in hosts.into_iter().enumerate() {
            let Some(loc) = crate::hosts_ui::location(&h) else { continue };
            let active = matches!((&cur, &loc), (Location::Remote(a), Location::Remote(b)) if a.starts_with(b));
            self.sidebar_host_kind.set(Some(h.kind));
            col = col.child(self.sidebar_item(("sb-host", i).into(), L::Server, p.text_secondary, h.name.clone(), loc, active, None, p, cx));
            // the host's bookmarked folders under it
            for (j, b) in h.bookmarks.iter().enumerate() {
                let Some(loc) = crate::hosts_ui::bookmark_location(&h, b) else { continue };
                let active = cur == loc;
                let name = b.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(b.as_str()).to_string();
                col = col.child(self.sidebar_item(("sb-host-bm", i * 1000 + j).into(), L::Bookmark, p.text_secondary, name, loc, active, None, p, cx));
            }
        }
        // LEAF_MOCKUP_DEMO: the mockup's two hosts (one offline), with their second line and state dot
        let demo = crate::mockup_demo::on() && !rail;
        let state_dot = |on: bool, color: Hsla| {
            div().size(px(6.)).flex_shrink_0().rounded_full().when(on, |d| d.bg(color)).when(!on, |d| d.border(px(1.5)).border_color(p.text_tertiary))
        };
        let second_line = |text: &'static str| {
            div().text_size(px(p.fs_xs)).line_height(px(p.fs_xs * 1.25)).text_color(p.text_tertiary).whitespace_nowrap().overflow_hidden().text_ellipsis().child(text).into_any_element()
        };
        if demo {
            col = col.child(header(t!("Hosts")).child(div().flex_1()).child(Icon::new(L::Plus).size(px(12.)).mr(px(4.)).text_color(p.text_tertiary)));
            for (i, (name, line, online)) in crate::mockup_demo::HOSTS.into_iter().enumerate() {
                let item = self.sidebar_item(("sb-demo-host", i).into(), L::Server, p.text_secondary, name.to_string(), Location::Home, false, Some(second_line(line)), p, cx);
                col = col.child(item.child(state_dot(online, p.success)));
            }
        }
        // The tags are known at once, the pins and drives above them a moment later: drawn before those, the coloured tag rows stood
        // at the top of the sidebar for the first 90 ms of a cold start and then jumped down (seen frame by frame: a user's report of
        // a red and blue flash when a folder is opened with the app closed). They wait for the places.
        let tag_defs = if shown("tags") && places_loaded { cx.global::<crate::tags::Tags>().0.defs.clone() } else { Vec::new() };
        if !tag_defs.is_empty() {
            col = col.child(header(t!("Tags")));
        }
        for d in tag_defs {
            let loc = Location::Tag(d.id);
            let active = cur == loc;
            // how many items wear the tag, at the right (none: no number)
            let n = cx.global::<crate::tags::Tags>().0.count_with(d.id);
            let item = self.sidebar_item(
                ("sb-tag", d.id as usize).into(),
                L::Tag,
                crate::tags::color(d.color),
                crate::i18n::td(&d.name),
                loc,
                active,
                None,
                p,
                cx,
            );
            col = col.child(item.when(n > 0 && !rail, |d| d.child(div().flex_shrink_0().text_size(px(p.fs_xs)).text_color(p.text_tertiary).child(n.to_string()))));
        }
        // the lanes (F1), under the tags as in the mockup; behind the agent layer's switch
        if !demo && !rail {
            col = self.lanes_sidebar(col, header(t!("Lanes")), &cur, p, cx);
        }
        // (LEAF_MOCKUP_DEMO: the mockup's lanes, under the tags)
        if demo {
            col = col.child(header(t!("Lanes")));
            for (i, (name, line, working)) in crate::mockup_demo::LANES.into_iter().enumerate() {
                let item = self.sidebar_item(("sb-demo-lane", i).into(), L::GitBranch, p.text_secondary, name.to_string(), Location::Home, false, Some(second_line(line)), p, cx);
                col = col.child(item.child(state_dot(working, p.accent)));
            }
        }
        // (F4) the Agents page, only while the agent layer is on
        if cx.global::<crate::settings::Settings>().ai_layer {
            let active = cur == Location::Agents;
            col = col.child(self.sidebar_item("sb-agents".into(), L::Brain, p.text_secondary, t!("Agents").into(), Location::Agents, active, None, p, cx));
        }
        let settings = self.sidebar_item(
            "sb-settings".into(),
            L::Settings,
            p.text_secondary,
            t!("Settings").into(),
            Location::Settings,
            cur == Location::Settings,
            None,
            p,
            cx,
        );
        // Files: Settings stays at the bottom of the sidebar, under a divider
        v_flex()
            .w(px(if self.sidebar_rail { RAIL_W } else { self.sidebar_w }))
            .h_full()
            .child(col)
            // (the mockup's sidebar ends with its sections)
            .when(!demo, |d| d.child(div().mx_2().h(px(1.)).bg(p.divider)).child(div().px_2().py_1().child(settings)))
    }

    /// The free bytes on the volume `path` is on, and its root: read in the background, at most every 20 seconds per volume.
    fn free_space(&mut self, path: &Path, cx: &mut Context<Self>) -> Option<(u64, PathBuf)> {
        let root = path.ancestors().last()?.to_path_buf();
        if crate::remote::is_address_path(path) || root.as_os_str().is_empty() {
            return None;
        }
        let now = std::time::Instant::now();
        if let Some((r, at, bytes)) = &self.free_cache
            && *r == root
            && now.duration_since(*at) < std::time::Duration::from_secs(20)
        {
            return bytes.map(|b| (b, root));
        }
        let kept = self.free_cache.as_ref().filter(|(r, _, _)| *r == root).and_then(|c| c.2);
        self.free_cache = Some((root.clone(), now, kept));
        let r2 = root.clone();
        cx.spawn(async move |this, cx| {
            let got = cx.background_spawn(async move { leaf_sys::volume::volume_info(&r2).ok().filter(|v| v.total > 0).map(|v| v.free) }).await;
            let _ = this.update(cx, |w, cx| {
                if let Some((r, _, bytes)) = &mut w.free_cache
                    && *r == root
                    && *bytes != got
                {
                    *bytes = got;
                    cx.notify();
                }
            });
        })
        .detach();
        kept.map(|b| (b, self.free_cache.as_ref().map(|c| c.0.clone()).unwrap_or_default()))
    }

    /// The Recycle Bin's item count for the sidebar, asked of the shell in the background at most every 15 seconds.
    fn recycle_items(&mut self, cx: &mut Context<Self>) -> Option<u64> {
        if crate::mockup_demo::on() {
            return Some(0);
        }
        let now = std::time::Instant::now();
        let epoch = crate::recycle_view::bin_epoch();
        let changed = std::mem::replace(&mut self.recycle_epoch, epoch) != epoch;
        if changed || self.recycle_asked.is_none_or(|at| now.duration_since(at) >= std::time::Duration::from_secs(15)) {
            self.recycle_asked = Some(now);
            cx.spawn(async move |this, cx| {
                let got = cx.background_spawn(async move { leaf_sys::recycle::info().ok().map(|(n, _)| n) }).await;
                let _ = this.update(cx, |w, cx| {
                    if got.is_some() && w.recycle_count != got {
                        w.recycle_count = got;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.recycle_count
    }

    fn render_statusbar(&mut self, p: &Palette, strip_bar_w: Option<f32>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        // the status line's own colours (Index: an inverted readout)
        let p = &p.for_status();
        // Settings > "Selection details": read off the UI thread, asked for only while something is selected
        let details = {
            let on = cx.global::<crate::settings::Settings>().selection_details;
            let tab = self.active_tab().clone();
            (on && tab.read(cx).selection_summary().0 > 0).then(|| tab.update(cx, |t, cx| t.selection_details(cx)))
        };
        let tab = self.active_tab().read(cx);
        let count = tab.item_count();
        let (sel, sel_size, has_dir) = tab.selection_summary();
        // with the details on, "+ folders" stays only until the folder sizes have answered
        let has_dir = details.as_ref().map_or(has_dir, |(_, pending)| has_dir && *pending);
        let loading = tab.loading;
        let ms = tab.load_ms;
        let in_folder = tab.location().path().is_some() || matches!(tab.location(), Location::Tag(_) | Location::Library { .. });
        let git = tab.git.clone();
        // Settings → Developer tools → "Open in editor" button
        let ide_button = {
            let prefs = cx.global::<crate::settings::Settings>();
            let show = match prefs.ide_button {
                crate::settings::IdePref::Always => true,
                crate::settings::IdePref::GitRepos => git.is_some(),
                crate::settings::IdePref::Never => false,
            };
            let dir = tab.location().path().map(Path::to_path_buf);
            let ide = prefs.ide_path.clone();
            (show && dir.is_some()).then(|| {
                let dir = dir.unwrap();
                h_flex()
                    .id("open-ide")
                    .h(px(22.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .hover(|s| s.bg(p.hover).text_color(p.text))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Open folder in editor")).build(w, cx))
                    .child(Icon::new(L::Code).size(px(12.)))
                    .child(t!("Open"))
                    .on_click(move |_, _, _| open_in_ide(dir.clone(), ide.clone()))
            })
        };
        let mut left = h_flex().gap_2().items_center();
        let mut busy = false;
        // the duplicate finder and build junk pages have no folder listing: their own counts instead
        if let Some(text) = tab.page_status(cx) {
            left = left.child(text);
        }
        // the free space of the drive the folder is on ("214 GB free on C:")
        let here = tab.location().path().map(Path::to_path_buf);
        let listed = here.as_ref().and_then(|path| {
            let drives = &self.places.read(cx).drives;
            let d = drives.iter().filter(|d| path.starts_with(&d.root)).max_by_key(|d| d.root.as_os_str().len())?;
            (d.total > 0).then(|| (d.free, d.root.clone()))
        });
        // (a drive the sidebar does not list, or the list not read yet: asked of the volume itself, off the UI thread)
        let free = here.as_ref().and_then(|path| crate::mockup_demo::free_space(path)).or(listed).or_else(|| here.as_ref().and_then(|path| self.free_space(path, cx))).map(|(bytes, root)| {
            format!("{} {} {}", format::size(bytes), t!("free on"), root.to_string_lossy().trim_end_matches('\\'))
        });
        let tab = self.active_tab().read(cx);
        let dot = || div().size(px(3.)).rounded_full().bg(p.text_tertiary).flex_shrink_0();
        // the duplicate finder's page: what it found and what is ticked
        if let Some(text) = tab.dups_view().and_then(|dv| dv.read(cx).status_text()) {
            left = left.child(text);
        }
        if in_folder {
            // "3 of 19 selected · 48.5 MB · 214 GB free on C:", or "19 items · 214 GB free on C:" (the mockups' status line)
            if sel > 0 {
                let picked = h_flex()
                    .id("sel-count")
                    .gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(sel.to_string()))
                    .child(format!("{} {} {}", t!("of"), count, t!("selected")));
                // a click on the count shows the breakdown by type
                left = left.child(match details.as_ref().and_then(|(st, _)| st.clone()) {
                    Some(st) => picked
                        .cursor_pointer()
                        .rounded(theme::RADIUS_SM)
                        .px_1()
                        .hover(|s| s.bg(p.hover).text_color(p.text))
                        .on_click(move |_, window, cx| {
                            let items = crate::selection_stats::breakdown(&st)
                                .into_iter()
                                .map(|(label, value)| crate::fluent_menu::Item::action(label, crate::fluent_menu::MenuIcon::None, |_, _| {}).hint(value))
                                .collect();
                            crate::fluent_menu::show(crate::fluent_menu::Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
                        })
                        .into_any_element(),
                    None => picked.into_any_element(),
                });
                if sel_size > 0 {
                    left = left.child(dot()).child(if has_dir { format!("{} + {}", format::size(sel_size), t!("folders")) } else { format::size(sel_size) });
                    // Settings > "Selection details": lines, play time and pixels, once they are read
                    if let Some((Some(st), _)) = &details {
                        use crate::selection_stats as ss;
                        let more = if st.capped { "+" } else { "" };
                        if st.text_files > 0 {
                            left = left.child(dot()).child(format!("{}{more} {}", st.lines, t!("lines")));
                        }
                        if st.media_files > 0 {
                            left = left.child(dot()).child(format!("{}{more}", ss::play_time(st.play_secs)));
                        }
                        if st.pictures > 0 {
                            left = left.child(dot()).child(format!("{}{more}", ss::megapixels(st.pixels)));
                        }
                    }
                    // Settings > Developer tools > "Show token estimate": what pasting the selected files into a chat would cost
                    if cx.global::<crate::settings::Settings>().show_token_estimate {
                        let phrase = leaf_dev::context::tokens_phrase(leaf_dev::context::estimate_tokens(sel_size));
                        left = left.child(dot()).child(if has_dir { format!("{phrase} + {}", t!("folders")) } else { phrase });
                    }
                }
            } else {
                left = left.child(crate::i18n::items(count));
            }
            // "Hide git-ignored files": how many items it leaves out; a click shows them again
            let ignored = tab.ignored_hidden();
            if ignored > 0 {
                left = left.child(dot()).child(
                    div()
                        .id("ignored-hidden")
                        .cursor_pointer()
                        .rounded(theme::RADIUS_SM)
                        .px_1()
                        .text_color(p.text_secondary)
                        .hover(|s| s.bg(p.hover).text_color(p.text))
                        .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Show the git-ignored items")).build(w, cx))
                        .child(format!("{} {}", ignored, t!("ignored items hidden")))
                        .on_click(|_, _, cx| crate::settings::update(cx, |s| s.hide_ignored = false)),
                );
            }
            if let Some(free) = free {
                left = left.child(dot()).child(div().text_color(p.text_tertiary).child(free));
            }
            // (D11) the agent layer's quiet line; a click opens the Agents page
            if let Some(text) = crate::agents::status_line(cx) {
                left = left.child(dot()).child(
                    div()
                        .id("agents-line")
                        .cursor_pointer()
                        .rounded(theme::RADIUS_SM)
                        .px_1()
                        .text_color(p.text_secondary)
                        .hover(|s| s.bg(p.hover).text_color(p.text))
                        .child(text)
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowAgents), cx)),
                );
            }
            // (D12) the budget ring, only when a daily budget is set in Settings
            if let Some(ring) = crate::agent_usage::ring(cx, p) {
                left = left.child(dot()).child(ring);
            }
            if loading {
                // a search says which step it is on (issue 67)
                let text = tab.search_stage.map_or_else(|| t!("Loading…").to_string(), |s| s.text().to_string());
                left = left.child(text);
                busy = true;
            }
            // values being read for a sort or grouping by a property column
            if let Some(s) = tab.prop_status() {
                left = left.child(s);
                busy = true;
            }
        }
        // a running operation's strip, given what else this bar holds (it takes the room that is left)
        let strip = strip_bar_w.and_then(|bar_w| {
            let held = rf_core::transfer::BarContents {
                items: in_folder,
                selected: in_folder && sel > 0,
                busy,
                git: git.is_some(),
                ide: ide_button.is_some(),
                update: matches!(crate::installer::state(), crate::installer::State::Ready(_)),
            };
            crate::transfer_view::strip(&self.ops, p, rf_core::transfer::strip_room(bar_w, held), self.viewport_w > 1280., cx)
        });
        let perf = cx.global::<crate::settings::Settings>().show_perf_info;
        let demo_lane = crate::mockup_demo::on() && self.viewport_w > 1280.;
        theme::sheet(p, p.status_bg)
            .flex()
            .flex_row()
            .h(px(p.status_h))
            .flex_shrink_0()
            .pl(px(16.))
            .pr(px(12.))
            .items_center()
            .when(p.gap <= 0., |d| d.border_t_1().border_color(p.sheet_border))
            .text_size(px(p.fs_small))
            .text_color(p.text_secondary)
            // Index: the status line is a readout in the monospace face
            .when(p.mono_chrome && !p.font_mono.is_empty(), |d| d.font_family(p.font_mono))
            .child(left)
            // LEAF_MOCKUP_DEMO, as the mockup in a window wider than 1280 px: the working lane in the middle, the copy at the right
            .when(demo_lane, |d| {
                d.child(div().flex_1()).child(
                    h_flex()
                        .ml(px(16.))
                        .gap_2()
                        .flex_shrink_0()
                        .items_center()
                        .text_size(px(p.fs_xs))
                        .text_color(p.text_tertiary)
                        .child(div().size(px(6.)).rounded_full().bg(p.accent))
                        .child(format!("{}: {}", crate::mockup_demo::LANES[0].0, crate::mockup_demo::LANES[0].1)),
                )
            })
            // a running operation's progress (Settings → Aktarım ilerlemesi → Durum çubuğu), right after the selection line
            .children(strip)
            .when(!demo_lane, |d| d.child(div().flex_1()))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .children(git.map(|g| {
                        // Files-style git controls: ↑↓ (sync menu) and the branch (branch menu)
                        let view = self.active_tab().downgrade();
                        let (root, ahead, behind) = (g.root.clone(), g.ahead, g.behind);
                        let pill = |id: &'static str| {
                            h_flex()
                                .id(id)
                                .h(px(22.))
                                .px_2()
                                .gap_1()
                                .items_center()
                                .rounded(theme::RADIUS_SM)
                                .hover(|s| s.bg(p.hover).text_color(p.text))
                                .active(|s| s.bg(p.pressed))
                        };
                        let (v1, r1) = (view.clone(), root.clone());
                        let changes = (!g.is_clean()).then(|| {
                            div().text_color(p.text_tertiary).child(format!("{} {}", g.files.len(), t!("changes")))
                        });
                        h_flex()
                            .gap_1()
                            .items_center()
                            .children(changes)
                            .child(
                                pill("git-sync")
                                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Pull / Push / Sync")).build(w, cx))
                                    .child(Icon::new(L::ArrowDownUp).size(px(12.)))
                                    .child(format!("{behind} / {ahead}"))
                                    .on_click(move |ev: &ClickEvent, window, cx| {
                                        crate::git_ui::sync_menu(v1.clone(), r1.clone(), ahead, behind, ev.position(), window, cx)
                                    }),
                            )
                            .child(
                                pill("git-branch")
                                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Branches")).build(w, cx))
                                    .child(Icon::new(L::GitBranch).size(px(12.)))
                                    .child(g.branch.clone())
                                    .on_click(move |ev: &ClickEvent, window, cx| {
                                        crate::git_ui::branch_menu(view.clone(), root.clone(), ev.position(), window, cx)
                                    }),
                            )
                    }))
                    .children(ide_button)
                    // a downloaded update: one click restarts into it
                    .children(matches!(crate::installer::state(), crate::installer::State::Ready(_)).then(|| {
                        h_flex()
                            .id("update-ready")
                            .h(px(22.))
                            .px_2()
                            .gap_1()
                            .items_center()
                            .rounded(theme::RADIUS_SM)
                            .text_color(p.accent)
                            .hover(|s| s.bg(p.hover))
                            .child(Icon::new(L::RefreshCw).size(px(12.)))
                            .child(t!("Update ready"))
                            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Restart and update")).build(w, cx))
                            .on_click(|_, _, _| {
                                std::thread::spawn(crate::installer::restart_to_update);
                            })
                    }))
                    .children(ms.filter(|_| in_folder && perf).map(|ms| format!("{ms} ms")))
                    .children(perf.then(|| render_backend_badge(p))),
            )
    }
}

/// Title bar: width of the leading corner button, tab width range.
const TITLEBAR_LEAD: f32 = 32.;
/// The brand box left of pill tabs (Canopy): the mockups' 44 px.
const TITLEBAR_LEAD_PILL: f32 = 44.;
/// Below this window width the title bar's right-hand controls step aside for the tabs.
const TITLEBAR_PREFS_MIN_W: f32 = 760.;
/// New tab: the "+" box grows to a tab, then the next "+" slides out from behind it.
const TAB_ANIM_MS: u64 = 200;
const PLUS_ANIM_MS: u64 = 160;
/// Dragged files held over a tab this long open it (Explorer uses about the same).
const DRAG_HOVER_MS: u64 = 500;
const TAB_MIN_W: f32 = 72.;
/// A tab's width besides its name: the padding, the icon and its gap, the close button and its gap.
const TAB_CHROME_W: f32 = 12. + 14. + 8. + 8. + 22. + 4. + 2.;
/// The terminal button and its gap, on a tab that has one.
const TAB_TERMINAL_W: f32 = 22. + 8.;
const SIDEBAR_MIN: f32 = 160.;
/// The sidebar width from which a drive row also says its free and total space in words (narrower: the bar alone).
const DRIVE_SPACE_MIN_W: f32 = 190.;
/// The icons-only sidebar, and the width below which a drag collapses to it.
const RAIL_W: f32 = 56.;
/// The full sidebar's way out of the rail: the spring's time and how far it swings past its place (a little: it is a panel, not a
/// toy); and the time of its way back in, which has no swing.
const RAIL_OUT_MS: u64 = 240;
const RAIL_BOUNCE: f32 = 0.18;
const RAIL_IN_MS: u64 = 130;

/// The width of the sidebar that is coming out of the rail, for how far out it is (0: the rail's own width, 1: the full width;
/// past 1 while the spring swings, by a few px). Never narrower than the rail.
fn rail_peek_width(full: f32, shown: f32) -> f32 {
    (RAIL_W + (full - RAIL_W) * shown).max(RAIL_W)
}
const RAIL_SNAP: f32 = 120.;
const SIDEBAR_MAX: f32 = 420.;
const PREVIEW_MIN: f32 = 220.;
/// The widest a saved preview width is taken. While it is dragged the pane may grow as far as the list keeps its own minimum width
/// (720 px used to be the end: too narrow to read a whole PDF page, a user asked on Threads, 2026-11-18).
const PREVIEW_MAX: f32 = 2400.;

/// Drag payloads for the pane edges.
#[derive(Clone)]
struct ResizeSidebar;
#[derive(Clone)]
struct ResizePreview;

struct ResizeGhost;
impl Render for ResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// A thin invisible strip on a pane edge (`left` = on the pane's left edge) that starts a resize.
fn resize_handle<T: Clone + 'static>(id: &'static str, payload: T, left: bool) -> impl IntoElement {
    div()
        .id(id)
        .absolute()
        .top_0()
        .h_full()
        .w(px(6.))
        .when(left, |d| d.left(px(-3.)))
        // the sidebar's right edge: stay inside it, the content card is painted on top
        .when(!left, |d| d.right_0())
        .cursor_col_resize()
        .on_drag(payload, |_, _, _, cx| cx.new(|_| ResizeGhost))
}

/// Toolbar tooltips (with shortcuts).
fn tooltip_text(id: &str) -> &'static str {
    match id {
        "back" => t!("Back  (Alt+←)"),
        "fwd" => t!("Forward  (Alt+→)"),
        "up" => t!("Up one level  (Alt+↑)"),
        "refresh" => t!("Refresh  (F5)"),
        "sidebar" => t!("Show or hide the sidebar  (Ctrl+B)"),
        "hidden" => t!("Show/hide hidden items  (Ctrl+H)"),
        "terminal" => t!("Open in terminal"),
        "sort" => t!("Sort and group"),
        "v-details" => t!("Details  (Ctrl+Shift+1)"),
        "v-grid" => t!("Grid  (Ctrl+Shift+2)"),
        "v-cols" => t!("Columns  (Ctrl+Shift+3)"),
        "v-dual" => t!("Dual pane  (Ctrl+Shift+D)"),
        "v-preview" => t!("Preview pane  (Alt+P)"),
        "v-shelf" => t!("Shelf"),
        _ => "",
    }
}

/// Drop onto a sidebar entry / breadcrumb segment: Explorer rules (same volume = move).
/// Opens `dir` in the user's editor (`ide` path, else VS Code's `code` launcher).
pub fn open_in_ide(dir: PathBuf, ide: String) {
    std::thread::spawn(move || {
        use leaf_sys::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let r = if ide.is_empty() {
            // "automatic": the first of the editors we know that is installed (`leaf_sys::editors`; looked for here, on this thread)
            match leaf_sys::editors::installed().first() {
                Some(editor) => std::process::Command::new(&editor.exe).arg(&dir).spawn(),
                // none of them: `code` on the PATH, as it always was (a .cmd launcher: through cmd, without a console window)
                None => std::process::Command::new("cmd").args(["/c", "code", "."]).current_dir(&dir).creation_flags(CREATE_NO_WINDOW).spawn(),
            }
        } else {
            std::process::Command::new(&ide).arg(&dir).spawn()
        };
        if let Err(e) = r {
            log::warn!("open in editor: {e}");
        }
    });
}

pub(crate) fn drop_files(paths: &ExternalPaths, dest: PathBuf, cx: &mut App) {
    // a web address dragged out of a browser: downloaded here
    if let Some(url) = crate::downloads::dropped_link(paths.paths()) {
        crate::downloads::start(cx, url, dest, None);
        return;
    }
    let sources: Vec<PathBuf> = paths.paths().to_vec();
    if sources.is_empty() || leaf_sys::dnd::is_invalid_drop(&sources, &dest) {
        return;
    }
    let (ctrl, shift) = (leaf_sys::input::ctrl_down_now(), leaf_sys::input::shift_down_now());
    let sources = hold_archiver_drop(sources, &std::env::temp_dir());
    // NZB files only: asked whether to download what they list (Usenet) or to copy them
    if sources.iter().all(|p| crate::usenet::is_nzb(p))
        && let Some(w) = cx.active_window()
    {
        let (nzbs, to) = (sources.clone(), dest.clone());
        let copy: Box<dyn FnOnce(&mut App)> = Box::new(move |cx| {
            let kind = crate::ops::drop_transfer(&sources, &dest, ctrl, shift);
            crate::status_center::transfer(cx, kind, sources, dest);
        });
        let _ = w.update(cx, move |_, window, cx| crate::usenet::ask_dropped(nzbs, to, copy, window, cx));
        return;
    }
    let kind = crate::ops::drop_transfer(&sources, &dest, ctrl, shift);
    crate::status_center::transfer(cx, kind, sources, dest);
}

/// Archivers hand a drop the files they extracted into a folder of their own under TEMP (7-Zip `7zE…`, WinRAR `Rar$DRa…`) and delete
/// that folder as soon as the drop returns. Our transfer starts after it (and after a "replace?" question) and found nothing: "The
/// system cannot find the file specified" (issue 139, 131). So while the drop is still going on such files are kept in leaf's
/// own folder under TEMP, hard-linked (instant on the same volume, and the file lives on when the archiver deletes its name) or else
/// copied, and the transfer takes them from there. Anything else is returned as it was: a user's own files are never touched.
fn hold_archiver_drop(sources: Vec<PathBuf>, temp: &Path) -> Vec<PathBuf> {
    // (TEMP can be written short, `C:\Users\USER~1\...`, by one side and long by the other: both are compared in their long form)
    let long = |p: &Path| -> PathBuf {
        std::fs::canonicalize(p).map_or_else(|_| p.to_path_buf(), |c| PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\").to_string()))
    };
    let temp_long = long(temp);
    let from_archiver = |p: &Path| {
        let p_long = long(p);
        [(p, temp), (p_long.as_path(), temp_long.as_path())].iter().any(|(p, t)| {
            p.strip_prefix(t).ok().and_then(|rest| rest.components().next()).is_some_and(|c| {
                let n = c.as_os_str().to_string_lossy().to_ascii_lowercase();
                n.starts_with("7z") || n.starts_with("rar$")
            })
        })
    };
    if sources.is_empty() || !sources.iter().all(|p| from_archiver(p)) {
        return sources;
    }
    let base = temp.join("leaf-drops");
    // (the drops of earlier days: whatever a transfer left there, links that cost no room, or copies)
    if let Ok(old) = std::fs::read_dir(&base) {
        for e in old.flatten() {
            if e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|a| a > std::time::Duration::from_secs(24 * 3600))) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let here = base.join(stamp.to_string());
    let mut held = Vec::with_capacity(sources.len());
    for src in &sources {
        let Some(name) = src.file_name() else { return sources };
        let to = here.join(name);
        if let Err(e) = keep_tree(src, &to) {
            log::warn!("drop from an archiver: could not keep {}: {e}; the transfer takes it from where it is", src.display());
            return sources;
        }
        held.push(to);
    }
    log::info!("drop from an archiver: {} item(s) kept in {}", held.len(), here.display());
    held
}

/// `from` (a file or a folder) at `to`: files hard-linked, else copied; folders made and filled.
fn keep_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            keep_tree(&e.path(), &to.join(e.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::hard_link(from, to).or_else(|_| std::fs::copy(from, to).map(|_| ()))
}

#[cfg(test)]
mod archiver_drop_tests {
    use super::hold_archiver_drop;

    #[test]
    fn an_archivers_extracted_files_are_kept_after_it_deletes_them_and_other_files_are_left_alone() {
        let temp = std::env::temp_dir().join(format!("rf-archdrop-{}", std::process::id()));
        let seven = temp.join("7zE5A1B2C3D");
        std::fs::create_dir_all(seven.join("folder")).unwrap();
        std::fs::write(seven.join("doc.txt"), b"inside the archive").unwrap();
        std::fs::write(seven.join("folder").join("deep.txt"), b"deeper").unwrap();
        let held = hold_archiver_drop(vec![seven.join("doc.txt"), seven.join("folder")], &temp);
        assert!(held.iter().all(|p| p.starts_with(temp.join("leaf-drops"))), "{held:?}");
        // the archiver deletes its folder when the drop returns: what the transfer will read is still there
        std::fs::remove_dir_all(&seven).unwrap();
        assert_eq!(std::fs::read(&held[0]).unwrap(), b"inside the archive");
        assert_eq!(std::fs::read(held[1].join("deep.txt")).unwrap(), b"deeper");
        // a user's own files (even under TEMP) go to the transfer as they are
        let own = temp.join("mine").join("notes.txt");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, b"x").unwrap();
        assert_eq!(hold_archiver_drop(vec![own.clone()], &temp), vec![own.clone()]);
        // and so does a drop that mixes the two
        assert_eq!(hold_archiver_drop(vec![own.clone(), temp.join("7zEother").join("a")], &temp).len(), 2);
        let _ = std::fs::remove_dir_all(&temp);
    }
}

/// The header of a pane: its folder's name, how many items it has and what kind of place it is; the focused one of two panes carries
/// the theme's marker (Stem: the stem under it, Index: a band on top; Canopy marks the whole pane instead). None for pages that are
/// no folder (Home, Settings) and where the theme has no header.
fn pane_header(view: &Entity<FolderView>, marked: bool, dual: bool, p: &Palette, cx: &App) -> Option<AnyElement> {
    if p.pane_header_h <= 0. {
        return None;
    }
    let v = view.read(cx);
    let loc = v.location();
    let place = match loc {
        Location::Path(path) if crate::archive_view::is_browsable_path(path) => t!("Archive"),
        Location::Path(path) if leaf_sys::network::server_root(path).is_some() => t!("Network"),
        Location::Path(_) => t!("Local"),
        Location::Search { .. } => t!("Search"),
        Location::Library { .. } => t!("Library"),
        Location::Tag(_) => t!("Tag"),
        Location::Shell { .. } => t!("Device"),
        Location::Remote(_) => t!("Server"),
        // Home and the full pages have no folder to name: the header's space stays, so the list does not move
        _ => return Some(div().h(px(p.pane_header_h)).flex_shrink_0().into_any_element()),
    };
    let name = v.title(cx);
    let count = crate::i18n::items(v.item_count());
    // (Stem and Index dim the name of the pane without the focus; Canopy's rim says it already)
    let dim = dual && !marked && p.pane_marker != "rim";
    Some(
        h_flex()
            .h(px(p.pane_header_h))
            .flex_shrink_0()
            .pl(px(16.))
            .pr(px(14.))
            .gap_2()
            .items_center()
            .relative()
            .text_size(px(p.fs_small))
            .text_color(p.text_secondary)
            .when(p.pane_marker == "band", |d| d.border_b_1().border_color(p.divider))
            .child(Icon::new(L::Folder).size(px(16.)).flex_shrink_0().text_color(p.text_tertiary))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(p.fs + 1.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if dim { p.text_secondary } else { p.text })
                    .child(name),
            )
            .child(div().flex_shrink_0().whitespace_nowrap().when(p.mono_numbers && !p.font_mono.is_empty(), |d| d.font_family(p.font_mono)).child(count))
            .child(div().flex_1())
            .child(div().flex_shrink_0().text_size(px(p.fs_xs)).child(place))
            .when(marked && p.pane_marker == "stem", |d| {
                d.child(div().absolute().left(px(14.)).bottom_0().h(px(p.focus_width)).w(px(if p.focus_width >= 3. { 56. } else { 40. })).rounded(px(1.)).bg(p.accent))
            })
            .when(marked && p.pane_marker == "band", |d| d.child(div().absolute().top_0().left_0().right_0().h(px(p.focus_width)).bg(p.accent)))
            .into_any_element(),
    )
}

/// Shows which device draws the window (hybrid renderer) and cycles Auto → GPU → CPU on click.
fn render_backend_badge(p: &Palette) -> impl IntoElement + use<> {
    use crate::platform::gpui::hybrid::{self, RendererMode};
    let mode = hybrid::mode();
    let backend = hybrid::current_backend_label();
    let mode_label = match mode {
        RendererMode::Auto => t!("Auto"),
        RendererMode::Gpu => t!("Fixed"),
        RendererMode::Cpu => t!("Fixed"),
    };
    let is_cpu = backend == "CPU";
    h_flex()
        .id("renderer-badge")
        .px_2()
        .py_0p5()
        .gap_1()
        .items_center()
        .rounded_full()
        .bg(p.hover)
        .hover(|s| s.bg(p.pressed))
        .cursor_pointer()
        .child(Icon::new(if is_cpu { L::Cpu } else { L::Gauge }).size(px(12.)).text_color(if is_cpu { p.danger } else { p.accent }))
        .child(format!("{backend} · {mode_label}"))
        .on_click(|_, _, cx| {
            use crate::settings::RendererPref;
            let next = match hybrid::mode() {
                RendererMode::Auto => RendererPref::Gpu,
                RendererMode::Gpu => RendererPref::Cpu,
                RendererMode::Cpu => RendererPref::Auto,
            };
            crate::settings::update(cx, |s| s.renderer = next);
        })
}

/// "C:\Users\user" → [("C:", C:\), ("Users", C:\Users), ("x", C:\Users\user)]
fn breadcrumbs(path: &Path) -> Vec<(String, Location)> {
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    for comp in path.components() {
        acc.push(comp.as_os_str());
        if matches!(comp, std::path::Component::RootDir) {
            // merge the root separator into the drive crumb
            if let Some(last) = out.last_mut() {
                let (_, loc): &mut (String, Location) = last;
                *loc = Location::Path(acc.clone());
            }
            continue;
        }
        // (the cache only: this runs while drawing; FolderView::warm_labels fills it from a worker)
        // (the mockup demo's made-up folders keep their English names)
        let label = match leaf_sys::localized::label_if_known(&acc).filter(|_| !crate::mockup_demo::on()) {
            Some(l) => l.to_string(),
            None => rf_core::history::redact_credentials(comp.as_os_str().to_string_lossy().trim_end_matches('\\')),
        };
        out.push((label, Location::Path(acc.clone())));
    }
    out
}

fn parse_location(text: &str) -> Option<Location> {
    let t = text.trim().trim_matches('"');
    if t.is_empty() {
        return None;
    }
    let lower = t.to_lowercase();
    if lower == "home" {
        return Some(Location::Home);
    }
    // `mem://demo/`, `sftp://...`, `ftp://...`: a place behind the virtual filesystem; schemes it has no backend for stay paths
    // as before. An address with a password in it opens nothing (it would land in the history as a path).
    if let Some(remote) = crate::remote::parse_typed(t) {
        // (typed: a server the user named, so its login dialog needs no caution line)
        match &remote {
            Location::Path(p) => leaf_remote::smb::mark_opened_here(p),
            Location::Remote(v) if v.scheme() == "smb" => leaf_remote::smb::mark_opened_here(Path::new(&format!(r"\\{}", v.host()))),
            _ => {}
        }
        return Some(remote);
    }
    if crate::remote::refused_address(t).is_some() {
        return None;
    }
    leaf_remote::smb::mark_opened_here(Path::new(t));
    // Environment variables like %USERPROFILE%
    let expanded = expand_env(t);
    let mut p = PathBuf::from(&expanded);
    if expanded.starts_with(crate::aliases::PREFIX) {
        // "@name\sub": a folder alias; unknown names fall through as a plain path
        if let Some(r) = crate::aliases::resolve(&expanded) {
            p = PathBuf::from(r);
        }
    }
    if expanded.len() == 2 && expanded.ends_with(':') {
        p = PathBuf::from(format!("{expanded}\\"));
    }
    Some(Location::Path(p))
}

fn expand_env(s: &str) -> String {
    if let Some(r) = crate::aliases::resolve(s) {
        return r;
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            match std::env::var(name) {
                Ok(v) => out.push_str(&v),
                Err(_) => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

impl Workspace {
    /// Gives this window the glass (the window is already set transparent). A failure to even start is treated like a failure later:
    /// the window goes to Mica and the settings page says why.
    fn start_glass(&mut self, dark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let hwnd = crate::look::window::hwnd_of(window);
        self.glass_gen = self.glass_gen.wrapping_add(1);
        let attached = hwnd != 0 && leaf_sys::glass::attach(hwnd, dark);
        if !attached {
            let why = match leaf_sys::glass::state(hwnd) {
                leaf_sys::glass::GlassState::Failed(why) => why,
                _ => "the window has no handle".into(),
            };
            self.glass_hwnd = hwnd;
            self.give_up_glass(why, cx);
            return;
        }
        self.glass_hwnd = hwnd;
        self.glass_dark = Some(dark);
        // this is a window the user works in: its glass may freeze while it is not in front, if the setting is on (a dock island or a
        // menu never does)
        leaf_sys::glass::set_freezable(hwnd, true);
        // The engine reports from its own thread, so ask it: every 50 ms until it has drawn (then the window turns see-through), and
        // after that every 500 ms, so a glass that is lost later (a removed graphics device) is noticed within half a second.
        let generation = self.glass_gen;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(shown) = this.update_in(cx, |ws, _, _| ws.glass_shown) else { break };
                let ms = if shown { 500 } else { 50 };
                cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                let Ok(go_on) = this.update_in(cx, |ws, window, cx| ws.check_glass(generation, window, cx)) else { break };
                if !go_on {
                    break;
                }
            }
        })
        .detach();
    }

    /// One look at the engine's report. False when there is nothing more to watch.
    fn check_glass(&mut self, generation: u32, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if generation != self.glass_gen || self.glass_hwnd == 0 {
            return false;
        }
        match leaf_sys::glass::state(self.glass_hwnd) {
            leaf_sys::glass::GlassState::Failed(why) => {
                self.give_up_glass(why, cx);
                false
            }
            // taken off from elsewhere
            leaf_sys::glass::GlassState::Off => false,
            leaf_sys::glass::GlassState::Running => {
                if !self.glass_shown {
                    // the glass is on the window: now the scene may be see-through
                    self.glass_shown = true;
                    crate::glass::forget_window(self.glass_hwnd); // a glass that shows now has no lens yet: send them all again
                    crate::glass::set_shown(self.glass_hwnd, true); // the palette thins its washes over the glass
                    window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                    cx.notify();
                }
                let mode = leaf_sys::glass::capture_mode();
                if mode != self.glass_capture {
                    self.glass_capture = mode;
                    cx.notify(); // the settings page tells whether the windows show in screenshots
                }
                true
            }
            // still starting: the window keeps its Mica
            leaf_sys::glass::GlassState::Starting => true,
        }
    }

    /// The glass is not kept: the engine lets go of the window, the settings page gets the reason and the next render puts Mica on. The
    /// saved setting stays; choosing it again (or another one and back) tries again.
    fn give_up_glass(&mut self, why: String, cx: &mut Context<Self>) {
        log::warn!("glass: giving up on this window, Mica is used: {why}");
        if self.glass_hwnd != 0 {
            leaf_sys::glass::detach(self.glass_hwnd);
            crate::glass::set_shown(self.glass_hwnd, false);
            crate::glass::forget_window(self.glass_hwnd); // the engine drops the lenses with the glass
        }
        self.glass_hwnd = 0;
        self.glass_dark = None;
        self.glass_shown = false;
        self.glass_failed = true;
        crate::glass::set_failure(Some(why));
        cx.notify();
    }

}

/// The leaf mark of the tab strip: the app's icon, drawn at 32 px for a 16 px place (sharp at 200 %).
fn leaf_mark() -> std::sync::Arc<Image> {
    static MARK: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    MARK.get_or_init(|| std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/leaf-32.png").to_vec()))).clone()
}

impl Workspace {
    /// Measures, a moment after a move, resize, activation or material change, how light the screen is in the tab strip's empty part
    /// (backdrop, veil and wash as the user sees them), so that `Palette::for_window` can thicken the sheets where the text would lose
    /// its contrast. One screen read of a short column per event; nothing runs while the window is left alone.
    fn schedule_behind_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p = Palette::for_window(window);
        let hwnd = crate::look::window::hwnd_of(window);
        if !p.backdrop || (self.glass_shown && self.glass_hwnd != 0) {
            if theme::note_behind(hwnd, None) {
                cx.refresh_windows();
            }
            return;
        }
        self.behind_gen += 1;
        let generation = self.behind_gen;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
            let _ = this.update_in(cx, |ws, window, cx| {
                if ws.behind_gen != generation || !window.is_window_active() {
                    return;
                }
                let s = window.scale_factor();
                let vs = window.viewport_size();
                let p = Palette::for_window(window);
                // (right of the tabs, left of the caption buttons: the strip's own see-through part)
                let x = ((f32::from(vs.width) - 170.).max(0.) * s) as i32;
                let (y0, y1) = ((6. * s) as i32, ((p.titlebar_h - 6.) * s) as i32);
                if let Some([r, g, b]) = leaf_sys::window::screen_color(hwnd, x, y0, y1) {
                    let luma = (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.;
                    log::debug!("behind the sheets: {luma:.2}");
                    if theme::note_behind(hwnd, Some(luma)) {
                        cx.refresh_windows();
                    }
                }
            });
        })
        .detach();
    }

    /// Takes the glass off (the backdrop changed). Safe to call without glass.
    fn stop_glass(&mut self, window: &mut Window) {
        self.glass_gen = self.glass_gen.wrapping_add(1);
        if self.glass_hwnd != 0 {
            leaf_sys::glass::detach(self.glass_hwnd);
            crate::glass::set_shown(self.glass_hwnd, false);
            crate::glass::forget_window(self.glass_hwnd); // the engine drops the lenses with the glass
            self.glass_hwnd = 0;
            self.glass_dark = None;
        }
        self.glass_shown = false;
        // "Transparent" turns on a window accent policy that the Mica calls do not turn off: clear it first so the next material is clean
        window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // the window closed
        if self.glass_hwnd != 0 {
            leaf_sys::glass::detach(self.glass_hwnd);
            crate::glass::set_shown(self.glass_hwnd, false);
            crate::glass::forget_window(self.glass_hwnd); // the engine drops the lenses with the glass
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        {
            static FIRST: std::sync::Once = std::sync::Once::new();
            FIRST.call_once(|| crate::start_mark("first render"));
        }
        // the text size (Settings > Appearance, issue 89): the window's base size, which the texts are given in
        let rem = crate::settings::rem_for(cx.global::<crate::settings::Settings>().text_percent);
        if f32::from(window.rem_size()) != rem {
            window.set_rem_size(px(rem));
        }
        // Shortcuts (Ctrl+T, Ctrl+W, …) only reach elements on the focus path: with nothing
        // focused (a window opened from a desktop icon, a closed popup took focus with it) they
        // would do nothing. The current folder takes focus back.
        if window.focused(cx).is_none() && !self.tabs.is_empty() {
            self.focus_active(window, cx);
        }
        let p = Palette::for_window(window);
        // (the speed overlay's figures: only read while it is on)
        let (hud_open, hud_items) = if crate::speed_hud::on(cx) {
            let t = self.active_tab().read(cx);
            (t.load_ms, t.item_count())
        } else {
            (None, 0)
        };
        // Mica / frame follow the app's theme, not only the system one (re-applied when either
        // changes: GPUI resets it on a system theme switch)
        let prefs = cx.global::<crate::settings::Settings>();
        let (backdrop, tint, bg_image, bg_opacity, show_toolbar, show_statusbar) = (
            prefs.backdrop_in_use(),
            prefs.tint,
            prefs.background_image.clone(),
            prefs.background_opacity,
            prefs.show_toolbar,
            prefs.show_statusbar,
        );
        // The status bar stays on every page (empty on Home and Settings): hiding it there made the content jump when a folder opened.
        let surface = crate::transfer_view::surface(cx.global::<crate::settings::Settings>().transfer_progress, show_statusbar, false);
        use crate::settings::BackdropPref;
        // LEAF_GLASS=1 forces Glass for this run (nothing is saved); a window whose glass failed is on Mica until the setting changes
        if self.backdrop_pref_seen != Some(backdrop) {
            if self.backdrop_pref_seen.is_some() {
                // the user chose again: whatever was said about an earlier failure is old
                crate::glass::set_failure(None);
            }
            self.backdrop_pref_seen = Some(backdrop);
            self.glass_failed = false;
        }
        let backdrop = if crate::glass::forced() { BackdropPref::Glass } else { backdrop };
        let backdrop = if backdrop == BackdropPref::Glass && self.glass_failed { BackdropPref::Mica } else { backdrop };
        // (and it asks for no glass, Acrylic or tinted Mica of its own: the palette paints it; so does the pure black theme, whose
        // black must not let anything through)
        let black = crate::settings::theme_override() == crate::settings::ThemePref::Black;
        let backdrop = if black { BackdropPref::Solid } else { backdrop };
        if self.applied_backdrop != Some(backdrop) {
            let was_glass = self.applied_backdrop == Some(BackdropPref::Glass);
            self.applied_backdrop = Some(backdrop);
            if was_glass {
                self.stop_glass(window);
            }
            window.set_background_appearance(theme::main_window_material(match backdrop {
                BackdropPref::Mica => WindowBackgroundAppearance::MicaBackdrop,
                BackdropPref::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
                BackdropPref::Acrylic => WindowBackgroundAppearance::Blurred,
                // opaque surface painted by the palette; keep Mica under it so a switch back is instant
                BackdropPref::Solid => WindowBackgroundAppearance::MicaBackdrop,
                // The glass is a second DirectComposition target behind the scene, so the scene must become see-through, but only once the
                // engine has drawn (`check_glass` does that): until then, and for good if the engine gives up, the window has Mica.
                BackdropPref::Glass => WindowBackgroundAppearance::MicaBackdrop,
                // (resolved by `backdrop_in_use`; never reaches here)
                BackdropPref::Theme => WindowBackgroundAppearance::MicaBackdrop,
            }));
            // Acrylic is the popups' material and asks Windows for their small corners: a main window keeps its own. And a main window
            // takes the plain blur in its place: Acrylic's grey luminosity layer turned the mockups' green and teal backdrop to grey
            // (measured on screen over the mockup's own backdrop); the theme's veil (its `window` colour) lies over the blur instead.
            if backdrop == BackdropPref::Acrylic {
                let hwnd = crate::look::window::hwnd_of(window);
                leaf_sys::window::set_round_corners(hwnd);
                leaf_sys::window::set_clear_blur(hwnd);
            }
            // a new material: what is behind the sheets looks different, measure again
            self.schedule_behind_sample(window, cx);
            if backdrop == BackdropPref::Glass {
                self.start_glass(p.dark, window, cx);
            }
        }
        // the theme changed: the glass changes its tint
        if self.glass_hwnd != 0 && self.glass_dark != Some(p.dark) {
            self.glass_dark = Some(p.dark);
            leaf_sys::glass::set_dark(self.glass_hwnd, p.dark);
        }
        // background color tint and picture (under everything)
        let mut backdrop_layers: Vec<AnyElement> = Vec::new();
        // the theme's colour wash (Canopy's glows), under the tint and the picture; the glass has washes of its own
        if !(self.glass_shown && self.glass_hwnd != 0) {
            let vs = window.viewport_size();
            backdrop_layers.extend(theme::wash_layers(&p, f32::from(vs.width), f32::from(vs.height)));
        }
        if let Some(c) = tint {
            let mut h: Hsla = rgb(c).into();
            h.a = if p.dark { 0.22 } else { 0.16 };
            backdrop_layers.push(div().absolute().inset_0().bg(h).into_any_element());
        }
        if let Some(path) = bg_image {
            backdrop_layers.push(
                img(path)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(bg_opacity.clamp(0.05, 0.6))
                    .into_any_element(),
            );
        }
        let frame_key = (p.dark, window.appearance());
        if self.frame_dark != Some(frame_key) {
            self.frame_dark = Some(frame_key);
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = window.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    leaf_sys::window::set_dark_mode(w.hwnd.get(), p.dark);
                }
            }
        }
        self.viewport_w = f32::from(window.viewport_size().width);
        self.viewport_h = f32::from(window.viewport_size().height);
        // (the tab strip's selected tab and "+" button are lenses over glass; they claim them while the strip is built)
        let glass_on = self.glass_shown && self.glass_hwnd != 0;
        self.tab_lens = glass_on;
        self.tab_lens_claimed = false;
        self.plus_lens_claimed = false;
        let titlebar = self.render_titlebar(&p, window, cx);
        self.tab_lens = false;
        let strip_frame = crate::glass::StripFrame { on_screen: true, tab_claimed: self.tab_lens_claimed, plus_claimed: self.plus_lens_claimed };
        let toolbar = self.render_toolbar(&p, cx);
        let typebar = self.render_typebar(&p, cx);
        let commandbar = self.render_commandbar(&p, cx);
        let pagebar = self.render_page_bar(&p, cx);
        let filter_strip = self.render_filter_bar(&p, window, cx);
        // off-screen check (tools/TESTING.md): `LEAF_CHECK_COMPARE=<folder>` splits the first tab at that folder and compares
        if let Ok(dir) = std::env::var("LEAF_CHECK_COMPARE") {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) && !self.tabs.is_empty() {
                self.open_second_pane(self.active, Location::Path(PathBuf::from(dir)), false, window, cx);
                self.toggle_compare(window, cx);
            }
        }
        // (likewise `LEAF_CHECK_DUPES=<folder>` opens the duplicate finder on that folder)
        if let Ok(dir) = std::env::var("LEAF_CHECK_DUPES") {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) && !self.tabs.is_empty() {
                self.open_tab(Location::Duplicates(vec![PathBuf::from(dir)]), window, cx);
            }
        }
        // LEAF_MOCKUP_DEMO: the mockup's tabs, panes, selection, cut row and rename box
        if crate::mockup_demo::on() && !self.tabs.is_empty() {
            self.demo_setup(window, cx);
        }
        self.compare_sync(cx);
        let compare_bar = match (&self.compare, self.tabs.get(self.active)) {
            (Some(st), Some(t)) if t.left == st.left => Some(crate::compare_mode::bar(st, &p, cx)),
            _ => None,
        };
        // the command bar acts on folder contents: not on Home, Settings, the Recycle Bin…
        let show_commandbar = matches!(self.active_tab().read(cx).location(), Location::Path(_) | Location::Search { .. } | Location::Library { .. } | Location::Tag(_) | Location::Shell { .. });
        // Adaptive layout (as Files does): a narrow window gives the list its room first by
        // hiding the preview, then the sidebar. The settings themselves are not changed.
        const LIST_MIN: f32 = 380.;
        let side_w = if self.sidebar_rail { RAIL_W } else { self.sidebar_w };
        let show_sidebar = self.sidebar_open && self.compact.is_none() && self.viewport_w >= side_w + LIST_MIN;
        // (at the right this is the old rule, `viewport_w >= preview_w + LIST_MIN + sidebar`; at the bottom the height decides)
        let preview_bottom = self.preview_at_bottom(cx);
        let side_used = if show_sidebar { side_w } else { 0. };
        // (the height and the split's shape matter only to the bottom position: the right one reads nothing else)
        let (viewport_h, preview_stacked) = if preview_bottom { (f32::from(window.viewport_size().height), self.preview_stacked(cx)) } else { (0., false) };
        let room_for_preview = self.preview_room(preview_bottom, preview_stacked, side_used, viewport_h);
        // Lenses (the selection pills of the sidebar and of the settings list) exist only while this window shows glass. The sidebar's
        // selected row claims its lens while it is built; a frame without a claim (no selected row, hidden sidebar, the rail) takes it
        // out. The settings lens belongs to the focused pane when that shows Settings: a split tab with Settings in both panes would
        // otherwise have two probes setting one slot to two places on every frame. The rail's peek is opaque, so its rows keep the
        // Fluent selection.
        self.sidebar_lens = glass_on && !self.sidebar_rail;
        self.sidebar_lens_claimed.set(false);
        self.ensure_side_tree(window, cx);
        let sidebar = show_sidebar.then(|| self.render_sidebar(&p, cx));
        self.sidebar_lens = false;
        let plan = crate::glass::lens_plan(
            glass_on,
            show_sidebar && !self.sidebar_rail,
            self.sidebar_lens_claimed.get(),
            matches!(self.active_tab().read(cx).location(), Location::Settings),
            strip_frame,
        );
        if let Some(fade) = plan.clear_sidebar {
            crate::glass::clear_lens(self.glass_hwnd, crate::glass::SLOT_SIDEBAR, fade);
        }
        if let Some(fade) = plan.clear_tab {
            crate::glass::clear_lens(self.glass_hwnd, crate::glass::SLOT_TAB, fade);
        }
        if let Some(fade) = plan.clear_new_tab {
            crate::glass::clear_lens(self.glass_hwnd, crate::glass::SLOT_NEW_TAB, fade);
        }
        if glass_on && !plan.settings_owner {
            crate::glass::clear_lens(self.glass_hwnd, crate::glass::SLOT_SETTINGS, false);
        }
        self.assign_settings_lens(plan.settings_owner, cx);
        // The full sidebar over the content, while the pointer is on the rail or on it. It comes out of the rail on a SPRING and goes
        // back in quickly (the user, of the Open window: "buraya hızlı bir animasyon ekle ... spring animasyonu olması gerektiğini
        // unutma"): until then it was there, or not, from one frame to the next.
        let peek_wanted = show_sidebar && self.sidebar_rail && (self.rail_peek[0] || self.rail_peek[1]);
        if peek_wanted != self.rail_peek_open {
            self.rail_peek_open = peek_wanted;
            self.rail_peek_serial += 1;
            self.rail_peek_closing = !peek_wanted && crate::settings::animations();
            if self.rail_peek_closing {
                let serial = self.rail_peek_serial;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(RAIL_IN_MS)).await;
                    let _ = this.update(cx, |ws, cx| {
                        // (not when it was opened again meanwhile)
                        if ws.rail_peek_serial == serial {
                            ws.rail_peek_closing = false;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        let peek = (peek_wanted || self.rail_peek_closing).then(|| {
            self.sidebar_rail = false;
            self.set_tree_peek(true);
            let full = self.render_sidebar(&p, cx);
            self.set_tree_peek(false);
            self.sidebar_rail = true;
            let (full_w, opening, serial) = (self.sidebar_w, peek_wanted, self.rail_peek_serial);
            let panel = div()
                .id("rail-peek")
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(px(full_w))
                // (the sidebar inside keeps its full width while the panel's own width moves: nothing in it is laid out anew)
                .overflow_hidden()
                .bg(self.rail_peek_bg.unwrap_or(Hsla { a: 1., ..p.content_bg }))
                .border_r_1()
                .border_color(p.border)
                .rounded_r(theme::RADIUS)
                .shadow_lg()
                .on_hover(cx.listener(|this, h: &bool, _, cx| {
                    this.rail_peek[1] = *h;
                    cx.notify();
                }))
                .child(div().h_full().w(px(full_w)).flex_shrink_0().child(full))
                // the rail's own handle lies under this panel while it is out: the panel's edge sizes the sidebar too, so a rail made by
                // dragging can be dragged wide again (a user on Discord, 2026-11-20: "now seems impossible")
                .child(resize_handle("rail-peek-resize", ResizeSidebar, false));
            if !crate::settings::animations() {
                return panel.into_any_element();
            }
            // out: a spring with a little swing past its place; in: the same curve with none, in less time
            let (ms, bounce) = if opening { (RAIL_OUT_MS, RAIL_BOUNCE) } else { (RAIL_IN_MS, 0.0) };
            panel
                .with_animation(
                    ("rail-peek-move", serial as usize),
                    Animation::new(std::time::Duration::from_millis(ms)).with_easing(move |t| leaf_sys::kit::spring::curve_with(t, bounce)),
                    move |el, t| {
                        let shown = if opening { t } else { 1.0 - t };
                        // (the width alone moves: the panel is matte from its first frame, nothing shows through it on the way)
                        el.w(px(rail_peek_width(full_w, shown)))
                    },
                )
                .into_any_element()
        });
        // (the status bar is the content panel's: the window less the sidebar when it is shown, and the panel's margin)
        let strip_bar_w = (surface == crate::transfer_view::Surface::Strip).then(|| (f32::from(window.viewport_size().width) - 2.0 * p.gap - 28.0).max(0.0));
        let cards = (surface == crate::transfer_view::Surface::Card)
            .then(|| crate::transfer_view::cards(&self.ops, &p, if show_statusbar { 44.0 } else { 12.0 }, cx))
            .flatten();
        let status = self.render_statusbar(&p, strip_bar_w, cx);
        // Taskbar title follows the active folder.
        let title = {
            let v = self.active_tab().read(cx);
            let full = cx.global::<crate::settings::Settings>().title_full_path;
            format!("{} - leaf", crate::full_path::window_text(v.location(), &v.title(cx), full))
        };
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
        // Keep the saved session current (so it survives a crash or forced shutdown).
        let (session, panes) = self.session_now(cx);
        let key = session_key(&session, &panes);
        if key != self.last_session {
            self.last_session = key;
            let active = self.active;
            cx.defer(move |cx| {
                cx.update_global::<crate::settings::Settings, _>(|s, _| {
                    s.session_tabs = session;
                    s.session_panes = panes;
                    s.session_active = active;
                });
                let snapshot = cx.global::<crate::settings::Settings>().clone();
                std::thread::spawn(move || snapshot.save());
            });
        }
        let tab_state = self.tabs[self.active].clone();
        // Each pane is a sheet of its own (Canopy: with a gap between them; Stem and Index: touching, split by a line), with a header
        // that names its folder; the focused one of two is marked the theme's way (`pane_marker`).
        let dual = tab_state.right.is_some();
        let side_by_side = cx.global::<crate::settings::Settings>().dual_pane_vertical;
        // (the pane without the focus shows its selection in the neutral colour)
        for (view, inactive) in [(Some(&tab_state.left), dual && tab_state.right_active), (tab_state.right.as_ref(), dual && !tab_state.right_active)] {
            if let Some(view) = view
                && view.read(cx).pane_inactive != inactive
            {
                view.update(cx, |v, cx| {
                    v.pane_inactive = inactive;
                    cx.notify();
                });
            }
        }
        let pane = |view: Entity<FolderView>, is_right: bool, active: bool, cx: &mut Context<Self>| {
            let marked = dual && active;
            let header = pane_header(&view, marked, dual, &p, cx);
            theme::sheet(&p, p.content_bg)
                .flex_1()
                .min_w_0()
                .min_h_0()
                .when(side_by_side || !dual, |d| d.h_full())
                .when(!side_by_side && dual, |d| d.w_full())
                .flex()
                .flex_col()
                .overflow_hidden()
                .relative()
                .when(marked && p.pane_marker == "rim", |d| d.shadow(p.focus_rim()))
                .capture_any_mouse_down(cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    if dual {
                        this.set_active_pane(is_right, cx)
                    }
                }))
                .children(header)
                .child(div().flex_1().min_h_0().child(view))
        };
        let content: AnyElement = match &tab_state.right {
            None => pane(tab_state.left.clone(), false, true, cx).into_any_element(),
            Some(right) => {
                let left = pane(tab_state.left.clone(), false, !tab_state.right_active, cx);
                let right = pane(right.clone(), true, tab_state.right_active, cx);
                // (touching panes: a line between them)
                let line = (p.gap <= 0.).then(|| div().flex_shrink_0().bg(p.sheet_border).when(side_by_side, |d| d.w(px(1.)).h_full()).when(!side_by_side, |d| d.h(px(1.)).w_full()));
                if side_by_side {
                    h_flex().size_full().gap(px(p.gap)).child(left).children(line).child(right).into_any_element()
                } else {
                    v_flex().size_full().gap(px(p.gap)).child(left).children(line).child(right).into_any_element()
                }
            }
        };
        let shelf = self.shelf_open.then(|| {
            theme::sheet(&p, p.content_bg)
                .w(px(260.))
                .flex_shrink_0()
                .h_full()
                .overflow_hidden()
                .when(p.gap <= 0., |d| d.border_l_1().border_color(p.sheet_border))
                .child(self.shelf.clone())
        });
        // (the same on every page: an open pane stays open on Home and Settings too, empty there, so the list's right edge does not move)
        let pane_shown = self.preview_open && self.compact.is_none() && room_for_preview;
        // the hosted preview handler is a separate window: hide it whenever something of ours
        // could be under it (pane closed, a dialog or the Status Center open, palette, bulk rename)
        let overlay_open = crate::prompt::current(window, cx).is_some() || crate::conflict_dialog::current(window, cx).is_some() || self.ops.read(cx).open || self.palette.is_some() || self.bulk.is_some() || crate::organizer::is_open(window, cx) || crate::prop_chooser::is_open(window, cx);
        let covered = !pane_shown || overlay_open || self.quick_look.is_some();
        self.preview.update(cx, |pv, _| pv.set_occluded(covered));
        // an open QuickLook preview follows the focused item too (`quicklook::selection_is` sends only what changed)
        if cx.global::<crate::settings::Settings>().space_quicklook
            && window.is_window_active()
            && let Some((path, _)) = self.active_tab().read(cx).preview_target()
        {
            crate::quicklook::selection_is(&path);
        }
        // Hızlı Bakış penceresi uygulamadan açıldıysa odaktaki öğeyi izler (oklar burada çalışır)
        let look_open = crate::folder_view::QUICK_LOOK_OPEN.load(std::sync::atomic::Ordering::Relaxed);
        if look_open && window.is_window_active() {
            if let Some((path, e)) = self.active_tab().read(cx).preview_target() {
                let mut last = LOOK_FOLLOWED.lock().unwrap();
                if last.as_ref() != Some(&path) {
                    *last = Some(path.clone());
                    crate::archive_view::look_send(path, e.size, true);
                }
            }
        }
        // Quick Look follows the focused item (arrow keys keep working underneath)
        let ql_target = self.quick_look.as_ref().and_then(|_| self.active_tab().read(cx).preview_target());
        if self.quick_look.is_some() && ql_target.is_none() {
            self.close_quick_look(cx);
        }
        let quick_look_layer = self.quick_look.clone().map(|ql| {
            ql.update(cx, |pv, cx| {
                pv.set_occluded(overlay_open);
                pv.set_target(ql_target, cx);
            });
            div()
                .id("quick-look")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.45))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_quick_look(cx)))
                .child(
                    div()
                        .id("quick-look-card")
                        .w(relative(0.8))
                        .h(relative(0.82))
                        .p_4()
                        .rounded(theme::RADIUS)
                        .bg(p.card_bg)
                        .border_1()
                        .border_color(p.border)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(ql)
                        .with_animation("ql-in", Animation::new(std::time::Duration::from_millis(140)).with_easing(ease_out_quint()), |el, t| {
                            el.opacity(t)
                        }),
                )
        });
        let preview = if pane_shown && !preview_bottom {
            let target = self.active_tab().read(cx).preview_target();
            // (on the change feed's page the pane opens on the Changes tab)
            let feed = matches!(self.active_tab().read(cx).location(), Location::Changes(_));
            self.preview.update(cx, |pv, cx| {
                pv.set_feed(feed, cx);
                pv.set_target(target, cx)
            });
            Some(
                theme::sheet(&p, p.content_bg)
                    .w(px(self.preview_w))
                    .flex_shrink_0()
                    .h_full()
                    .relative()
                    .when(p.gap <= 0., |d| d.border_l_1().border_color(p.sheet_border))
                    .child(self.preview.clone())
                    .child(resize_handle("preview-resize", ResizePreview, true)),
            )
        } else {
            None
        };
        let bottom_pane = self.preview_bottom_pane(preview_dock::BottomFrame { bottom: preview_bottom, shown: pane_shown, stacked: preview_stacked, side_w: side_used, viewport_h }, &p, cx);
        // the file list's slot, and the row that holds it with the shelf and the preview pane (at the right, or under them)
        let list_slot = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            // Drop a tab here: it moves into a new window.
            .drag_over::<TabDrag>(move |s, _, _, _| s.bg(p.selection))
            .on_drop(cx.listener(|this, d: &TabDrag, window, cx| this.detach_tab(d.ix, window, cx)))
            .child(content);
        let main_row: AnyElement = if preview_bottom {
            self.preview_bottom_row(list_slot, shelf, bottom_pane, cx)
        } else {
            h_flex().flex_1().min_h_0().gap(px(p.gap)).child(list_slot).children(shelf).children(preview).into_any_element()
        };

        crate::platform::watch_input(v_flex().id("workspace"))
            .size_full()
            .bg(p.window_bg)
            .text_color(p.text)
            // the theme's typeface (Index: Bahnschrift) unless the user chose one; a missing font falls back to the system's
            .when(!p.font_ui.is_empty() && cx.global::<crate::settings::Settings>().font.is_none(), |d| d.font_family(p.font_ui))
            // the pointer coming over the window gives its glass back the full rate (and thaws it): one event on entering, none while
            // it moves
            .on_hover(cx.listener(|this, over: &bool, _, _| {
                if *over && this.glass_hwnd != 0 {
                    leaf_sys::glass::pointer_over(this.glass_hwnd);
                }
            }))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| this.with_tab(cx, |v, cx| v.back(cx))))
            // Windows' own dialogs (credentials, reconnect at sign-in); the drive list notices the new / removed drive by itself.
            // On a thread of their own, as every other Windows dialog here (`settings_view::pick_into`): run here, inside the
            // action, the dialog's modal loop handed our windows their messages while the app was borrowed, and the app closed
            // (issue 46: "Clicking on map network drive crashes and closes the explorer window").
            .on_action(cx.listener(|_, _: &RestartApp, _, cx| crate::restart(cx)))
            .on_action(cx.listener(|this, _: &SaveSession, window, cx| this.save_named_session(window, cx)))
            .on_action(cx.listener(|_, _: &MapNetworkDrive, window, _| {
                let owner = Self::hwnd(window).unwrap_or(0);
                std::thread::spawn(move || leaf_sys::network::map_drive_dialog(owner));
            }))
            .on_action(cx.listener(|_, _: &DisconnectNetworkDrive, window, _| {
                let owner = Self::hwnd(window).unwrap_or(0);
                std::thread::spawn(move || leaf_sys::network::disconnect_drive_dialog(owner));
            }))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| this.with_tab(cx, |v, cx| v.forward(cx))))
            .on_action(cx.listener(|this, _: &GoUp, _, cx| this.with_tab(cx, |v, cx| v.up(cx))))
            .on_action(cx.listener(|this, _: &GoHome, window, cx| this.navigate(Location::Home, window, cx)))
            .on_action(cx.listener(|this, _: &CustomizeHome, window, cx| {
                this.navigate(Location::Home, window, cx);
                this.with_tab(cx, |v, cx| {
                    v.home_edit = true;
                    cx.notify();
                });
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.with_tab(cx, |v, cx| v.refresh(cx))))
            .on_action(cx.listener(|this, _: &ToggleHidden, _, cx| this.with_tab(cx, |v, cx| v.toggle_hidden(cx))))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.open_new_tab(window, cx)))
            .on_action(cx.listener(|this, _: &NewWindow, window, cx| {
                // New window at the current folder, offset from this one.
                let loc = this.active_tab().read(cx).location().clone();
                let mut b = window.bounds();
                b.origin.x += px(36.);
                b.origin.y += px(36.);
                cx.defer(move |cx| crate::open_main_window(Some(loc), None, Some(WindowBounds::Windowed(b)), cx));
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let i = this.active;
                this.close_tab(i, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                let n = this.tabs.len();
                this.activate((this.active + 1) % n, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                let n = this.tabs.len();
                this.activate((this.active + n - 1) % n, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                if let Some(loc) = this.closed.pop() {
                    this.open_tab(loc, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| this.begin_address_edit(window, cx)))
            .on_action(cx.listener(|this, _: &FocusNextPane, window, cx| this.cycle_focus(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPrevPane, window, cx| this.cycle_focus(false, window, cx)))
            .on_action(cx.listener(|this, _: &TogglePreview, _, cx| {
                this.toggle_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &DuplicateTab, window, cx| {
                let loc = this.active_tab().read(cx).location().clone();
                this.open_tab(loc, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseOtherTabs, window, cx| this.close_other_tabs(window, cx)))
            .on_action(cx.listener(|this, _: &CloseAllTabs, window, cx| this.close_all_tabs(window, cx)))
            .on_action(cx.listener(|this, _: &SplitPaneSideBySide, window, cx| this.split_pane(true, window, cx)))
            .on_action(cx.listener(|this, _: &SplitPaneStacked, window, cx| this.split_pane(false, window, cx)))
            .on_action(cx.listener(|this, _: &CloseActivePane, window, cx| this.close_active_pane(window, cx)))
            .on_action(cx.listener(|this, _: &OpenTerminalAdmin, _, cx| {
                if let Some(dir) = crate::terminal::location_dir(this.active_tab().read(cx).location()) {
                    crate::terminal::open_admin(dir);
                }
            }))
            .on_action(cx.listener(|_, _: &ToggleTheme, window, cx| {
                use crate::settings::ThemePref;
                let dark = crate::theme::Palette::for_window(window).dark;
                crate::settings::update(cx, |s| s.theme = if dark { ThemePref::Light } else { ThemePref::Dark });
            }))
            .on_action(cx.listener(|this, _: &CompareFolders, window, cx| this.toggle_compare(window, cx)))
            .on_action(cx.listener(|this, _: &FindDuplicates, _, cx| {
                // the selected folders, else this folder
                let v = this.active_tab().clone();
                let roots: Vec<PathBuf> = {
                    let v = v.read(cx);
                    let sel: Vec<PathBuf> = v.selection_paths().into_iter().filter(|p| p.is_dir()).collect();
                    if sel.is_empty() { v.current_dir().into_iter().collect() } else { sel }
                };
                if !roots.is_empty() {
                    v.update(cx, |_, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::Duplicates(roots))));
                }
            }))
            .on_action(cx.listener(|this, _: &FindBuildJunk, _, cx| {
                // the selected folder, else this folder
                let v = this.active_tab().clone();
                let root = {
                    let v = v.read(cx);
                    let sel: Vec<PathBuf> = v.selection_paths().into_iter().filter(|p| p.is_dir()).collect();
                    if sel.len() == 1 { sel.into_iter().next() } else { v.current_dir() }
                };
                if let Some(root) = root {
                    v.update(cx, |_, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::BuildJunk(root))));
                }
            }))
            .on_action(cx.listener(|this, _: &OpenDocker, window, cx| {
                // this PC's Docker; in a folder on an sftp:// host, that host's
                let authority = match this.active_tab().read(cx).location() {
                    Location::Remote(v) if v.scheme() == "sftp" => v.authority().to_string(),
                    _ => "local".to_string(),
                };
                if let Ok(v) = rf_core::vpath::VPath::new("docker", &authority, "/") {
                    this.navigate(Location::Remote(v), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DownloadHere, window, cx| {
                // the folder shown, when it is a folder on this PC
                let v = this.active_tab().clone();
                let dir = v.read(cx).current_dir().filter(|d| crate::downloads::can_save_in(d));
                match dir {
                    Some(dir) => crate::downloads::ask(v.downgrade(), dir, window, cx),
                    None => crate::prompt::notice(t!("Download here"), t!("This works in a folder on this PC. Open the folder the file should go to first.").to_string(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &DownloadNzb, window, cx| {
                let paths = this.active_tab().read(cx).selection_paths();
                crate::usenet::download_selected(paths, window, cx);
            }))
            .on_action(cx.listener(|_, _: &UsenetPauseAll, _, cx| {
                crate::usenet::pause_all(!leaf_nzb::queue::all_paused());
                cx.refresh_windows();
            }))
            .on_action(cx.listener(|this, _: &DownloadMany, window, cx| {
                // the folder shown, when it is a folder on this PC
                let v = this.active_tab().clone();
                let dir = v.read(cx).current_dir().filter(|d| crate::downloads::can_save_in(d));
                match dir {
                    Some(dir) => crate::downloads::ask_many(v.downgrade(), dir, window, cx),
                    None => crate::prompt::notice(t!("Download several"), t!("This works in a folder on this PC. Open the folder the file should go to first.").to_string(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &BookmarkRemoteFolder, window, cx| {
                // the folder shown, when it is on a saved host
                let loc = this.active_tab().read(cx).location().clone();
                let done = match &loc {
                    Location::Remote(v) => crate::hosts_ui::toggle_bookmark(v, cx),
                    // (Windows: a folder of a saved `smb://` host is a UNC path)
                    Location::Path(p) if cfg!(windows) && leaf_remote::smb::unc_share(p).is_some() => crate::hosts_ui::toggle_unc_bookmark(p, cx),
                    _ => return crate::prompt::notice(t!("Bookmark this folder"), t!("This works in a folder on a saved host (Settings > Hosts).").to_string(), window, cx),
                };
                if done.is_none() {
                    crate::prompt::notice(t!("Bookmark this folder"), t!("This server is not in the saved hosts. Add it under Settings > Hosts first.").to_string(), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowProjectBrain, _, cx| {
                // the folder shown (the selected folder when one is)
                let v = this.active_tab().clone();
                let dir = {
                    let v = v.read(cx);
                    let sel: Vec<PathBuf> = v.selection_paths().into_iter().filter(|p| p.is_dir()).collect();
                    if sel.len() == 1 { sel.into_iter().next() } else { v.current_dir() }
                };
                if let Some(d) = dir {
                    v.update(cx, |_, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::Brain(d))));
                }
            }))
            .on_action(cx.listener(|this, _: &ShowChanges, window, cx| {
                // the folder shown, else the folder of the selected item; it must be inside a git repository
                let v = this.active_tab().clone();
                let dir = v.read(cx).current_dir();
                match dir.filter(|d| leaf_dev::gitignore::repo_root(d).is_some()) {
                    Some(d) => v.update(cx, |_, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::Changes(d)))),
                    None => crate::prompt::notice(
                        t!("Not in a git repository"),
                        t!("Show changes lists the changed files of a git repository. This folder is not in one.").to_string(),
                        window,
                        cx,
                    ),
                }
            }))
            .on_action(cx.listener(|this, _: &FollowRemoteFile, _, cx| {
                this.active_tab().clone().update(cx, |v, _| {
                    v.toggle_remote_follow();
                });
            }))
            .on_action(cx.listener(|this, _: &Checkpoint, window, cx| {
                let v = this.active_tab().clone();
                match v.read(cx).checkpoint_dir() {
                    Some(d) => crate::checkpoint_ui::new_checkpoint(d, v.downgrade(), window, cx),
                    None => crate::prompt::notice(t!("Checkpoint"), t!("A checkpoint is taken of a folder on this computer. Open one first.").to_string(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &ShowCheckpoints, window, cx| {
                let v = this.active_tab().clone();
                match v.read(cx).checkpoint_dir() {
                    Some(d) => crate::checkpoint_ui::show_list(d, v.downgrade(), cx),
                    None => crate::prompt::notice(t!("Checkpoints"), t!("Checkpoints are kept for folders on this computer. Open one first.").to_string(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &ReviewChanges, window, cx| {
                if !crate::review_view::enabled(cx) {
                    crate::prompt::notice(t!("Review changes"), t!("Review changes is part of the agent layer, which is off. Turn it on in Settings, Developer.").to_string(), window, cx);
                    return;
                }
                let v = this.active_tab().clone();
                // a folder of a repository, or any folder on this computer (compared with its checkpoints)
                let dir = { let r = v.read(cx); r.current_dir().filter(|d| leaf_dev::gitignore::repo_root(d).is_some()).or_else(|| r.checkpoint_dir()) };
                match dir {
                    Some(d) => crate::review_view::show(d, v.downgrade(), cx),
                    None => crate::prompt::notice(t!("Review changes"), t!("Review changes looks at a folder on this computer. Open one first.").to_string(), window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &ShowAgents, _, cx| {
                this.active_view().update(cx, |_, cx| cx.emit(FolderEvent::OpenInNewTab(Location::Agents)));
            }))
            .on_action(cx.listener(|_, _: &VpnConnect, _, cx| crate::vpn::connect(cx)))
            .on_action(cx.listener(|_, _: &VpnDisconnect, _, cx| crate::vpn::disconnect(cx)))
            .on_action(cx.listener(|this, _: &SyncJobs, _, cx| {
                this.active_view().update(cx, |_, cx| cx.emit(FolderEvent::OpenInNewTab(Location::SyncJobs)));
            }))
            .on_action(cx.listener(|this, _: &SyncWith, window, cx| {
                // the folder shown (the selected folder when one is) on the left, the other pane's folder on the right
                let v = this.active_view();
                let left = {
                    let v = v.read(cx);
                    let sel: Vec<PathBuf> = v.selection_paths().into_iter().filter(|p| p.is_dir()).collect();
                    if sel.len() == 1 {
                        crate::sync_ui::address_of_location(&Location::Path(sel[0].clone()))
                    } else {
                        crate::sync_ui::address_of_location(v.location())
                    }
                };
                let right = this.other_pane(&v).and_then(|o| crate::sync_ui::address_of_location(o.read(cx).location()));
                let tab = v.downgrade();
                let open: std::rc::Rc<dyn Fn(&mut Window, &mut App)> = std::rc::Rc::new(move |_, cx| {
                    let _ = tab.update(cx, |_, cx| cx.emit(FolderEvent::OpenInNewTab(Location::SyncJobs)));
                });
                crate::sync_ui::sync_with(left, right, open, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShareFolder, window, cx| {
                // the selected folder when one is, else the folder shown (a folder on this computer only)
                let folder = {
                    let v = this.active_view().read(cx);
                    let sel: Vec<PathBuf> = v.selection_paths().into_iter().filter(|p| p.is_dir()).collect();
                    if sel.len() == 1 { Some(sel[0].clone()) } else { v.location().path().map(Path::to_path_buf) }
                };
                crate::share_ui::share_folder(folder, window, cx);
            }))
            .on_action(cx.listener(|_, _: &JoinSharedFolder, window, cx| crate::share_ui::join_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &SharedFolders, _, cx| {
                this.active_view().update(cx, |_, cx| cx.emit(FolderEvent::OpenInNewTab(Location::SyncJobs)));
            }))
            .on_action(cx.listener(|this, _: &ShowWorktrees, window, cx| this.show_worktrees(window, cx)))
            .on_action(cx.listener(|this, _: &NewWorktree, window, cx| this.new_worktree(window, cx)))
            .on_action(cx.listener(|this, _: &NewLane, window, cx| this.new_lane(window, cx)))
            .on_action(cx.listener(|this, _: &EndLane, window, cx| this.end_lane(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleHideIgnored, window, cx| {
                let on = cx.global::<crate::settings::Settings>().hide_ignored;
                let dir = this.active_tab().read(cx).location().path().map(Path::to_path_buf);
                // outside a git repository the switch has nothing to hide: it stays off and says so
                if !on && !dir.as_deref().is_some_and(|d| leaf_dev::gitignore::repo_root(d).is_some()) {
                    crate::prompt::notice(
                        t!("Not in a git repository"),
                        t!("Hide git-ignored files works inside a git repository. This folder is not in one, so nothing changes.").to_string(),
                        window,
                        cx,
                    );
                    return;
                }
                crate::settings::update(cx, |s| s.hide_ignored = !on);
            }))
            .on_action(cx.listener(|this, _: &OrganizeFolder, window, cx| {
                match this.active_tab().read(cx).location().path().map(Path::to_path_buf) {
                    Some(dir) => crate::organizer::organize_folder(dir, window, cx),
                    None => crate::organizer::not_a_folder(window, cx),
                }
            }))
            .on_action(cx.listener(|this, _: &OpenInIde, _, cx| {
                if let Some(dir) = this.active_tab().read(cx).location().path().map(Path::to_path_buf) {
                    let ide = cx.global::<crate::settings::Settings>().ide_path.clone();
                    open_in_ide(dir, ide);
                }
            }))
            .on_action(cx.listener(|_, _: &ToggleToolbar, _, cx| {
                let v = !cx.global::<crate::settings::Settings>().show_toolbar;
                crate::settings::update(cx, |s| s.show_toolbar = v);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                let v = !this.sidebar_open;
                crate::settings::update(cx, |s| s.sidebar_open = v);
            }))
            .on_action(cx.listener(|this, _: &ToggleDualPane, window, cx| this.toggle_dual_pane(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleShelf, _, cx| this.toggle_shelf(cx)))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|this, _: &ToggleAlwaysOnTop, window, _| this.toggle_always_on_top(window)))
            .on_action(cx.listener(|this, _: &ToggleCompactOverlay, window, cx| this.toggle_compact(window, cx)))
            .on_action(cx.listener(|_, _: &ToggleAddressFullPath, _, cx| crate::settings::update(cx, |s| s.address_full_path = !s.address_full_path)))
            .on_action(cx.listener(|this, _: &GoToFolderAlias, window, cx| {
                // Files: the address box opens with "@" and lists the aliases
                this.begin_address_edit(window, cx);
                this.address.update(cx, |s, cx| s.set_value("@", window, cx));
                this.update_suggestions("@".into(), cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| this.toggle_command_palette(window, cx)))
            .on_action(cx.listener(|this, _: &SearchFolderHistory, window, cx| {
                this.toggle_palette(crate::command_palette::PaletteMode::Folders, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SearchTabs, window, cx| this.toggle_palette(crate::command_palette::PaletteMode::Tabs, window, cx)))
            .on_action(cx.listener(|this, _: &GoSettings, window, cx| this.navigate(Location::Settings, window, cx)))
            .on_action(cx.listener(|this, _: &GoRecycleBin, window, cx| this.navigate(Location::RecycleBin, window, cx)))
            .on_action(cx.listener(|this, _: &SwitchPane, window, cx| {
                let r = !this.tabs[this.active].right_active;
                this.set_active_pane(r, cx);
                this.focus_active(window, cx);
            }))
            .on_action(cx.listener(|this, _: &ViewDetails, _, cx| this.with_tab(cx, |v, cx| v.set_view_mode(ViewMode::Details, cx))))
            .on_action(cx.listener(|this, _: &ViewGrid, _, cx| this.with_tab(cx, |v, cx| v.set_view_mode(ViewMode::Grid, cx))))
            .on_action(cx.listener(|this, _: &ViewColumns, _, cx| this.with_tab(cx, |v, cx| v.set_view_mode(ViewMode::Columns, cx))))
            .on_action(cx.listener(|this, _: &ViewCards, _, cx| {
                this.with_tab(cx, |v, cx| v.set_layout(ViewMode::Grid, crate::folder_view::GridStyle::Cards, cx))
            }))
            .on_action(cx.listener(|this, _: &ViewList, _, cx| {
                this.with_tab(cx, |v, cx| v.set_layout(ViewMode::Grid, crate::folder_view::GridStyle::List, cx))
            }))
            .on_action(cx.listener(|this, _: &SearchInContents, window, cx| {
                this.enter_content_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.enter_search(window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenTerminal, _, cx| {
                let loc = this.active_tab().read(cx).location().clone();
                crate::terminal::open_location(&loc, cx);
            }))
            .relative()
            .children(backdrop_layers)
            .child(titlebar)
            // Under the tab strip the window is a column of sheets: the address row, then the sidebar beside the tool bar and the panes,
            // then the status line across the whole width (the mockups' layout). The theme's gap lies between them and round them.
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .px(px(p.gap))
                    // (the mockups: a gap under the tab strip too, the address sheet floats like the others)
                    .pt(px(p.gap))
                    .pb(px(p.gap))
                    .gap(px(p.gap))
                    .child(
                        theme::sheet(&p, p.toolbar_bg)
                            .w_full()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .when(p.gap <= 0., |d| d.border_b_1().border_color(p.sheet_border))
                            .child(toolbar)
                            .children(typebar.or_else(|| (p.typebar_h > 0.).then(|| div().h(px(p.typebar_h)).flex_shrink_0().into_any_element()))),
                    )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(p.gap))
                    .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizePreview>, _, cx| {
                        // (the preview sheet ends at the row's right edge)
                        let w = f32::from(e.bounds.right() - e.event.position.x);
                        this.set_pane_width(false, w, cx);
                    }))
                    .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizeSidebar>, _, cx| {
                        let w = f32::from(e.event.position.x - e.bounds.left());
                        this.set_pane_width(true, w, cx);
                    }))
                    .children(sidebar.map(|s| {
                        theme::sheet(&p, p.sidebar_bg)
                            .id("sidebar-slot")
                            .h_full()
                            .relative()
                            .flex_shrink_0()
                            .when(p.gap <= 0., |d| d.border_r_1().border_color(p.sheet_border))
                            .child(s)
                            .child(resize_handle("sidebar-resize", ResizeSidebar, false))
                            // on the rail, hovering opens the full sidebar over the content
                            .on_hover(cx.listener(|this, h: &bool, window, cx| {
                                if this.sidebar_rail && this.rail_peek[0] != *h {
                                    if *h && !this.rail_peek[1] {
                                        // before the sidebar covers it: a column near the rail's
                                        // left edge, clear of the icons
                                        let s = window.scale_factor();
                                        let height = f32::from(window.viewport_size().height);
                                        let at = |v: f32| (v * s) as i32;
                                        // (over glass the screen shows the desktop behind the window, not the window's tint)
                                        this.rail_peek_bg = leaf_sys::window::screen_color(crate::look::window::hwnd_of(window), at(3.), at(110.), at(height - 60.))
                                            .filter(|_| this.glass_hwnd == 0)
                                            .map(|[r, g, b]| Hsla::from(gpui_kit::rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))));
                                    }
                                    this.rail_peek[0] = *h;
                                    cx.notify();
                                }
                            }))
                            // drawn after the list and the preview, so it lies over them
                            .children(peek.map(|d| deferred(d).with_priority(1)))
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .gap(px(p.gap))
                            .when(show_commandbar && show_toolbar, |d| {
                                d.child(
                                    theme::sheet(&p, p.toolbar_bg)
                                        .flex_shrink_0()
                                        .when(p.gap <= 0., |d| d.border_b_1().border_color(p.sheet_border))
                                        .child(commandbar),
                                )
                            })
                            // Home and the other pages have no item commands: the row keeps its height and holds the page's own (render_page_bar)
                            .when(!show_commandbar && show_toolbar, |d| {
                                d.child(
                                    theme::sheet(&p, p.toolbar_bg)
                                        .h(px(p.toolbar_h))
                                        .flex_shrink_0()
                                        .when(p.gap <= 0., |d| d.border_b_1().border_color(p.sheet_border))
                                        .child(pagebar),
                                )
                            })
                            .children(filter_strip)
                            .children(compare_bar)
                            .child(main_row)
                            // the floating card of a running operation (Settings → Aktarım ilerlemesi → Yüzen kart)
                            .children(cards),
                    ),
            )
            .when(show_statusbar, |d| d.child(status)),
            )
            // Last children = painted on top.
            .children(crate::status_center::render_flyout(&self.ops, &p, theme::TITLEBAR_H + px(44.), cx))
            .children(crate::status_center::update_toast(&p, false))
            .children(crate::speed_hud::overlay(&p, hud_open, hud_items, cx))
            .children(quick_look_layer)
            .children(self.palette.clone())
            .children(crate::fluent_menu::current(window, cx))
            .children(crate::prompt::current(window, cx))
            .children(crate::prop_chooser::overlay(window, cx))
            .children(crate::conflict_dialog::current(window, cx))
            .children(crate::organizer::overlay(window, cx))
            .children(self.bulk.clone().map(|br| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(hsla(0., 0., 0., 0.3))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(br)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{breadcrumbs, parse_location};
    use crate::workspace::tab_terminal_dir;
    use rf_core::Location;
    use std::path::{Path, PathBuf};

    #[test]
    fn crumbs() {
        // the names are read on a worker in the app (FolderView::warm_labels); here the same call, first
        leaf_sys::localized::warm(crate::nav_touch::label_paths(Path::new(r"C:\Users\user")));
        let c = breadcrumbs(Path::new(r"C:\Users\user"));
        let labels: Vec<_> = c.iter().map(|(l, _)| l.as_str()).collect();
        // C:\Users shows Explorer's localized name ("Kullanıcılar" on Turkish Windows)
        let users = leaf_sys::localized::label_if_known(Path::new(r"C:\Users")).map_or("Users".to_string(), |l| l.to_string());
        assert_eq!(labels, vec!["C:", users.as_str(), "user"]);
        assert_ne!(users, "");
        assert_eq!(c[0].1, Location::Path(PathBuf::from(r"C:\")));
        assert_eq!(parse_location("d:"), Some(Location::Path(PathBuf::from(r"d:\"))));
    }

    #[test]
    fn the_terminal_button_is_only_for_real_folders() {
        assert!(tab_terminal_dir(&Location::Home).is_none());
        assert!(tab_terminal_dir(&Location::Settings).is_none());
        let dir = std::env::temp_dir();
        assert_eq!(tab_terminal_dir(&Location::Path(dir.clone())), Some(dir));
        assert!(tab_terminal_dir(&Location::Path(PathBuf::from(r"C:\Downloads\photos.zip\2024"))).is_none());
        assert!(tab_terminal_dir(&Location::Path(PathBuf::from("ftp://example.com/pub"))).is_none());
    }
}

#[cfg(test)]
mod launch_tests {
    use super::{SELECT_PREFIX, parse_launch};
    use rf_core::Location;
    use std::path::PathBuf;

    #[test]
    fn select_messages() {
        let (loc, sel) = parse_launch(&format!(r"{SELECT_PREFIX}C:\Klasör\rapor ü.txt"));
        assert_eq!(loc, Location::Path(PathBuf::from(r"C:\Klasör")));
        assert_eq!(sel.as_deref(), Some("rapor ü.txt"));
        assert_eq!(parse_launch(r"C:\Windows"), (Location::Path(PathBuf::from(r"C:\Windows")), None));
        assert_eq!(parse_launch("  "), (Location::Home, None));
    }

    #[test]
    fn empty_launch_restores_session_only_when_no_browser_window_is_open() {
        use super::launch_window_start;
        assert_eq!(launch_window_start("", false, Location::Home), None);
        assert_eq!(launch_window_start("  ", false, Location::Home), None);
        assert_eq!(launch_window_start("", true, Location::Home), Some(Location::Home));
        let docs = Location::Path(PathBuf::from(r"C:\Docs"));
        assert_eq!(launch_window_start(r"C:\Docs", false, docs.clone()), Some(docs.clone()));
        assert_eq!(launch_window_start(r"C:\Docs", true, docs.clone()), Some(docs));
    }

    /// Issue 20: a window split in two panes at different folders came back with one pane. The second pane of a tab is kept with its
    /// own place and with whether it had the focus, and comes back for the tab it belonged to.
    #[test]
    fn the_second_pane_of_a_split_tab_comes_back_at_its_own_folder() {
        use super::{panes_to_restore, session_key, session_record};
        use crate::settings::SessionPane;
        let p = |s: &str| Location::Path(PathBuf::from(s));
        let tabs = vec![(p(r"C:\Work"), Some((p(r"D:\Backup"), true))), (p(r"C:\Users"), None), (Location::Home, Some((Location::RecycleBin, false)))];
        let (left, panes) = session_record(&tabs);
        assert_eq!(left, [r"C:\Work", r"C:\Users", ""]);
        assert_eq!(
            panes,
            [SessionPane { tab: 0, location: r"D:\Backup".into(), active: true }, SessionPane { tab: 2, location: "shell:RecycleBinFolder".into(), active: false }]
        );
        assert_eq!(panes_to_restore(&panes, 3), [(0, p(r"D:\Backup"), true), (2, Location::RecycleBin, false)]);
        // fewer tabs came back than the record knows (a tab's folder is gone, the list was edited): no pane for a tab that is not there
        assert_eq!(panes_to_restore(&panes, 2), [(0, p(r"D:\Backup"), true)]);
        // two entries for one tab: the first counts
        let twice = [SessionPane { tab: 1, location: r"C:\a".into(), active: false }, SessionPane { tab: 1, location: r"C:\b".into(), active: true }];
        assert_eq!(panes_to_restore(&twice, 2), [(1, p(r"C:\a"), false)]);
        assert!(panes_to_restore(&[], 4).is_empty(), "a session from before this, or one with no split tab");
        // the record is written again when a pane goes somewhere else or is opened or closed, not when only the focus moves
        let moved = session_record(&[(p(r"C:\Work"), Some((p(r"E:\"), true)))]);
        let focus = session_record(&[(p(r"C:\Work"), Some((p(r"D:\Backup"), false)))]);
        let one = session_record(&[(p(r"C:\Work"), Some((p(r"D:\Backup"), true)))]);
        let closed = session_record(&[(p(r"C:\Work"), None)]);
        assert_ne!(session_key(&one.0, &one.1), session_key(&moved.0, &moved.1));
        assert_ne!(session_key(&one.0, &one.1), session_key(&closed.0, &closed.1));
        assert_eq!(session_key(&one.0, &one.1), session_key(&focus.0, &focus.1));
    }

    #[test]
    fn session_tabs_come_back_as_they_were() {
        let phone = Location::Shell {
            parsing: r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\?\usb#vid_04e8&pid_6860#r58n#{6ac27878-a6fa-4155-ba85-f98f491d4f33}".into(),
            name: "Galaxy S24".into(),
        };
        for loc in [Location::Path(PathBuf::from(r"C:\Kullanıcılar\user")), Location::RecycleBin, Location::Downloads, phone] {
            assert_eq!(parse_launch(&super::session_text(&loc)).0, loc);
        }
        // places a session can't keep come back as Home
        assert_eq!(super::session_text(&Location::Settings), "");
    }
}

#[cfg(test)]
mod rail_peek_tests {
    use super::{RAIL_W, rail_peek_width};

    /// The sidebar that comes out of the rail: from the rail's own width to the full one, a little past it while the spring swings,
    /// and never narrower than the rail.
    #[test]
    fn the_sidebar_comes_out_of_the_rail_and_is_never_narrower_than_it() {
        assert_eq!(rail_peek_width(240.0, 0.0), RAIL_W);
        assert_eq!(rail_peek_width(240.0, 1.0), 240.0);
        assert!(rail_peek_width(240.0, 1.05) > 240.0, "the swing past its place");
        assert_eq!(rail_peek_width(240.0, -0.2), RAIL_W);
        assert!(rail_peek_width(240.0, 0.5) > RAIL_W && rail_peek_width(240.0, 0.5) < 240.0);
    }
}

/// The stops of Tab in a window, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Address,
    Tree,
    List,
}

/// The pane after `here` (`forward`) or before it, round; the tree is skipped when it is not on screen.
fn next_pane(here: Pane, forward: bool, tree: bool) -> Pane {
    let order: Vec<Pane> = [Pane::Address, Pane::Tree, Pane::List].into_iter().filter(|p| *p != Pane::Tree || tree).collect();
    let i = order.iter().position(|p| *p == here).unwrap_or(order.len() - 1);
    let n = order.len();
    order[if forward { (i + 1) % n } else { (i + n - 1) % n }]
}

#[cfg(test)]
mod pane_tests {
    use super::{Pane, next_pane};

    #[test]
    fn tab_goes_round_the_address_bar_the_tree_and_the_list() {
        assert_eq!(next_pane(Pane::List, true, true), Pane::Address);
        assert_eq!(next_pane(Pane::Address, true, true), Pane::Tree);
        assert_eq!(next_pane(Pane::Tree, true, true), Pane::List);
        assert_eq!(next_pane(Pane::List, false, true), Pane::Tree);
        assert_eq!(next_pane(Pane::Address, false, true), Pane::List);
        assert_eq!(next_pane(Pane::Address, true, false), Pane::List, "no tree on screen: it is skipped");
        assert_eq!(next_pane(Pane::List, false, false), Pane::Address);
    }
}

/// The cloud and WSL places that are not hidden one by one (`Settings::hidden_places`, paths compared without regard to case).
pub(crate) fn visible_places(places: Vec<crate::places::Pinned>, hidden: &[String]) -> Vec<crate::places::Pinned> {
    places.into_iter().filter(|c| !hidden.iter().any(|h| rf_core::pathkey::name_eq(&c.path.to_string_lossy(), h))).collect()
}

#[cfg(test)]
mod named_session_tests {
    use super::remember_named;
    use crate::settings::NamedSession;

    #[test]
    fn a_session_saved_again_under_its_name_replaces_the_old_one_in_place() {
        let s = |name: &str, tab: &str| NamedSession { name: name.into(), tabs: vec![tab.into()], ..Default::default() };
        let mut list = Vec::new();
        remember_named(&mut list, s("Work", "C:\\A"));
        remember_named(&mut list, s("Photos", "D:\\P"));
        remember_named(&mut list, s("work", "C:\\B"));
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_str(), list[0].tabs[0].as_str()), ("work", "C:\\B"), "same place, the new tabs");
        assert_eq!(list[1].name, "Photos");
    }
}

#[cfg(test)]
mod hidden_places_tests {
    use super::visible_places;
    use crate::places::Pinned;
    use gpui_kit::assets::IconName as L;

    #[test]
    fn a_place_hidden_on_its_own_leaves_the_others() {
        let place = |p: &str| Pinned { title: p.into(), path: p.into(), icon: L::Cloud };
        let all = vec![place(r"C:\Users\user\OneDrive"), place(r"C:\Users\user\Proton Drive"), place(r"\wsl.localhost\Ubuntu")];
        let shown = visible_places(all.clone(), &[r"c:\users\user\onedrive".into()]);
        let paths: Vec<_> = shown.iter().map(|p| p.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, [r"C:\Users\user\Proton Drive", r"\wsl.localhost\Ubuntu"], "the case of a path does not matter");
        assert_eq!(visible_places(all, &[]).len(), 3);
    }
}
