//! The right-click menu as data. A row of it ([`MenuPlace`]) says where an entry sits (which targets, which section, in what order),
//! when it is there (a predicate over [`MenuContext`]) and which of Settings > Right-click menu's switches hides it. The rows that are
//! a command of the registry hang on that command (`CommandSpec::menu`); the others are in [`MENU_ONLY`]. [`plan`] turns the rows
//! and a context into the list of what the menu shows, separators included. `ctx_menu.rs` makes the real items from that list: what
//! an entry does stays there, in one builder for each [`Slot`].
//!
//! Nothing here touches the window, the file system or the settings: a context is plain data, so the menu can be tested by giving
//! it contexts and comparing what comes out (`menu_snapshots.txt`).

use gpui_kit::assets::IconName as L;

use crate::settings::MenuPrefs;

/// What the menu is opened on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Target {
    /// The empty part of the file list: the folder itself.
    #[default]
    Background,
    /// One file.
    File,
    /// One folder.
    Folder,
    /// Several items.
    Multi,
    /// An item inside an archive has no menu of its own: it is a [`Target::File`] or [`Target::Folder`] with `in_archive` set.
    #[allow(dead_code)]
    ArchiveItem,
    /// A row of the Recycle Bin page (or its background).
    RecycleItem,
    /// A folder or drive of the sidebar.
    Sidebar,
}

/// A set of [`Target`]s. A bit set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Targets(u8);

#[allow(dead_code)]
impl Targets {
    pub const BACKGROUND: Targets = Targets(1);
    pub const FILE: Targets = Targets(2);
    pub const FOLDER: Targets = Targets(4);
    pub const MULTI: Targets = Targets(8);
    pub const ARCHIVE_ITEM: Targets = Targets(16);
    pub const RECYCLE_ITEM: Targets = Targets(32);
    pub const SIDEBAR: Targets = Targets(64);
    /// A selection of files and folders in the file list.
    pub const ITEMS: Targets = Targets(2 | 4 | 8);

    pub const fn or(self, o: Targets) -> Targets {
        Targets(self.0 | o.0)
    }

    pub const fn contains(self, t: Target) -> bool {
        self.0 & (1 << t as u8) != 0
    }
}

/// A part of the menu. Sections of one block (see [`Section::block`]) follow each other without a separator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    /// How the folder is shown: layout, sort, group, refresh.
    View,
    /// Opening: Open, in a new tab, Open with, Send to.
    Open,
    /// The path of the item on the clipboard.
    CopyPath,
    /// Doing something with the items or the folder: undo, new, duplicate, pin, the type's own actions.
    Organize,
    /// The folder's tools: terminal, organizer, Git.
    Tools,
    /// Packing and spreading the selection: archives, tags, the shelf, the other pane.
    Pack,
    /// "Open in terminal" under a selected folder.
    Terminal,
    /// The user's own actions (`actions.toml`).
    YourActions,
    /// "Show more options": the shell's own items.
    More,
}

/// Whether a block of the menu has a separator before it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sep {
    None,
    /// Always, also when the block has nothing (as the hand-written menu did).
    Always,
    /// Only when the block has something.
    IfAny,
}

impl Section {
    /// The blocks of the menu, top to bottom; a separator may stand between two of them.
    pub const fn block(self) -> u8 {
        match self {
            Section::View | Section::Open => 0,
            Section::CopyPath | Section::Organize => 1,
            Section::Tools | Section::Pack => 2,
            Section::Terminal => 3,
            Section::YourActions => 4,
            Section::More => 5,
        }
    }
}

/// The separator before block `b`.
pub const fn sep_before(block: u8) -> Sep {
    match block {
        0 => Sep::None,
        3 | 4 => Sep::IfAny,
        _ => Sep::Always,
    }
}

/// One of the switches of Settings > Right-click menu: it hides every row that names it. Stored in `settings.toml` `[menu]` under `key`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Switch {
    pub key: &'static str,
    /// The English text beside the switch.
    pub label: &'static str,
    /// Where it stands in the list.
    pub pos: u8,
}

pub const SW_NEW_TAB: Switch = Switch { key: "open_in_new_tab", label: "Open in new tab", pos: 0 };
pub const SW_NEW_WINDOW: Switch = Switch { key: "open_in_new_window", label: "Open in new window", pos: 1 };
pub const SW_COPY_PATH: Switch = Switch { key: "copy_path", label: "Copy item path", pos: 2 };
pub const SW_SHORTCUT: Switch = Switch { key: "create_shortcut", label: "Create shortcut", pos: 3 };
pub const SW_PIN: Switch = Switch { key: "pin", label: "Pin to sidebar", pos: 4 };
pub const SW_COMPRESS: Switch = Switch { key: "compress", label: "Compress / Extract", pos: 5 };
pub const SW_TAGS: Switch = Switch { key: "tags", label: "Tags", pos: 6 };
pub const SW_SHELF: Switch = Switch { key: "shelf", label: "Add to shelf", pos: 7 };
pub const SW_TERMINAL: Switch = Switch { key: "terminal", label: "Open in terminal", pos: 8 };
pub const SW_SEND_TO: Switch = Switch { key: "send_to", label: "Send to", pos: 9 };

/// What a row's builder in `ctx_menu.rs` makes. One variant for each kind of entry; the label, icon and shortcut hint come from the row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Layout,
    Sort,
    GroupBy,
    Refresh,
    Undo,
    Redo,
    New,
    PasteShortcut,
    Pin,
    CopyPath,
    CopyAsContext,
    DiskUsage,
    FindDuplicates,
    FindBuildJunk,
    Checkpoint,
    SyncWith,
    ShowHidden,
    HideIgnored,
    Terminal,
    OrganizeFolder,
    Git,
    YourActions,
    More,
    Open,
    OpenNewTab,
    OpenNewWindow,
    OpenOtherPane,
    OpenWith,
    SendTo,
    OpenLocation,
    CopyPathAs,
    Duplicate,
    Shortcut,
    FolderWithSelection,
    Alias,
    IndexOut,
    Flatten,
    RotateRight,
    RotateLeft,
    Wallpaper,
    RunAdmin,
    InstallFont,
    Compress,
    Extract,
    Tags,
    Shelf,
    Basket,
    Nearby,
    BulkRename,
    CopyToPane,
    MoveToPane,
    // the Recycle Bin page's menu (made in `recycle_view.rs`)
    BinRestore,
    BinDelete,
    BinSettings,
    // the sidebar's menu (made in `workspace.rs`; it shares Open, OpenNewTab, OpenNewWindow, Pin, Tags, CopyPath, Terminal and More)
    Unpin,
    Properties,
    HidePlace,
    HideDrive,
}

/// Whether a row is in the menu, given the situation.
pub type MenuWhen = fn(&MenuContext) -> bool;

/// Where an entry sits in the menu.
#[derive(Clone, Copy, Debug)]
pub struct MenuPlace {
    pub slot: Slot,
    /// The English text of the entry (a few have a dynamic one, see [`MenuPlace::label`]).
    pub label: &'static str,
    pub icon: L,
    /// The shortcut shown at the right; empty: none.
    pub hint: &'static str,
    pub targets: Targets,
    pub section: Section,
    /// Within its block, the lower first.
    pub order: u16,
    /// `None`: always (for its targets).
    pub when: Option<MenuWhen>,
    pub switch: Option<Switch>,
    /// The entry opens a submenu.
    pub sub: bool,
}

pub const fn place(slot: Slot, label: &'static str, icon: L, targets: Targets, section: Section, order: u16) -> MenuPlace {
    MenuPlace { slot, label, icon, hint: "", targets, section, order, when: None, switch: None, sub: false }
}

impl MenuPlace {
    pub const fn hint(mut self, hint: &'static str) -> Self {
        self.hint = hint;
        self
    }
    pub const fn when(mut self, when: MenuWhen) -> Self {
        self.when = Some(when);
        self
    }
    pub const fn switch(mut self, switch: Switch) -> Self {
        self.switch = Some(switch);
        self
    }
    pub const fn sub(mut self) -> Self {
        self.sub = true;
        self
    }

    /// The text shown (translated).
    pub fn label(&self, ctx: &MenuContext) -> String {
        match self.slot {
            Slot::Undo => crate::ctx_menu::undo_text(ctx.undo.as_deref()),
            Slot::Redo => crate::ctx_menu::redo_text(ctx.redo.as_deref()),
            Slot::Alias => match &ctx.alias {
                Some(n) => format!("{} (@{n})…", crate::i18n::t("Edit alias")),
                None => format!("{}…", crate::i18n::t(self.label)),
            },
            _ => crate::i18n::t(self.label).to_string(),
        }
    }

    /// Whether the menu has it in this situation: the target, the switch of Settings and the row's own condition.
    pub fn shown(&self, ctx: &MenuContext) -> bool {
        self.targets.contains(ctx.target) && self.switch.is_none_or(|s| ctx.prefs.get(s.key)) && self.when.is_none_or(|w| w(ctx))
    }
}

/// What a row's condition can look at: the situation the menu is opened in, read once when it opens.
#[derive(Clone, Debug, Default)]
pub struct MenuContext {
    pub target: Target,
    /// How many items are selected (0 on the background).
    pub count: usize,
    pub single_dir: bool,
    /// One archive that opens like a folder.
    pub single_archive: bool,
    /// Something that is not one folder is selected.
    pub has_files: bool,
    /// The selection is inside an archive (or, on the background, the folder is).
    pub in_archive: bool,
    /// The folder of the list is the result of a search.
    pub in_search: bool,
    pub dual: bool,
    pub prefs: MenuPrefs,
    pub show_flatten: bool,
    pub name_index: bool,
    /// "Send to" has its place: files of one folder on disk.
    pub send_to_fits: bool,
    /// The folder the menu acts on (the selected one, or the one of the list) is an archive's folder.
    pub archive_path: bool,
    /// The same folder is on an FTP server.
    pub ftp: bool,
    /// The folder has rules for the organizer.
    pub organizer: bool,
    /// Every selected item can be rotated.
    pub rotatable: bool,
    /// The one selected file is a picture.
    pub image: bool,
    /// The one selected file can be run.
    pub runnable: bool,
    /// Every selected item is a font.
    pub fonts: bool,
    /// Every selected item is an archive that can be extracted.
    pub extractable: bool,
    /// Nearby sharing is on.
    pub nearby: bool,
    /// The sidebar's folder is pinned.
    pub pinned: bool,
    /// The sidebar's folder is the root of a drive.
    pub sidebar_root: bool,
    /// Tags exist (the sidebar offers them only then).
    pub tags_defined: bool,
    /// The sidebar's place is a cloud or WSL place.
    pub cloud_place: bool,
    /// The sidebar's place is a drive.
    pub drive: bool,
    /// The user has actions for this selection (`actions.toml`).
    pub own_actions: bool,
    /// What Ctrl+Z would undo, when there is something.
    pub undo: Option<String>,
    pub redo: Option<String>,
    /// The alias of the folder, when it has one.
    pub alias: Option<String>,
}

impl MenuContext {
    /// Which [`Target`] a selection of `count` items is (`single_dir`: it is one folder).
    pub fn target_of(count: usize, single_dir: bool) -> Target {
        match (count, single_dir) {
            (0, _) => Target::Background,
            (1, true) => Target::Folder,
            (1, false) => Target::File,
            _ => Target::Multi,
        }
    }
}

// ---- the rows that are not a command of the registry

use Section as S;
use Slot as K;
use Targets as T;

/// Rows with no command of their own in the registry.
pub static MENU_ONLY: &[MenuPlace] = &[
    // ---- folder background
    place(K::Layout, "Layout", L::LayoutGrid, T::BACKGROUND, S::View, 10).sub(),
    place(K::Sort, "Sort", L::ArrowUpDown, T::BACKGROUND, S::View, 20).sub(),
    place(K::GroupBy, "Group by", L::ListTree, T::BACKGROUND, S::View, 30).sub(),
    place(K::New, "New", L::CirclePlus, T::BACKGROUND, S::Organize, 30).sub(),
    place(K::Pin, "Pin to sidebar", L::Pin, T::BACKGROUND, S::Organize, 50),
    // ---- files and folders
    place(K::OpenNewTab, "Open in new tab", L::SquarePlus, T::FILE.or(T::FOLDER), S::Open, 20).switch(SW_NEW_TAB).when(|c| c.single_dir || c.single_archive),
    place(K::OpenNewWindow, "Open in new window", L::AppWindow, T::FILE.or(T::FOLDER), S::Open, 30).switch(SW_NEW_WINDOW).when(|c| c.single_dir || c.single_archive),
    place(K::OpenOtherPane, "Open in other pane", L::Columns2, T::FILE.or(T::FOLDER), S::Open, 40).when(|c| (c.single_dir || c.single_archive) && c.dual),
    place(K::SendTo, "Send to", L::Send, T::ITEMS, S::Open, 60).sub().switch(SW_SEND_TO).when(|c| c.send_to_fits),
    place(K::OpenLocation, "Open file location", L::FolderOpen, T::ITEMS, S::Open, 70).when(|c| c.in_search && c.count == 1),
    place(K::CopyPathAs, "Copy path as", L::ClipboardCopy, T::ITEMS, S::CopyPath, 20).sub().switch(SW_COPY_PATH),
    place(K::Shortcut, "Create shortcut", L::Link, T::ITEMS, S::Organize, 50).switch(SW_SHORTCUT).when(|c| !c.in_archive),
    place(K::FolderWithSelection, "New folder with selection", L::FolderInput, T::ITEMS, S::Organize, 60).when(|c| !c.in_search && !c.in_archive),
    place(K::DiskUsage, "Show disk usage", L::ChartPie, T::FOLDER, S::Organize, 70).when(|c| !c.archive_path && !c.ftp),
    place(K::Pin, "Pin to sidebar", L::Pin, T::FOLDER, S::Organize, 100).switch(SW_PIN),
    place(K::Alias, "Add alias", L::AtSign, T::FOLDER, S::Organize, 110).when(|c| !c.archive_path),
    place(K::IndexOut, "Leave out of the search index", L::SearchSlash, T::FOLDER, S::Organize, 120).when(|c| c.name_index && !c.archive_path),
    place(K::Flatten, "Flatten folder", L::FolderTree, T::FOLDER, S::Organize, 130).when(|c| c.show_flatten && !c.in_archive),
    place(K::Wallpaper, "Set as desktop background", L::Monitor, T::FILE, S::Organize, 160).when(|c| c.image),
    place(K::RunAdmin, "Run as administrator", L::ShieldCheck, T::FILE, S::Organize, 170).when(|c| !c.image && c.runnable && !c.in_archive),
    place(K::InstallFont, "Install font", L::Type, T::ITEMS, S::Organize, 180).when(|c| c.fonts),
    place(K::Compress, "Compress", L::Archive, T::ITEMS, S::Pack, 10).sub().switch(SW_COMPRESS).when(|c| !c.in_archive),
    place(K::Extract, "Extract", L::FolderOutput, T::ITEMS, S::Pack, 20).sub().switch(SW_COMPRESS).when(|c| c.extractable),
    place(K::Tags, "Tags", L::Tag, T::ITEMS, S::Pack, 30).sub().switch(SW_TAGS).when(|c| !c.in_archive),
    place(K::Nearby, "Send with Nearby", L::Send, T::ITEMS, S::Pack, 50).sub().when(|c| !c.in_archive && c.nearby),
    place(K::BulkRename, "Bulk rename…", L::PencilLine, T::MULTI, S::Pack, 60).hint("F2").when(|c| !c.in_search && !c.in_archive),
    place(K::DiskUsage, "Show disk usage", L::ChartPie, T::BACKGROUND, S::Organize, 80).when(|c| !c.in_search && !c.archive_path && !c.ftp),
    place(K::Git, "Git", L::GitBranch, T::BACKGROUND, S::Tools, 30).sub().when(|c| !c.in_search && !c.archive_path),
    place(K::YourActions, "Your actions", L::Zap, T::BACKGROUND.or(T::ITEMS), S::YourActions, 10).sub().when(|c| c.own_actions),
    place(K::More, "Show more options", L::Ellipsis, T::BACKGROUND.or(T::ITEMS), S::More, 10).sub(),
    // ---- the Recycle Bin page (a row, or the page itself)
    place(K::BinRestore, "Restore", L::Undo2, T::RECYCLE_ITEM, S::Open, 10),
    place(K::BinDelete, "Delete permanently", L::TrashOff, T::RECYCLE_ITEM, S::Open, 20).hint("Shift+Del"),
    place(K::BinSettings, "Trash settings…", L::Settings, T::RECYCLE_ITEM, S::Organize, 10),
    // ---- a folder or drive of the sidebar (no switch of Settings reaches this menu except Tags)
    place(K::Open, "Open", L::SquareArrowOutUpRight, T::SIDEBAR, S::Open, 10),
    place(K::OpenNewTab, "Open in new tab", L::SquarePlus, T::SIDEBAR, S::Open, 20),
    place(K::OpenNewWindow, "Open in new window", L::AppWindow, T::SIDEBAR, S::Open, 30),
    place(K::Unpin, "Unpin from sidebar", L::PinOff, T::SIDEBAR, S::Organize, 10).when(|c| c.pinned),
    place(K::Pin, "Pin to sidebar", L::Pin, T::SIDEBAR, S::Organize, 10).when(|c| !c.pinned),
    place(K::Tags, "Tags", L::Tag, T::SIDEBAR, S::Organize, 20).sub().switch(SW_TAGS).when(|c| !c.sidebar_root && c.tags_defined),
    place(K::CopyPath, "Copy path", L::ClipboardCopy, T::SIDEBAR, S::Organize, 30),
    place(K::Terminal, "Open in terminal", L::SquareTerminal, T::SIDEBAR, S::Organize, 40),
    place(K::Properties, "Properties", L::Wrench, T::SIDEBAR, S::Organize, 50).hint("Alt+Enter"),
    place(K::HidePlace, "Hide this place", L::EyeOff, T::SIDEBAR, S::Organize, 60).when(|c| c.cloud_place),
    place(K::HideDrive, "Hide this drive", L::EyeOff, T::SIDEBAR, S::Organize, 70).when(|c| c.drive),
    place(K::More, "Show more options", L::Ellipsis, T::SIDEBAR, S::More, 10).sub(),
];

/// Every row, the registry's commands' first.
pub fn places() -> impl Iterator<Item = &'static MenuPlace> {
    crate::registry::COMMANDS.iter().flat_map(|c| c.menu.iter()).chain(MENU_ONLY.iter())
}

/// One line of the menu.
#[derive(Clone, Copy, Debug)]
pub enum Entry {
    Separator,
    Row(&'static MenuPlace),
}

/// What the menu shows in this situation, top to bottom, separators in.
pub fn plan(ctx: &MenuContext) -> Vec<Entry> {
    let mut rows: Vec<&'static MenuPlace> = places().filter(|p| p.shown(ctx)).collect();
    rows.sort_by_key(|p| (p.section.block(), p.order));
    let mut out = Vec::with_capacity(rows.len() + 6);
    let mut first = true;
    // (the menus of the Recycle Bin and the sidebar have no empty block between their separators)
    let lean = matches!(ctx.target, Target::RecycleItem | Target::Sidebar);
    for block in 0..=5u8 {
        let mine: Vec<&&'static MenuPlace> = rows.iter().filter(|p| p.section.block() == block).collect();
        match sep_before(block) {
            Sep::Always if !first && (!lean || !mine.is_empty()) => out.push(Entry::Separator),
            Sep::IfAny if !mine.is_empty() && !first => out.push(Entry::Separator),
            _ => {}
        }
        if !mine.is_empty() {
            first = false;
        }
        out.extend(mine.into_iter().map(|p| Entry::Row(*p)));
    }
    out
}

/// The switches of Settings > Right-click menu, from the rows that name one, in the order of their `pos`.
pub fn switches() -> Vec<Switch> {
    let mut out: Vec<Switch> = Vec::new();
    for s in places().filter_map(|p| p.switch) {
        if !out.iter().any(|o| o.key == s.key) {
            out.push(s);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// The menu as text, one line for each entry: `label[ >] | Icon[ | hint]`, a separator as `-`.
#[cfg(test)]
pub fn render(ctx: &MenuContext) -> String {
    let mut s = String::new();
    for e in plan(ctx) {
        match e {
            Entry::Separator => s.push_str("-\n"),
            Entry::Row(p) => {
                s.push_str(&p.label(ctx));
                if p.sub {
                    s.push_str(" >");
                }
                s.push_str(&format!(" | {:?}", p.icon));
                if !p.hint.is_empty() {
                    s.push_str(&format!(" | {}", p.hint));
                }
                s.push('\n');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MenuPrefs;

    /// The cases of `menu_snapshots.txt` whose name starts with `prefix`, in the file's order: (name, text).
    fn cases(prefix: &str) -> Vec<(String, String)> {
        let file = include_str!("menu_snapshots.txt").replace("\r\n", "\n");
        let mut out: Vec<(String, String)> = Vec::new();
        for part in file.split("\n## ").skip(1) {
            let (name, body) = part.split_once('\n').unwrap();
            if name.starts_with(prefix) {
                out.push((name.to_string(), body.trim_end().to_string() + "\n"));
            }
        }
        out
    }

    fn check(name: &str, ctx: MenuContext, prefix: &str) {
        let want = cases(prefix).into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no snapshot {name}")).1;
        let have = render(&ctx);
        assert_eq!(have, want, "the menu for \"{name}\" is not the recorded one");
    }

    #[test]
    fn background_menu_is_as_recorded() {
        let bg = MenuContext { target: Target::Background, ..Default::default() };
        assert_eq!(cases("background").len(), 4);
        check("background plain", bg.clone(), "background");
        check(
            "background busy",
            MenuContext {
                undo: Some("Delete".into()),
                redo: Some("Rename".into()),
                organizer: true,
                own_actions: true,
                ..bg.clone()
            },
            "background",
        );
        check("background in archive", MenuContext { in_archive: true, archive_path: true, ..bg.clone() }, "background");
        check("background search results", MenuContext { in_search: true, ..bg }, "background");
    }

    /// One file selected, in a folder on disk.
    fn file() -> MenuContext {
        MenuContext { target: Target::File, count: 1, has_files: true, send_to_fits: true, show_flatten: true, name_index: true, ..Default::default() }
    }

    #[test]
    fn file_menu_is_as_recorded() {
        assert_eq!(cases("file").len(), 3);
        check("file plain", file(), "file");
        check(
            "file picture in search results, dual pane",
            MenuContext { in_search: true, dual: true, rotatable: true, image: true, ..file() },
            "file",
        );
        check("file inside an archive", MenuContext { in_archive: true, send_to_fits: false, ..file() }, "file");
    }

    fn folder() -> MenuContext {
        MenuContext { target: Target::Folder, single_dir: true, has_files: false, ..file() }
    }

    #[test]
    fn folder_menu_is_as_recorded() {
        assert_eq!(cases("folder").len(), 3);
        check("folder plain", folder(), "folder");
        check(
            "folder in search results, dual pane, with an alias and your actions",
            MenuContext { in_search: true, dual: true, alias: Some("docs".into()), own_actions: true, show_flatten: false, ..folder() },
            "folder",
        );
        check(
            "folder inside an archive",
            MenuContext { in_archive: true, archive_path: true, send_to_fits: false, ..folder() },
            "folder",
        );
    }

    #[test]
    fn multi_selection_menu_is_as_recorded() {
        assert_eq!(cases("three").len() + cases("two").len(), 3);
        let multi = |count| MenuContext { target: Target::Multi, count, ..file() };
        check("three files", multi(3), "three");
        check(
            "two archives, dual pane, copy path and tags switched off",
            MenuContext {
                dual: true,
                extractable: true,
                prefs: MenuPrefs { copy_path: false, tags: false, ..Default::default() },
                ..multi(2)
            },
            "two",
        );
        check("two fonts in search results", MenuContext { in_search: true, fonts: true, ..multi(2) }, "two");
    }

    #[test]
    fn recycle_bin_and_sidebar_menus_are_as_recorded() {
        assert_eq!(cases("bin").len() + cases("sidebar").len(), 4);
        check("bin", MenuContext { target: Target::RecycleItem, ..Default::default() }, "bin");
        let side = MenuContext { target: Target::Sidebar, ..Default::default() };
        check("sidebar folder, pinned, tags defined", MenuContext { pinned: true, tags_defined: true, ..side.clone() }, "sidebar");
        check("sidebar drive root", MenuContext { sidebar_root: true, drive: true, tags_defined: true, ..side.clone() }, "sidebar");
        check(
            "sidebar cloud place, tags switched off",
            MenuContext { cloud_place: true, tags_defined: true, prefs: MenuPrefs { tags: false, ..Default::default() }, ..side },
            "sidebar",
        );
    }

    #[test]
    fn the_switches_are_the_ones_settings_had() {
        let keys: Vec<&str> = switches().iter().map(|s| s.key).collect();
        assert_eq!(keys, ["open_in_new_tab", "open_in_new_window", "copy_path", "create_shortcut", "pin", "compress", "tags", "shelf", "terminal", "send_to"]);
        for k in &keys {
            assert!(MenuPrefs::default().get(k), "{k} is on by default");
        }
    }
}
