//! The command registry: one constant table of every command leaf has ([`COMMANDS`]). The command palette, the default key
//! bindings, the shortcut editor, the "What leaf can do" page and `FEATURES.md` are all made from it, so a command is described
//! once. A row's `id` is its action's type name (`NewTab`): the user's own bindings in `settings.toml` `[keybindings]` name
//! actions that way, so they keep working.
//!
//! The table is a `static` of plain data: nothing runs and nothing is allocated for it at start-up.

use gpui_kit::assets::IconName as L;
use gpui_kit::{Action, App, KeyBinding};

use crate::actions::{DIALOG_CTX, FOLDER_CTX, TREE_CTX};

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
}

impl Category {
    pub const ALL: [Category; 9] =
        [Category::Tabs, Category::Navigation, Category::Selection, Category::Files, Category::CopyPath, Category::View, Category::Panes, Category::Tools, Category::App];

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
    /// The context menu (not generated from the registry yet).
    pub const MENU: Places = Places(4);
    /// The tool bar (not generated from the registry yet).
    pub const TOOLBAR: Places = Places(8);
    pub const LISTED: Places = Places(1 | 2);

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
    /// Whether it is offered right now; `None`: always.
    pub when: Option<fn(&App) -> bool>,
    /// Makes the action.
    pub action: fn() -> Box<dyn Action>,
    /// Makes one of its key bindings (`KeyBinding::new` wants the action's own type).
    pub bind: fn(&'static str, Option<&'static str>) -> KeyBinding,
    /// The leaf version it came with (for a later "What's new" page).
    #[allow(dead_code)]
    pub since: &'static str,
    pub platforms: Platforms,
}

impl CommandSpec {
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
    when: None,
    action: || Box::new(crate::actions::ToggleCommandPalette),
    bind: |k, c| KeyBinding::new(k, crate::actions::ToggleCommandPalette, c),
    since: "0.9.0",
    platforms: Platforms::ALL,
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
    c!(Refresh, Navigation, "Refresh", "Reads the folder again.", RotateCw, [w("f5"), w("ctrl-r")], keywords: &["reload"]),
    c!(MapNetworkDrive, Tools, "Map network drive…", "Gives a shared network folder a drive letter.", Network, [], keywords: &["smb", "share"], platforms: Platforms::WINDOWS),
    c!(RestartApp, App, "Restart leaf", "Closes leaf and starts it again with the same tabs.", RotateCw, [w("ctrl-alt-shift-r")]),
    c!(SaveSession, App, "Save session…", "Saves the open tabs as a session to open later.", Save, [], keywords: &["workspace", "tabs"]),
    c!(DisconnectNetworkDrive, Tools, "Disconnect network drive…", "Removes a mapped network drive.", Network, [], keywords: &["unmap", "smb"], platforms: Platforms::WINDOWS),
    c!(GoBack, Navigation, "Back", "Goes back to the folder before.", ArrowLeft, [w("alt-left"), f("backspace")], keywords: &["history"]),
    c!(GoForward, Navigation, "Forward", "Goes forward again after going back.", ArrowRight, [w("alt-right")], keywords: &["history"]),
    c!(GoUp, Navigation, "Up one level", "Opens the folder this one is in.", ArrowUp, [w("alt-up")], keywords: &["parent"]),
    c!(GoHome, Navigation, "Home", "Opens the Home page.", House, [w("alt-home")]),
    c!(GoSettings, App, "Settings", "Opens leaf's settings.", Settings, [w("ctrl-,")], keywords: &["options", "preferences"]),
    c!(ShowFeatures, App, "What leaf can do", "Lists every command with what it does and its shortcut.", Sparkles, [], keywords: &["features", "commands", "help", "shortcuts"], since: "0.9.4"),
    c!(GoRecycleBin, Navigation, "Recycle Bin", "Opens the Recycle Bin.", Trash, [], keywords: &["trash", "deleted"]),
    c!(FocusAddress, Navigation, "Focus address bar", "Puts the keyboard in the address bar to type a path.", TextCursorInput, [w("ctrl-l"), w("alt-d")], keywords: &["path", "location"]),
    c!(FocusSearch, Navigation, "Search", "Searches this folder.", Search, [w("ctrl-f"), w("ctrl-e")], keywords: &["find", "filter"]),
    c!(ToggleHidden, View, "Show/hide hidden items", "Shows or hides hidden files and folders.", Eye, [w("ctrl-h")], keywords: &["dotfiles"]),
    c!(ViewDetails, View, "Details view", "Shows the folder as a list with columns.", List, [w("ctrl-shift-1")], keywords: &["layout"]),
    c!(ViewGrid, View, "Grid view", "Shows the folder as large icons.", LayoutGrid, [w("ctrl-shift-2")], keywords: &["layout", "icons", "thumbnails"]),
    c!(ViewColumns, View, "Columns view", "Shows the folder and the ones above it side by side.", Columns3, [w("ctrl-shift-3")], keywords: &["layout", "miller"]),
    c!(ViewCards, View, "Cards view", "Shows each item as a card with its details.", LayoutList, [w("ctrl-shift-4")], keywords: &["layout", "tiles"]),
    c!(ViewList, View, "List view", "Shows the folder as a compact list of names.", TextAlignJustify, [w("ctrl-shift-5")], keywords: &["layout"]),
    c!(ZoomIn, View, "Zoom in", "Makes the items larger.", ZoomIn, [f("ctrl-="), f("ctrl-+")], keywords: &["bigger", "size"]),
    c!(AddToShelf, Files, "Add to shelf", "Puts the selected items on the shelf to drop them somewhere else later.", Inbox, [f("ctrl-shift-s")], keywords: &["collect", "basket"]),
    c!(ToggleShelf, View, "Show/hide shelf", "Shows or hides the shelf.", Inbox, []),
    c!(ToggleFullscreen, Tabs, "Full screen", "Fills the screen with the window.", Maximize, [w("f11")]),
    c!(ToggleAlwaysOnTop, Tabs, "Always on top", "Keeps the window above other windows.", Pin, [], keywords: &["pin"]),
    c!(ToggleCompactOverlay, Tabs, "Compact overlay", "Turns the window into a small one that stays on top.", PictureInPicture2, [], keywords: &["mini", "picture in picture"]),
    c!(ToggleAddressFullPath, View, "Show full path in address bar", "Shows the path as text instead of the clickable parts.", TextCursorInput, [], keywords: &["breadcrumbs"]),
    c!(GoToFolderAlias, Navigation, "Go to folder alias", "Opens a folder by its short name: type @ and the name in the address bar.", AtSign, [], hint: "@", keywords: &["bookmark", "shortcut"]),
    c!(SearchFolderHistory, Navigation, "Search past folders", "Finds a folder you went to before.", Clock, [w("ctrl-p")], keywords: &["history", "recent", "jump"]),
    c!(SearchTabs, Tabs, "Search tabs", "Finds an open tab or one closed recently.", PanelTop, [w("ctrl-shift-a")]),
    c!(RotateRight, Tools, "Rotate right", "Turns the selected pictures a quarter to the right.", RotateCw, [], keywords: &["image", "photo"]),
    c!(RotateLeft, Tools, "Rotate left", "Turns the selected pictures a quarter to the left.", RotateCcw, [], keywords: &["image", "photo"]),
    c!(ZoomOut, View, "Zoom out", "Makes the items smaller.", ZoomOut, [f("ctrl--")], keywords: &["smaller", "size"]),
    c!(ToggleDualPane, Panes, "Toggle dual pane", "Splits the tab into two folders side by side, or joins them again.", Columns2, [w("ctrl-shift-d")], keywords: &["split", "commander"]),
    c!(SwitchPane, Panes, "Switch pane", "Moves the keyboard to the other pane.", ArrowLeftRight, [w("f6")]),
    c!(SplitPaneSideBySide, Panes, "Split pane side by side", "Splits the current pane into two, left and right.", Columns2, []),
    c!(SplitPaneStacked, Panes, "Split pane top and bottom", "Splits the current pane into two, one above the other.", Rows2, []),
    c!(CloseActivePane, Panes, "Close active pane", "Closes the pane that has the keyboard.", X, [w("ctrl-shift-w")]),
    c!(TogglePreview, View, "Preview pane", "Shows or hides the preview of the selected file.", PanelRight, [w("alt-p")]),
    c!(ToggleSidebar, View, "Sidebar", "Shows or hides the sidebar.", PanelLeft, [w("ctrl-b")]),
    c!(OpenTerminal, Tools, "Open in terminal", "Opens a terminal in this folder.", SquareTerminal, [w("ctrl-shift-`")], keywords: &["console", "shell", "command prompt"]),
    c!(OpenTerminalAdmin, Tools, "Open in Terminal as administrator", "Opens an elevated terminal in this folder.", ShieldCheck, [], keywords: &["elevated", "admin"], platforms: Platforms::WINDOWS),
    c!(OpenInIde, Tools, "Open in editor", "Opens this folder in your code editor.", Code, [], keywords: &["ide", "vscode"]),
    c!(OrganizeFolder, Tools, "Organize folder", "Sorts the folder's files into folders by kind.", WandSparkles, [], keywords: &["tidy", "sort", "clean up"]),
    c!(ToggleTheme, View, "Light / dark theme", "Switches between the light and the dark theme.", SunMoon, [w("ctrl-alt-t")], keywords: &["dark mode"]),
    c!(OpenInNewWindow, Tabs, "Open in new window", "Opens the selected folder in a new window.", AppWindow, [f("ctrl-alt-enter")]),
    c!(SelectAll, Selection, "Select all", "Selects every item in the folder.", SquareCheck, [f("ctrl-a")]),
    c!(InvertSelection, Selection, "Invert selection", "Selects what is not selected, and the other way round.", ArrowLeftRight, []),
    c!(ToggleSelection, Selection, "Toggle item selection", "Adds the item under the cursor to the selection or takes it out.", SquareCheck, [f("ctrl-space")]),
    c!(CopyItems, Files, "Copy", "Copies the selected items to paste them somewhere else.", Copy, [f("ctrl-c")]),
    c!(CutItems, Files, "Cut", "Takes the selected items to paste them somewhere else.", Scissors, [f("ctrl-x")], keywords: &["move"]),
    c!(PasteItems, Files, "Paste", "Puts the copied or cut items into this folder.", ClipboardPaste, [f("ctrl-v")]),
    c!(PasteIntoSelected, Files, "Paste into selected folder", "Puts the copied items into the selected folder.", ClipboardPaste, [f("ctrl-shift-v")]),
    c!(PasteShortcut, Files, "Paste as shortcut", "Makes shortcuts to the copied items here.", Link, [], keywords: &["link"]),
    c!(DeleteItems, Files, "Move to Recycle Bin", "Moves the selected items to the Recycle Bin.", Trash, [f("delete"), f("ctrl-d")], keywords: &["delete", "remove"]),
    c!(DeletePermanently, Files, "Delete permanently", "Deletes the selected items without the Recycle Bin.", TrashOff, [f("shift-delete")], keywords: &["remove", "erase"]),
    c!(RenameItem, Files, "Rename", "Renames the selected item.", Pencil, [f("f2")]),
    c!(DuplicateItems, Files, "Duplicate", "Makes a copy of the selected items next to them.", CopyPlus, [f("ctrl-alt-d")]),
    c!(CopyToOtherPane, Panes, "Copy to other pane", "Copies the selected items into the other pane's folder.", CopyPlus, [f("shift-f5")]),
    c!(MoveToOtherPane, Panes, "Move to other pane", "Moves the selected items into the other pane's folder.", MoveRight, [f("shift-f6")]),
    c!(Undo, Files, "Undo", "Takes back the last copy, move, rename or delete.", Undo2, [f("ctrl-z")]),
    c!(CopyPath, CopyPath, "Copy path", "Copies the full path of the selected items.", Link, [f("ctrl-shift-c")]),
    c!(CopyPathQuoted, CopyPath, "Copy path as quoted", "Copies the paths in quotes, ready for a command line.", Link, [f("ctrl-alt-c")]),
    c!(CopyPathName, CopyPath, "Copy path as name only", "Copies just the names of the selected items.", Link, [], since: "0.9.4"),
    c!(CopyPathFolder, CopyPath, "Copy path as folder path", "Copies the path of the folder the items are in.", Link, [], since: "0.9.4"),
    c!(CopyPathSlashes, CopyPath, "Copy path with forward slashes", "Copies the paths written with / instead of \\.", Link, [], since: "0.9.4"),
    c!(CopyPathRelative, CopyPath, "Copy path relative to repository", "Copies the paths from the root of their Git repository.", Link, [], keywords: &["git"], since: "0.9.4"),
    c!(CopyPathAt, CopyPath, "Copy path as @path", "Copies the paths with an @ in front, as coding assistants take them.", Link, [], keywords: &["ai", "agent"], since: "0.9.4"),
    c!(CopyPathWsl, CopyPath, "Copy path as WSL path", "Copies the paths as Linux under WSL sees them (/mnt/c/...).", Link, [], keywords: &["linux"], since: "0.9.4", platforms: Platforms::WINDOWS),
    c!(CopyPathUrl, CopyPath, "Copy path as file URL", "Copies the paths as file:/// addresses.", Link, [], keywords: &["uri", "link"], since: "0.9.4"),
    c!(OpenWith, Files, "Open with…", "Opens the selected file in a program you choose.", AppWindow, []),
    c!(Properties, Files, "Properties", "Shows the details of the selected items.", Info, [f("alt-enter")], keywords: &["info", "size", "attributes"]),
    c!(ShowContextMenu, Files, "Context menu", "Opens the right-click menu for the selected items.", Menu, [f("shift-f10"), f("menu")], keywords: &["right click"]),
    // ---- on the page only
    c!(OpenSelected, Navigation, "Open", "Opens the selected file or folder.", FolderOpen, [f("enter")], places: PAGE_ONLY),
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
    c!(Redo, Files, "Redo", "Does again what Undo took back.", Redo2, [f("ctrl-y")], places: PAGE_ONLY),
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

/// The commands the palette lists, in its order.
pub fn palette() -> impl Iterator<Item = &'static CommandSpec> {
    COMMANDS.iter().filter(|c| c.places.contains(Places::PALETTE))
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

/// Every command for the shortcut editor: (title, default keys as shown, action id).
pub fn shortcut_entries() -> Vec<(&'static str, String, String)> {
    palette().map(|c| (c.title, c.shown_keys(), c.id.to_string())).collect()
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
