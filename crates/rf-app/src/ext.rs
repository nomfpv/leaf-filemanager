//! The extension seam: a small, typed way for a separate program crate to add to leaf without leaf knowing about it.
//!
//! An extension is a program of its own that depends on `rf-app` as a library and starts leaf with its registrations:
//!
//! ```ignore
//! fn main() {
//!     let ext = rf_app::ext::Extensions::new().add("hello", "Hello", |e| {
//!         e.subscribe(|ev, _cx| println!("{ev:?}"));
//!     });
//!     rf_app::run_with(ext);
//! }
//! ```
//!
//! Public leaf built normally registers nothing: every hook below starts with one cheap check of an empty set, and the behaviour is
//! what it was. Nothing in this repository depends on an extension.
//!
//! Rules that hold for everything here (see `docs/extensions.md`):
//! - Callbacks run on the UI thread and must be quick. Events are delivered after the code that caused them has finished (never
//!   while a view of leaf is being updated), so a subscriber may read and write leaf's state through `&mut App`.
//! - Presentation hooks (display name, row decoration, icon override, layers, slots) are called from render code only. The real
//!   operations read their names and paths from leaf's model and never see what a hook returned.
//! - Each extension can be switched off for the session in Settings > About: its registrations are then skipped everywhere.
//! - A panic in a callback is caught and logged where the build unwinds (the dev and fast profiles). The release profile aborts on
//!   panic, so there a panicking callback ends the program like any other panic; keep callbacks free of `unwrap`.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The toolkit leaf is built on, so an extension uses the same version of it as the application (elements, `App`, `Window`, icons).
pub use gpui_kit;

// ------------------------------------------------------------------------------------------------------------------------- events

/// What kind of place a folder-opened event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    /// A folder on this PC (including one inside an archive).
    Folder,
    /// A folder on a saved host (sftp:// and the like).
    Remote,
    /// A search result, a tag, a library or another list that is not one folder.
    List,
    /// Home, Settings and the other pages.
    Page,
}

/// How an operation of the Status Center ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpResult {
    Done,
    Cancelled,
    /// Failed; leaf's own words for it.
    Failed(String),
}

/// A read-only notification. New variants may be added: match with a `_` arm.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Event {
    /// The first window is about to open.
    AppStart,
    /// The application is quitting (not delivered when the process is killed).
    AppExit,
    /// A folder (or list) finished loading in a tab.
    FolderOpened { path: Option<PathBuf>, kind: PlaceKind, items: usize, load_ms: u128 },
    /// The selection of the active tab changed.
    SelectionChanged { count: usize, total_bytes: u64 },
    /// A job of the Status Center (copy, move, delete, extract, compress, download, upload, sync, ...) started. `title` is leaf's own
    /// wording for it ("Moving to Trash: 3 items"); the kind is not given separately.
    OperationStarted { id: u64, title: String },
    /// A job reported progress (at most a few times a second). `bytes` is `(done, total)` when the engine counts bytes, and
    /// `bytes_per_sec` the speed leaf shows for it.
    OperationProgress { id: u64, title: String, items_done: u32, items_total: u32, bytes: Option<(u64, u64)>, bytes_per_sec: Option<u64>, current: Option<String> },
    /// A job ended.
    OperationFinished { id: u64, title: String, result: OpResult, bytes: u64, elapsed_ms: u64 },
    /// Items were deleted (to the Trash, or for good) and the job succeeded. Names are the top-level items; `sizes` are the file
    /// sizes read just before (0 for a folder). At most 200 items are listed.
    ItemsDeleted { names: Vec<String>, sizes: Vec<u64>, permanent: bool, total: usize },
    /// The window gained or lost the focus.
    WindowFocus { focused: bool },
    /// A message from a friend's leaf on a shared folder's link (see `ext::peer`). Delivered only when both switches allow it.
    PeerMessage { friend: String, friend_name: String, kind: u16, payload: Vec<u8> },
}

// ------------------------------------------------------------------------------------------------------------------------- types

/// Where an item goes in the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Which right-click menu a [`Command`] is offered in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuTarget {
    /// The menu of selected files (at least one is not a folder).
    Files,
    /// The menu of selected folders only.
    Folders,
    /// The menu of the empty space of a folder.
    Background,
}

/// What a command or menu entry is run on: the folder shown and the selected items of the active tab.
#[derive(Clone, Debug, Default)]
pub struct CmdCtx {
    pub dir: Option<PathBuf>,
    pub selection: Vec<PathBuf>,
}

type Pred = Rc<dyn Fn(&CmdCtx) -> bool>;

/// A command of the command search (Ctrl+Shift+P), optionally with a key, and the same item in right-click menus.
#[derive(Clone)]
pub struct Command {
    id: String,
    title: String,
    icon: IconName,
    keys: Option<String>,
    when: Option<Pred>,
    run: Rc<dyn Fn(&CmdCtx, &mut App)>,
}

impl Command {
    /// `id` is unique inside the extension; `title` is what the user reads.
    pub fn new(id: &str, title: &str, run: impl Fn(&CmdCtx, &mut App) + 'static) -> Self {
        Self { id: id.to_string(), title: title.to_string(), icon: IconName::Command, keys: None, when: None, run: Rc::new(run) }
    }
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }
    /// A key in the toolkit's syntax (`ctrl-alt-j`). Bound at start; an invalid one is logged and ignored.
    pub fn keys(mut self, keys: &str) -> Self {
        self.keys = Some(keys.to_string());
        self
    }
    /// A menu entry shows only while this is true for the selection. (The command search lists a command always.)
    pub fn when(mut self, pred: impl Fn(&CmdCtx) -> bool + 'static) -> Self {
        self.when = Some(Rc::new(pred));
        self
    }
}

/// One row of a sidebar group.
#[derive(Clone)]
pub struct SidebarRow {
    pub icon: IconName,
    pub label: String,
    pub on_click: Rc<dyn Fn(&mut Window, &mut App)>,
    /// The right-click menu of the row; empty for none.
    pub menu: Vec<MenuEntry>,
}

/// One entry of a menu an extension builds.
#[derive(Clone)]
pub struct MenuEntry {
    pub label: String,
    pub icon: IconName,
    pub run: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl SidebarRow {
    pub fn new(icon: IconName, label: &str, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Self { icon, label: label.to_string(), on_click: Rc::new(on_click), menu: Vec::new() }
    }
}

/// What a row decoration or icon hook is told about a file row.
#[derive(Clone, Debug)]
pub struct RowInfo {
    pub path: PathBuf,
    pub is_dir: bool,
    pub selected: bool,
}

/// A pane docked at the right side of the workspace, like the preview pane.
pub struct SidePane {
    pub title: String,
    pub width: f32,
    pub render: Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>,
}

/// The colours of the app's theme, read only.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub window_bg: Hsla,
    pub content_bg: Hsla,
    pub card_bg: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub hover: Hsla,
    pub selection: Hsla,
}

impl Default for Theme {
    fn default() -> Self {
        let g = |l: f32| hsla(0., 0., l, 1.);
        Self { dark: false, window_bg: g(0.95), content_bg: g(1.), card_bg: g(1.), text: g(0.1), text_secondary: g(0.35), text_tertiary: g(0.5), border: g(0.85), accent: hsla(0.6, 0.7, 0.5, 1.), hover: hsla(0., 0., 0., 0.06), selection: hsla(0.6, 0.7, 0.5, 0.2) }
    }
}

// ------------------------------------------------------------------------------------------------------------------------- builder

/// The extensions to start leaf with. `Extensions::new()` is empty: leaf then behaves as it does without this seam.
#[derive(Default)]
pub struct Extensions {
    list: Vec<Entry>,
}

impl Extensions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one extension. `id` is short and stable (it names the extension's data folder); `name` is shown in Settings > About.
    /// `register` is called at once and states what the extension adds.
    pub fn add(mut self, id: &str, name: &str, register: impl FnOnce(&mut Registrar)) -> Self {
        let mut r = Registrar::default();
        register(&mut r);
        self.list.push(Entry { id: sanitize(id), name: name.to_string(), off: Cell::new(false), r });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// What one extension registers. Everything is optional.
#[derive(Default)]
pub struct Registrar {
    subs: Vec<Rc<dyn Fn(&Event, &mut App)>>,
    overlay: Option<Layer>,
    underlay: Option<Layer>,
    status: Vec<(Side, Rc<dyn Fn(&mut App) -> Option<AnyElement>>)>,
    title: Vec<(f32, Rc<dyn Fn(&mut App) -> Option<AnyElement>>)>,
    groups: Vec<(String, Rc<dyn Fn(&mut App) -> Vec<SidebarRow>>)>,
    pane: Option<SidePane>,
    commands: Vec<Command>,
    menus: Vec<(MenuTarget, Command)>,
    name_hook: Option<Rc<dyn Fn(&Path) -> Option<String>>>,
    row_hook: Option<Rc<dyn Fn(&RowInfo) -> Option<AnyElement>>>,
    icon_hook: Option<Rc<dyn Fn(&Path) -> Option<Arc<RenderImage>>>>,
    about: Option<Rc<dyn Fn(&mut App) -> AnyElement>>,
}

type Layer = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

impl Registrar {
    /// Events. One subscription gets every [`Event`]; match the ones you need.
    pub fn subscribe(&mut self, f: impl Fn(&Event, &mut App) + 'static) {
        self.subs.push(Rc::new(f));
    }
    /// An element drawn above the whole workspace of each window (below dialogs and menus). It fills the window and lets the mouse
    /// through except where the extension's own elements take it (give them a mouse handler). Redrawn when leaf redraws or on
    /// [`request_frame`].
    pub fn overlay(&mut self, f: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.overlay = Some(Rc::new(f));
    }
    /// The same, behind the panes (above the window background).
    pub fn underlay(&mut self, f: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.underlay = Some(Rc::new(f));
    }
    /// An item in the status bar, left group or right group; `None` shows nothing this time.
    pub fn status_item(&mut self, side: Side, f: impl Fn(&mut App) -> Option<AnyElement> + 'static) {
        self.status.push((side, Rc::new(f)));
    }
    /// An item at the right of the title bar, before the system buttons. `width` (logical pixels) is the room it needs.
    pub fn title_item(&mut self, width: f32, f: impl Fn(&mut App) -> Option<AnyElement> + 'static) {
        self.title.push((width, Rc::new(f)));
    }
    /// A group of rows in the sidebar (a heading and the rows); no rows, no heading.
    pub fn sidebar_group(&mut self, title: &str, rows: impl Fn(&mut App) -> Vec<SidebarRow> + 'static) {
        self.groups.push((title.to_string(), Rc::new(rows)));
    }
    /// A pane docked at the right of the workspace. It is shown after the command "Show/hide <title>" (added for it) is run.
    pub fn side_pane(&mut self, title: &str, width: f32, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.pane = Some(SidePane { title: title.to_string(), width: width.clamp(160., 640.), render: Rc::new(render) });
    }
    /// A command in the command search, with its optional key.
    pub fn command(&mut self, c: Command) {
        self.commands.push(c);
    }
    /// An entry in a right-click menu (label, icon, the `when` predicate over the selection, the handler).
    pub fn menu_entry(&mut self, target: MenuTarget, c: Command) {
        self.menus.push((target, c));
    }
    /// Display only: the text shown instead of a file's name in lists (`None` keeps the name). Every operation, copy path, rename and
    /// properties still use the real name.
    pub fn display_name(&mut self, f: impl Fn(&Path) -> Option<String> + 'static) {
        self.name_hook = Some(Rc::new(f));
    }
    /// Display only: an extra element at the end of a file row's name (a badge, a dot).
    pub fn row_decoration(&mut self, f: impl Fn(&RowInfo) -> Option<AnyElement> + 'static) {
        self.row_hook = Some(Rc::new(f));
    }
    /// Display only: a picture shown instead of a file's icon.
    pub fn icon_override(&mut self, f: impl Fn(&Path) -> Option<Arc<RenderImage>> + 'static) {
        self.icon_hook = Some(Rc::new(f));
    }
    /// A section under this extension's row in Settings > About (its own switches and texts).
    pub fn about_section(&mut self, f: impl Fn(&mut App) -> AnyElement + 'static) {
        self.about = Some(Rc::new(f));
    }
}

struct Entry {
    id: String,
    name: String,
    /// Switched off for this session (Settings > About).
    off: Cell<bool>,
    r: Registrar,
}

// ------------------------------------------------------------------------------------------------------------------------- registry

struct Registry {
    list: Vec<Entry>,
    pane_open: Cell<Option<usize>>,
    scene: RefCell<Option<Scene>>,
    scene_gen: Cell<u64>,
}

struct Scene {
    ext: usize,
    render: Layer,
}

thread_local! {
    static REG: RefCell<Option<Rc<Registry>>> = const { RefCell::new(None) };
    static THEME: Cell<Option<Theme>> = const { Cell::new(None) };
    static SEL: Cell<(usize, u64)> = const { Cell::new((0, 0)) };
}

/// Any extension registered: the one check every hook starts with.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The per-row hooks have their own flags: a registered but idle extension costs one load per row.
static NAME_HOOK: AtomicBool = AtomicBool::new(false);
static ROW_HOOK: AtomicBool = AtomicBool::new(false);
static ICON_HOOK: AtomicBool = AtomicBool::new(false);

fn registry() -> Option<Rc<Registry>> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    REG.with(|r| r.borrow().clone())
}

/// A file name part of a folder name: letters, digits, `-` and `_` only.
fn sanitize(id: &str) -> String {
    let s: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.is_empty() { "ext".to_string() } else { s }
}

/// Runs a callback of extension `who`; a panic is caught (where the build unwinds) and logged.
fn guard<R>(who: &str, f: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            log::warn!("extension {who}: a callback panicked and was skipped");
            None
        }
    }
}

impl Registry {
    fn on(&self) -> impl Iterator<Item = (usize, &Entry)> {
        self.list.iter().enumerate().filter(|(_, e)| !e.off.get())
    }
    fn refresh_flags(&self) {
        let any = |f: fn(&Registrar) -> bool| self.on().any(|(_, e)| f(&e.r));
        NAME_HOOK.store(any(|r| r.name_hook.is_some()), Ordering::Relaxed);
        ROW_HOOK.store(any(|r| r.row_hook.is_some()), Ordering::Relaxed);
        ICON_HOOK.store(any(|r| r.icon_hook.is_some()), Ordering::Relaxed);
    }
}

/// Called once, on the UI thread, before the first window. An empty set leaves everything off.
pub(crate) fn install(ext: Extensions, cx: &mut App) {
    if ext.list.is_empty() {
        return;
    }
    let reg = Rc::new(Registry { list: ext.list, pane_open: Cell::new(None), scene: RefCell::new(None), scene_gen: Cell::new(0) });
    reg.refresh_flags();
    REG.with(|r| *r.borrow_mut() = Some(reg.clone()));
    ACTIVE.store(true, Ordering::Relaxed);
    bind_keys(&reg, cx);
    cx.on_app_quit(|cx| {
        dispatch(&Event::AppExit, cx);
        std::future::ready(())
    })
    .detach();
    emit(cx, || Event::AppStart);
}

// ------------------------------------------------------------------------------------------------------------------------- events out

fn dispatch(ev: &Event, cx: &mut App) {
    let Some(reg) = registry() else { return };
    for (_, e) in reg.on() {
        for s in &e.r.subs {
            let s = s.clone();
            guard(&e.id, || s(ev, cx));
        }
    }
}

/// Tells the subscribers, after the current update has finished. `make` is not called when nothing is registered.
pub(crate) fn emit(cx: &mut App, make: impl FnOnce() -> Event) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let ev = make();
    cx.defer(move |cx| dispatch(&ev, cx));
}

/// Whether any extension is registered (for callers that would do work to build an event).
pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Looked at while the status bar is drawn: the selection's count and size; an event only when they changed.
pub(crate) fn note_selection(count: usize, bytes: u64, cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) || SEL.get() == (count, bytes) {
        return;
    }
    SEL.set((count, bytes));
    emit(cx, || Event::SelectionChanged { count, total_bytes: bytes });
}

/// Redraws every window (for a layer whose picture changed).
pub fn request_frame(cx: &mut App) {
    cx.refresh_windows();
}

// ------------------------------------------------------------------------------------------------------------------------- theme, storage

/// The app's theme colours as the active window last used them (read only).
pub fn theme() -> Theme {
    THEME.get().unwrap_or_default()
}

pub(crate) fn note_theme(p: &crate::theme::Palette) {
    if ACTIVE.load(Ordering::Relaxed) {
        THEME.set(Some(Theme {
            dark: p.dark,
            window_bg: p.window_bg,
            content_bg: p.content_bg,
            card_bg: p.card_bg,
            text: p.text,
            text_secondary: p.text_secondary,
            text_tertiary: p.text_tertiary,
            border: p.border,
            accent: p.accent,
            hover: p.hover,
            selection: p.selection,
        }));
    }
}

/// The extension's own folder under leaf's config folder (made when asked for). `id` is the one given to [`Extensions::add`].
pub fn data_dir(id: &str) -> Option<PathBuf> {
    let d = crate::settings::app_dir()?.join("extensions").join(sanitize(id));
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// Plays the notification sound of the user's sound scheme (what leaf plays for its own notices). leaf has no sound setting of its
/// own: the scheme decides, and a scheme without a sound plays nothing.
pub fn play_sound() {
    leaf_sys::sound::notify();
}

// ------------------------------------------------------------------------------------------------------------------------- layers and slots

fn layer_of(pick: impl Fn(&Registrar) -> Option<Layer>, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let mut out = Vec::new();
    for (_, e) in reg.on() {
        if let Some(f) = pick(&e.r)
            && let Some(el) = guard(&e.id, || f(window, cx))
        {
            out.push(div().id(SharedString::from(format!("ext-layer-{}", e.id))).absolute().inset_0().child(el).into_any_element());
        }
    }
    out
}

pub(crate) fn overlays(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    layer_of(|r| r.overlay.clone(), window, cx)
}

pub(crate) fn underlays(window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    layer_of(|r| r.underlay.clone(), window, cx)
}

pub(crate) fn status_items(side: Side, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let mut out = Vec::new();
    for (_, e) in reg.on() {
        for (s, f) in &e.r.status {
            if *s == side
                && let Some(Some(el)) = guard(&e.id, || f(cx))
            {
                out.push(el);
            }
        }
    }
    out
}

/// The title bar's extra items and the room they take.
pub(crate) fn title_items(cx: &mut App) -> (Vec<AnyElement>, f32) {
    let Some(reg) = registry() else { return (Vec::new(), 0.) };
    let (mut out, mut w) = (Vec::new(), 0.);
    for (_, e) in reg.on() {
        for (width, f) in &e.r.title {
            if let Some(Some(el)) = guard(&e.id, || f(cx)) {
                out.push(el);
                w += width;
            }
        }
    }
    (out, w)
}

/// The sidebar groups, drawn as a heading and rows in the palette `p`.
pub(crate) fn sidebar_groups(p: &crate::theme::Palette, rail: bool, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let mut out = Vec::new();
    for (_, e) in reg.on() {
        for (title, rows) in &e.r.groups {
            let Some(rows) = guard(&e.id, || rows(cx)).filter(|r| !r.is_empty()) else { continue };
            let mut col = v_flex().flex_shrink_0().w_full();
            col = col.child(if rail {
                div().mx_2().my_2().h(px(1.)).bg(p.divider)
            } else {
                div().h(px(28.)).flex_shrink_0().mt(px(12.)).px_2().flex().items_center().text_size(px(p.fs_xs)).font_weight(FontWeight::SEMIBOLD).text_color(p.text_tertiary).child(title.clone())
            });
            for (i, row) in rows.into_iter().enumerate() {
                let (click, menu) = (row.on_click.clone(), row.menu.clone());
                let (hover, text) = (p.hover, p.text_secondary);
                let el = h_flex()
                    .id(SharedString::from(format!("ext-side-{}-{}-{i}", e.id, title)))
                    .mx_1()
                    .h(px(30.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(crate::theme::RADIUS_SM)
                    .text_color(text)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(Icon::new(row.icon).size(px(16.)).flex_shrink_0())
                    .when(!rail, |d| d.child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(row.label.clone())))
                    .on_click(move |_, window, cx| click(window, cx))
                    .when(!menu.is_empty(), |d| {
                        d.on_mouse_down(MouseButton::Right, move |ev, window, cx| {
                            let items = menu
                                .iter()
                                .map(|m| {
                                    let run = m.run.clone();
                                    crate::fluent_menu::Item::action(m.label.clone(), crate::fluent_menu::MenuIcon::Lucide(m.icon), move |w, cx| run(w, cx))
                                })
                                .collect();
                            crate::fluent_menu::show(crate::fluent_menu::Menu { quick: Vec::new(), items }, ev.position, window, cx);
                            cx.stop_propagation();
                        })
                    });
                col = col.child(el);
            }
            out.push(col.into_any_element());
        }
    }
    out
}

/// The docked panes that are open (at most one per extension).
pub(crate) fn side_panes(p: &crate::theme::Palette, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let Some(open) = reg.pane_open.get() else { return Vec::new() };
    let Some(e) = reg.list.get(open).filter(|e| !e.off.get()) else { return Vec::new() };
    let Some(pane) = &e.r.pane else { return Vec::new() };
    let Some(el) = guard(&e.id, || (pane.render)(window, cx)) else { return Vec::new() };
    vec![
        crate::theme::sheet(p, p.content_bg)
            .w(px(pane.width))
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .when(p.gap <= 0., |d| d.border_l_1().border_color(p.sheet_border))
            .child(el)
            .into_any_element(),
    ]
}

// ------------------------------------------------------------------------------------------------------------------------- scene

/// The longest a scene may last; it also ends at once on Esc.
pub const SCENE_MAX_MS: u64 = 30_000;

/// Takes over the whole window content for a short scene: `render` is drawn above the workspace, below leaf's own dialogs and menus.
/// Esc ends it at any time and it ends by itself after `ms` (at most [`SCENE_MAX_MS`]). One scene at a time; a new one replaces the
/// old. `ext_id` is the extension's id as given to [`Extensions::add`]; a switched-off extension's scene is not drawn.
pub fn start_scene(ext_id: &str, ms: u64, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static, cx: &mut App) {
    let Some(reg) = registry() else { return };
    let Some(ix) = reg.list.iter().position(|e| e.id == sanitize(ext_id)) else { return };
    let gen_now = reg.scene_gen.get() + 1;
    reg.scene_gen.set(gen_now);
    *reg.scene.borrow_mut() = Some(Scene { ext: ix, render: Rc::new(render) });
    cx.refresh_windows();
    let ms = ms.min(SCENE_MAX_MS);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
        let _ = cx.update(|cx| {
            if registry().is_some_and(|r| r.scene_gen.get() == gen_now) {
                end_scene(cx);
            }
        });
    })
    .detach();
}

/// Ends the scene, if one runs.
pub fn end_scene(cx: &mut App) {
    if let Some(reg) = registry() {
        reg.scene.borrow_mut().take();
        reg.scene_gen.set(reg.scene_gen.get() + 1);
        cx.refresh_windows();
    }
}

pub(crate) fn scene_active() -> bool {
    registry().is_some_and(|r| r.scene.borrow().is_some())
}

pub(crate) fn scene_element(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let reg = registry()?;
    let (ix, render) = reg.scene.borrow().as_ref().map(|s| (s.ext, s.render.clone()))?;
    let e = reg.list.get(ix).filter(|e| !e.off.get())?;
    let el = guard(&e.id, || render(window, cx))?;
    // (it takes the mouse: nothing of the workspace under it is clicked by accident)
    Some(div().id("ext-scene").absolute().inset_0().occlude().child(el).into_any_element())
}

// ------------------------------------------------------------------------------------------------------------------------- commands

/// Runs an extension command from the command search or its key.
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = leaf, no_json)]
pub struct RunExtCommand {
    /// `<extension id>/<command id>`
    pub id: String,
}

fn full_id(ext: &str, cmd: &str) -> String {
    format!("{ext}/{cmd}")
}

fn bind_keys(reg: &Registry, cx: &mut App) {
    let mut b = Vec::new();
    for (_, e) in reg.on() {
        for c in &e.r.commands {
            if let Some(k) = &c.keys {
                match KeyBinding::load(k, Box::new(RunExtCommand { id: full_id(&e.id, &c.id) }), None, false, None, cx) {
                    Ok(kb) => b.push(kb),
                    Err(err) => log::warn!("extension {}: key {k:?}: {err}", e.id),
                }
            }
        }
    }
    cx.bind_keys(b);
}

/// The commands for the command search: `(full id, title, icon, keys)`, switched-off extensions left out. A pane's toggle is one.
pub(crate) fn palette_commands() -> Vec<(String, String, IconName, String)> {
    let Some(reg) = registry() else { return Vec::new() };
    let mut out = Vec::new();
    for (_, e) in reg.on() {
        for c in &e.r.commands {
            out.push((full_id(&e.id, &c.id), c.title.clone(), c.icon, c.keys.clone().unwrap_or_default()));
        }
        if let Some(p) = &e.r.pane {
            out.push((full_id(&e.id, "pane"), format!("{} {}", crate::t!("Show or hide"), p.title), IconName::PanelRight, String::new()));
        }
    }
    out
}

pub(crate) fn run_command(id: &str, ctx: &CmdCtx, cx: &mut App) {
    let Some(reg) = registry() else { return };
    for (ix, e) in reg.on() {
        let Some(rest) = id.strip_prefix(e.id.as_str()).and_then(|r| r.strip_prefix('/')) else { continue };
        if rest == "pane" && e.r.pane.is_some() {
            reg.pane_open.set(if reg.pane_open.get() == Some(ix) { None } else { Some(ix) });
            cx.refresh_windows();
            return;
        }
        if let Some(c) = e.r.commands.iter().find(|c| c.id == rest) {
            let run = c.run.clone();
            guard(&e.id, || run(ctx, cx));
            cx.refresh_windows();
        }
        return;
    }
}

/// Adds the extension entries to a right-click menu about to be shown.
pub(crate) fn extend_menu(menu: &mut crate::fluent_menu::Menu, target: MenuTarget, ctx: &CmdCtx) {
    let Some(reg) = registry() else { return };
    let mut mine = Vec::new();
    for (_, e) in reg.on() {
        for (t, c) in &e.r.menus {
            if *t != target {
                continue;
            }
            if let Some(w) = &c.when
                && !guard(&e.id, || w(ctx)).unwrap_or(false)
            {
                continue;
            }
            let (run, ctx, who) = (c.run.clone(), ctx.clone(), e.id.clone());
            mine.push(crate::fluent_menu::Item::action(c.title.clone(), crate::fluent_menu::MenuIcon::Lucide(c.icon), move |_, cx| {
                guard(&who, || run(&ctx, cx));
            }));
        }
    }
    if !mine.is_empty() {
        menu.items.push(crate::fluent_menu::Item::Separator);
        menu.items.extend(mine);
    }
}

/// Which menu the items of a selection get.
pub(crate) fn target_of(background: bool, items: &[PathBuf]) -> MenuTarget {
    if background || items.is_empty() {
        MenuTarget::Background
    } else if items.iter().all(|p| p.is_dir()) {
        MenuTarget::Folders
    } else {
        MenuTarget::Files
    }
}

// ------------------------------------------------------------------------------------------------------------------------- presentation hooks

/// Display only. The text to show instead of `shown` for the file at `path`; `shown` itself when no hook changes it. Used by the
/// file list's rows and the tab titles; nothing that acts on a file reads it.
pub(crate) fn display_name(path: &Path, shown: SharedString) -> SharedString {
    if !NAME_HOOK.load(Ordering::Relaxed) {
        return shown;
    }
    let Some(reg) = registry() else { return shown };
    for (_, e) in reg.on() {
        if let Some(f) = &e.r.name_hook
            && let Some(Some(n)) = guard(&e.id, || f(path))
        {
            return n.into();
        }
    }
    shown
}

pub(crate) fn has_name_hook() -> bool {
    NAME_HOOK.load(Ordering::Relaxed)
}

pub(crate) fn has_row_hook() -> bool {
    ROW_HOOK.load(Ordering::Relaxed)
}

pub(crate) fn row_decoration(info: &RowInfo) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    reg.on().filter_map(|(_, e)| e.r.row_hook.as_ref().and_then(|f| guard(&e.id, || f(info)).flatten())).collect()
}

pub(crate) fn has_icon_hook() -> bool {
    ICON_HOOK.load(Ordering::Relaxed)
}

pub(crate) fn icon_override(path: &Path) -> Option<Arc<RenderImage>> {
    let reg = registry()?;
    reg.on().find_map(|(_, e)| e.r.icon_hook.as_ref().and_then(|f| guard(&e.id, || f(path)).flatten()))
}

// ------------------------------------------------------------------------------------------------------------------------- About

/// Settings > About: "Extensions: none", or one row per extension with a switch for this session.
pub(crate) fn about_rows(p: &crate::theme::Palette, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings_view::{card, section, switch};
    let Some(reg) = registry() else {
        return vec![div().px_1().text_xs().text_color(p.text_secondary).child(format!("{}: {}", crate::t!("Extensions"), crate::t!("none"))).into_any_element()];
    };
    let mut out = vec![section(p, crate::t!("Extensions"))];
    for (ix, e) in reg.list.iter().enumerate() {
        let (r2, pp) = (reg.clone(), *p);
        let on = !e.off.get();
        let sw = switch(SharedString::from(format!("ext-sw-{}", e.id)), &pp, on, move |_, cx| {
            if let Some(e) = r2.list.get(ix) {
                e.off.set(!e.off.get());
            }
            r2.refresh_flags();
            if r2.pane_open.get() == Some(ix) && r2.list[ix].off.get() {
                r2.pane_open.set(None);
            }
            cx.refresh_windows();
        });
        out.push(card(p, IconName::Puzzle, &e.name, Some(&crate::t!("Switch off for this session: its additions are not drawn and not called.")), sw));
        if on && let Some(f) = &e.r.about {
            let f = f.clone();
            if let Some(el) = guard(&e.id, || f(cx)) {
                out.push(el);
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------------------------------- painting

/// How a [`FrameSurface`] enlarges its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scaling {
    /// Smooth (the toolkit's sampling).
    Linear,
    /// Each pixel becomes a `factor` x `factor` block (1 to 8) before the picture is handed to the toolkit: hard edges.
    Nearest(u32),
}

/// A caller-supplied RGBA picture painted into the bounds of an element, replaced as often as the caller likes (once per frame):
/// no PNG encoding, one copy of the pixels. Alpha is the straight alpha of the RGBA bytes, as for any picture in leaf.
#[derive(Clone)]
pub struct FrameSurface {
    st: Rc<RefCell<Surface>>,
}

struct Surface {
    image: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    scaling: Scaling,
}

impl FrameSurface {
    pub fn new(scaling: Scaling) -> Self {
        Self { st: Rc::new(RefCell::new(Surface { image: None, retired: Vec::new(), scaling })) }
    }

    /// Sets the next frame from `width * height * 4` bytes of RGBA. Returns false (and keeps the old frame) when the length does not
    /// match or a side is 0. Call [`request_frame`] afterwards to show it.
    pub fn set_frame(&self, width: u32, height: u32, rgba: &[u8]) -> bool {
        let Some(want) = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4)) else { return false };
        if width == 0 || height == 0 || rgba.len() != want || want > 256 << 20 {
            return false;
        }
        let mut st = self.st.borrow_mut();
        let k = match st.scaling {
            Scaling::Nearest(f) => f.clamp(1, 8),
            Scaling::Linear => 1,
        };
        let (w, h) = (width * k, height * k);
        let mut buf = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            let row = &rgba[(y / k) as usize * width as usize * 4..][..width as usize * 4];
            for px4 in row.chunks_exact(4) {
                for _ in 0..k {
                    buf.extend_from_slice(&[px4[2], px4[1], px4[0], px4[3]]);
                }
            }
        }
        let Some(img) = image::RgbaImage::from_raw(w, h, buf) else { return false };
        let new = Arc::new(RenderImage::new(vec![image::Frame::new(img)]));
        if let Some(old) = st.image.replace(new) {
            st.retired.push(old);
        }
        true
    }

    /// The element: fills its parent and paints the latest frame into its bounds.
    pub fn element(&self) -> AnyElement {
        let st = self.st.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let (img, retired) = {
                    let mut s = st.borrow_mut();
                    (s.image.clone(), std::mem::take(&mut s.retired))
                };
                // the pictures that were replaced are given back to the toolkit's cache
                for old in retired {
                    let _ = window.drop_image(old);
                }
                if let Some(img) = img {
                    let _ = window.paint_image(bounds, bounds, Corners::default(), img, 0, false);
                }
            },
        )
        .size_full()
        .into_any_element()
    }
}

// ------------------------------------------------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// A display-name hook changes what is shown and nothing else: the path it is asked about is the real one and the real name
    /// stays what `Path` says.
    #[test]
    fn display_name_hook_changes_only_what_is_shown() {
        let mut ext = Extensions::new();
        ext = ext.add("t", "T", |e| e.display_name(|p| p.file_name().map(|n| format!("shown {}", n.to_string_lossy()))));
        let reg = Rc::new(Registry { list: ext.list, pane_open: Cell::new(None), scene: RefCell::new(None), scene_gen: Cell::new(0) });
        reg.refresh_flags();
        REG.with(|r| *r.borrow_mut() = Some(reg.clone()));
        ACTIVE.store(true, Ordering::Relaxed);

        let path = Path::new("C:/data/report.txt");
        let shown = display_name(path, "report.txt".into());
        assert_eq!(shown.as_ref(), "shown report.txt");
        // the model is untouched: the same path still names the same file, and the real name is what operations use
        assert_eq!(path.file_name().unwrap(), "report.txt");
        // switched off: the real name is shown again
        reg.list[0].off.set(true);
        reg.refresh_flags();
        assert_eq!(display_name(path, "report.txt".into()).as_ref(), "report.txt");

        ACTIVE.store(false, Ordering::Relaxed);
        REG.with(|r| *r.borrow_mut() = None);
        NAME_HOOK.store(false, Ordering::Relaxed);
    }
}
