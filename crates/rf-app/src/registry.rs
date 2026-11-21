//! The command registry: one constant table of every command leaf has ([`COMMANDS`]). The command palette, the default key
//! bindings, the shortcut editor, the "What leaf can do" page and `FEATURES.md` are all made from it, so a command is described
//! once. A row's `id` is its action's type name (`NewTab`): the user's own bindings in `settings.toml` `[keybindings]` name
//! actions that way, so they keep working.
//!
//! The table is a `static` of plain data: nothing runs and nothing is allocated for it at start-up.

use gpui_kit::assets::IconName as L;
use gpui_kit::{Action, App, KeyBinding};

use crate::actions::{DIALOG_CTX, FOLDER_CTX, TREE_CTX};
use crate::menu_model::{MenuPlace, SW_COPY_PATH, SW_SHELF, SW_TERMINAL, Section as S, Slot as K, Targets as T, place};

/// Where a default key works: everywhere in the window, or only while that part has the keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum KeyCtx {
    Window,
    /// The file list.
    Folder,
    /// The folder tree.
    Tree,
    /// A question dialog.
    Dialog,
}

impl KeyCtx {
    pub fn context(self) -> Option<&'static str> {
        match self {
            KeyCtx::Window => None,
            KeyCtx::Folder => Some(FOLDER_CTX),
            KeyCtx::Tree => Some(TREE_CTX),
            KeyCtx::Dialog => Some(DIALOG_CTX),
        }
    }
}

/// One default key binding, in GPUI's syntax (`ctrl-shift-n`).
#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub keys: &'static str,
    pub ctx: KeyCtx,
}

const fn w(keys: &'static str) -> Key {
    Key { keys, ctx: KeyCtx::Window }
}
const fn f(keys: &'static str) -> Key {
    Key { keys, ctx: KeyCtx::Folder }
}
const fn tr(keys: &'static str) -> Key {
    Key { keys, ctx: KeyCtx::Tree }
}
const fn dl(keys: &'static str) -> Key {
    Key { keys, ctx: KeyCtx::Dialog }
}

/// The groups of the "What leaf can do" page and `FEATURES.md`, in the order they are listed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Tabs,
    Navigation,
    Selection,
    Files,
    CopyPath,
    View,
    Panes,
    Tools,
    App,
    /// The user's own actions (`actions.toml`); only ever run-time rows.
    Custom,
}

impl Category {
    pub const ALL: [Category; 10] = [
        Category::Tabs,
        Category::Navigation,
        Category::Selection,
        Category::Files,
        Category::CopyPath,
        Category::View,
        Category::Panes,
        Category::Tools,
        Category::App,
        Category::Custom,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::Tabs => "Tabs and windows",
            Category::Navigation => "Going places",
            Category::Selection => "Selecting",
            Category::Files => "Files and folders",
            Category::CopyPath => "Copying paths",
            Category::View => "View",
            Category::Panes => "Panes",
            Category::Tools => "Tools",
            Category::App => "leaf itself",
            Category::Custom => "Your actions",
        }
    }
}

/// Where a command is offered. A bit set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Places(u8);

#[allow(dead_code)]
impl Places {
    pub const NONE: Places = Places(0);
    /// The command palette, and with it the shortcut editor in Settings.
    pub const PALETTE: Places = Places(1);
    /// The "What leaf can do" page and `FEATURES.md`.
    pub const PAGE: Places = Places(2);
    /// The right-click menu: the command has rows in `menu` (`menu_model.rs` makes the menu from them).
    pub const MENU: Places = Places(4);
    /// The tool bar (not generated from the registry yet).
    pub const TOOLBAR: Places = Places(8);
    pub const LISTED: Places = Places(1 | 2);
    /// In the palette and on the page, and in the right-click menu.
    pub const LISTED_MENU: Places = Places(1 | 2 | 4);
    /// On the page and in the right-click menu, not in the palette.
    pub const PAGE_MENU: Places = Places(2 | 4);

    pub const fn contains(self, other: Places) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The systems a command exists on. A bit set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Platforms(u8);

#[allow(dead_code)]
impl Platforms {
    pub const WINDOWS: Platforms = Platforms(1);
    pub const LINUX: Platforms = Platforms(2);
    pub const ALL: Platforms = Platforms(1 | 2);

    pub const fn contains(self, other: Platforms) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn label(self) -> &'static str {
        match self.0 {
            1 => "Windows",
            2 => "Linux",
            _ => "",
        }
    }
}

pub struct CommandSpec {
    /// The action's type name (`NewTab`); `leaf::NewTab` is the action.
    pub id: &'static str,
    /// The English text (translated where it is drawn, `i18n::t`).
    pub title: &'static str,
    /// One sentence: what it does.
    pub description: &'static str,
    /// More words the page's search finds it by.
    pub keywords: &'static [&'static str],
    pub category: Category,
    /// Bound at start-up, in this order; the first one is the one shown.
    pub default_keys: &'static [Key],
    /// Shown instead of a key when the command is reached by typing something, not by a binding (`@` for an alias).
    pub hint: &'static str,
    pub icon: L,
    pub places: Places,
    /// Where the right-click menu has it (empty: nowhere); `places` has MENU exactly when this is not empty.
    pub menu: &'static [MenuPlace],
    /// Whether it is offered right now; `None`: always.
    pub when: Option<fn(&App) -> bool>,
    /// Makes the action.
    pub action: fn() -> Box<dyn Action>,
    /// Makes one of its key bindings (`KeyBinding::new` wants the action's own type).
    pub bind: fn(&'static str, Option<&'static str>) -> KeyBinding,
    /// The leaf version it came with (Settings > What's new groups the rows by it).
    pub since: &'static str,
    pub platforms: Platforms,
    /// The name of the user's own action (`actions.toml`) when this row is one; empty for the built-in rows.
    pub custom: &'static str,
}

impl CommandSpec {
    /// The action to run: for a user's action one carrying its name, else the row's own.
    pub fn make_action(&self) -> Box<dyn Action> {
        if self.custom.is_empty() { (self.action)() } else { Box::new(crate::actions::RunCustomAction { name: self.custom.to_string(), keys: String::new() }) }
    }

    /// The shortcut as shown ("Ctrl+Shift+N", "Alt+←", "Del"); empty when it has none.
    pub fn shown_keys(&self) -> String {
        if !self.hint.is_empty() {
            return self.hint.to_string();
        }
        self.default_keys.first().map(|k| shown(k.keys)).unwrap_or_default()
    }

    pub fn available(&self, cx: &App) -> bool {
        self.when.is_none_or(|w| w(cx))
    }
}

/// "alt-left" → "Alt+←", "shift-delete" → "Shift+Del", "ctrl-shift-n" → "Ctrl+Shift+N".
pub fn shown(keys: &str) -> String {
    crate::keymap_editor::pretty(keys)
        .split('+')
        .map(|k| match k {
            "Left" => "←",
            "Right" => "→",
            "Up" => "↑",
            "Down" => "↓",
            "Delete" => "Del",
            k => k,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// What every row has unless it says otherwise.
const BASE: CommandSpec = CommandSpec {
    id: "",
    title: "",
    description: "",
    keywords: &[],
    category: Category::App,
    default_keys: &[],
    hint: "",
    icon: L::Command,
    places: Places::LISTED,
    menu: &[],
    when: None,
    action: || Box::new(crate::actions::ToggleCommandPalette),
    bind: |k, c| KeyBinding::new(k, crate::actions::ToggleCommandPalette, c),
    since: "0.9.0",
    platforms: Platforms::ALL,
    custom: "",
};

/// One row: `c!(Action, Category, "Title", "Description.", Icon, [keys] , field: value ...)`.
macro_rules! c {
    ($id:ident, $cat:ident, $title:literal, $desc:literal, $icon:ident, [$($k:expr),* $(,)?] $(, $field:ident : $val:expr)* $(,)?) => {
        CommandSpec {
            id: stringify!($id),
            title: $title,
            description: $desc,
            category: Category::$cat,
            icon: L::$icon,
            default_keys: &[$($k),*],
            action: || Box::new(crate::actions::$id),
            bind: |k, c| KeyBinding::new(k, crate::actions::$id, c),
            $($field: $val,)*
            ..BASE
        }
    };
}

/// Not in the palette, but on the page (keys that move around the list and the like).
const PAGE_ONLY: Places = Places::PAGE;

/// Every command. The palette lists its rows in this order.
pub static COMMANDS: &[CommandSpec] = &[
    // ---- in the palette (its order)
    c!(NewTab, Tabs, "New tab", "Opens a new tab next to the current one.", Plus, [w("ctrl-t")]),
    c!(NewWindow, Tabs, "New window", "Opens another leaf window.", AppWindow, [w("ctrl-n")]),
    c!(CloseTab, Tabs, "Close tab", "Closes the current tab.", X, [w("ctrl-w")]),
    c!(DuplicateTab, Tabs, "Duplicate tab", "Opens the current folder again in a new tab.", Copy, [w("ctrl-shift-k")]),
    c!(CloseOtherTabs, Tabs, "Close other tabs", "Closes every tab but the current one.", X, []),
    c!(CloseAllTabs, Tabs, "Close all tabs", "Closes every tab of the window.", X, []),
    c!(ReopenClosedTab, Tabs, "Reopen closed tab", "Opens the tab closed last again.", RotateCcw, [w("ctrl-shift-t")], keywords: &["undo close", "restore"]),
    c!(NewFolder, Files, "New folder", "Makes a new folder here and lets you name it.", FolderPlus, [f("ctrl-shift-n")], keywords: &["create", "directory"]),
    c!(NewTextFile, Files, "New text document", "Makes a new empty text file here.", FilePlus, [], keywords: &["create", "txt"]),
    c!(DownloadHere, Files, "Download here…", "Downloads a web address (http or https) into this folder: several connections at once, resumed after a cut line or a restart, shown in the Status Center with pause and cancel. An address on the clipboard is already in the box. From a terminal: leaf download <address> [folder].", Download, [], keywords: &["http", "https", "url", "link", "web", "fetch", "internet"], places: Places::LISTED, since: "0.9.6"),
    c!(SendToDevice, Files, "Send to device", "Sends the selected items to a device nearby: a phone or computer running LocalSend, or another leaf. With several devices in reach, the right-click menu (Send with Nearby) lets you pick; this sends to the one used last, or to the only one found.", Send, [], keywords: &["localsend", "phone", "share", "airdrop", "nearby", "send"], places: Places::LISTED, since: "0.9.6"),
    c!(Refresh, Navigation, "Refresh", "Reads the folder again.", RotateCw, [w("f5"), w("ctrl-r")], keywords: &["reload"], places: Places::LISTED_MENU, menu: &[place(K::Refresh, "Refresh", L::RotateCw, T::BACKGROUND, S::View, 40).hint("Ctrl+R")]),
    c!(MapNetworkDrive, Tools, "Map network drive…", "Gives a shared network folder a drive letter.", Network, [], keywords: &["smb", "share"], platforms: Platforms::WINDOWS),
    c!(RestartApp, App, "Restart leaf", "Closes leaf and starts it again with the same tabs.", RotateCw, [w("ctrl-alt-shift-r")]),
    c!(SaveSession, App, "Save session…", "Saves the open tabs as a session to open later.", Save, [], keywords: &["workspace", "tabs"]),
    c!(DisconnectNetworkDrive, Tools, "Disconnect network drive…", "Removes a mapped network drive.", Network, [], keywords: &["unmap", "smb"], platforms: Platforms::WINDOWS),
    c!(GoBack, Navigation, "Back", "Goes back to the folder before.", ArrowLeft, [w("alt-left"), f("backspace")], keywords: &["history"]),
    c!(GoForward, Navigation, "Forward", "Goes forward again after going back.", ArrowRight, [w("alt-right")], keywords: &["history"]),
    c!(GoUp, Navigation, "Up one level", "Opens the folder this one is in.", ArrowUp, [w("alt-up")], keywords: &["parent"]),
    c!(GoHome, Navigation, "Home", "Opens the Home page.", House, [w("alt-home")]),
    c!(CustomizeHome, View, "Customize Home", "Opens Home with buttons to add, remove, resize and reorder its widgets.", LayoutGrid, [], keywords: &["dashboard", "widgets", "edit layout"], since: "0.9.5"),
    c!(ResetHome, View, "Reset Home", "Puts the Home page back to its default widgets.", RotateCcw, [], keywords: &["dashboard", "widgets", "default"], since: "0.9.5"),
    c!(GoSettings, App, "Settings", "Opens leaf's settings.", Settings, [w("ctrl-,")], keywords: &["options", "preferences"]),
    c!(ShowFeatures, App, "What leaf can do", "Lists every command with what it does and its shortcut.", Sparkles, [], keywords: &["features", "commands", "help", "shortcuts"], since: "0.9.5"),
    c!(GoRecycleBin, Navigation, "Trash", "Opens the Trash.", Trash, [], keywords: &["trash", "deleted"]),
    c!(FocusAddress, Navigation, "Focus address bar", "Puts the keyboard in the address bar to type a path.", TextCursorInput, [w("ctrl-l"), w("alt-d")], keywords: &["path", "location"]),
    c!(FocusSearch, Navigation, "Search", "Searches this folder.", Search, [w("ctrl-f"), w("ctrl-e")], keywords: &["find", "filter"]),
    c!(SearchInContents, Navigation, "Search in file contents", "Searches the text inside the files of this folder and its subfolders (plain text, or a regular expression with regex:); text files only, big and binary files are skipped; hits stream in with the matching line.", Search, [], keywords: &["find", "grep", "text", "regex", "inside"], places: Places::LISTED),
    c!(ToggleHidden, View, "Show/hide hidden items", "Shows or hides hidden files and folders.", Eye, [w("ctrl-h")], keywords: &["dotfiles"], places: Places::LISTED_MENU, menu: &[place(K::ShowHidden, "Show hidden items", L::Eye, T::BACKGROUND, S::Organize, 110).hint("Ctrl+H")]),
    c!(ToggleHideIgnored, View, "Hide git-ignored files", "Inside a git repository, leaves out the files and folders .gitignore ignores (node_modules, target, logs...). The status line says how many are hidden.", EyeOff, [], keywords: &["gitignore", "ignored", "node_modules", "build", "developer"], since: "0.9.5", places: Places::LISTED_MENU, menu: &[place(K::HideIgnored, "Hide git-ignored files", L::EyeOff, T::BACKGROUND, S::Organize, 120)]),
    c!(ViewDetails, View, "Details view", "Shows the folder as a list with columns.", List, [w("ctrl-shift-1")], keywords: &["layout"]),
    c!(ViewGrid, View, "Grid view", "Shows the folder as large icons.", LayoutGrid, [w("ctrl-shift-2")], keywords: &["layout", "icons", "thumbnails"]),
    c!(ViewColumns, View, "Columns view", "Shows the folder and the ones above it side by side.", Columns3, [w("ctrl-shift-3")], keywords: &["layout", "miller"]),
    c!(ViewCards, View, "Cards view", "Shows each item as a card with its details.", LayoutList, [w("ctrl-shift-4")], keywords: &["layout", "tiles"]),
    c!(ViewList, View, "List view", "Shows the folder as a compact list of names.", TextAlignJustify, [w("ctrl-shift-5")], keywords: &["layout"]),
    c!(ZoomIn, View, "Zoom in", "Makes the items larger.", ZoomIn, [f("ctrl-="), f("ctrl-+")], keywords: &["bigger", "size"]),
    c!(AddToShelf, Files, "Add to shelf", "Puts the selected items on the shelf to drop them somewhere else later.", Inbox, [f("ctrl-shift-s")], keywords: &["collect", "basket"], places: Places::LISTED_MENU, menu: &[place(K::Shelf, "Add to shelf", L::Inbox, T::ITEMS, S::Pack, 40).hint("Ctrl+Shift+S").switch(SW_SHELF).when(|c| !c.in_archive)]),
    c!(ShowProjectBrain, Tools, "Show project brain", "Lists what an agent reads in this project: CLAUDE.md, AGENTS.md, the .claude folder (skills, settings, memory) and .mcp.json, with a search by name and the file shown in the preview pane.", Brain, [], keywords: &["claude", "agents", "skills", "memory", "mcp", "ai", "instructions"], places: Places::LISTED),
    c!(BookmarkRemoteFolder, Navigation, "Bookmark remote folder", "Adds the folder shown on a saved host to that host's bookmarks, shown under it in the sidebar; run it again in a bookmarked folder to take the bookmark away.", Bookmark, [], keywords: &["sftp", "ftp", "server", "host", "favorite", "remote"], places: Places::LISTED),
    c!(AddToBasket, Files, "Add to basket", "Puts the selected items in this project's context basket, a named set of files to hand to an assistant later.", ShoppingBasket, [], keywords: &["context", "ai", "llm", "claude", "collect"], places: Places::LISTED_MENU, menu: &[place(K::Basket, "Context basket", L::ShoppingBasket, T::ITEMS, S::Pack, 41).when(|c| !c.in_archive)]),
    c!(RemoveFromBasket, Files, "Remove from basket", "Takes the selected items out of this project's context basket.", ShoppingBasket, [], keywords: &["context"]),
    c!(NewBasket, Files, "New basket", "Starts a named context basket for this project; the selected items go in it.", ShoppingBasket, [], keywords: &["context"]),
    c!(CopyBasketAsContext, CopyPath, "Copy basket as context", "Copies the files of this project's context basket as one text block, with a token estimate.", ShoppingBasket, [], keywords: &["ai", "llm", "prompt", "tokens"]),
    c!(CopyBasketAsMentions, CopyPath, "Copy basket as @paths", "Copies the files of this project's context basket as @path mentions, one per line.", ShoppingBasket, [], keywords: &["ai", "llm", "prompt", "mention"]),
    c!(ToggleShelf, View, "Show/hide shelf", "Shows or hides the shelf.", Inbox, []),
    c!(ToggleFullscreen, Tabs, "Full screen", "Fills the screen with the window.", Maximize, [w("f11")]),
    c!(ToggleAlwaysOnTop, Tabs, "Always on top", "Keeps the window above other windows.", Pin, [], keywords: &["pin"]),
    c!(ToggleCompactOverlay, Tabs, "Compact overlay", "Turns the window into a small one that stays on top.", PictureInPicture2, [], keywords: &["mini", "picture in picture"]),
    c!(ToggleAddressFullPath, View, "Show full path in address bar", "Shows the path as text instead of the clickable parts.", TextCursorInput, [], keywords: &["breadcrumbs"]),
    c!(GoToFolderAlias, Navigation, "Go to folder alias", "Opens a folder by its short name: type @ and the name in the address bar.", AtSign, [], hint: "@", keywords: &["bookmark", "shortcut"]),
    c!(SearchFolderHistory, Navigation, "Search past folders", "Finds a folder you went to before.", Clock, [w("ctrl-p")], keywords: &["history", "recent", "jump"]),
    c!(SearchTabs, Tabs, "Search tabs", "Finds an open tab or one closed recently.", PanelTop, [w("ctrl-shift-a")]),
    c!(RotateRight, Tools, "Rotate right", "Turns the selected pictures a quarter to the right.", RotateCw, [], keywords: &["image", "photo"], places: Places::LISTED_MENU, menu: &[place(K::RotateRight, "Rotate right", L::RotateCw, T::ITEMS, S::Organize, 140).when(|c| c.rotatable)]),
    c!(RotateLeft, Tools, "Rotate left", "Turns the selected pictures a quarter to the left.", RotateCcw, [], keywords: &["image", "photo"], places: Places::LISTED_MENU, menu: &[place(K::RotateLeft, "Rotate left", L::RotateCcw, T::ITEMS, S::Organize, 150).when(|c| c.rotatable)]),
    c!(ZoomOut, View, "Zoom out", "Makes the items smaller.", ZoomOut, [f("ctrl--")], keywords: &["smaller", "size"]),
    c!(ToggleDualPane, Panes, "Toggle dual pane", "Splits the tab into two folders side by side, or joins them again.", Columns2, [w("ctrl-shift-d")], keywords: &["split", "commander"]),
    c!(SwitchPane, Panes, "Switch pane", "Moves the keyboard to the other pane.", ArrowLeftRight, [w("f6")]),
    c!(SplitPaneSideBySide, Panes, "Split pane side by side", "Splits the current pane into two, left and right.", Columns2, []),
    c!(SplitPaneStacked, Panes, "Split pane top and bottom", "Splits the current pane into two, one above the other.", Rows2, []),
    c!(CloseActivePane, Panes, "Close active pane", "Closes the pane that has the keyboard.", X, [w("ctrl-shift-w")]),
    c!(TogglePreview, View, "Preview pane", "Shows or hides the preview of the selected file.", PanelRight, [w("alt-p")]),
    c!(ToggleSidebar, View, "Sidebar", "Shows or hides the sidebar.", PanelLeft, [w("ctrl-b")]),
    c!(OpenTerminal, Tools, "Open in terminal", "Opens a terminal in this folder; in a server (sftp://) folder it runs ssh into it.", SquareTerminal, [w("ctrl-shift-`")], keywords: &["console", "shell", "command prompt"], places: Places::LISTED_MENU, menu: &[place(K::Terminal, "Open in terminal", L::SquareTerminal, T::BACKGROUND, S::Tools, 10).hint("Ctrl+Shift+`"), place(K::Terminal, "Open in terminal", L::SquareTerminal, T::FOLDER, S::Terminal, 10).hint("Ctrl+Shift+`").switch(SW_TERMINAL)]),
    c!(OpenTerminalAdmin, Tools, "Open in Terminal as administrator", "Opens an elevated terminal in this folder.", ShieldCheck, [], keywords: &["elevated", "admin"], platforms: Platforms::WINDOWS),
    c!(OpenInIde, Tools, "Open in editor", "Opens this folder in your code editor.", Code, [], keywords: &["ide", "vscode"]),
    c!(CompareFolders, Tools, "Compare folders", "Marks what differs between the two panes' folders: only here, newer, older, other size.", Columns2, [], keywords: &["diff", "difference", "sync", "dual pane"], since: "0.9.5"),
    c!(FindDuplicates, Tools, "Find duplicates", "Looks for files with identical content in this folder or the selected folders.", Copy, [], keywords: &["duplicate", "same files", "identical", "free space", "clean up"], since: "0.9.5", places: Places::LISTED_MENU, menu: &[place(K::FindDuplicates, "Find duplicates", L::Copy, T::BACKGROUND, S::Organize, 90).when(|c| !c.in_search && !c.archive_path && !c.ftp), place(K::FindDuplicates, "Find duplicates", L::Copy, T::FOLDER, S::Organize, 80).when(|c| !c.archive_path && !c.ftp)]),
    c!(FindBuildJunk, Tools, "Find build junk", "Looks in this folder for node_modules, target, .venv, caches and other folders a build makes again, with their size, and moves the ones you tick to the Trash.", Eraser, [], keywords: &["node_modules", "target", "venv", "pycache", "clean up", "free space", "developer", "build"], since: "0.9.5", places: Places::LISTED_MENU, menu: &[place(K::FindBuildJunk, "Find build junk", L::Eraser, T::BACKGROUND, S::Organize, 100).when(|c| !c.in_search && !c.archive_path && !c.ftp), place(K::FindBuildJunk, "Find build junk", L::Eraser, T::FOLDER, S::Organize, 90).when(|c| !c.archive_path && !c.ftp)]),
    c!(FollowRemoteFile, Tools, "Follow file on a server", "In a server (sftp://) folder: keeps reading the end of the file under the cursor every 2 seconds and shows it in leaf-look (a log that grows). Press it again to stop; it stops when the tab closes.", Eye, [f("alt-f")], keywords: &["tail", "log", "follow", "ssh"], since: "0.9.5"),
    c!(ShowChanges, Tools, "Show changes", "Lists every changed, new, deleted and renamed file of the git repository this folder is in, grouped by folder, with the difference of each in the preview pane. It only reads, apart from \"Revert this file\" on a row, which asks first and keeps your version in the Trash.", GitCompare, [], keywords: &["git", "diff", "changes", "modified", "status", "uncommitted", "developer"], since: "0.9.5"),
    c!(Checkpoint, Tools, "Checkpoint", "Saves how this folder is now, to put it back later with one restore. In a git repository it is a hidden snapshot inside the repository (your files, the index, the branch and the stash stay as they are; ignored files are left out); elsewhere a copy in leaf's data folder, with its size shown first.", FolderClock, [], keywords: &["snapshot", "backup", "restore", "undo", "save state", "agent", "developer"], since: "0.9.6", places: Places::LISTED_MENU, menu: &[place(K::Checkpoint, "Checkpoint", L::FolderClock, T::BACKGROUND, S::Organize, 101).when(|c| !c.in_search && !c.archive_path && !c.ftp), place(K::Checkpoint, "Checkpoint", L::FolderClock, T::FOLDER, S::Organize, 91).when(|c| !c.archive_path && !c.ftp)]),
    c!(ShowCheckpoints, Tools, "Checkpoints", "Lists the checkpoints of this folder (or of its git repository) with their time, note and size; each can be restored after a look at what it would change, and the restore can be taken back.", ArchiveRestore, [], keywords: &["snapshot", "backup", "restore", "undo", "developer"], since: "0.9.6"),
    c!(ShowAgents, Tools, "Agents", "Lists the coding agent sessions running now and the recent ones (Claude Code), grouped by project folder, with their branch, state (running, waiting for you, idle, ended) and last activity; open a session's folder or a terminal in it. Read from the agent's own session files; needs the agent layer on (Settings).", Brain, [], keywords: &["agent", "claude", "ai", "session", "lane", "worktree"], since: "0.9.6"),
    c!(SyncJobs, Tools, "Sync jobs", "Lists the saved sync and backup jobs (Mirror, Backup, Two-way; between folders on this computer or with a server) with their last run; each can be previewed row by row, run in the Status Center, edited or deleted. Removed and replaced files go to the Trash, on a server into .leaf-sync-trash.", FolderSync, [], keywords: &["sync", "backup", "mirror", "two-way", "synchronize", "robocopy", "rsync"], since: "0.9.6"),
    c!(SyncWith, Tools, "Sync with…", "Starts a sync job from this folder (or the selected folder), with the other pane's folder as the right side: choose Mirror, Backup or Two-way and the filters, then preview what it would do before anything is copied.", FolderSync, [], keywords: &["sync", "backup", "mirror", "synchronize", "dual pane"], since: "0.9.6", places: Places::LISTED_MENU, menu: &[place(K::SyncWith, "Sync with…", L::FolderSync, T::FOLDER, S::Organize, 92).when(|c| !c.archive_path)]),
    c!(OpenDocker, Tools, "Docker containers", "Lists the Docker containers (running or stopped) as folders; a container lists its bind mounts and volumes, which open as the folders they stand for. In a folder on an sftp:// host it asks that host. It only reads.", Container, [], keywords: &["docker", "container", "volume", "mount", "developer"], since: "0.9.6"),
    c!(ShowWorktrees, Tools, "Worktrees", "Lists the worktrees of the git repository this folder is in, by branch, to jump to one. It only reads.", GitBranch, [], keywords: &["git", "worktree", "branch", "checkout", "developer"], since: "0.9.5"),
    c!(NewWorktree, Tools, "New worktree…", "Adds a second working folder to the git repository this folder is in, on a new branch from the current head or on an existing branch, and opens it in a new tab. Nothing that exists is changed.", GitBranchPlus, [], keywords: &["git", "worktree", "branch", "developer"], since: "0.9.5"),
    c!(OrganizeFolder, Tools, "Organize folder", "Sorts the folder's files into folders by kind.", WandSparkles, [], keywords: &["tidy", "sort", "clean up"], places: Places::LISTED_MENU, menu: &[place(K::OrganizeFolder, "Organize folder", L::WandSparkles, T::BACKGROUND, S::Tools, 20).when(|c| !c.in_search && c.organizer)]),
    c!(ToggleTheme, View, "Light / dark theme", "Switches between the light and the dark theme.", SunMoon, [w("ctrl-alt-t")], keywords: &["dark mode"]),
    c!(OpenInNewWindow, Tabs, "Open in new window", "Opens the selected folder in a new window.", AppWindow, [f("ctrl-alt-enter")]),
    c!(SelectAll, Selection, "Select all", "Selects every item in the folder.", SquareCheck, [f("ctrl-a")]),
    c!(InvertSelection, Selection, "Invert selection", "Selects what is not selected, and the other way round.", ArrowLeftRight, []),
    c!(ToggleSelection, Selection, "Toggle item selection", "Adds the item under the cursor to the selection or takes it out.", SquareCheck, [f("ctrl-space")]),
    c!(CopyItems, Files, "Copy", "Copies the selected items to paste them somewhere else.", Copy, [f("ctrl-c")]),
    c!(CutItems, Files, "Cut", "Takes the selected items to paste them somewhere else.", Scissors, [f("ctrl-x")], keywords: &["move"]),
    c!(PasteItems, Files, "Paste", "Puts the copied or cut items into this folder.", ClipboardPaste, [f("ctrl-v")]),
    c!(PasteIntoSelected, Files, "Paste into selected folder", "Puts the copied items into the selected folder.", ClipboardPaste, [f("ctrl-shift-v")]),
    c!(PasteShortcut, Files, "Paste as shortcut", "Makes shortcuts to the copied items here.", Link, [], keywords: &["link"], places: Places::LISTED_MENU, menu: &[place(K::PasteShortcut, "Paste shortcut", L::ClipboardPaste, T::BACKGROUND, S::Organize, 40).when(|c| !c.in_archive)]),
    c!(DeleteItems, Files, "Move to Trash", "Moves the selected items to the Trash.", Trash, [f("delete"), f("ctrl-d")], keywords: &["delete", "remove"]),
    c!(DeletePermanently, Files, "Delete permanently", "Deletes the selected items without the Trash.", TrashOff, [f("shift-delete")], keywords: &["remove", "erase"]),
    c!(RenameItem, Files, "Rename", "Renames the selected item.", Pencil, [f("f2")]),
    c!(DuplicateItems, Files, "Duplicate", "Makes a copy of the selected items next to them.", CopyPlus, [f("ctrl-alt-d")], places: Places::LISTED_MENU, menu: &[place(K::Duplicate, "Duplicate", L::CopyPlus, T::ITEMS, S::Organize, 40).hint("Ctrl+Alt+D").when(|c| !c.in_archive)]),
    c!(CopyToOtherPane, Panes, "Copy to other pane", "Copies the selected items into the other pane's folder.", CopyPlus, [f("shift-f5")], places: Places::LISTED_MENU, menu: &[place(K::CopyToPane, "Copy to other pane", L::CopyPlus, T::ITEMS, S::Pack, 70).hint("Shift+F5").when(|c| c.dual && !c.in_archive)]),
    c!(MoveToOtherPane, Panes, "Move to other pane", "Moves the selected items into the other pane's folder.", MoveRight, [f("shift-f6")], places: Places::LISTED_MENU, menu: &[place(K::MoveToPane, "Move to other pane", L::MoveRight, T::ITEMS, S::Pack, 80).hint("Shift+F6").when(|c| c.dual && !c.in_archive)]),
    c!(Undo, Files, "Undo", "Takes back the last copy, move, rename or delete.", Undo2, [f("ctrl-z")], places: Places::LISTED_MENU, menu: &[place(K::Undo, "Undo", L::Undo2, T::BACKGROUND, S::Organize, 10).hint("Ctrl+Z").when(|c| c.undo.is_some())]),
    c!(CopyPath, CopyPath, "Copy path", "Copies the full path of the selected items.", Link, [f("ctrl-shift-c")], places: Places::LISTED_MENU, menu: &[place(K::CopyPath, "Copy path", L::ClipboardCopy, T::BACKGROUND, S::CopyPath, 60), place(K::CopyPath, "Copy item path", L::ClipboardCopy, T::ITEMS, S::CopyPath, 10).hint("Ctrl+Shift+C").switch(SW_COPY_PATH)]),
    c!(CopyPathQuoted, CopyPath, "Copy path as quoted", "Copies the paths in quotes, ready for a command line.", Link, [f("ctrl-alt-c")]),
    c!(CopyPathName, CopyPath, "Copy path as name only", "Copies just the names of the selected items.", Link, [], since: "0.9.5"),
    c!(CopyPathFolder, CopyPath, "Copy path as folder path", "Copies the path of the folder the items are in.", Link, [], since: "0.9.5"),
    c!(CopyPathSlashes, CopyPath, "Copy path with forward slashes", "Copies the paths written with / instead of \\.", Link, [], since: "0.9.5"),
    c!(CopyPathRelative, CopyPath, "Copy path relative to repository", "Copies the paths from the root of their Git repository.", Link, [], keywords: &["git"], since: "0.9.5"),
    c!(CopyPathAt, CopyPath, "Copy path as @path", "Copies the paths with an @ in front, as coding assistants take them.", Link, [], keywords: &["ai", "agent"], since: "0.9.5"),
    c!(CopyPathWsl, CopyPath, "Copy path as WSL path", "Copies the paths as Linux under WSL sees them (/mnt/c/...).", Link, [], keywords: &["linux"], since: "0.9.5", platforms: Platforms::WINDOWS),
    c!(CopyPathUrl, CopyPath, "Copy path as file URL", "Copies the paths as file:/// addresses.", Link, [], keywords: &["uri", "link"], since: "0.9.5"),
    c!(CopyAsContext, CopyPath, "Copy as context", "Copies the selected files (or this folder) as one text block for a chat: a tree, then each text file under its path. Leaves out .gitignore'd, binary, big and secret files.", FileText, [], keywords: &["ai", "llm", "prompt", "claude", "chatgpt", "tokens", "agent"], since: "0.9.5", places: Places::LISTED_MENU, menu: &[place(K::CopyAsContext, "Copy as context", L::FileText, T::BACKGROUND, S::Organize, 70).when(|c| !c.in_archive), place(K::CopyAsContext, "Copy as context", L::FileText, T::ITEMS, S::CopyPath, 30).when(|c| !c.in_archive)]),
    c!(OpenWith, Files, "Open with…", "Opens the selected file in a program you choose.", AppWindow, [], places: Places::LISTED_MENU, menu: &[place(K::OpenWith, "Open with", L::LayoutGrid, T::ITEMS, S::Open, 50).sub().when(|c| c.has_files)]),
    c!(Properties, Files, "Properties", "Shows the details of the selected items.", Info, [f("alt-enter")], keywords: &["info", "size", "attributes"]),
    c!(ShowContextMenu, Files, "Context menu", "Opens the right-click menu for the selected items.", Menu, [f("shift-f10"), f("menu")], keywords: &["right click"]),
    // ---- on the page only
    c!(OpenSelected, Navigation, "Open", "Opens the selected file or folder.", FolderOpen, [f("enter")], places: Places::PAGE_MENU, menu: &[place(K::Open, "Open", L::SquareArrowOutUpRight, T::ITEMS, S::Open, 10).hint("Enter")]),
    c!(FocusNextPane, Navigation, "Next part of the window", "Moves the keyboard to the next part: address bar, folder tree, file list.", ArrowRight, [f("tab")], places: PAGE_ONLY),
    c!(FocusPrevPane, Navigation, "Previous part of the window", "Moves the keyboard to the part before.", ArrowLeft, [f("shift-tab"), tr("shift-tab")], places: PAGE_ONLY),
    c!(NextTab, Tabs, "Next tab", "Goes to the tab on the right.", ArrowRight, [w("ctrl-tab")], places: PAGE_ONLY),
    c!(PrevTab, Tabs, "Previous tab", "Goes to the tab on the left.", ArrowLeft, [w("ctrl-shift-tab")], places: PAGE_ONLY),
    c!(SelectNext, Selection, "Select next item", "Moves the selection down by one.", ArrowDown, [f("down")], places: PAGE_ONLY),
    c!(SelectPrev, Selection, "Select previous item", "Moves the selection up by one.", ArrowUp, [f("up")], places: PAGE_ONLY),
    c!(SelectLeft, Selection, "Select item to the left", "Moves the selection left in the grid, or out of a folder in the columns view.", ArrowLeft, [f("left")], places: PAGE_ONLY),
    c!(SelectRight, Selection, "Select item to the right", "Moves the selection right in the grid, or into a folder in the columns view.", ArrowRight, [f("right")], places: PAGE_ONLY),
    c!(SelectFirst, Selection, "Select first item", "Selects the first item of the folder.", ArrowUp, [f("home")], places: PAGE_ONLY),
    c!(SelectLast, Selection, "Select last item", "Selects the last item of the folder.", ArrowDown, [f("end")], places: PAGE_ONLY),
    c!(ExtendNext, Selection, "Extend selection down", "Adds the next item to the selection.", ArrowDown, [f("shift-down")], places: PAGE_ONLY),
    c!(ExtendPrev, Selection, "Extend selection up", "Adds the item above to the selection.", ArrowUp, [f("shift-up")], places: PAGE_ONLY),
    c!(PageDown, Selection, "Page down", "Moves the selection down by a screenful.", ArrowDown, [f("pagedown")], places: PAGE_ONLY),
    c!(PageUp, Selection, "Page up", "Moves the selection up by a screenful.", ArrowUp, [f("pageup")], places: PAGE_ONLY),
    c!(ClearSelection, Selection, "Clear selection", "Selects nothing.", X, [f("escape")], places: PAGE_ONLY),
    c!(Redo, Files, "Redo", "Does again what Undo took back.", Redo2, [f("ctrl-y")], places: Places::PAGE_MENU, menu: &[place(K::Redo, "Redo", L::Redo2, T::BACKGROUND, S::Organize, 20).hint("Ctrl+Y").when(|c| c.redo.is_some())]),
    c!(ToggleToolbar, View, "Tool bar", "Shows or hides the tool bar.", PanelTop, [w("ctrl-shift-b")], places: PAGE_ONLY),
    c!(ToggleCommandPalette, App, "Command palette", "Finds any command by typing a few letters of its name, or a path to go to.", Command, [w("ctrl-shift-p"), w("f1")], places: PAGE_ONLY),
    // ---- keyboard plumbing (bound, not listed)
    c!(LeaveTree, Navigation, "Leave the folder tree", "Tab in the folder tree moves on to the file list.", FolderTree, [tr("tab")], places: Places::NONE),
    c!(DialogNext, App, "Next button", "Tab in a question dialog moves to the next button.", ArrowRight, [dl("tab")], places: Places::NONE),
    c!(DialogPrev, App, "Previous button", "Shift+Tab in a question dialog moves to the button before.", ArrowLeft, [dl("shift-tab")], places: Places::NONE),
];

/// The command whose action is `leaf::{id}`.
#[allow(dead_code)]
pub fn by_id(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The user's own actions as rows, set by `custom_actions` when `actions.toml` is read. The rows are made once per change of the
/// file and live to the end of the program (a few hundred bytes each time the file changes).
static CUSTOM: std::sync::RwLock<Vec<&'static CommandSpec>> = std::sync::RwLock::new(Vec::new());

pub fn set_custom(rows: Vec<&'static CommandSpec>) {
    *CUSTOM.write().unwrap_or_else(|e| e.into_inner()) = rows;
}

/// The row of one of the user's own actions (`actions.toml`). Made once for each distinct action and kept for the life of the program (a row is reused when the file is read again).
pub fn custom_row(name: &str, description: &str, shortcut: Option<&str>, icon: L, palette: bool, toolbar: bool) -> &'static CommandSpec {
    fn leak(s: &str) -> &'static str {
        Box::leak(s.to_string().into_boxed_str())
    }
    // the same row again (the file was read again, nothing in this action changed): the row made before, nothing leaked
    static MADE: std::sync::Mutex<Vec<(String, &'static CommandSpec)>> = std::sync::Mutex::new(Vec::new());
    let sig = format!("{name}\0{description}\0{}\0{icon:?}\0{palette}{toolbar}", shortcut.unwrap_or(""));
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, spec)) = made.iter().find(|(s, _)| *s == sig) {
        return spec;
    }
    let keys: &'static [Key] = match shortcut {
        Some(k) => Box::leak(Box::new([Key { keys: leak(k), ctx: KeyCtx::Window }])),
        None => &[],
    };
    let mut places = Places::PAGE;
    if palette {
        places = Places(places.0 | Places::PALETTE.0);
    }
    if toolbar {
        places = Places(places.0 | Places::TOOLBAR.0);
    }
    let spec: &'static CommandSpec = Box::leak(Box::new(CommandSpec {
        id: leak(&format!("{}{name}", crate::custom_actions::ID_PREFIX)),
        title: leak(name),
        description: leak(description),
        category: Category::Custom,
        icon,
        default_keys: keys,
        places,
        custom: leak(name),
        ..BASE
    }));
    made.push((sig, spec));
    spec
}

/// The rows of the user's own actions (every one, whether the palette lists it or not).
pub fn custom() -> Vec<&'static CommandSpec> {
    CUSTOM.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The built-in rows, then the user's own.
pub fn all() -> impl Iterator<Item = &'static CommandSpec> {
    COMMANDS.iter().chain(custom())
}

/// The commands the palette lists, in its order: the built-in ones, then the user's own that asked for the palette.
pub fn palette() -> impl Iterator<Item = &'static CommandSpec> {
    all().filter(|c| c.places.contains(Places::PALETTE))
}

/// The default key bindings, every row's keys in table order.
pub fn default_bindings() -> Vec<KeyBinding> {
    let mut out = Vec::with_capacity(COMMANDS.len() + 16);
    for c in COMMANDS {
        for k in c.default_keys {
            out.push((c.bind)(k.keys, k.ctx.context()));
        }
    }
    out
}

/// Bindings that switch off the default keys of the command `id` (a shortcut of the user's replaces them), except `keep`. They come
/// after the defaults, and a `NoAction` binding in the same context blocks the ones before it.
pub fn unbind_defaults(id: &str, keep: &str) -> Vec<KeyBinding> {
    let Some(c) = COMMANDS.iter().find(|c| c.id == id) else { return Vec::new() };
    c.default_keys.iter().filter(|k| !k.keys.eq_ignore_ascii_case(keep)).map(|k| KeyBinding::new(k.keys, gpui_kit::NoAction, k.ctx.context())).collect()
}

/// Every command for the shortcut editor: (title, default keys as shown, action id).
pub fn shortcut_entries() -> Vec<(&'static str, String, String)> {
    // (the user's own actions are all here, even those the palette does not list: their shortcut is edited here)
    COMMANDS.iter().filter(|c| c.places.contains(Places::PALETTE)).chain(custom()).map(|c| (c.title, c.shown_keys(), c.id.to_string())).collect()
}

/// `FEATURES.md`: the listed commands grouped as on the page.
#[cfg_attr(not(test), allow(dead_code))]
pub fn features_markdown() -> String {
    let mut s = String::from(
        "# What leaf can do\n\n\
         Generated from the command registry (`crates/rf-app/src/registry.rs`); do not edit by hand.\n\
         The same list is in leaf under Settings > What leaf can do, and in the command palette (Ctrl+Shift+P).\n\
         To write it again: `LEAF_WRITE_FEATURES=1 cargo test -p rf-app features_md`.\n",
    );
    for cat in Category::ALL {
        let rows: Vec<&CommandSpec> = COMMANDS.iter().filter(|c| c.category == cat && c.places.contains(Places::PAGE)).collect();
        if rows.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {}\n\n| Command | What it does | Shortcut |\n|---|---|---|\n", cat.title()));
        for c in rows {
            // (a key that is itself a backtick needs a doubled one around it)
            let code = |k: String| if k.contains('`') { format!("`` {k} ``") } else { format!("`{k}`") };
            let keys = c.default_keys.iter().map(|k| code(shown(k.keys))).chain((!c.hint.is_empty()).then(|| code(c.hint.to_string()))).collect::<Vec<_>>().join(", ");
            let only = c.platforms.label();
            let desc = if only.is_empty() { c.description.to_string() } else { format!("{} ({only} only)", c.description) };
            s.push_str(&format!("| {} | {} | {} |\n", c.title.trim_end_matches('…'), desc.replace('|', "\\|"), keys));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The names inside `actions!( leaf, [ ... ] )` in actions.rs.
    fn declared_actions() -> Vec<String> {
        let src = include_str!("actions.rs");
        let start = src.find('[').unwrap();
        let end = src[start..].find(']').unwrap() + start;
        src[start + 1..end].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }

    #[test]
    fn every_action_has_exactly_one_row_and_no_default_key_is_bound_twice() {
        let declared = declared_actions();
        assert!(declared.len() > 90, "the actions! list was read: {}", declared.len());
        for a in &declared {
            let n = COMMANDS.iter().filter(|c| c.id == a).count();
            assert_eq!(n, 1, "{a} has {n} registry rows");
        }
        for c in COMMANDS {
            assert!(declared.iter().any(|a| a == c.id), "{} is not in actions!", c.id);
            assert_eq!((c.action)().name(), format!("leaf::{}", c.id), "{}: id and action differ", c.id);
            assert!(!c.title.is_empty() && c.description.ends_with('.'), "{}: title and a one-sentence description", c.id);
        }
        let mut seen = HashSet::new();
        for c in COMMANDS {
            for k in c.default_keys {
                assert!(seen.insert((k.keys, k.ctx)), "{} in {:?} is bound twice (again by {})", k.keys, k.ctx, c.id);
            }
        }
    }

    #[test]
    fn menu_rows_hang_on_commands_that_say_so() {
        for c in COMMANDS {
            assert_eq!(!c.menu.is_empty(), c.places.contains(Places::MENU), "{}: places and menu rows differ", c.id);
        }
    }

    #[test]
    fn shown_keys_read_as_before() {
        let keys = |id| by_id(id).unwrap().shown_keys();
        assert_eq!(keys("GoBack"), "Alt+←");
        assert_eq!(keys("DeleteItems"), "Del");
        assert_eq!(keys("DeletePermanently"), "Shift+Del");
        assert_eq!(keys("ZoomOut"), "Ctrl+-");
        assert_eq!(keys("GoSettings"), "Ctrl+,");
        assert_eq!(keys("GoToFolderAlias"), "@");
        assert_eq!(keys("OpenTerminal"), "Ctrl+Shift+`");
        assert_eq!(keys("CloseOtherTabs"), "");
    }

    /// `FEATURES.md` in the repository root is the registry written out (`LEAF_WRITE_FEATURES=1` writes it again).
    #[test]
    fn features_md_is_up_to_date() {
        let want = features_markdown();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../FEATURES.md");
        if std::env::var_os("LEAF_WRITE_FEATURES").is_some() {
            std::fs::write(path, &want).unwrap();
        }
        let have = std::fs::read_to_string(path).unwrap_or_default().replace("\r\n", "\n");
        assert!(have == want, "FEATURES.md is out of date: run `LEAF_WRITE_FEATURES=1 cargo test -p rf-app features_md`");
    }
}
