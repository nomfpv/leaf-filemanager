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

/// What a Status Center job does. `Other` for the jobs that are none of the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Delete,
    Rename,
    Extract,
    Compress,
    Download,
    Upload,
    Sync,
    Other,
}

/// The kind of a file as leaf groups them for its icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Document,
    Spreadsheet,
    Pdf,
    Executable,
    Text,
    Other,
}

impl FileKind {
    /// The kind of an item called `name` (by its extension; `is_dir` for a folder).
    pub fn of_name(name: &str, is_dir: bool) -> Self {
        use crate::icons::Category as C;
        if is_dir {
            return Self::Folder;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        match crate::icons::category_for_ext(&ext) {
            C::Folder => Self::Folder,
            C::Image => Self::Image,
            C::Video => Self::Video,
            C::Audio => Self::Audio,
            C::Archive => Self::Archive,
            C::Code => Self::Code,
            C::Document => Self::Document,
            C::Spreadsheet => Self::Spreadsheet,
            C::Pdf => Self::Pdf,
            C::Executable => Self::Executable,
            C::Text => Self::Text,
            C::Other => Self::Other,
        }
    }
}

/// What an archive extraction is about (see [`Event::ArchiveExtractStarting`]).
#[derive(Clone, Debug)]
pub struct ExtractInfo {
    pub archive: PathBuf,
    /// The folder the files are written into.
    pub dest: PathBuf,
    /// Entries in the archive (files and folders); 0 when the list could not be read.
    pub entries: usize,
    /// The uncompressed size of all of them.
    pub total_bytes: u64,
    /// The name and kind of the largest entry.
    pub largest: Option<(String, FileKind)>,
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
    OperationStarted { id: u64, kind: OpKind, title: String },
    /// A job reported progress (at most a few times a second). `bytes` is `(done, total)` when the engine counts bytes, and
    /// `bytes_per_sec` the speed leaf shows for it.
    OperationProgress { id: u64, kind: OpKind, title: String, items_done: u32, items_total: u32, bytes: Option<(u64, u64)>, bytes_per_sec: Option<u64>, current: Option<String> },
    /// A job ended.
    OperationFinished { id: u64, kind: OpKind, title: String, result: OpResult, bytes: u64, elapsed_ms: u64 },
    /// Items were deleted (to the Trash, or for good) and the job succeeded. Names are the top-level items; `sizes` are the file
    /// sizes read just before (0 for a folder). At most 200 items are listed.
    ItemsDeleted { names: Vec<String>, sizes: Vec<u64>, permanent: bool, total: usize },
    /// An archive is about to be extracted (the extraction starts at once and runs on; an extension may show a scene meanwhile).
    ArchiveExtractStarting { info: ExtractInfo },
    /// The extraction of an archive ended.
    ArchiveExtracted { info: ExtractInfo, result: OpResult },
    /// A drive's free or total space changed by a real amount (at least 1 percent of its size). `drive` is its root ("C:\\").
    DriveSpace { drive: PathBuf, free: u64, total: u64 },
    /// The live speed of a running transfer (at most twice a second per job); `id` is the one of `OperationStarted`.
    TransferSpeed { id: u64, bytes_per_sec: u64 },
    /// A download finished and its file is in place.
    DownloadFinished { name: String, size: u64, kind: FileKind },
    /// The window gained or lost the focus.
    WindowFocus { focused: bool },
    /// The window was minimised (`true`) or brought back (`false`).
    WindowMinimized { minimized: bool },
    /// A frame is being drawn while an extension asked for frames (see [`animate`] and [`want_frames`]); `dt_ms` since the last one.
    Frame { dt_ms: u32 },
    /// The link to a friend's leaf came up (`online`) or went down. `friend` is the friend's stable id (see [`peer::friends`]).
    FriendLink { friend: String, friend_name: String, online: bool },
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
    /// The menu of a sidebar row of a folder (`selection` holds its path).
    SidebarRow,
    /// The menu of a tab (`dir` holds the tab's folder when it shows one).
    Tab,
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

/// Why rows leave the file list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveCause {
    /// Deleted: to the Trash, or for good.
    Delete { permanent: bool },
    /// Moved to another folder.
    Move,
}

/// One row that is about to leave the file list.
#[derive(Clone, Debug)]
pub struct LeavingRow {
    pub path: PathBuf,
    /// The name as the list shows it (the real name; a display-name hook is not applied).
    pub name: String,
    /// The kind by name (a folder is not told apart from a file without a name extension: `Folder` only when the row was on screen).
    pub kind: FileKind,
    /// Where the row is drawn right now (window coordinates); `None` when it is not on screen.
    pub rect: Option<Bounds<Pixels>>,
}

/// What a "rows leave the list" hook is told, just before the list drops the rows. Visual only: the operation does not wait for the
/// hook and never sees what it returns.
#[derive(Clone, Debug)]
pub struct RowsLeaving {
    pub cause: LeaveCause,
    /// The folder the rows are in.
    pub dir: Option<PathBuf>,
    pub rows: Vec<LeavingRow>,
}

/// What a "a folder opens" hook is told.
#[derive(Clone, Debug)]
pub struct FolderOpening {
    /// The place left (`None` on a first load or when it is not a folder).
    pub from: Option<PathBuf>,
    pub to: Option<PathBuf>,
    /// The content area as it is now (window coordinates).
    pub content: Option<Bounds<Pixels>>,
}

/// What the pointer did in a region of a layer or in a scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Move,
    Down(MouseButton),
    Up(MouseButton),
}

/// A pointer event for [`Registrar::on_pointer`].
#[derive(Clone, Copy, Debug)]
pub struct PointerEvent {
    pub kind: PointerKind,
    /// Window coordinates.
    pub position: Point<Pixels>,
    /// The index of the declared region (see [`Registrar::overlay_regions`]); `None` in a scene, which takes the whole window.
    pub region: Option<usize>,
}

/// A key press while the extension's scene is up (see [`Registrar::on_key`]).
#[derive(Clone, Debug)]
pub struct KeyPress {
    /// The key as the toolkit names it ("a", "enter", "left", "space").
    pub key: String,
    /// The text the key types, when it types one.
    pub text: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// The longest the list keeps leaving rows painted (see [`Registrar::on_rows_leave`]).
pub const KEEP_ROWS_MAX_MS: u64 = 1500;

/// What a progress bar visual hook is told about the bar it may replace.
#[derive(Clone, Copy, Debug)]
pub struct ProgressInfo {
    /// The job's id (the one of the operation events).
    pub id: u64,
    pub kind: OpKind,
    /// 0 to 1, `None` while the total is not known.
    pub fraction: Option<f32>,
    pub bytes_per_sec: Option<u64>,
    /// The room the element gets, in pixels: it is clipped to it.
    pub width: f32,
    pub height: f32,
}

/// The height of the slot an extension's progress bar visual gets (the normal bar is 4 pixels).
pub const PROGRESS_SLOT_H: f32 = 20.;

/// What a row decoration or icon hook is told about a file row.
#[derive(Clone, Debug)]
pub struct RowInfo {
    pub path: PathBuf,
    pub is_dir: bool,
    pub selected: bool,
}

/// A pane docked at the right side of the workspace, like the preview pane, or (`left`) a block of `width` x height under the
/// sidebar's own groups.
pub struct SidePane {
    pub title: String,
    /// The width of a right pane; the height of a left one.
    pub width: f32,
    pub left: bool,
    pub render: Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>,
}

/// The size a Home widget is drawn in (the user picks it in Customize mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetSize {
    Small,
    Medium,
    Wide,
    Large,
}

type WidgetBody = Rc<dyn Fn(&mut App, WidgetSize, f32) -> Option<AnyElement>>;

struct HomeWidgetReg {
    id: String,
    title: String,
    icon: IconName,
    sizes: Vec<WidgetSize>,
    rows: u16,
    body: WidgetBody,
}

/// A full page with its own tab title and icon (like Settings or Storage usage).
struct PageReg {
    id: String,
    title: String,
    icon: IconName,
    render: Layer,
}

/// A section with its own entry in the list of Settings.
struct SettingsReg {
    title: String,
    icon: IconName,
    body: Rc<dyn Fn(&mut App) -> Vec<AnyElement>>,
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
    panes: Vec<SidePane>,
    pages: Vec<PageReg>,
    widgets: Vec<HomeWidgetReg>,
    settings: Option<SettingsReg>,
    commands: Vec<Command>,
    menus: Vec<(MenuTarget, Command)>,
    name_hook: Option<Rc<dyn Fn(&Path) -> Option<String>>>,
    row_hook: Option<Rc<dyn Fn(&RowInfo) -> Option<AnyElement>>>,
    icon_hook: Option<Rc<dyn Fn(&Path) -> Option<Arc<RenderImage>>>>,
    about: Option<Rc<dyn Fn(&mut App) -> AnyElement>>,
    progress_hook: Option<Rc<dyn Fn(&ProgressInfo) -> Option<AnyElement>>>,
    regions: Option<Rc<dyn Fn(&mut App) -> Vec<Bounds<Pixels>>>>,
    pointer_hook: Option<Rc<dyn Fn(&PointerEvent, &mut App)>>,
    key_hook: Option<Rc<dyn Fn(&KeyPress, &mut App)>>,
    leave_hook: Option<Rc<dyn Fn(&RowsLeaving, &mut App) -> Option<u64>>>,
    open_hook: Option<Rc<dyn Fn(&FolderOpening, &mut App)>>,
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
    /// Regions (window coordinates, asked on every draw) of the overlay that take the mouse: moves and clicks inside one go to
    /// [`Registrar::on_pointer`] and do not reach leaf. Everything outside the regions stays pointer-transparent.
    pub fn overlay_regions(&mut self, f: impl Fn(&mut App) -> Vec<Bounds<Pixels>> + 'static) {
        self.regions = Some(Rc::new(f));
    }
    /// Pointer moves and button presses inside the declared overlay regions, and anywhere in this extension's scene while it is up.
    pub fn on_pointer(&mut self, f: impl Fn(&PointerEvent, &mut App) + 'static) {
        self.pointer_hook = Some(Rc::new(f));
    }
    /// Key presses while this extension's scene is up; they do not reach leaf. Esc is not delivered: it always ends the scene.
    pub fn on_key(&mut self, f: impl Fn(&KeyPress, &mut App) + 'static) {
        self.key_hook = Some(Rc::new(f));
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
    /// A pane docked at the right of the workspace. It is shown after the command "Show or hide <title>" (added for it) is run. An
    /// extension may register several; they can be open together.
    pub fn side_pane(&mut self, title: &str, width: f32, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.panes.push(SidePane { title: title.to_string(), width: width.clamp(160., 640.), left: false, render: Rc::new(render) });
    }
    /// A pane docked at the left: a block of `height` (60 to 600 pixels) under the sidebar's own groups, as wide as the sidebar (not
    /// shown while the sidebar is a narrow rail). Toggled by "Show or hide <title>" like the right panes.
    pub fn side_pane_left(&mut self, title: &str, height: f32, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.panes.push(SidePane { title: title.to_string(), width: height.clamp(60., 600.), left: true, render: Rc::new(render) });
    }
    /// A full page with its own tab title and icon, opened by a command ("Open <title>" in the command search) or by
    /// [`open_page`]. `id` is unique inside the extension. The page fills the content area; leaf's own dialogs and menus come above it.
    pub fn page(&mut self, id: &str, title: &str, icon: IconName, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) {
        self.pages.push(PageReg { id: id.to_string(), title: title.to_string(), icon, render: Rc::new(render) });
    }
    /// A widget of the Home page, in the same table as leaf's own: it can be hidden, shown, moved and resized in Customize mode
    /// (and has a switch in Settings > Files and folders > Home page widgets). `body` gets the size and the usable width and may
    /// return `None` to show nothing for now. Off by default: the user adds it.
    pub fn home_widget(
        &mut self,
        id: &str,
        title: &str,
        icon: IconName,
        sizes: &[WidgetSize],
        rows: u16,
        body: impl Fn(&mut App, WidgetSize, f32) -> Option<AnyElement> + 'static,
    ) {
        self.widgets.push(HomeWidgetReg { id: id.to_string(), title: title.to_string(), icon, sizes: sizes.to_vec(), rows: rows.clamp(1, 6), body: Rc::new(body) });
    }
    /// Display only: the element drawn in place of a transfer's progress bar (in the Status Center cards), given the fraction, speed,
    /// kind and the room (`width` x [`PROGRESS_SLOT_H`], clipped). `None` keeps the normal bar. The job never sees it.
    pub fn progress_bar(&mut self, f: impl Fn(&ProgressInfo) -> Option<AnyElement> + 'static) {
        self.progress_hook = Some(Rc::new(f));
    }
    /// A section with its own entry in the list of Settings (not only a part of About).
    pub fn settings_section(&mut self, title: &str, icon: IconName, body: impl Fn(&mut App) -> Vec<AnyElement> + 'static) {
        self.settings = Some(SettingsReg { title: title.to_string(), icon, body: Rc::new(body) });
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
    /// Transition hook (visual only): rows are about to leave the list because they are deleted or moved away. The hook gets their
    /// names, kinds and on-screen rectangles and may play its own effect in an overlay. Returning `Some(ms)` asks the list to keep
    /// those rows painted, greyed and not clickable, for up to `ms` (at most [`KEEP_ROWS_MAX_MS`]) after they are gone, then it closes
    /// the gap. Rows of items that were not deleted stay as they are. Only the details list keeps rows (not the grid or columns).
    /// The operation itself starts at once and never waits.
    pub fn on_rows_leave(&mut self, f: impl Fn(&RowsLeaving, &mut App) -> Option<u64> + 'static) {
        self.leave_hook = Some(Rc::new(f));
    }
    /// Transition hook (visual only): a folder is being opened in a tab; the old and new place and the content area's rectangle.
    pub fn on_folder_open(&mut self, f: impl Fn(&FolderOpening, &mut App) + 'static) {
        self.open_hook = Some(Rc::new(f));
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
    /// The docked panes that are open: (extension, pane).
    panes_open: RefCell<Vec<(usize, usize)>>,
    scene: RefCell<Option<Scene>>,
    scene_gen: Cell<u64>,
    frames: RefCell<Vec<String>>,
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
    fn new(list: Vec<Entry>) -> Self {
        Self { list, panes_open: RefCell::new(Vec::new()), scene: RefCell::new(None), scene_gen: Cell::new(0), frames: RefCell::new(Vec::new()) }
    }
    /// The Home widget table rows of all extensions (ids are `ext:<extension>/<widget>`; the strings live as long as the program).
    fn home_widgets(&self) -> Vec<crate::home::Widget> {
        let mut out = Vec::new();
        for e in &self.list {
            for w in &e.r.widgets {
                let sizes: Vec<crate::home::Size> = w
                    .sizes
                    .iter()
                    .map(|z| match z {
                        WidgetSize::Small => crate::home::Size::Small,
                        WidgetSize::Medium => crate::home::Size::Medium,
                        WidgetSize::Wide => crate::home::Size::Wide,
                        WidgetSize::Large => crate::home::Size::Large,
                    })
                    .collect();
                let sizes: &'static [crate::home::Size] = Box::leak(if sizes.is_empty() { vec![crate::home::Size::Medium] } else { sizes }.into_boxed_slice());
                out.push(crate::home::Widget {
                    id: Box::leak(format!("ext:{}/{}", e.id, w.id).into_boxed_str()),
                    title: Box::leak(w.title.clone().into_boxed_str()),
                    icon: w.icon,
                    sizes,
                    default_size: sizes[0],
                    rows: w.rows,
                    folder: false,
                    render: crate::home::ext_widget,
                    header: None,
                    since: "",
                });
            }
        }
        out
    }
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
    let reg = Rc::new(Registry::new(ext.list));
    reg.refresh_flags();
    crate::home::add_ext_widgets(reg.home_widgets());
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

/// Events raised on other threads (an archive job's worker) wait here until the UI thread takes them with [`pump`].
static POSTED: std::sync::Mutex<Vec<Event>> = std::sync::Mutex::new(Vec::new());

/// Queues an event from any thread. Nothing is kept when no extension is registered.
pub(crate) fn post(ev: Event) {
    if ACTIVE.load(Ordering::Relaxed)
        && let Ok(mut q) = POSTED.lock()
        && q.len() < 256
    {
        q.push(ev);
    }
}

/// Delivers what [`post`] queued (called from the UI thread where jobs report).
pub(crate) fn pump(cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let list = POSTED.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
    for ev in list {
        emit(cx, || ev);
    }
}

thread_local! {
    static MINIMIZED: Cell<bool> = const { Cell::new(false) };
    static KIND_HINT: Cell<Option<OpKind>> = const { Cell::new(None) };
    static ANIM_UNTIL: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static LAST_FRAME: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static SPEED_SENT: RefCell<std::collections::HashMap<u64, std::time::Instant>> = RefCell::new(std::collections::HashMap::new());
    static SPACE: RefCell<std::collections::HashMap<PathBuf, (u64, u64)>> = RefCell::new(std::collections::HashMap::new());
}

/// Says what the job started next is (the Status Center reads it once). Jobs that note a transfer kind need no hint.
pub(crate) fn kind_hint(k: OpKind) {
    if ACTIVE.load(Ordering::Relaxed) {
        KIND_HINT.set(Some(k));
    }
}

pub(crate) fn take_kind() -> OpKind {
    if !ACTIVE.load(Ordering::Relaxed) {
        return OpKind::Other;
    }
    KIND_HINT.take().unwrap_or(OpKind::Other)
}

/// A transfer's speed, passed on at most twice a second per job.
pub(crate) fn note_speed(id: u64, bytes_per_sec: u64, cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let now = std::time::Instant::now();
    let due = SPEED_SENT.with(|m| {
        let mut m = m.borrow_mut();
        if m.get(&id).is_some_and(|t| now.duration_since(*t).as_millis() < 500) {
            return false;
        }
        m.insert(id, now);
        true
    });
    if due {
        emit(cx, || Event::TransferSpeed { id, bytes_per_sec });
    }
}

pub(crate) fn forget_speed(id: u64) {
    if ACTIVE.load(Ordering::Relaxed) {
        SPEED_SENT.with(|m| m.borrow_mut().remove(&id));
    }
}

/// Drives seen by the sidebar: an event for each whose space moved by at least 1 percent of its size (the first sight is silent).
pub(crate) fn note_drives(list: &[(PathBuf, u64, u64)], cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    for (root, free, total) in list {
        // the reference moves only when an event is sent, so a slow drift adds up to one
        let moved = SPACE.with(|m| {
            let mut m = m.borrow_mut();
            match m.get(root) {
                None => {
                    m.insert(root.clone(), (*free, *total));
                    false
                }
                Some(&(f, t)) if t != *total || f.abs_diff(*free) >= (*total / 100).max(1) => {
                    m.insert(root.clone(), (*free, *total));
                    true
                }
                Some(_) => false,
            }
        });
        if moved {
            let (root, free, total) = (root.clone(), *free, *total);
            emit(cx, || Event::DriveSpace { drive: root, free, total });
        }
    }
}

/// Called when a window's size or activation changed: an event when its minimised state differs from the last one seen.
pub(crate) fn note_minimized(window: &Window, cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let minimized = leaf_sys::window::is_minimized(crate::look::window::hwnd_of(window));
    if MINIMIZED.replace(minimized) != minimized {
        emit(cx, || Event::WindowMinimized { minimized });
    }
}

/// Keeps frames coming for `ms` milliseconds (at most a minute) without the extension running a loop of its own: every window draws
/// again each frame and [`Event::Frame`] is delivered. Frames stop by themselves when nothing asked for them. Calling it again extends.
pub fn animate(cx: &mut App, ms: u64) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(ms.min(60_000));
    if ANIM_UNTIL.get().is_none_or(|t| t < until) {
        ANIM_UNTIL.set(Some(until));
    }
    cx.refresh_windows();
}

/// Asks for a frame every refresh until `on` is false (for the extension `ext_id`): for an extension with a long-running effect. Prefer
/// [`animate`], which ends by itself.
pub fn want_frames(ext_id: &str, on: bool, cx: &mut App) {
    let Some(reg) = registry() else { return };
    let id = sanitize(ext_id);
    let mut w = reg.frames.borrow_mut();
    w.retain(|x| *x != id);
    if on {
        w.push(id);
    }
    drop(w);
    cx.refresh_windows();
}

/// Called while a window is drawn: asks for the next frame and delivers [`Event::Frame`] while something wants frames.
pub(crate) fn frame(window: &mut Window, cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    geo_new_frame();
    let now = std::time::Instant::now();
    let anim = ANIM_UNTIL.get().is_some_and(|t| t > now);
    let wanted = registry().is_some_and(|r| r.on().any(|(_, e)| r.frames.borrow().contains(&e.id)));
    if !anim && !wanted {
        ANIM_UNTIL.set(None);
        return;
    }
    window.request_animation_frame();
    let dt = LAST_FRAME.get().map_or(16, |t| now.duration_since(t).as_millis().min(1000) as u32);
    // (several windows draw in one frame: one event for all)
    if dt >= 4 || LAST_FRAME.get().is_none() {
        LAST_FRAME.set(Some(now));
        emit(cx, || Event::Frame { dt_ms: dt });
    }
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

// ------------------------------------------------------------------------------------------------------------------------- geometry

/// Where things were drawn, as the last frames saw them (window coordinates), for an extension that places an effect on something.
/// Recorded only while an extension is registered; an entry older than a frame is not given out.
#[derive(Default)]
struct Geo {
    frame: u64,
    rows: std::collections::HashMap<PathBuf, (u64, Bounds<Pixels>, FileKind, String)>,
    content: Option<(u64, Bounds<Pixels>)>,
    panes: std::collections::HashMap<usize, (u64, Bounds<Pixels>)>,
    cards: std::collections::HashMap<u64, (u64, Bounds<Pixels>)>,
    bars: std::collections::HashMap<u64, (u64, Bounds<Pixels>)>,
}

thread_local! {
    static GEO: RefCell<Geo> = RefCell::new(Geo::default());
}

/// What a probe records.
pub(crate) enum Spot {
    Row(PathBuf, FileKind, String),
    Content,
    Pane(usize),
    Card(u64),
    Bar(u64),
}

/// An invisible element that records its bounds under `spot` each time it is laid out. Put it as a child of a `relative` element.
pub(crate) fn probe(spot: Spot) -> AnyElement {
    canvas(
        move |bounds, _, _| {
            GEO.with(|g| {
                let mut g = g.borrow_mut();
                let f = g.frame;
                match &spot {
                    Spot::Row(p, k, n) => {
                        g.rows.insert(p.clone(), (f, bounds, *k, n.clone()));
                    }
                    Spot::Content => g.content = Some((f, bounds)),
                    Spot::Pane(i) => {
                        g.panes.insert(*i, (f, bounds));
                    }
                    Spot::Card(i) => {
                        g.cards.insert(*i, (f, bounds));
                    }
                    Spot::Bar(i) => {
                        g.bars.insert(*i, (f, bounds));
                    }
                }
            })
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// Called once per window draw: moves the frame counter on and drops what is not drawn any more.
fn geo_new_frame() {
    GEO.with(|g| {
        let mut g = g.borrow_mut();
        g.frame += 1;
        let f = g.frame;
        if g.rows.len() > 600 {
            g.rows.retain(|_, v| v.0 + 1 >= f);
        }
        g.cards.retain(|_, v| v.0 + 2 >= f);
        g.bars.retain(|_, v| v.0 + 2 >= f);
    });
}

fn fresh(g: &Geo, at: u64) -> bool {
    at + 1 >= g.frame
}

/// The rectangle (window coordinates) of the file row of `path`, when that row is on screen.
pub fn row_rect(path: &Path) -> Option<Bounds<Pixels>> {
    GEO.with(|g| {
        let g = g.borrow();
        g.rows.get(path).filter(|r| fresh(&g, r.0)).map(|r| r.1)
    })
}

/// The rectangle of the content area: the file panes together.
pub fn content_rect() -> Option<Bounds<Pixels>> {
    GEO.with(|g| {
        let g = g.borrow();
        g.content.filter(|c| fresh(&g, c.0)).map(|c| c.1)
    })
}

/// The rectangles of the file panes on screen, left to right (one without a split view).
pub fn pane_rects() -> Vec<Bounds<Pixels>> {
    GEO.with(|g| {
        let g = g.borrow();
        let mut v: Vec<Bounds<Pixels>> = g.panes.values().filter(|c| fresh(&g, c.0)).map(|c| c.1).collect();
        v.sort_by(|a, b| a.origin.x.partial_cmp(&b.origin.x).unwrap_or(std::cmp::Ordering::Equal));
        v
    })
}

/// The rectangle of the Status Center card of job `id` (the `id` of the operation events), while the card is shown.
pub fn card_rect(id: u64) -> Option<Bounds<Pixels>> {
    GEO.with(|g| {
        let g = g.borrow();
        g.cards.get(&id).filter(|c| fresh(&g, c.0)).map(|c| c.1)
    })
}

/// The rectangle of the progress bar of job `id` in its card.
pub fn progress_rect(id: u64) -> Option<Bounds<Pixels>> {
    GEO.with(|g| {
        let g = g.borrow();
        g.bars.get(&id).filter(|c| fresh(&g, c.0)).map(|c| c.1)
    })
}

/// The pointer's position in window coordinates.
pub fn pointer(window: &Window) -> Point<Pixels> {
    window.mouse_position()
}

/// A row recorded for the transition hooks: `(rect, name, kind)`.
pub(crate) fn row_geo(path: &Path) -> Option<(Bounds<Pixels>, String, FileKind)> {
    GEO.with(|g| {
        let g = g.borrow();
        g.rows.get(path).filter(|r| fresh(&g, r.0)).map(|r| (r.1, r.3.clone(), r.2))
    })
}

/// Plays a sound FILE (wav, ogg, mp3, flac and the other audio files the preview plays) at `volume` (0 to 1), through an audio output
/// opened once and kept for the session. Sounds overlap: each call adds a voice to one mixer. It plays nothing while "Sounds from
/// extensions" (Settings > About > Extensions) is off, for a file that cannot be decoded (logged), for one longer than
/// [`SOUND_MAX_SECS`], and while the system is muted (the output is the system's shared one, so its mute applies). Returns at
/// once; decoding and mixing run on a thread of their own that waits without polling while nothing plays.
pub fn play_sound_file(cx: &App, path: impl Into<PathBuf>, volume: f32) {
    if !ACTIVE.load(Ordering::Relaxed) || !cx.global::<crate::settings::Settings>().ext_sounds {
        return;
    }
    mixer::play(path.into(), volume.clamp(0., 1.));
}

/// The longest sound file that is played.
pub const SOUND_MAX_SECS: u32 = 30;

#[cfg(windows)]
mod mixer {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock, mpsc};

    struct Req {
        path: PathBuf,
        volume: f32,
    }

    struct Voice {
        /// Interleaved at the output's rate and channel count.
        data: Vec<f32>,
        pos: usize,
        volume: f32,
    }

    static TX: OnceLock<Mutex<mpsc::Sender<Req>>> = OnceLock::new();

    pub(super) fn play(path: PathBuf, volume: f32) {
        let tx = TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            if let Err(e) = std::thread::Builder::new().name("leaf-ext-sound".into()).spawn(move || run(rx)) {
                log::warn!("extension sound: no thread: {e}");
            }
            Mutex::new(tx)
        });
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(Req { path, volume });
        }
    }

    fn decode(req: &Req, rate: u32, ch: usize) -> Option<Voice> {
        let media = rf_player::open(&req.path).map_err(|e| log::warn!("extension sound {}: {e:#}", req.path.display())).ok()?;
        let from = media.info().audio()?.channels as usize;
        let secs = media.duration().clamp(0., f64::from(super::SOUND_MAX_SECS));
        let frames = (secs * f64::from(rate)) as usize;
        if frames == 0 {
            return None;
        }
        let raw = media.audio(0, frames, rate).map_err(|e| log::warn!("extension sound {}: {e:#}", req.path.display())).ok()?;
        Some(Voice { data: rf_player::map_channels(&raw, from, ch), pos: 0, volume: req.volume })
    }

    fn run(rx: mpsc::Receiver<Req>) {
        let mut sink: Option<Box<dyn rf_player::AudioSink>> = None;
        let mut voices: Vec<Voice> = Vec::new();
        let mut pending: Vec<Req> = Vec::new();
        loop {
            // nothing playing: wait for a request (no polling)
            if voices.is_empty() && pending.is_empty() {
                match rx.recv() {
                    Ok(r) => pending.push(r),
                    Err(_) => return,
                }
            }
            while let Ok(r) = rx.try_recv() {
                pending.push(r);
            }
            if sink.is_none() {
                match rf_player::wasapi_sink() {
                    Ok(s) => sink = Some(s),
                    Err(e) => {
                        log::warn!("extension sound: no audio output: {e:#}");
                        pending.clear();
                        continue;
                    }
                }
            }
            let Some(out) = sink.as_mut() else { continue };
            let (rate, ch) = (out.sample_rate(), out.channels() as usize);
            for r in pending.drain(..) {
                if voices.len() < 16
                    && let Some(v) = decode(&r, rate, ch)
                {
                    voices.push(v);
                }
            }
            if voices.is_empty() {
                continue;
            }
            out.pause(false);
            const CHUNK: usize = 1024;
            while !voices.is_empty() {
                let mut buf = vec![0f32; CHUNK * ch];
                for v in &mut voices {
                    let n = (v.data.len() - v.pos).min(buf.len());
                    for (o, i) in buf.iter_mut().zip(&v.data[v.pos..v.pos + n]) {
                        *o += i * v.volume;
                    }
                    v.pos += n;
                }
                voices.retain(|v| v.pos < v.data.len());
                for x in &mut buf {
                    *x = x.clamp(-1., 1.);
                }
                let mut off = 0;
                while off < buf.len() {
                    let w = out.write(&buf[off..]);
                    off += w * ch;
                    if w == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                while let Ok(r) = rx.try_recv() {
                    pending.push(r);
                }
                for r in pending.drain(..) {
                    if voices.len() < 16
                        && let Some(v) = decode(&r, rate, ch)
                    {
                        voices.push(v);
                    }
                }
            }
            // the queued tail (the output holds about 100 ms) plays out, then the output rests
            std::thread::sleep(std::time::Duration::from_millis(160));
            out.pause(true);
        }
    }
}

#[cfg(not(windows))]
mod mixer {
    pub(super) fn play(_path: std::path::PathBuf, _volume: f32) {}
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
    let mut out = layer_of(|r| r.overlay.clone(), window, cx);
    // the regions that take the mouse (one flag check while no extension declared any)
    let Some(reg) = registry() else { return out };
    for (ix, e) in reg.on() {
        let (Some(regions), Some(_)) = (&e.r.regions, &e.r.pointer_hook) else { continue };
        let Some(list) = guard(&e.id, || regions(cx)) else { continue };
        for (ri, b) in list.into_iter().enumerate() {
            let region = Some(ri);
            out.push(
                div()
                    .id(SharedString::from(format!("ext-region-{}-{ri}", e.id)))
                    .absolute()
                    .left(b.origin.x)
                    .top(b.origin.y)
                    .w(b.size.width)
                    .h(b.size.height)
                    .occlude()
                    .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| call_pointer(ix, PointerKind::Move, ev.position, region, cx))
                    .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| call_pointer(ix, PointerKind::Down(ev.button), ev.position, region, cx))
                    .on_mouse_down(MouseButton::Right, move |ev: &MouseDownEvent, _, cx| call_pointer(ix, PointerKind::Down(ev.button), ev.position, region, cx))
                    .on_mouse_up(MouseButton::Left, move |ev: &MouseUpEvent, _, cx| call_pointer(ix, PointerKind::Up(ev.button), ev.position, region, cx))
                    .on_mouse_up(MouseButton::Right, move |ev: &MouseUpEvent, _, cx| call_pointer(ix, PointerKind::Up(ev.button), ev.position, region, cx))
                    .into_any_element(),
            );
        }
    }
    out
}

fn call_pointer(ix: usize, kind: PointerKind, position: Point<Pixels>, region: Option<usize>, cx: &mut App) {
    let Some(reg) = registry() else { return };
    let Some(e) = reg.list.get(ix).filter(|e| !e.off.get()) else { return };
    if let Some(f) = e.r.pointer_hook.clone() {
        let ev = PointerEvent { kind, position, region };
        guard(&e.id, || f(&ev, cx));
    }
}

/// A key press while a scene is up: handed to the scene's extension. True when it was taken (it then goes no further).
pub(crate) fn scene_key(ev: &KeyDownEvent, cx: &mut App) -> bool {
    let Some(reg) = registry() else { return false };
    let Some(ix) = reg.scene.borrow().as_ref().map(|s| s.ext) else { return false };
    let Some(e) = reg.list.get(ix).filter(|e| !e.off.get()) else { return false };
    let Some(f) = e.r.key_hook.clone() else { return false };
    let k = &ev.keystroke;
    let press = KeyPress { key: k.key.to_string(), text: k.key_char.clone(), ctrl: k.modifiers.control, alt: k.modifiers.alt, shift: k.modifiers.shift };
    guard(&e.id, || f(&press, cx));
    true
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

/// The right panes that are open, in the order they were opened.
pub(crate) fn side_panes(p: &crate::theme::Palette, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let open = reg.panes_open.borrow().clone();
    let mut out = Vec::new();
    for (ei, pi) in open {
        let Some(e) = reg.list.get(ei).filter(|e| !e.off.get()) else { continue };
        let Some(pane) = e.r.panes.get(pi).filter(|p| !p.left) else { continue };
        let Some(el) = guard(&e.id, || (pane.render)(window, cx)) else { continue };
        out.push(
            crate::theme::sheet(p, p.content_bg)
                .w(px(pane.width))
                .flex_shrink_0()
                .h_full()
                .overflow_hidden()
                .when(p.gap <= 0., |d| d.border_l_1().border_color(p.sheet_border))
                .child(el)
                .into_any_element(),
        );
    }
    out
}

/// The left panes that are open: blocks under the sidebar's groups.
pub(crate) fn left_panes(p: &crate::theme::Palette, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let open = reg.panes_open.borrow().clone();
    let mut out = Vec::new();
    for (ei, pi) in open {
        let Some(e) = reg.list.get(ei).filter(|e| !e.off.get()) else { continue };
        let Some(pane) = e.r.panes.get(pi).filter(|p| p.left) else { continue };
        let Some(el) = guard(&e.id, || (pane.render)(window, cx)) else { continue };
        out.push(div().flex_shrink_0().w_full().h(px(pane.width)).mt_2().overflow_hidden().border_t_1().border_color(p.divider).child(el).into_any_element());
    }
    out
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
    let mut scene = div().id("ext-scene").absolute().inset_0().occlude().child(el);
    if e.r.pointer_hook.is_some() {
        scene = scene
            .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| call_pointer(ix, PointerKind::Move, ev.position, None, cx))
            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| call_pointer(ix, PointerKind::Down(ev.button), ev.position, None, cx))
            .on_mouse_down(MouseButton::Right, move |ev: &MouseDownEvent, _, cx| call_pointer(ix, PointerKind::Down(ev.button), ev.position, None, cx))
            .on_mouse_up(MouseButton::Left, move |ev: &MouseUpEvent, _, cx| call_pointer(ix, PointerKind::Up(ev.button), ev.position, None, cx))
            .on_mouse_up(MouseButton::Right, move |ev: &MouseUpEvent, _, cx| call_pointer(ix, PointerKind::Up(ev.button), ev.position, None, cx));
    }
    Some(scene.into_any_element())
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
        for (i, p) in e.r.panes.iter().enumerate() {
            out.push((full_id(&e.id, &format!("pane:{i}")), format!("{} {}", crate::t!("Show or hide"), p.title), if p.left { IconName::PanelLeft } else { IconName::PanelRight }, String::new()));
        }
        for pg in &e.r.pages {
            out.push((full_id(&e.id, &format!("page:{}", pg.id)), format!("{} {}", crate::t!("Open"), pg.title), pg.icon, String::new()));
        }
    }
    out
}

/// Runs a command by its full id. A page command answers with the place to open (the caller opens it in a tab).
pub(crate) fn run_command(id: &str, ctx: &CmdCtx, cx: &mut App) -> Option<rf_core::Location> {
    let reg = registry()?;
    for (ix, e) in reg.on() {
        let Some(rest) = id.strip_prefix(e.id.as_str()).and_then(|r| r.strip_prefix('/')) else { continue };
        if let Some(pi) = rest.strip_prefix("pane:").and_then(|n| n.parse::<usize>().ok())
            && pi < e.r.panes.len()
        {
            let mut open = reg.panes_open.borrow_mut();
            match open.iter().position(|&x| x == (ix, pi)) {
                Some(at) => {
                    open.remove(at);
                }
                None => open.push((ix, pi)),
            }
            drop(open);
            cx.refresh_windows();
            return None;
        }
        if let Some(pg) = rest.strip_prefix("page:").and_then(|n| e.r.pages.iter().find(|p| p.id == n)) {
            return Some(rf_core::Location::Ext { ext: e.id.clone(), page: pg.id.clone(), title: pg.title.clone() });
        }
        if let Some(c) = e.r.commands.iter().find(|c| c.id == rest) {
            let run = c.run.clone();
            guard(&e.id, || run(ctx, cx));
            cx.refresh_windows();
        }
        return None;
    }
    None
}

/// Opens page `page` of extension `ext_id` in a tab of the active window (the page registered with [`Registrar::page`]).
pub fn open_page(ext_id: &str, page: &str, cx: &mut App) {
    let Some(win) = cx.active_window() else { return };
    let action = RunExtCommand { id: full_id(&sanitize(ext_id), &format!("page:{page}")) };
    let _ = win.update(cx, |_, window, cx| window.dispatch_action(Box::new(action), cx));
}

/// The body of an extension's page, for the view that shows the place `Location::Ext`.
pub(crate) fn page_element(ext: &str, page: &str, window: &mut Window, cx: &mut App) -> AnyElement {
    let missing = || div().p_6().text_sm().text_color(theme().text_secondary).child(crate::t!("This page is not available (its extension is off).")).into_any_element();
    let Some(reg) = registry() else { return missing() };
    let Some(e) = reg.on().map(|(_, e)| e).find(|e| e.id == ext) else { return missing() };
    let Some(pg) = e.r.pages.iter().find(|p| p.id == page) else { return missing() };
    let render = pg.render.clone();
    guard(&e.id, || render(window, cx)).unwrap_or_else(missing)
}

/// The extension's picture for a progress bar, when one is registered and gives one.
pub(crate) fn progress_visual(id: u64, kind: OpKind, fraction: Option<f32>, bytes_per_sec: Option<u64>, width: f32) -> Option<AnyElement> {
    let reg = registry()?;
    let info = ProgressInfo { id, kind, fraction, bytes_per_sec, width, height: PROGRESS_SLOT_H };
    reg.on().find_map(|(_, e)| e.r.progress_hook.as_ref().and_then(|f| guard(&e.id, || f(&info)).flatten()))
}

/// The icon of an extension page's tab.
pub(crate) fn page_icon(ext: &str, page: &str) -> IconName {
    registry().and_then(|r| r.list.iter().find(|e| e.id == ext).and_then(|e| e.r.pages.iter().find(|p| p.id == page).map(|p| p.icon))).unwrap_or(IconName::Puzzle)
}

/// A Home widget's body: its id is `ext:<extension>/<widget>`.
pub(crate) fn home_widget_body(id: &str, size: crate::home::Size, width: f32, cx: &mut App) -> Option<AnyElement> {
    let reg = registry()?;
    let rest = id.strip_prefix("ext:")?;
    let (eid, wid) = rest.split_once('/')?;
    let e = reg.on().map(|(_, e)| e).find(|e| e.id == eid)?;
    let w = e.r.widgets.iter().find(|w| w.id == wid)?;
    let size = match size {
        crate::home::Size::Small => WidgetSize::Small,
        crate::home::Size::Medium => WidgetSize::Medium,
        crate::home::Size::Wide => WidgetSize::Wide,
        crate::home::Size::Large => WidgetSize::Large,
    };
    let body = w.body.clone();
    guard(&e.id, || body(cx, size, width)).flatten()
}

/// The extensions' settings sections: `(extension index, title, icon)` for the list of Settings (switched-off ones left out).
pub(crate) fn settings_entries() -> Vec<(usize, String, IconName)> {
    let Some(reg) = registry() else { return Vec::new() };
    reg.on().filter_map(|(ix, e)| e.r.settings.as_ref().map(|s| (ix, s.title.clone(), s.icon))).collect()
}

/// The cards of settings section `ix`.
pub(crate) fn settings_body(ix: usize, cx: &mut App) -> Vec<AnyElement> {
    let Some(reg) = registry() else { return Vec::new() };
    let Some(e) = reg.list.get(ix).filter(|e| !e.off.get()) else { return Vec::new() };
    let Some(s) = &e.r.settings else { return Vec::new() };
    let body = s.body.clone();
    guard(&e.id, || body(cx)).unwrap_or_default()
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

// ------------------------------------------------------------------------------------------------------------------------- transitions

thread_local! {
    /// Rows asked to stay painted after they left the list: path -> until.
    static KEEP: RefCell<std::collections::HashMap<PathBuf, std::time::Instant>> = RefCell::new(std::collections::HashMap::new());
}

/// Runs the "rows leave" hooks for `paths` (in folder `dir`). Called on the UI thread just before the job starts.
pub(crate) fn rows_leaving(cause: LeaveCause, dir: Option<PathBuf>, paths: &[PathBuf], cx: &mut App) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let Some(reg) = registry() else { return };
    if !reg.on().any(|(_, e)| e.r.leave_hook.is_some()) {
        return;
    }
    let rows: Vec<LeavingRow> = paths
        .iter()
        .take(200)
        .map(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match row_geo(p) {
                Some((rect, name, kind)) => LeavingRow { path: p.clone(), name, kind, rect: Some(rect) },
                None => LeavingRow { path: p.clone(), kind: FileKind::of_name(&name, false), name, rect: None },
            }
        })
        .collect();
    let info = RowsLeaving { cause, dir, rows };
    let mut keep = 0u64;
    for (_, e) in reg.on() {
        if let Some(f) = &e.r.leave_hook
            && let Some(Some(ms)) = guard(&e.id, || f(&info, cx))
        {
            keep = keep.max(ms);
        }
    }
    let keep = keep.min(KEEP_ROWS_MAX_MS);
    if keep > 0 {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(keep);
        KEEP.with(|k| {
            let mut k = k.borrow_mut();
            let now = std::time::Instant::now();
            k.retain(|_, t| *t > now);
            for p in paths {
                k.insert(p.clone(), until);
            }
        });
    }
}

/// How long the row for `path` is still to be kept painted after it left the list (`None`: not at all).
pub(crate) fn keep_for(path: &Path) -> Option<std::time::Duration> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    KEEP.with(|k| k.borrow().get(path).and_then(|t| t.checked_duration_since(std::time::Instant::now())))
}

/// Whether any row is waiting to be kept (a cheap check before the list looks).
pub(crate) fn keeping_rows() -> bool {
    ACTIVE.load(Ordering::Relaxed) && KEEP.with(|k| !k.borrow().is_empty())
}

/// Runs the "a folder opens" hooks.
pub(crate) fn folder_opening(from: Option<PathBuf>, to: Option<PathBuf>, cx: &mut App) {
    let Some(reg) = registry() else { return };
    let info = FolderOpening { from, to, content: content_rect() };
    for (_, e) in reg.on() {
        if let Some(f) = &e.r.open_hook {
            let f = f.clone();
            guard(&e.id, || f(&info, cx));
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------------- About

/// Settings > About: "Extensions: none", or one row per extension with a switch for this session.
pub(crate) fn about_rows(p: &crate::theme::Palette, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings_view::{card, section, switch};
    let Some(reg) = registry() else {
        return vec![div().px_1().text_xs().text_color(p.text_secondary).child(format!("{}: {}", crate::t!("Extensions"), crate::t!("none"))).into_any_element()];
    };
    let mut out = vec![section(p, crate::t!("Extensions"))];
    {
        let on = cx.global::<crate::settings::Settings>().ext_sounds;
        out.push(card(
            p,
            IconName::Volume2,
            &crate::t!("Sounds from extensions"),
            Some(&crate::t!("Lets extensions play sound files. Off: they stay silent.")),
            switch("ext-sounds", p, on, move |_, cx| crate::settings::update(cx, |s| s.ext_sounds = !on)),
        ));
    }
    for (ix, e) in reg.list.iter().enumerate() {
        let (r2, pp) = (reg.clone(), *p);
        let on = !e.off.get();
        let sw = switch(SharedString::from(format!("ext-sw-{}", e.id)), &pp, on, move |_, cx| {
            if let Some(e) = r2.list.get(ix) {
                e.off.set(!e.off.get());
            }
            r2.refresh_flags();
            if r2.list[ix].off.get() {
                r2.panes_open.borrow_mut().retain(|(e, _)| *e != ix);
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
    // (named imports: the toolkit's glob brings a `test` of its own that would replace the standard `#[test]`)
    use super::{ACTIVE, Cell, Extensions, NAME_HOOK, Path, REG, RefCell, Registry, Rc, Ordering, display_name};

    /// A display-name hook changes what is shown and nothing else: the path it is asked about is the real one and the real name
    /// stays what `Path` says.
    #[test]
    fn display_name_hook_changes_only_what_is_shown() {
        let mut ext = Extensions::new();
        ext = ext.add("t", "T", |e| e.display_name(|p| p.file_name().map(|n| format!("shown {}", n.to_string_lossy()))));
        let reg = Rc::new(Registry::new(ext.list));
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

// ------------------------------------------------------------------------------------------------------------------------- peer channel

/// The side channel to a linked friend's leaf (`docs/extensions.md`, "Peer channel"): small typed messages over the link a shared
/// folder already has. Only between friends already linked, only when the user allowed extension messages (a global switch and one
/// per friend, both off by default), at most a few a second, at most [`peer::MAX_PAYLOAD`] bytes. A message is bytes for the
/// extension and nothing more: it cannot touch files or settings.
pub mod peer {
    pub use leaf_share::ext_msg::{BURST, MAX_PAYLOAD, PER_SEC};

    /// One friend a folder is shared with.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Friend {
        /// The friend's device id: stable across restarts and the same for every folder shared with them. It is the id of
        /// [`super::Event::PeerMessage`], [`super::Event::FriendLink`] and [`send`].
        pub id: String,
        /// The name the friend gave their PC.
        pub name: String,
        /// The link to them is up now.
        pub online: bool,
        /// Their leaf said in its hello that it understands extension messages (an older leaf does not; known while online).
        pub can_receive: bool,
    }

    /// The friends folders are shared with (one entry each, whether or not they are online).
    pub fn friends(cx: &gpui_kit::App) -> Vec<Friend> {
        crate::share_ui::connected_friends(cx).into_iter().map(|(id, name, online, can_receive)| Friend { id, name, online, can_receive }).collect()
    }

    /// Whether the link to the friend with this id is up.
    pub fn online(cx: &gpui_kit::App, friend: &str) -> bool {
        crate::share_ui::connected_friends(cx).iter().any(|f| f.0 == friend && f.2)
    }

    /// Sends one message to a friend (see the limits above). Fails, without queueing, when the friend is not connected, their leaf does
    /// not take extension messages, the payload is too long or messages go out too often.
    pub fn send(cx: &gpui_kit::App, friend: &str, kind: u16, payload: &[u8]) -> Result<(), String> {
        crate::share_ui::send_ext(cx, friend, kind, payload)
    }
}
