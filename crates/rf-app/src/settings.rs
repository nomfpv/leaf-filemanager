//! Persistent user settings (%APPDATA%\leaf\settings.toml) as a GPUI global.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui_kit::*;
use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
    /// Pure black for OLED screens: black surfaces, white text, no see-through material (issue 45: "A true black background
    /// (#000000). Pure white text (#FFFFFF)").
    Black,
}

/// Settings > Appearance > Visibility: the calmer Standard (the default; the owner, 2026-11-21: it looks better), or the same theme with
/// tighter text, stronger lines and a fuller selection (High).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    High,
    #[default]
    Standard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DefaultView {
    Details,
    Grid,
    Columns,
    /// Picks per folder, like Explorer's folder types: mostly pictures/videos → grid, Pictures
    /// and Videos → grid, Downloads → newest first in date groups, otherwise details.
    #[default]
    Adaptive,
}

/// Checking copies after they are written (our engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CopyVerify {
    #[default]
    Off,
    /// Size and modification time.
    Quick,
    /// Every byte, read back from the disk.
    Full,
}

impl CopyVerify {
    pub fn engine(self) -> leaf_sys::fastcopy::Verify {
        match self {
            CopyVerify::Off => leaf_sys::fastcopy::Verify::Off,
            CopyVerify::Quick => leaf_sys::fastcopy::Verify::Quick,
            CopyVerify::Full => leaf_sys::fastcopy::Verify::Full,
        }
    }
}

/// Where the progress of a running copy, move or delete is shown besides the Status Center
/// (whose button always has a progress ring).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransferProgress {
    /// A strip in the status bar of every window.
    #[default]
    StatusBar,
    /// A card at the bottom right corner of the window's content.
    #[serde(rename = "card")]
    FloatingCard,
    /// Only in the Status Center (how it was before).
    #[serde(rename = "statuscenter")]
    StatusCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DatePref {
    /// "5 dakika önce" for recent items.
    #[default]
    Application,
    System,
    Universal,
}

/// A window's tabs saved under a name, to open again later in a window of their own (issue 41: "a feature to save or load sessions
/// (similar to the Lister feature in Directory Opus)"). Written as the last session is: `session_tabs`, `session_panes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NamedSession {
    pub name: String,
    pub tabs: Vec<String>,
    pub panes: Vec<SessionPane>,
    pub active: usize,
}

/// The second pane of a tab in the saved session (issue 20: a window split in two came back with one pane, at the left pane's
/// folder; only the left pane of each tab was written).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPane {
    /// The tab it belongs to: an index into `session_tabs`.
    pub tab: usize,
    /// Where it was, written the way a tab of `session_tabs` is ("" = Home).
    pub location: String,
    /// It was the pane with the focus (the toolbar, the sidebar and the status bar acted on it).
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StartupPref {
    /// Reopen last session's tabs.
    #[default]
    Continue,
    /// One Home tab.
    NewTab,
    /// The folders in `startup_pages`.
    Pages,
}

/// Settings → Appearance → Animations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnimPref {
    /// Follow Windows (Settings → Accessibility → Visual effects → Animation effects).
    #[default]
    System,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackdropPref {
    /// Default: the material the theme in use is made for (its `material` key: Canopy Acrylic, Stem and Index Mica under their
    /// opaque surfaces). An older version does not know this value and reads it as Mica (see `parse_lenient`).
    #[default]
    Theme,
    /// Like Explorer: a quiet desktop tint.
    Mica,
    /// Files' default: a more colorful variant of Mica.
    MicaAlt,
    Acrylic,
    Solid,
    /// Experimental: the screen behind the window, bent and frosted like glass (leaf_sys::glass).
    /// An older version does not know this value and reads the field as the default (Mica);
    /// the other settings are kept (see `parse_lenient`).
    Glass,
}

/// Hızlı Bakış penceresinin arka planı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LookBackdrop {
    #[default]
    Mica,
    Acrylic,
    Tabbed,
    /// Düz renk (tema rengi ya da `look_backdrop_color`).
    None,
}

/// Hızlı Bakış resim görüntüleyicisinin zemini.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LookImageTheme {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeletePref {
    Always,
    /// Only for permanent deletes (the Recycle Bin can undo the rest) — Explorer's behaviour.
    #[default]
    PermanentOnly,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClickPref {
    #[default]
    Never,
    Folders,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum IdePref {
    #[default]
    GitRepos,
    Always,
    Never,
}

/// Which of our own entries the context menu shows (Settings → General → Context menu).
/// Where an archive opens when it is opened like a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveOpen {
    /// In the same tab, as a folder does.
    #[default]
    Here,
    NewTab,
    NewWindow,
}

/// The format a click on "Sıkıştır" compresses in (Discord, 2026-11-20: "a default zipping method setting in settings").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuickCompress {
    /// A click only opens the submenu, as before.
    Menu,
    #[default]
    Zip,
    SevenZ,
    TarZst,
    TarGz,
    Tar,
    /// WinRAR's format, through its Rar.exe (only when WinRAR is installed).
    Rar,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuPrefs {
    pub open_in_new_tab: bool,
    pub open_in_new_window: bool,
    pub copy_path: bool,
    pub create_shortcut: bool,
    pub pin: bool,
    pub compress: bool,
    pub tags: bool,
    pub shelf: bool,
    pub terminal: bool,
    /// Explorer's "Send to" (the SendTo folder's entries and the drives) in the first menu. Issue 117.
    pub send_to: bool,
    /// Shell extensions under "Show more options" (off = listed at the bottom of the menu).
    pub shell_submenu: bool,
    /// Switches of rows added later (`menu_model::Switch::key`): the ones with no field of their own. Missing: on.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub more: std::collections::BTreeMap<String, bool>,
}

impl MenuPrefs {
    /// Whether the switch `key` of Settings > Right-click menu is on.
    pub fn get(&self, key: &str) -> bool {
        match key {
            "open_in_new_tab" => self.open_in_new_tab,
            "open_in_new_window" => self.open_in_new_window,
            "copy_path" => self.copy_path,
            "create_shortcut" => self.create_shortcut,
            "pin" => self.pin,
            "compress" => self.compress,
            "tags" => self.tags,
            "shelf" => self.shelf,
            "terminal" => self.terminal,
            "send_to" => self.send_to,
            "shell_submenu" => self.shell_submenu,
            k => self.more.get(k).copied().unwrap_or(true),
        }
    }

    pub fn set(&mut self, key: &str, on: bool) {
        match key {
            "open_in_new_tab" => self.open_in_new_tab = on,
            "open_in_new_window" => self.open_in_new_window = on,
            "copy_path" => self.copy_path = on,
            "create_shortcut" => self.create_shortcut = on,
            "pin" => self.pin = on,
            "compress" => self.compress = on,
            "tags" => self.tags = on,
            "shelf" => self.shelf = on,
            "terminal" => self.terminal = on,
            "send_to" => self.send_to = on,
            "shell_submenu" => self.shell_submenu = on,
            k => {
                self.more.insert(k.to_string(), on);
            }
        }
    }
}

impl Default for MenuPrefs {
    fn default() -> Self {
        Self {
            open_in_new_tab: true,
            open_in_new_window: true,
            copy_path: true,
            create_shortcut: true,
            pin: true,
            compress: true,
            tags: true,
            shelf: true,
            terminal: true,
            send_to: true,
            shell_submenu: true,
            more: Default::default(),
        }
    }
}

/// Details-view columns that can be hidden (name is always shown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColumnPrefs {
    pub modified: bool,
    pub created: bool,
    pub kind: bool,
    pub size: bool,
    /// Widths (px) of modified, created, type, size; dragged in the header.
    pub widths: [u16; 4],
    /// Files' "File tag", "Git status" and "Sync status" columns. Git shows only inside a
    /// repository, sync only in folders with cloud files.
    pub tags: bool,
    pub git: bool,
    pub sync: bool,
    /// Widths of tags, git, sync.
    pub extra_widths: [u16; 3],
    /// Files' commit columns (inside a repository): last commit message, author, date, hash.
    pub git_message: bool,
    pub git_author: bool,
    pub git_date: bool,
    pub git_hash: bool,
    pub git_widths: [u16; 4],
    /// Unix permissions and owner (`rwxr-xr-x  user`): local Linux paths and SFTP hosts; nothing on a Windows path.
    pub permissions: bool,
    /// Columns for Windows properties the user added ("Diğer sütunlar…"): `System.Media.Duration:140`
    /// (canonical name, width), in the order they show after the fixed columns. Empty = none, and
    /// then nothing of the property columns runs and the file has no such key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<String>,
    /// The Name column's width once a divider was dragged (issue 135: dividers work as in Explorer, the columns stand from the left
    /// and the room left over is at the right end). 0: the Name column takes whatever the others leave, as before any drag.
    #[serde(skip_serializing_if = "is_zero_u16")]
    pub name_width: u16,
}

fn is_hundred(v: &u16) -> bool {
    *v == 100
}

/// The base size (rem) for a text size of `percent`, within 75 % and 125 % of the usual 16 px.
pub fn rem_for(percent: u16) -> f32 {
    16.0 * f32::from(percent.clamp(75, 125)) / 100.0
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

impl Default for ColumnPrefs {
    fn default() -> Self {
        Self {
            modified: true,
            created: false,
            kind: true,
            size: true,
            widths: [150, 150, 150, 100],
            // (tags show as dots after the names, the mockups'; the column is one click in the header's menu)
            tags: false,
            git: true,
            sync: true,
            extra_widths: [130, 120, 150],
            git_message: false,
            git_author: false,
            git_date: false,
            git_hash: false,
            git_widths: [240, 130, 140, 90],
            permissions: false,
            properties: Vec::new(),
            name_width: 0,
        }
    }
}

/// Where the quick search's "search the web" row sends the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebSearch {
    #[default]
    Google,
    Bing,
    DuckDuckGo,
    Yandex,
}

/// The renderer mode a process runs in. The dock (a process of its own, three small windows) is kept on the graphics card when the
/// setting is Auto: under Auto a card that another program keeps busy (a video render) makes the renderer build a software device and
/// move the islands to it, which costs seconds of CPU for windows that are a few hundred pixels wide. A user who chose the software
/// renderer on purpose keeps it.
pub(crate) fn renderer_mode(pref: RendererPref, dock: bool) -> crate::platform::gpui::hybrid::RendererMode {
    use crate::platform::gpui::hybrid::RendererMode;
    match pref {
        RendererPref::Auto if dock => RendererMode::Gpu,
        RendererPref::Auto => RendererMode::Auto,
        RendererPref::Gpu => RendererMode::Gpu,
        RendererPref::Cpu => RendererMode::Cpu,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RendererPref {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

fn is_default_preview_height(h: &f32) -> bool {
    *h == rf_core::previewpane::BOTTOM_DEFAULT_H
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The mode of the theme: system, light, dark or pure black.
    pub theme: ThemePref,
    /// The theme (Settings > Appearance): "canopy" (the default), "stem", "index", or a user theme by its file name.
    pub theme_name: String,
    pub visibility: Visibility,
    /// The accent: a name from the theme's set ("" = its first), or "windows" (Windows' own). `accent_color`, when set, wins.
    pub accent: String,
    pub default_view: DefaultView,
    pub show_hidden: bool,
    /// Inside a git repository the list leaves out what `.gitignore` (and the other git excludes) ignores.
    pub hide_ignored: bool,
    pub preview_open: bool,
    pub sidebar_open: bool,
    /// The sidebar as a narrow strip of icons (drag it narrow to get there).
    pub sidebar_rail: bool,
    /// Pane widths (drag the pane edge to resize).
    pub sidebar_width: f32,
    /// Sidebar sections the user turned off ("drives", "cloud", "libraries", "network", "tags").
    pub sidebar_hidden: Vec<String>,
    /// Before the Home page had widgets: its sections that were hidden ("quick", "drives", "devices", "tags", "network", "recent").
    /// Read once into `home_layout` and emptied (`home::migrate`).
    pub home_hidden: Vec<String>,
    /// The Home page's widgets in order, with their sizes; not written: the default set (`home::DEFAULT`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_layout: Option<Vec<crate::home::Slot>>,
    /// "Clear" on Home's recent items: only items used after this moment are listed (Windows'
    /// own list of recent items is left alone).
    pub recent_cleared_ms: i64,
    /// The last searches run with Enter (newest first, ten at most): offered under the empty
    /// search box.
    pub search_history: Vec<String>,
    /// Buttons of the command bar that are hidden ("new", "cut", "copy", "paste", "rename",
    /// "share", "delete", "props", "filter", "sort", "view", "preview").
    pub toolbar_hidden: Vec<String>,
    /// Buttons of the command bar that are off unless asked for ("newfolder", "terminal",
    /// "hidden", "refresh").
    pub toolbar_extra: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub toolbar_commands: Vec<crate::quick_commands::QuickCommand>,
    pub preview_width: f32,
    /// Where the preview pane sits: at the right of the list (the default) or under it. The key is written only for "bottom".
    #[serde(skip_serializing_if = "rf_core::previewpane::PreviewPos::is_right")]
    pub preview_position: rf_core::previewpane::PreviewPos,
    /// The pane's height at the bottom (drag its top edge). Written only once the user has moved it.
    #[serde(skip_serializing_if = "is_default_preview_height")]
    pub preview_height: f32,
    /// The file's details under its preview (type, size, dates, location, the hash button). Asked for on the upstream Discord: "Hope
    /// the Properties can have an option to hide".
    pub preview_details: bool,
    pub renderer: RendererPref,
    pub restore_session: bool,
    /// Last session's tab locations ("" = Home).
    pub session_tabs: Vec<String>,
    pub session_active: usize,
    /// The tabs of the last session that were split in two: the second pane of each (see [`SessionPane`]). A tab that is not named
    /// here had one pane.
    pub session_panes: Vec<SessionPane>,
    /// Sessions saved by name (the tab strip's menu: "Save session…", "Open session").
    pub saved_sessions: Vec<NamedSession>,
    /// User pins. `None` = the default known folders.
    pub pinned: Option<Vec<PathBuf>>,
    /// Folder aliases: `@name` in the address box (Files' folder aliases).
    pub folder_aliases: std::collections::BTreeMap<String, PathBuf>,
    /// The drives that are not listed (the sidebar, the Home page, the address bar's list of drives), as letters ("V"). A drive
    /// Explorer hides by its own policy is hidden too, without being named here (`leaf_sys::drives::visible_drives`).
    pub hidden_drives: Vec<String>,
    /// Cloud and WSL places taken off the sidebar one by one (their paths). Issue 141: "a way to hide a single entry in that section
    /// (right click > Hide), like "Hidden drives"".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_places: Vec<String>,
    /// The accent colour of leaf as "#RRGGBB"; empty: Windows' own accent (Settings > Appearance).
    pub accent_color: String,
    /// The user's own programs for kinds of files (Settings > File types; `rf_core::openrules`): the first program of a list opens
    /// such a file on a double click, all of them stand first under "Open with". Only leaf follows them.
    pub open_rules: Vec<rf_core::openrules::OpenRule>,
    /// Closing the last window keeps leaf in the notification area (Files' "leave app
    /// running in the background"): the next window opens instantly.
    pub run_in_background: bool,
    /// Status bar: listing time and the GPU/CPU renderer badge (developer info).
    pub show_perf_info: bool,
    /// A notification when a long operation finishes while leaf is not in front.
    pub notify_long_ops: bool,
    /// The one-time move from "details" to "adaptive" (0.3.3) has been made.
    pub adaptive_offered: bool,
    /// The one-time move from Mica (the old default, written into every settings file) to the theme's own material has been made.
    pub backdrop_theme_offered: bool,
    pub show_extensions: bool,
    /// File icons from Windows (the colourful system icons, a program's own picture). Off (the default, the mockups' look): quiet line
    /// icons in the theme's kind colours.
    pub system_icons: bool,
    /// The icon pack in use (a folder's name in `<config>/icons`, see `icon_pack`); empty: the built-in line icons. Ignored while
    /// `system_icons` is on.
    pub icon_pack: String,
    /// The version whose "What's new" page the user has opened (empty: never); the Settings entry carries a dot until it is the
    /// running version.
    pub whats_new_seen: String,
    /// Custom shortcuts: keystroke → action, e.g. `"ctrl-k" = "NewFolder"` (names as in the
    /// command palette's actions; `leaf::` prefix optional). Applied at startup.
    pub keybindings: std::collections::BTreeMap<String, String>,
    /// Last window placement: [x, y, width, height, maximized (0/1)].
    pub window: Option<[f32; 5]>,
    /// Display UUID and the normal window's origin relative to that display.
    pub window_display: Option<(String, [f32; 2])>,

    // ---- General
    pub date_format: DatePref,
    pub startup: StartupPref,
    pub startup_pages: Vec<String>,
    /// Switch to tabs opened with Ctrl+click / middle click / "Open in new tab".
    pub focus_new_tab: bool,
    /// What a new tab opens (Ctrl+T, the "+" button, the tab menu): Home (the default) or the folder of the tab in front (a Reddit
    /// suggestion, 2026-11-18). Duplicating a tab keeps its folder either way.
    pub new_tab_opens: NewTabOpens,
    /// A folder opened from outside (Win+E, the desktop, another program) gets a window of its own instead of a tab in the newest
    /// window (issue 77). Off by default.
    pub launch_in_new_window: bool,
    /// A saved session opened from the tab menu takes the place of this window's tabs (they can be reopened, Ctrl+Shift+T) instead of
    /// opening a new window (issue 41). Off by default.
    pub sessions_in_this_window: bool,
    /// New tabs start split in two panes.
    pub dual_pane_new_tabs: bool,
    /// Dual pane side by side (true) or stacked (false).
    pub dual_pane_vertical: bool,
    pub menu: MenuPrefs,
    pub smooth_scroll: bool,

    // ---- Appearance
    /// A scrollbar over the Details and Grid lists.
    pub show_scrollbar: bool,
    pub animations: AnimPref,
    pub backdrop: BackdropPref,
    /// Background tint color (0xRRGGBB), `None` = the backdrop as is.
    pub tint: Option<u32>,
    pub background_image: Option<PathBuf>,
    /// 0.05 … 0.6
    pub background_opacity: f32,
    /// HyperGlass look (Settings > Appearance, shown while the backdrop is Glass), each 0..=100; a value outside is cut to that when it is
    /// used. See `leaf_sys::glass::LookParams` for what the first four do; the surface value is the app's own two washes over the glass.
    pub hg_frost: u8,
    pub hg_tint: u8,
    pub hg_refraction: u8,
    pub hg_saturation: u8,
    pub hg_surface: u8,
    /// Freeze the glass of this app's windows while they are not in front: the last picture stays and nothing is captured or drawn for
    /// the window until it is in front again or the pointer is over it (`leaf_sys::glass::set_freeze_when_behind`). Off by default.
    pub hg_freeze_behind: bool,
    /// UI font family; `None` = Segoe UI Variable / system.
    pub font: Option<String>,
    /// The text size of the file manager's windows, percent of the usual (87, 100 or 112). Issue 89: "For lower resolution screens,
    /// the current font size is too large". It sets the windows' base size, which the texts are measured in.
    #[serde(skip_serializing_if = "is_hundred")]
    pub text_percent: u16,
    pub show_toolbar: bool,
    pub show_statusbar: bool,

    // ---- Layout
    /// One layout/sort for every folder (off = each folder remembers its own).
    pub sync_layout: bool,
    pub default_sort: rf_core::SortKey,
    pub default_sort_desc: bool,
    pub default_group: String,
    pub folders_first: bool,
    /// Files before folders (ignored while `folders_first` is on).
    pub files_first: bool,
    pub columns: ColumnPrefs,

    // ---- Files & folders
    pub show_system_files: bool,
    /// Names starting with "." count as hidden when off.
    pub show_dot_files: bool,
    pub show_thumbnails: bool,
    /// The Changes tab wraps long lines instead of scrolling sideways.
    pub diff_wrap_lines: bool,
    /// The marks sync and version-control programs put on icons in Explorer (Google Drive, OneDrive, TortoiseGit, Dropbox), drawn
    /// over ours. A user on Discord, 2026-11-20.
    pub show_overlays: bool,
    pub show_checkboxes: bool,
    pub single_click: ClickPref,
    pub open_folders_in_new_tab: bool,
    pub confirm_delete: DeletePref,
    pub warn_extension_change: bool,
    pub select_on_hover: bool,
    /// Each tab as wide as its name (within the usual limits) instead of all tabs equally wide. Issue 119.
    pub tabs_fit_names: bool,
    /// The terminal button on each folder's tab. Issue 119: it was in the way of clicks on narrow tabs.
    pub tab_terminal_button: bool,
    /// The address bar shows the full path as text (one click edits it) instead of the clickable folder parts.
    pub address_full_path: bool,
    /// A tab is titled with the folder's full path (the middle left out when it is long) instead of the folder's name.
    pub tab_full_path: bool,
    /// The window's title shows the folder's full path.
    pub title_full_path: bool,
    /// Typing in a folder's list filters it by name (the filter bar opens with the letters); off: typing selects the item whose
    /// name starts with them. Issue 106.
    pub type_to_filter: bool,
    pub double_click_up: bool,
    pub select_previous_on_up: bool,
    pub decimal_sizes: bool,
    pub calculate_folder_sizes: bool,
    /// Folders show their size on disk (files rounded up to whole clusters) instead of the sum
    /// of their file sizes.
    pub folder_size_on_disk: bool,
    /// Our own parallel engine for copies, moves to other drives and permanent deletes
    /// (off: Windows' engine for everything, like Explorer).
    pub fast_file_ops: bool,
    /// Check copies after writing them.
    pub copy_verify: CopyVerify,
    /// A copy into a WSL distribution leaves no "file:Zone.Identifier" file beside it (the download mark Windows writes there as a
    /// file of its own). A user on Discord, 2026-11-20.
    pub wsl_drop_zone: bool,
    /// Copy speed limit in MB/s (0 = none).
    pub copy_speed_limit: u32,
    /// Connections to one file in a download (the server must take byte ranges).
    pub download_segments: u32,
    /// Download speed limit in MB/s over all connections of a download (0 = none).
    pub download_speed_limit: u32,
    /// Downloads that run at once; the others wait in line.
    pub download_parallel: u32,
    /// Where `leaf download <address>` saves without a folder; empty: the Downloads folder.
    pub download_folder: String,
    /// Where a running operation's progress is shown (see [`TransferProgress`]).
    pub transfer_progress: TransferProgress,
    /// The thumbnail cache's limit in MB (the cache is in memory); `None` = the default, 64 MB, and no key in the file.
    pub thumb_cache_mb: Option<u32>,
    pub show_libraries: bool,
    /// A "Klasörler" section in the sidebar: the folder tree (drives and the profile folder as roots). Off by default.
    pub sidebar_tree: bool,
    /// The tree opens the branch to the open folder, marks it and scrolls it into view after every navigation.
    pub sidebar_tree_follow: bool,
    /// Text typed into the address field that names no place that exists is tried as a program (`cmd`, `wt`, `"C:\Tools\x.exe" /s`) and
    /// started in the open folder, as Explorer does (see `workspace/address_cmd.rs`). On by default, as in Explorer (the issue that asked
    /// for it expects it to work); only a name that IS a program on the search path starts anything, and the switch turns it off.
    pub address_commands: bool,
    /// Double-clicking a ZIP/7z/tar opens it like a folder (read-only).
    pub browse_archives: bool,
    /// Where an archive opened like a folder goes (Discord, 2026-11-20: "an option/toggle to make zips open separately from the
    /// normal window").
    pub archives_open_in: ArchiveOpen,
    /// What a click on "Sıkıştır" in the right-click menu does: compress at once in this format, or (`Menu`) only open its submenu.
    pub quick_compress: QuickCompress,
    /// After extracting an archive into a new folder, that folder opens in a tab.
    pub open_after_extract: bool,
    /// Search through the Windows Search index (contents, AQS) where the folder is indexed.
    pub search_index: bool,
    /// Our own name index of the local drives: searching in subfolders takes milliseconds.
    pub name_index: bool,
    /// Folders left out of the name index (searching inside them walks the disk).
    pub index_excluded: Vec<String>,

    // ---- Developer tools
    pub ide_button: IdePref,
    /// Editor to open folders with ("code" when empty and VS Code is installed).
    pub ide_path: String,
    /// The terminal "Open in terminal" starts (Automatic: Windows Terminal, else PowerShell).
    pub terminal: rf_core::terminal::Terminal,
    /// The command line of the custom terminal; `{path}` is the folder.
    pub terminal_custom: String,
    /// A small lock after the name of a file the secret guard flags by its name (.env, private keys ...).
    pub secret_marks: bool,
    /// A small letter after the name of what git reports changed in the folder (M, A, ?, U, D, R), in every view.
    pub git_marks: bool,
    /// The biggest file "Copy as context" takes in, in KB (larger ones are listed as left out).
    pub context_max_file_kb: u32,
    /// The most text "Copy as context" builds, in KB; it stops with a message past this.
    pub context_max_total_kb: u32,
    /// The status line shows the token estimate (bytes / 4) of the selected files.
    pub show_token_estimate: bool,
    /// The status line adds lines, play time and pixels of the selected files (read off the UI thread); a click on it breaks the selection down by type.
    pub selection_details: bool,
    /// Counts what is done with Leaf (files copied, folders opened, ...) for the "Your numbers" page; the numbers stay in the config folder.
    pub count_usage: bool,
    /// Rows of files changed in the last ten minutes get a faint tint that fades.
    pub fresh_tint: bool,

    // ---- Advanced
    pub show_flatten: bool,

    // ---- leaf-look (Hızlı Bakış, ayrı program): ayarları kendi dosyasında (`look::config`)
    /// leaf-look kullanılsın mı (kapalıyken leaf onu başlatmaz, Boşluk bir şey yapmaz).
    pub look_enabled: bool,
    /// Space hands the file to QuickLook (QL-Win) instead of leaf-look (`quicklook`).
    pub space_quicklook: bool,

    // ---- Quick search (quick_search.rs)
    /// A global shortcut opens the quick search: apps, files, bookmarks, the web.
    pub quick_search: bool,
    /// Its shortcut, in GPUI's syntax ("alt-space").
    pub quick_search_keys: String,
    pub web_search: WebSearch,
    /// Chrome's and Edge's bookmarks are searched too (read on this PC only).
    pub quick_search_bookmarks: bool,
    /// With nothing typed, HyperSearch lists what was opened lately; off: it shows nothing until something is typed (issue 41:
    /// "Add an option to show/hide recent searches").
    pub quick_search_recent: bool,
    /// The command palette (Ctrl+Shift+P) lists past folders among the commands once something is typed (`folder_history`). Off:
    /// commands only; Ctrl+P still searches past folders.
    pub palette_folder_history: bool,

    // ---- Nearby (sending files to leaf on the network's other devices; Settings -> "Nearby"; crates/rf-nearby and nearby.rs)
    /// Nearby runs in the main process (off by default: it listens on a network port, and Windows' firewall may ask about that).
    pub nearby_enabled: bool,
    /// Who sees this PC: nobody, its trusted devices (the default), or everyone. "Everyone" lasts ten minutes and a start reads it as
    /// trusted devices.
    pub nearby_visibility: NearbyVisibility,
    /// The name the other devices show for this PC; empty: the PC's own name.
    pub nearby_name: String,
    /// Where received files go; empty: the Downloads folder.
    pub nearby_inbox: String,
    /// Files from a trusted device are taken without asking, into the folder above (on).
    pub nearby_auto_accept_trusted: bool,
}

/// Settings -> General: what a new tab opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NewTabOpens {
    #[default]
    Home,
    Current,
}

/// Settings -> Nearby: who sees this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NearbyVisibility {
    Off,
    #[default]
    Trusted,
    Everyone,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            theme_name: crate::theme::data::DEFAULT_THEME.into(),
            visibility: Visibility::Standard,
            accent: String::new(),
            default_view: DefaultView::Adaptive,
            show_hidden: false,
            hide_ignored: false,
            preview_open: true,
            sidebar_open: true,
            sidebar_rail: false,
            sidebar_width: 236.,
            sidebar_hidden: Vec::new(),
            home_hidden: Vec::new(),
            home_layout: None,
            recent_cleared_ms: 0,
            search_history: Vec::new(),
            // (the mockups' bar: Share, Properties and the filter are in the menus and the search field; Settings brings them back)
            toolbar_hidden: vec!["share".into(), "props".into(), "filter".into()],
            toolbar_extra: Vec::new(),
            toolbar_commands: Vec::new(),
            preview_width: 300.,
            preview_position: rf_core::previewpane::PreviewPos::Right,
            preview_height: rf_core::previewpane::BOTTOM_DEFAULT_H,
            renderer: RendererPref::Auto,
            restore_session: true,
            session_tabs: Vec::new(),
            session_panes: Vec::new(),
            session_active: 0,
            saved_sessions: Vec::new(),
            pinned: None,
            folder_aliases: Default::default(),
            hidden_drives: Vec::new(),
            hidden_places: Vec::new(),
            accent_color: String::new(),
            open_rules: Vec::new(),
            run_in_background: false,
            show_perf_info: false,
            notify_long_ops: true,
            adaptive_offered: false,
            backdrop_theme_offered: false,
            show_extensions: true,
            system_icons: false,
            icon_pack: String::new(),
            whats_new_seen: String::new(),
            keybindings: Default::default(),
            window: None,
            window_display: None,
            date_format: DatePref::Application,
            startup: StartupPref::Continue,
            startup_pages: Vec::new(),
            focus_new_tab: false,
            new_tab_opens: NewTabOpens::Home,
            launch_in_new_window: false,
            sessions_in_this_window: false,
            dual_pane_new_tabs: false,
            dual_pane_vertical: true,
            menu: MenuPrefs::default(),
            smooth_scroll: true,
            show_scrollbar: true,
            animations: AnimPref::System,
            backdrop: BackdropPref::Theme,
            tint: None,
            background_image: None,
            background_opacity: 0.2,
            hg_frost: HG_DEFAULT[0],
            hg_tint: HG_DEFAULT[1],
            hg_refraction: HG_DEFAULT[2],
            hg_saturation: HG_DEFAULT[3],
            hg_surface: HG_DEFAULT[4],
            hg_freeze_behind: false,
            font: None,
            text_percent: 100,
            show_toolbar: true,
            show_statusbar: true,
            sync_layout: false,
            default_sort: rf_core::SortKey::Name,
            default_sort_desc: false,
            default_group: "none".into(),
            folders_first: true,
            files_first: false,
            columns: ColumnPrefs::default(),
            show_system_files: false,
            show_dot_files: true,
            show_thumbnails: true,
            diff_wrap_lines: false,
            show_overlays: true,
            show_checkboxes: false,
            single_click: ClickPref::Never,
            open_folders_in_new_tab: false,
            confirm_delete: DeletePref::PermanentOnly,
            warn_extension_change: true,
            select_on_hover: false,
            tabs_fit_names: false,
            tab_terminal_button: true,
            address_full_path: false,
            tab_full_path: false,
            title_full_path: false,
            preview_details: true,
            type_to_filter: false,
            double_click_up: true,
            select_previous_on_up: true,
            decimal_sizes: false,
            calculate_folder_sizes: false,
            folder_size_on_disk: false,
            fast_file_ops: true,
            copy_verify: CopyVerify::Off,
            wsl_drop_zone: false,
            copy_speed_limit: 0,
            download_segments: 6,
            download_speed_limit: 0,
            download_parallel: 3,
            download_folder: String::new(),
            transfer_progress: TransferProgress::StatusBar,
            thumb_cache_mb: None,
            show_libraries: false,
            sidebar_tree: false,
            sidebar_tree_follow: true,
            address_commands: true,
            browse_archives: true,
            quick_compress: QuickCompress::Zip,
            archives_open_in: ArchiveOpen::Here,
            open_after_extract: false,
            search_index: true,
            name_index: true,
            index_excluded: Vec::new(),
            ide_button: IdePref::GitRepos,
            ide_path: String::new(),
            terminal: rf_core::terminal::Terminal::Auto,
            terminal_custom: String::new(),
            secret_marks: true,
            git_marks: true,
            context_max_file_kb: 256,
            context_max_total_kb: 2048,
            show_token_estimate: false,
            selection_details: false,
            count_usage: true,
            fresh_tint: false,
            show_flatten: false,
            space_quicklook: false,
            look_enabled: true,
            quick_search: true,
            quick_search_keys: "alt-space".into(),
            web_search: WebSearch::Google,
            quick_search_bookmarks: true,
            quick_search_recent: true,
            palette_folder_history: true,
            nearby_enabled: false,
            nearby_visibility: NearbyVisibility::Trusted,
            nearby_name: String::new(),
            nearby_inbox: String::new(),
            nearby_auto_accept_trusted: true,
        }
    }
}

impl Global for Settings {}

/// The HyperGlass defaults in the order frost, tint, refraction, saturation, surface. The first four are the engine's own defaults
/// (`LookParams::DEFAULT`, a test keeps them equal); the surface default is the wash the app shipped with over glass.
pub const HG_DEFAULT: [u8; 5] = [52, 50, 67, 62, 50];

static THEME: AtomicU8 = AtomicU8::new(0);
/// The surface value (0..=100) the palette reads on every call, cheaper than the settings struct.
static HG_SURFACE: AtomicU8 = AtomicU8::new(HG_DEFAULT[4]);
static SOLID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The chosen material is one a Windows without materials can still give the MAIN window (Acrylic, HyperGlass): see
/// [`main_window_only`].
static MAIN_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static ANIM: AtomicU8 = AtomicU8::new(0);

/// Whether UI animations play (Settings → Appearance → Animations; "System" follows Windows'
/// Animation effects, read each time: it's one cheap call and may change while we run).
pub fn animations() -> bool {
    match ANIM.load(Ordering::Relaxed) {
        x if x == AnimPref::On as u8 => true,
        x if x == AnimPref::Off as u8 => false,
        _ => leaf_sys::sysparams::client_area_animation(),
    }
}

/// "Solid" background material: the palette paints an opaque window background. Also on a Windows without materials, whatever the
/// setting says and in every process (a side process that never loads these settings asks too).
pub fn solid_backdrop() -> bool {
    SOLID.load(Ordering::Relaxed) || (!materials_available() && !MAIN_ONLY.load(Ordering::Relaxed))
}

/// The materials a Windows without Windows 11's materials (Windows 10) still gives the MAIN window: Acrylic, as the old blur behind
/// it, and HyperGlass, which leaf draws itself. Menus and popups stay plain there, since the blur-behind of a popup covered its
/// whole frame (issue 24). Mica is not among them: there it left the window see-through. Issue 78: a Windows 10 user found Acrylic
/// right on their PC and asked for it and HyperGlass back.
pub fn main_window_only(b: BackdropPref) -> bool {
    matches!(b, BackdropPref::Acrylic | BackdropPref::Glass)
}

/// Whether Windows gives windows a material (Mica, Mica Alt, Acrylic): from Windows 11 22H2 on (`leaf_sys::osver`, issue 24). Where it
/// does not, every window is the "Solid" look and every popup paints itself: see `theme::window_material` and `theme::popup_fill`.
/// `LEAF_NO_BACKDROP=1` answers "no" on any Windows, to look at that state on a developer's PC. Read once.
pub fn materials_available() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var_os("LEAF_NO_BACKDROP").is_none() && leaf_sys::osver::system_backdrops())
}

/// How strongly the app's own washes lie over the glass, 0..=100 (Settings > HyperGlass > Surface wash).
pub fn hg_surface() -> u8 {
    HG_SURFACE.load(Ordering::Relaxed)
}

/// Read by `Palette::for_window` without needing an `App`.
pub fn theme_override() -> ThemePref {
    match THEME.load(Ordering::Relaxed) {
        1 => ThemePref::Light,
        2 => ThemePref::Dark,
        3 => ThemePref::Black,
        _ => ThemePref::System,
    }
}

pub fn path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("settings.toml"))
}

/// What reading the settings file gave.
enum FileRead {
    /// There is no file (or no folder to keep one in): the defaults apply.
    Missing,
    /// The file could not be read or understood.
    Bad,
    Read(Box<Settings>),
}

impl Settings {
    /// Reads the file. With `set_aside_bad` an unreadable or broken file is moved aside (so the next save does not overwrite what
    /// the user may want to repair); a reader that is not the owner of the file leaves it where it is.
    fn read_file(set_aside_bad: bool) -> FileRead {
        let Some(p) = path() else { return FileRead::Missing };
        let text = match rf_core::read_config(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FileRead::Missing,
            Err(e) => {
                // unreadable (not UTF-8, locked): keep it aside rather than overwrite it
                log::warn!("settings: {e}");
                if set_aside_bad {
                    rf_core::backup_corrupt(&p);
                }
                return FileRead::Bad;
            }
        };
        match Self::parse_lenient(text.trim_start_matches('\u{feff}')) {
            Ok(s) => FileRead::Read(Box::new(s)),
            Err(e) => {
                // a typo while hand-editing must not cost the user every setting:
                // keep the file aside, start with defaults
                log::warn!("settings: {e}");
                if set_aside_bad {
                    rf_core::backup_corrupt(&p);
                }
                FileRead::Bad
            }
        }
    }

    pub fn load() -> Self {
        let mut s = match Self::read_file(true) {
            FileRead::Read(s) => *s,
            FileRead::Missing | FileRead::Bad => Settings::default(),
        };
        // what the file held, as a save would write it: the base of every later save's "what did THIS process change" (settings_merge.rs).
        // Taken before the migration, so a migrated value counts as this process's change and is written.
        *BASELINE.lock() = toml::Table::try_from(&s).ok();
        s.migrate();
        s.apply_side_effects();
        s
    }

    /// For a process that only follows the file (the dock): the settings as the file has them now, brought up to date but with no
    /// side effects yet (call [`Settings::apply_side_effects`] on the thread that wants them) and without touching the file. `None`
    /// when the file is there but cannot be read or understood right now (locked, mid-edit): the caller keeps what it has.
    /// Blocks briefly (the read retries a locked file): not for the UI thread.
    pub fn load_readonly() -> Option<Self> {
        Self::load_followed().map(|(s, _)| s)
    }

    /// [`Settings::load_readonly`] and the file as it was read (in the table form a save produces, before the migration): for a process
    /// that REPLACES its settings with the reload and so also has to move its baseline to this table ([`Settings::follow_file`]).
    pub fn load_followed() -> Option<(Self, toml::Table)> {
        let mut s = match Self::read_file(false) {
            FileRead::Read(s) => *s,
            FileRead::Missing => Settings::default(),
            FileRead::Bad => return None,
        };
        let file = toml::Table::try_from(&s).unwrap_or_default();
        s.migrate();
        Some((s, file))
    }

    /// The material the windows really have: the chosen one, or "Solid" on a Windows without materials. The setting itself is left
    /// as it is (the same file may be read on a Windows 11 later).
    pub fn backdrop_in_use(&self) -> BackdropPref {
        let b = self.backdrop_chosen();
        if materials_available() || main_window_only(b) { b } else { BackdropPref::Solid }
    }

    /// The setting with "the theme's" replaced by the theme's own material. On a Windows without materials the theme's choice is
    /// Solid: Acrylic there is the old blur, which the user may pick but which is not made the default.
    fn backdrop_chosen(&self) -> BackdropPref {
        match self.backdrop {
            BackdropPref::Theme if materials_available() => crate::theme::material_pref(),
            BackdropPref::Theme => BackdropPref::Solid,
            b => b,
        }
    }

    /// Brings settings written by older versions up to date.
    fn migrate(&mut self) {
        // older files: "restore_session = false" means "start with a new tab"
        if !self.restore_session && self.startup == StartupPref::Continue {
            self.startup = StartupPref::NewTab;
            self.restore_session = true;
        }
        // "details" was the default before 0.3.3, so it says nothing about a choice: move it
        // once to "adaptive" (plain folders still open as details). Grid and columns stay.
        if !self.adaptive_offered {
            if self.default_view == DefaultView::Details {
                self.default_view = DefaultView::Adaptive;
            }
            self.adaptive_offered = true;
        }
        // Mica was the default and every save wrote it, so it says nothing about a choice: move it once to the theme's material
        // (Canopy is made for Acrylic: on Mica it read as solid black). A material picked afterwards stays.
        if !self.backdrop_theme_offered {
            if self.backdrop == BackdropPref::Mica {
                self.backdrop = BackdropPref::Theme;
            }
            self.backdrop_theme_offered = true;
        }
        // the Home page's old hide list becomes its widget layout
        crate::home::migrate(self);
    }

    /// Field by field: a value this version doesn't understand (a newer build's theme, a
    /// mistyped enum) falls back to its default instead of resetting every setting. Only a
    /// TOML syntax error fails. Keys this build does not know (the cut dock / desktop / snip settings of older files) are ignored here
    /// and kept in the file by the save (see `settings_merge.rs`).
    fn parse_lenient(text: &str) -> Result<Settings, toml::de::Error> {
        let table: toml::Table = toml::from_str(text)?;
        let mut base = toml::Table::try_from(Settings::default()).unwrap_or_default();
        for (k, v) in table {
            let mut trial = base.clone();
            trial.insert(k.clone(), v);
            if Settings::deserialize(toml::Value::Table(trial.clone())).is_ok() {
                base = trial;
            } else {
                log::warn!("settings: ignoring invalid value for '{k}'");
            }
        }
        Ok(Settings::deserialize(toml::Value::Table(base)).unwrap_or_default())
    }

    /// What `load` makes of a file's text (parsed, then brought up to date): for the tests of what an old file becomes.
    #[cfg(test)]
    pub(crate) fn parse_migrated(text: &str) -> Result<Settings, toml::de::Error> {
        let mut s = Self::parse_lenient(text)?;
        s.migrate();
        Ok(s)
    }

    /// The four values the engine takes, cut to 0..=100.
    pub fn hyperglass_look(&self) -> leaf_sys::glass::LookParams {
        leaf_sys::glass::LookParams {
            frost: f32::from(self.hg_frost.min(100)),
            tint: f32::from(self.hg_tint.min(100)),
            refraction: f32::from(self.hg_refraction.min(100)),
            saturation: f32::from(self.hg_saturation.min(100)),
        }
    }

    /// Back to the shipped look (the "Reset to defaults" button).
    pub fn reset_hyperglass(&mut self) {
        [self.hg_frost, self.hg_tint, self.hg_refraction, self.hg_saturation, self.hg_surface] = HG_DEFAULT;
    }

    /// The theme, visibility and accent these settings choose.
    pub fn theme_selection(&self) -> crate::theme::Selection {
        use crate::theme::AccentChoice;
        let accent = match leaf_sys::accent::parse_hex(&self.accent_color) {
            Some(c) => AccentChoice::Custom(c),
            None if self.accent.eq_ignore_ascii_case("windows") => AccentChoice::Windows,
            None => AccentChoice::Theme(self.accent.clone()),
        };
        crate::theme::Selection { theme: self.theme_name.trim().to_ascii_lowercase(), high: self.visibility == Visibility::High, accent }
    }

    /// For "Import settings".
    pub fn parse_lenient_pub(text: &str) -> Result<Settings, toml::de::Error> {
        Self::parse_lenient(text)
    }

    pub(crate) fn apply_side_effects(&self) {
        sync_name_index(self.name_index, &self.index_excluded);
        crate::usage::set_enabled(self.count_usage);
        crate::aliases::set(self.folder_aliases.clone());
        leaf_sys::openrules::set(self.open_rules.clone());
        leaf_sys::drives::set_hidden(leaf_sys::drives::mask_of_letters(&self.hidden_drives));
        // (a text that is no colour is no colour: Windows' accent)
        leaf_sys::accent::set_override(leaf_sys::accent::parse_hex(&self.accent_color));
        crate::theme::select(&self.theme_selection());
        crate::background::sync_tray(self.run_in_background);
        crate::quick_search::sync(self);
        // (the system's own short date and time for the "System" style and older dates, issue 59; set once)
        rf_core::format::set_system_format(leaf_sys::datefmt::short_date_time);
        rf_core::format::set_date_style(match self.date_format {
            // (the theme may want its own: Index writes ISO dates in its monospace columns)
            DatePref::Application if crate::theme::iso_dates() => rf_core::format::DateStyle::Universal,
            DatePref::Application => rf_core::format::DateStyle::Calendar,
            DatePref::System => rf_core::format::DateStyle::Local,
            DatePref::Universal => rf_core::format::DateStyle::Universal,
        });
        rf_core::format::set_decimal_sizes(self.decimal_sizes);
        // (after `theme::select`: "the theme's" material is read from the theme just chosen)
        let chosen = self.backdrop_chosen();
        SOLID.store(chosen == BackdropPref::Solid, Ordering::Relaxed);
        MAIN_ONLY.store(main_window_only(chosen), Ordering::Relaxed);
        ANIM.store(self.animations as u8, Ordering::Relaxed);
        // HyperGlass: the engine redraws every glass window once when the look changes, and does nothing for the same look again
        HG_SURFACE.store(self.hg_surface.min(100), Ordering::Relaxed);
        leaf_sys::glass::set_look(self.hyperglass_look());
        leaf_sys::glass::set_freeze_when_behind(self.hg_freeze_behind);
        crate::platform::gpui::SMOOTH_SCROLL.store(self.smooth_scroll, Ordering::Relaxed);
        THEME.store(
            match self.theme {
                ThemePref::System => 0,
                ThemePref::Light => 1,
                ThemePref::Dark => 2,
                ThemePref::Black => 3,
            },
            Ordering::Relaxed,
        );
        // An explicit LEAF_RENDERER env var wins (debugging).
        if std::env::var_os("LEAF_RENDERER").is_none() {
            crate::platform::gpui::hybrid::set_mode(renderer_mode(self.renderer, crate::background::DOCK_PROCESS.load(Ordering::Relaxed)));
        }
    }

    /// Writes the settings file: only what THIS process changed since it read the file, on top of what the file holds now (a change
    /// another writer made meanwhile survives, a key this build does not know survives, a process that changed nothing writes nothing).
    /// The rule and its tests are in `settings_merge.rs`.
    pub fn save(&self) {
        // One writer at a time inside this process (saves come from several threads); other processes are kept apart by the file lock.
        static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _one = LOCK.lock();
        let Some(p) = path() else { return };
        let mine = match toml::Table::try_from(self) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("settings save: {e}");
                return;
            }
        };
        // (the baseline moves to what we hold once the save went through or needed no write; a save that did not happen keeps it, so the
        // next one carries the same changes again)
        crate::settings_merge::save_tracked(&BASELINE, &p, mine, || toml::Table::try_from(Settings::default()).unwrap_or_default());
    }
}

/// What this process last knew the settings file to hold (see [`Settings::save`]); `None` before the first `load`.
static BASELINE: crate::settings_merge::BaselineCell = parking_lot::Mutex::new(None);

/// Keeps gpui-component widgets (inputs, menus) in the same light/dark mode as our palette.
pub fn apply_component_theme(cx: &mut App) {
    use gpui_kit::component::theme::{Theme, ThemeMode};
    match cx.global::<Settings>().theme {
        ThemePref::Light => Theme::change(ThemeMode::Light, None, cx),
        ThemePref::Dark | ThemePref::Black => Theme::change(ThemeMode::Dark, None, cx),
        ThemePref::System => Theme::sync_system_appearance(None, cx),
    }
    // The component root paints the theme background under every window; keep it clear so
    // the Mica backdrop shows through (our own surfaces bring their own fills).
    Theme::update(cx, |t| t.background = gpui_kit::transparent_black());
    // the chosen font, else the theme's (a font that is not installed falls back to the system's), else Windows 11's own typeface
    // (optical sizes) when present
    let theme_font = crate::theme::Palette::dark().font_ui;
    if let Some(f) = cx.global::<Settings>().font.clone().filter(|f| crate::platform::fonts::installed(f)) {
        Theme::update(cx, |t| t.font_family = f.into());
    } else if !theme_font.is_empty() {
        Theme::update(cx, |t| t.font_family = theme_font.into());
    } else if !cfg!(windows) {
        Theme::update(cx, |t| t.font_family = crate::platform::fonts::ui().into());
    } else if std::path::Path::new(r"C:\Windows\Fonts\SegUIVar.ttf").exists() {
        Theme::update(cx, |t| t.font_family = "Segoe UI Variable Text".into());
    }
}


/// Mutates the global settings, applies side effects, saves in the background and refreshes windows.
pub fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    cx.update_global::<Settings, _>(|s, _| f(s));
    let s = cx.global::<Settings>().clone();
    s.apply_side_effects();
    apply_component_theme(cx);
    std::thread::spawn(move || s.save());
    // (Nearby follows its settings: started, stopped, started anew, or shown to others differently)
    crate::nearby::apply(cx);
    cx.refresh_windows();
}

/// Like [`update`] for a control that changes on every step of a drag (a slider): the change is applied and drawn at once, the file is
/// written once the changes have settled (the pane resize does the same), so a drag is not hundreds of writes.
pub fn update_live(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    cx.update_global::<Settings, _>(|s, _| f(s));
    cx.global::<Settings>().apply_side_effects();
    cx.refresh_windows();
    let mine = SERIAL.fetch_add(1, Ordering::Relaxed) + 1;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        // a later step has its own timer: only the last one writes
        if SERIAL.load(Ordering::Relaxed) == mine {
            cx.update(|cx| {
                let snapshot = cx.global::<Settings>().clone();
                std::thread::spawn(move || snapshot.save());
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_dock_stays_on_the_graphics_card_unless_the_user_chose_the_software_renderer() {
        use super::{RendererPref as P, renderer_mode};
        use crate::platform::gpui::hybrid::RendererMode as M;
        assert_eq!(renderer_mode(P::Auto, false), M::Auto, "the file manager keeps the automatic switch");
        assert_eq!(renderer_mode(P::Auto, true), M::Gpu, "the dock never builds a software device on its own");
        assert_eq!(renderer_mode(P::Cpu, true), M::Cpu, "a choice made on purpose is kept");
        assert_eq!(renderer_mode(P::Gpu, true), M::Gpu);
        assert_eq!(renderer_mode(P::Gpu, false), M::Gpu);
        assert_eq!(renderer_mode(P::Cpu, false), M::Cpu);
    }

    #[test]
    fn details_moves_to_adaptive_once() {
        // a file from before the move: details becomes adaptive
        let mut s = super::Settings::parse_lenient("default_view = \"details\"\n").unwrap();
        s.migrate();
        assert_eq!(s.default_view, super::DefaultView::Adaptive);
        assert!(s.adaptive_offered);
        // chosen afterwards: stays
        let mut s = super::Settings::parse_lenient("default_view = \"details\"\nadaptive_offered = true\n").unwrap();
        s.migrate();
        assert_eq!(s.default_view, super::DefaultView::Details);
        // grid was a real choice
        let mut s = super::Settings::parse_lenient("default_view = \"grid\"\n").unwrap();
        s.migrate();
        assert_eq!(s.default_view, super::DefaultView::Grid);
    }

    use super::{Settings, ThemePref};

    #[test]
    fn roundtrip_and_defaults() {
        let s = Settings { show_hidden: true, session_tabs: vec!["C:\\".into()], ..Default::default() };
        let t = toml::to_string_pretty(&s).unwrap();
        let back: Settings = toml::from_str(&t).unwrap();
        assert!(back.show_hidden);
        assert_eq!(back.session_tabs, vec!["C:\\".to_string()]);
        // unknown/missing keys fall back to defaults
        let partial: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert_eq!(partial.theme, ThemePref::Dark);
        assert!(partial.restore_session);
        // an older file still has the language setting (the UI is English only now): it loads
        let old = Settings::parse_lenient("language = \"tr\"\ntheme = \"dark\"").unwrap();
        assert_eq!(old.theme, ThemePref::Dark);
    }

    /// The look's defaults are Canopy at Standard with the theme's own accent; a file from before themes keeps its mode and loads.
    #[test]
    fn the_theme_defaults_to_canopy_standard_and_old_files_still_load() {
        use super::Visibility;
        use crate::theme::AccentChoice;
        let d = Settings::default();
        assert_eq!((d.theme_name.as_str(), d.visibility, d.accent.as_str()), ("canopy", Visibility::Standard, ""));
        let sel = d.theme_selection();
        assert_eq!((sel.theme.as_str(), sel.high, sel.accent), ("canopy", false, AccentChoice::Theme(String::new())));
        let old = Settings::parse_lenient("theme = \"black\"\naccent_color = \"\"\nshow_hidden = true").unwrap();
        assert_eq!((old.theme, old.theme_name.as_str(), old.visibility), (ThemePref::Black, "canopy", Visibility::Standard));
        let chosen = Settings::parse_lenient("theme_name = \"Index\"\nvisibility = \"standard\"\naccent = \"windows\"").unwrap();
        let sel = chosen.theme_selection();
        assert_eq!((sel.theme.as_str(), sel.high, sel.accent), ("index", false, AccentChoice::Windows));
        // a colour the user wrote wins over a named accent
        let own = Settings::parse_lenient("accent = \"Mint\"\naccent_color = \"#F7630C\"").unwrap();
        assert_eq!(own.theme_selection().accent, AccentChoice::Custom(0xF7630C));
        // a visibility this build does not know falls back to the default alone
        let odd = Settings::parse_lenient("visibility = \"max\"\nshow_hidden = true").unwrap();
        assert!(odd.show_hidden && odd.visibility == Visibility::Standard);
    }

    /// The second panes of the session survive the settings file, and a file from before them has none.
    #[test]
    fn the_sessions_second_panes_survive_the_file() {
        use super::SessionPane;
        let panes = vec![SessionPane { tab: 0, location: "D:\\Yedek".into(), active: true }, SessionPane { tab: 2, location: String::new(), active: false }];
        let s = Settings { session_tabs: vec!["C:\\".into(), "E:\\".into(), "F:\\".into()], session_panes: panes.clone(), ..Default::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.session_panes, panes);
        assert_eq!(back.session_tabs.len(), 3);
        let old: Settings = toml::from_str("session_tabs = [\"C:\\\\\"]").unwrap();
        assert!(old.session_panes.is_empty());
        // an entry that names only its tab reads with the rest at their defaults
        let part: Settings = toml::from_str("[[session_panes]]\ntab = 1").unwrap();
        assert_eq!(part.session_panes, [SessionPane { tab: 1, location: String::new(), active: false }]);
    }

    #[test]
    fn the_preview_position_leaves_no_key_until_it_is_asked_for() {
        use rf_core::previewpane::{BOTTOM_DEFAULT_H, PreviewPos};
        let d = Settings::default();
        assert_eq!((d.preview_position, d.preview_height), (PreviewPos::Right, BOTTOM_DEFAULT_H));
        let text = toml::to_string_pretty(&d).unwrap();
        assert!(!text.contains("preview_position") && !text.contains("preview_height"), "default settings must not gain keys");
        let bottom = Settings { preview_position: PreviewPos::Bottom, preview_height: 260., ..Default::default() };
        let text = toml::to_string_pretty(&bottom).unwrap();
        assert!(text.contains("preview_position = \"bottom\"") && text.contains("preview_height = 260.0"), "{text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!((back.preview_position, back.preview_height), (PreviewPos::Bottom, 260.));
        // an unknown word does not break the file
        let odd: Settings = toml::from_str("preview_position = \"left\"
show_hidden = true").unwrap();
        assert_eq!(odd.preview_position, PreviewPos::Right);
        assert!(odd.show_hidden);
    }
}

/// Where data that can be rebuilt lives (remembered folder sizes): `%LOCALAPPDATA%\leaf`,
/// or `LEAF_CONFIG_DIR` when set, so test copies keep their data apart.
pub fn local_data_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("LEAF_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(d));
    }
    // the portable copy keeps everything in its own folder (issue 71)
    if let Some(d) = leaf_sys::portable::data_dir() {
        return Some(d);
    }
    dirs::data_local_dir().map(|d| d.join("leaf"))
}

/// `%APPDATA%\leaf`, or `LEAF_CONFIG_DIR` when set (portable use, tests).
pub fn app_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("LEAF_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(d));
    }
    // the portable copy: its own `data` folder (`leaf_sys::portable`, issue 71)
    if let Some(d) = leaf_sys::portable::data_dir() {
        return Some(d);
    }
    dirs::config_dir().map(|d| d.join("leaf"))
}

#[cfg(test)]
mod lenient_tests {
    use super::Settings;

    #[test]
    fn unknown_values_fall_back_per_field() {
        let s = Settings::parse_lenient("theme = \"mor\"
show_hidden = true
sidebar_width = \"geniş\"
yeni_alan = 3").unwrap();
        assert!(s.show_hidden, "valid fields survive");
        assert_eq!(s.sidebar_width, Settings::default().sidebar_width);
        assert!(Settings::parse_lenient("[[[bozuk").is_err(), "syntax errors still fail");
    }

    /// A file written by a build that still had the dock, the desktop, the snip and the update / donate strips loads: those keys are
    /// ignored and every other setting is kept.
    #[test]
    fn a_file_with_the_cut_features_keeps_the_rest() {
        let s = Settings::parse_migrated(
            "dock_enabled = true\ndock_one_switch = true\ndock_magnify = \"subtle\"\ndock_size = \"large\"\nshell_win_key = false\n\
             shell_hide_taskbar = true\nsnip_autosave = true\ndesktop_icon_this_pc = \"show\"\ndesktop_corner_radius = 12\n\
             auto_update = true\nbeta_updates = true\nstar_asked = true\nstarts = 40\ndonate_never = true\n\
             show_hidden = true\nsidebar_width = 333.0\ntheme = \"dark\"\n",
        )
        .unwrap();
        assert!(s.show_hidden);
        assert_eq!(s.sidebar_width, 333.0);
        assert_eq!(s.theme, super::ThemePref::Dark);
    }

    #[test]
    fn glass_backdrop_round_trips() {
        use super::BackdropPref;
        let s = Settings { backdrop: BackdropPref::Glass, ..Default::default() };
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("backdrop = \"glass\""), "serialized as \"glass\": {text}");
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.backdrop, BackdropPref::Glass);
        // through the loader the app uses
        assert_eq!(Settings::parse_lenient(&text).unwrap().backdrop, BackdropPref::Glass);
        // the default is the theme's own material
        assert_eq!(Settings::default().backdrop, BackdropPref::Theme);
    }

    #[test]
    fn the_old_default_mica_moves_once_to_the_themes_material() {
        use super::BackdropPref;
        let mut s = Settings::parse_lenient("backdrop = \"mica\"\n").unwrap();
        s.migrate();
        assert_eq!(s.backdrop, BackdropPref::Theme);
        assert!(s.backdrop_theme_offered);
        // chosen afterwards: stays
        let mut s = Settings::parse_lenient("backdrop = \"mica\"\nbackdrop_theme_offered = true\n").unwrap();
        s.migrate();
        assert_eq!(s.backdrop, BackdropPref::Mica);
        let mut s = Settings::parse_lenient("backdrop = \"solid\"\n").unwrap();
        s.migrate();
        assert_eq!(s.backdrop, BackdropPref::Solid);
    }

    #[test]
    fn hyperglass_defaults_match_the_engine_and_the_shipped_look() {
        let d = Settings::default();
        let engine = leaf_sys::glass::LookParams::DEFAULT;
        assert_eq!(d.hyperglass_look(), engine, "the settings default is the engine's default");
        assert_eq!(super::HG_DEFAULT.map(f32::from)[..4], [engine.frost, engine.tint, engine.refraction, engine.saturation]);
        assert_eq!(d.hg_surface, 50);
        assert_eq!(super::hg_surface(), 50, "the palette's atomic starts at the default as well");
    }

    #[test]
    fn hyperglass_values_round_trip() {
        let s = Settings { hg_frost: 0, hg_tint: 100, hg_refraction: 13, hg_saturation: 77, hg_surface: 5, ..Default::default() };
        let text = toml::to_string_pretty(&s).unwrap();
        let back = Settings::parse_lenient(&text).unwrap();
        assert_eq!((back.hg_frost, back.hg_tint, back.hg_refraction, back.hg_saturation, back.hg_surface), (0, 100, 13, 77, 5));
        // an old file has none of them: every one is the default
        let old = Settings::parse_lenient("theme = \"dark\"
").unwrap();
        assert_eq!(super::HG_DEFAULT, [old.hg_frost, old.hg_tint, old.hg_refraction, old.hg_saturation, old.hg_surface]);
    }

    #[test]
    fn a_bad_hyperglass_value_costs_only_that_field() {
        // a negative number, a word, a float and a number too big for the field: each falls back alone, the neighbours and the other
        // settings survive
        let s = Settings::parse_lenient(
            "hg_frost = -3
hg_tint = \"cok\"
hg_refraction = 12.5
hg_saturation = 9999
hg_surface = 80
show_hidden = true
",
        )
        .unwrap();
        assert_eq!(s.hg_frost, super::HG_DEFAULT[0]);
        assert_eq!(s.hg_tint, super::HG_DEFAULT[1]);
        assert_eq!(s.hg_refraction, super::HG_DEFAULT[2]);
        assert_eq!(s.hg_saturation, super::HG_DEFAULT[3]);
        assert_eq!(s.hg_surface, 80, "the good value next to the bad ones is kept");
        assert!(s.show_hidden);
    }

    #[test]
    fn a_hyperglass_value_above_100_is_cut_when_used() {
        let s = Settings::parse_lenient("hg_frost = 200
hg_tint = 101
").unwrap();
        assert_eq!((s.hg_frost, s.hg_tint), (200, 101), "the file's number is kept as it is");
        let look = s.hyperglass_look();
        assert_eq!((look.frost, look.tint), (100.0, 100.0));
    }

    #[test]
    fn resetting_hyperglass_leaves_the_rest_alone() {
        let mut s = Settings { hg_frost: 1, hg_tint: 2, hg_refraction: 3, hg_saturation: 4, hg_surface: 5, show_hidden: true, ..Default::default() };
        s.reset_hyperglass();
        assert_eq!(super::HG_DEFAULT, [s.hg_frost, s.hg_tint, s.hg_refraction, s.hg_saturation, s.hg_surface]);
        assert!(s.show_hidden);
    }

    #[test]
    fn unknown_backdrop_value_costs_only_that_field() {
        // what a version without Glass meets after the user picked it and went back: the value
        // is dropped (the default: the theme's), every other setting of the file survives
        use super::BackdropPref;
        let s = Settings::parse_lenient("backdrop = \"cam\"
show_hidden = true
theme = \"dark\"
").unwrap();
        assert_eq!(s.backdrop, BackdropPref::Theme);
        assert!(s.show_hidden);
        assert_eq!(s.theme, super::ThemePref::Dark);
    }
}

/// Starts or stops the name index when the setting changes. The first start waits a few seconds
/// so it never competes with opening the window.
fn sync_name_index(on: bool, excluded: &[String]) {
    use std::sync::atomic::{AtomicU8, Ordering};
    // leaf-look reads these settings too: the index is leaf's (a second indexer would walk
    // every drive again and write the same index files)
    if crate::background::side_process() {
        return;
    }
    // 0 = not decided yet, 1 = off, 2 = on
    static STATE: AtomicU8 = AtomicU8::new(0);
    static EXCLUDED: parking_lot::Mutex<Option<Vec<String>>> = parking_lot::Mutex::new(None);
    let list_changed = {
        let mut e = EXCLUDED.lock();
        let changed = e.as_deref() != Some(excluded);
        if changed {
            *e = Some(excluded.to_vec());
            leaf_sys::nameindex::set_excluded(&excluded.iter().map(std::path::PathBuf::from).collect::<Vec<_>>());
        }
        changed
    };
    let want = if on { 2 } else { 1 };
    let was = STATE.swap(want, Ordering::Relaxed);
    // a changed exclusion list rebuilds a running index (the fresh walk leaves the folders out)
    if was == want && !(on && list_changed && was != 0) {
        return;
    }
    if !on {
        std::thread::spawn(leaf_sys::indexer::stop);
        return;
    }
    let delay = if was == 0 { 5 } else { 0 };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(delay));
        // extra instances (tests, the speed test's) leave it to the main one
        // (LEAF_INDEX=1 indexes there too, for testing)
        let extra = (std::env::var_os("LEAF_MULTI_INSTANCE").is_some() || std::env::var_os("LEAF_INSTANCE").is_some()) && std::env::var_os("LEAF_INDEX").is_none();
        if STATE.load(Ordering::Relaxed) != 2 || std::env::var_os("LEAF_NO_INDEX").is_some() || extra {
            return;
        }
        // a copy with its own settings folder (tests, portable) keeps its index there too
        let dir = match std::env::var_os("LEAF_CONFIG_DIR").map(std::path::PathBuf::from).or_else(leaf_sys::portable::data_dir) {
            Some(cfg) => cfg.join("index"),
            // Linux: $XDG_CACHE_HOME/leaf/index (the index is rebuildable)
            #[cfg(not(windows))]
            None => std::env::var_os("XDG_CACHE_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
                .unwrap_or_else(std::env::temp_dir)
                .join("leaf")
                .join("index"),
            #[cfg(windows)]
            None => std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, std::path::PathBuf::from).join("leaf").join("index"),
        };
        // LEAF_INDEX_ROOTS="E:\;F:\" picks the roots (testing)
        let roots = match std::env::var("LEAF_INDEX_ROOTS") {
            Ok(v) => v.split(';').filter(|s| !s.is_empty()).map(std::path::PathBuf::from).collect(),
            Err(_) => leaf_sys::indexer::local_roots(),
        };
        leaf_sys::indexer::start(roots, dir);
    });
}
