//! The topics of the guide ("How do I ..."): one table, so a topic is easy to add or change. Each topic is a task, 3 to 8 numbered
//! steps with the real keys and menu paths, a button that starts it where a command can be started from the guide, and lines for
//! limits. Every key, menu path and setting name here was checked against the code (`registry.rs`, `actions.rs`,
//! `settings_view.rs` and the views); a step that could not be confirmed there is not written. The page is `guide_page.rs`.
//!
//! Wording: plain English, short sentences. Names of commands are as the command palette shows them. `>` separates the parts of a
//! menu path. Example addresses use `user`, `nas.example.com`.

use crate::modules::Module;
use crate::settings_view::Page;

/// What the "Do it" button does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Go {
    /// No button: the task needs a folder or a selection, which the guide page has not got.
    No,
    /// Runs this command (the registry's id) in the window.
    Cmd(&'static str),
    /// Opens this page of Settings.
    Page(Page),
}

pub struct Topic {
    /// The name the "?" buttons and the search use.
    pub id: &'static str,
    /// The group in the list; the names of the groups of `FEATURES.md` where one fits.
    pub group: &'static str,
    /// A task: "Preview a file without opening it".
    pub title: &'static str,
    /// More words the search finds it by ("ftp", "nas", "zip").
    pub words: &'static [&'static str],
    pub steps: &'static [&'static str],
    pub good: &'static [&'static str],
    pub go: Go,
    /// The label of the button, when "Do it" does not say it.
    pub go_label: &'static str,
    /// The part of leaf the task needs (Settings > Features switches it).
    pub needs: Option<Module>,
}

const fn t(
    id: &'static str,
    group: &'static str,
    title: &'static str,
    words: &'static [&'static str],
    steps: &'static [&'static str],
    good: &'static [&'static str],
    go: Go,
) -> Topic {
    Topic { id, group, title, words, steps, good, go, go_label: "", needs: None }
}

impl Topic {
    const fn needs(mut self, m: Module) -> Topic {
        self.needs = Some(m);
        self
    }
    const fn label(mut self, l: &'static str) -> Topic {
        self.go_label = l;
        self
    }
}

pub const G_TABS: &str = "Tabs and windows";
pub const G_GO: &str = "Going places";
pub const G_SELECT: &str = "Selecting";
pub const G_FILES: &str = "Files and folders";
pub const G_PATHS: &str = "Copying paths";
pub const G_VIEW: &str = "View";
pub const G_PANES: &str = "Panes";
pub const G_TOOLS: &str = "Tools";
pub const G_VIEWER: &str = "The viewer (Space)";
pub const G_SERVERS: &str = "Servers";
pub const G_DOWNLOADS: &str = "Downloads";
pub const G_AGENTS: &str = "Coding agents";
pub const G_APP: &str = "leaf itself";

pub static TOPICS: &[Topic] = &[
    // ---------------------------------------------------------------- Tabs and windows
    t(
        "tabs",
        G_TABS,
        "Work in several tabs",
        &["tab", "browser tabs", "new tab", "close tab"],
        &[
            "Press Ctrl+T for a new tab.",
            "Press Ctrl+Tab to go to the tab on the right and Ctrl+Shift+Tab for the one on the left.",
            "Press Ctrl+W to close the tab you are in.",
            "Press Ctrl+Shift+T to open the tab you closed last.",
            "Press Ctrl+Shift+K to open the same folder again in a new tab.",
            "Press Ctrl+Shift+A to find an open tab, or one closed a short while ago, by its name.",
        ],
        &["What a new tab shows is chosen in Settings > General, under \"A new tab opens\": Home or the current folder."],
        Go::Page(Page::General),
    ),
    t(
        "windows",
        G_TABS,
        "Open another window or fill the screen",
        &["window", "full screen", "fullscreen", "on top", "overlay", "restart"],
        &[
            "Press Ctrl+N for another leaf window.",
            "Press Ctrl+Alt+Enter to open the selected folder in a new window.",
            "Press F11 to fill the screen with the window, and F11 again to go back.",
            "Press Ctrl+Shift+P and type \"Always on top\" to keep the window above others, or \"Compact overlay\" for a small window that stays on top.",
        ],
        &["Ctrl+Alt+Shift+R closes leaf and starts it again with the same tabs."],
        Go::No,
    ),
    t(
        "session",
        G_TABS,
        "Save the open tabs to open later",
        &["session", "restore tabs", "workspace"],
        &[
            "Open the main menu in the title bar, or the tab menu, and choose \"Save session…\".",
            "Type a name and press Save.",
            "Open it later from the tab menu: the saved sessions are listed there.",
        ],
        &["Settings > General, \"Open saved sessions in this window\": a session then replaces the tabs of this window instead of opening a new one."],
        Go::No,
    ),
    t(
        "startup",
        G_TABS,
        "Choose what leaf shows when it starts",
        &["startup", "start", "launch", "home page"],
        &[
            "Open Settings (Ctrl+,) and choose General.",
            "In the startup choices, pick \"Continue where you left off\", \"Open a new tab\" or \"Open specific pages\".",
            "For specific pages, use \"Use currently open tabs\" to take the tabs you have now.",
        ],
        &[],
        Go::Page(Page::General),
    ),
    // ---------------------------------------------------------------- Going places
    t(
        "goto",
        G_GO,
        "Go to a folder by typing its path",
        &["path", "address", "address bar", "location", "type path"],
        &[
            "Press Ctrl+L or Alt+D. The keyboard goes to the address bar.",
            "Type or paste the path, for example C:\\Users\\user\\Documents, and press Enter.",
            "You can also press Ctrl+Shift+P and type a path there: the first row says \"Go to:\".",
        ],
        &["Settings > Files and folders, \"Show the full path in the address bar\": the path shows as text instead of clickable parts."],
        Go::No,
    ),
    t(
        "back",
        G_GO,
        "Go back, forward or up a folder",
        &["back", "forward", "up", "parent", "history", "home", "refresh"],
        &[
            "Press Alt+Left or Backspace to go back to the folder before.",
            "Press Alt+Right to go forward again.",
            "Press Alt+Up to open the folder this one is in.",
            "Press Alt+Home to open the Home page.",
            "Press F5 or Ctrl+R to read the folder again.",
        ],
        &["Settings > Files and folders, \"Double-click empty space to go up a folder\", lets a double click on empty space go up too."],
        Go::No,
    ),
    t(
        "find",
        G_GO,
        "Find a file by its name",
        &["search", "find", "filter", "look for"],
        &[
            "Press Ctrl+F or Ctrl+E. The search box opens for this folder.",
            "Type part of the name.",
            "To search somewhere else, open that folder first: the search is for the folder you are in.",
        ],
        &["Settings > Files and folders, \"Typing filters this folder\": when it is on, typing in the list filters it at once. When it is off, typing jumps to the item that starts with those letters."],
        Go::No,
    ),
    t(
        "contents",
        G_GO,
        "Search inside the files of a folder",
        &["grep", "text", "regex", "contents", "inside files"],
        &[
            "Open the folder to search in.",
            "Press Ctrl+Shift+P, type \"Search in file contents\" and press Enter.",
            "Type the words to look for. Start with regex: for a regular expression.",
            "The hits come in as they are found, each with the matching line.",
        ],
        &["Only text files are searched. Big files and binary files are skipped.", "The folder and its subfolders are searched."],
        Go::No,
    ),
    t(
        "pastfolders",
        G_GO,
        "Jump back to a folder you used before",
        &["recent", "history", "visited", "frequent"],
        &[
            "Press Ctrl+P. A list of folders you went to before opens.",
            "Type a few letters of the folder's name.",
            "Press Enter on the one you want.",
        ],
        &["Settings > Actions, \"Show past folders in the command palette\": folders are also listed with the commands as you type in Ctrl+Shift+P."],
        Go::No,
    ),
    t(
        "alias",
        G_GO,
        "Open a folder by a short name",
        &["alias", "shortcut", "bookmark", "@"],
        &[
            "Right-click the folder and choose \"Add alias…\".",
            "Type the short name it should have.",
            "Later, press Ctrl+L, type @ and the name, and press Enter.",
        ],
        &["Settings > Files and folders, under \"Folder aliases\", lists your aliases."],
        Go::Page(Page::Files),
    ),
    t(
        "palette",
        G_GO,
        "Find any command by typing",
        &["command palette", "commands", "menu", "shortcut", "search command"],
        &[
            "Press Ctrl+Shift+P.",
            "Type a few letters of what you want to do, for example \"hidden\".",
            "Use the arrow keys and press Enter, or click the row.",
        ],
        &[
            "The same list, with what each command does and its shortcut, is under Settings > What leaf can do.",
            "Typing in the palette also offers guide topics, starting with \"How do I:\".",
        ],
        Go::Cmd("ToggleCommandPalette"),
    )
    .label("Open the palette"),
    // ---------------------------------------------------------------- Selecting
    t(
        "select",
        G_SELECT,
        "Select several files",
        &["select all", "multi", "pick", "checkbox", "invert"],
        &[
            "Press Ctrl+A to select everything in the folder.",
            "Hold Shift and press the Up or Down arrow to add the next item. Home and End jump to the first and the last item.",
            "Press Ctrl+Space to add or take out the item under the cursor.",
            "Press Escape to select nothing.",
            "For the opposite selection, press Ctrl+Shift+P and choose \"Invert selection\".",
        ],
        &["Settings > Files and folders, \"Show checkboxes when selecting\", adds a tick box to each item."],
        Go::No,
    ),
    // ---------------------------------------------------------------- Files and folders
    t(
        "newfolder",
        G_FILES,
        "Make a new folder or a text file",
        &["create", "mkdir", "new file", "new folder"],
        &[
            "Press Ctrl+Shift+N for a new folder. Type its name and press Enter.",
            "For a text file, right-click an empty spot and choose New, or press Ctrl+Shift+P and choose \"New text document\".",
            "To put files into a new folder at once, select them, right-click and choose \"New folder with selection\".",
        ],
        &[],
        Go::No,
    ),
    t(
        "copymove",
        G_FILES,
        "Copy or move files",
        &["copy", "move", "paste", "cut", "transfer", "drag"],
        &[
            "Select the files. Press Ctrl+C to copy them or Ctrl+X to move them.",
            "Open the folder they should go to.",
            "Press Ctrl+V. Ctrl+Shift+V puts them into the selected folder instead.",
            "Watch the progress in the Status Center, the button in the title bar. A file with the same name asks what to do.",
        ],
        &[
            "Settings > Advanced > Performance has \"Verify copies\" and \"Copy speed limit\".",
            "To copy between two folders at once, use two panes: see \"Work in two folders side by side\".",
        ],
        Go::Page(Page::Advanced),
    )
    .label("Open Settings > Advanced"),
    t(
        "delete",
        G_FILES,
        "Delete a file and get it back",
        &["delete", "trash", "recycle bin", "undo", "restore", "remove"],
        &[
            "Select the files and press Del. They go to the Trash.",
            "Press Ctrl+Z right away to take it back. Ctrl+Y does it again.",
            "To bring one back later, press Ctrl+Shift+P, choose \"Trash\", select the item and press Restore.",
            "Shift+Del deletes for good, without the Trash.",
        ],
        &["Settings > Files and folders, \"Confirm before deleting\": always, only for permanent deletion, or never."],
        Go::Cmd("GoRecycleBin"),
    )
    .label("Open the Trash"),
    t(
        "rename",
        G_FILES,
        "Rename one file or many",
        &["rename", "bulk", "batch", "numbering", "case", "extension"],
        &[
            "Select one item and press F2. Type the new name and press Enter.",
            "To rename many, select them and press F2, or right-click and choose \"Bulk rename…\".",
            "Pick what to do: replace text, insert text, remove characters or text, change the letter case, add a number, tidy white space, change the extension, or build the name from a template.",
            "The list shows the new names before anything is renamed. Rows that clash are counted.",
        ],
        &["A rule you use often can be saved as a preset."],
        Go::No,
    ),
    t(
        "details",
        G_FILES,
        "See the details of a file or open it with another program",
        &["properties", "open with", "info", "size", "right click", "context menu"],
        &[
            "Select the item and press Alt+Enter for its properties.",
            "Press Shift+F10 or the Menu key, or right-click, for the menu of the item.",
            "Choose \"Open with…\" to open the file in a program you choose.",
        ],
        &["Settings > General, \"Context menu options\", chooses which commands the menu shows."],
        Go::No,
    ),
    t(
        "duplicate",
        G_FILES,
        "Make a copy next to the original or a shortcut",
        &["duplicate", "clone", "shortcut", "link"],
        &[
            "Select the items and press Ctrl+Alt+D. The copies appear next to them.",
            "For a shortcut, right-click and choose \"Create shortcut\".",
            "To paste copied items as shortcuts, press Ctrl+Shift+P and choose \"Paste as shortcut\".",
        ],
        &[],
        Go::No,
    ),
    t(
        "zip",
        G_FILES,
        "Make a zip file or open one",
        &["zip", "7z", "rar", "tar", "archive", "compress", "extract", "unzip", "unpack"],
        &[
            "Select the files, right-click and choose Compress. Pick the kind of archive.",
            "To unpack, right-click the archive and choose Extract.",
            "To look inside without unpacking, open the archive: it opens like a folder.",
        ],
        &["Opening or copying one item from inside an archive unpacks just that item to a temporary folder."],
        Go::No,
    ),
    t(
        "tags",
        G_FILES,
        "Mark files with colored tags",
        &["tag", "label", "color", "colour", "group"],
        &[
            "Open Settings > Tags and press \"New tag\" to make a tag. Click its color to change it.",
            "Select files, right-click and choose Tags, then pick the tag.",
            "The tag now shows under Tags in the sidebar. Click it to list everything that wears it.",
        ],
        &["Deleting a tag removes it from all items. The files are not deleted."],
        Go::Page(Page::Tags),
    ),
    t(
        "shelf",
        G_FILES,
        "Collect files from several folders and move them together",
        &["shelf", "collect", "basket", "stash"],
        &[
            "Select files and press Ctrl+Shift+S, or right-click and choose \"Add to shelf\". They go on the shelf.",
            "Go to other folders and add more.",
            "Open the folder they should go to. In the shelf, press \"Copy here\" or \"Move here\".",
            "\"Clear\" empties the shelf.",
        ],
        &["Press Ctrl+Shift+P and choose \"Show/hide shelf\" to show or hide it."],
        Go::No,
    ),
    t(
        "organize",
        G_FILES,
        "Sort a messy folder",
        &["organize", "tidy", "sort", "clean up", "downloads folder", "rules"],
        &[
            "Open the folder.",
            "Press Ctrl+Shift+P and choose \"Organize folder\". Its files are sorted into folders by kind.",
            "For your own rules, open Settings > File organizer. Each rule has \"Run now\" and \"Edit rule\". Rules are tried from the top; \"Move up\" puts one first.",
        ],
        &["\"Organize files by themselves as they arrive\" in that page watches the folders of rules set to run by themselves, and works without a preview."],
        Go::Page(Page::Organizer),
    ),
    t(
        "duplicates",
        G_FILES,
        "Find duplicate files",
        &["duplicate", "same files", "identical", "clean up", "space"],
        &[
            "Open the folder, or select the folders to look in.",
            "Press Ctrl+Shift+P and choose \"Find duplicates\".",
            "The result lists the files that have identical content.",
        ],
        &[],
        Go::No,
    ),
    // ---------------------------------------------------------------- Copying paths
    t(
        "copypath",
        G_PATHS,
        "Copy the path of a file",
        &["path", "clipboard", "copy path", "wsl", "url"],
        &[
            "Select the item and press Ctrl+Shift+C. The full path is on the clipboard.",
            "Press Ctrl+Alt+C for the path in quotes, ready for a command line.",
            "For other forms, right-click and open \"Copy path as\": name only, folder path, forward slashes, relative to the repository, @path, WSL path, file URL.",
        ],
        &[],
        Go::No,
    ),
    t(
        "context",
        G_PATHS,
        "Copy files as text to paste into a chat",
        &["context", "chat", "assistant", "llm", "prompt", "tokens", "basket"],
        &[
            "Select the files, or none for the whole folder.",
            "Press Ctrl+Shift+P and choose \"Copy as context\".",
            "Paste into the chat. You get a tree and then each text file under its path.",
        ],
        &[
            "Files that .gitignore leaves out, binary files, big files and files that look like secrets are left out.",
            "For a named set of files, use \"New basket\" and \"Add to basket\", then \"Copy basket as context\" or \"Copy basket as @paths\".",
        ],
        Go::No,
    )
    .needs(Module::Dev),
    // ---------------------------------------------------------------- View
    t(
        "views",
        G_VIEW,
        "Change how a folder looks",
        &["view", "layout", "details", "grid", "columns", "cards", "list", "zoom", "hidden files", "sort"],
        &[
            "Press Ctrl+Shift+1 for Details, 2 for Grid, 3 for Columns, 4 for Cards, 5 for List.",
            "Press Ctrl+= to make items larger and Ctrl+- to make them smaller.",
            "Press Ctrl+H to show or hide hidden items.",
            "Right-click an empty spot and use Layout, Sort or Group by.",
        ],
        &[],
        Go::No,
    ),
    t(
        "panes",
        G_VIEW,
        "Show or hide the preview pane, the sidebar and the tool bar",
        &["preview pane", "sidebar", "toolbar", "tool bar", "layout"],
        &[
            "Press Alt+P to show or hide the preview of the selected file.",
            "Press Ctrl+B to show or hide the sidebar.",
            "Press Ctrl+Shift+B to show or hide the tool bar.",
        ],
        &["The preview pane is a small preview inside the window. For a large one, see \"Preview a file without opening it\"."],
        Go::No,
    ),
    t(
        "theme",
        G_VIEW,
        "Change the look and the colors",
        &["theme", "dark", "light", "accent", "color", "appearance", "canopy", "stem", "index"],
        &[
            "Press Ctrl+Alt+T to switch between the light and the dark theme.",
            "For more, open Settings > Appearance. Pick a theme and a mode (use the system setting, light, dark or black).",
            "Pick an accent color there too.",
            "To add your own theme, press \"Open themes folder\" and put a .toml file in it.",
        ],
        &["Changes apply at once."],
        Go::Page(Page::Appearance),
    ),
    t(
        "home",
        G_VIEW,
        "Change what the Home page shows",
        &["home", "widgets", "dashboard", "start page"],
        &[
            "Press Alt+Home to open Home.",
            "Press Customize. Buttons appear to add, remove, resize and reorder the widgets.",
            "Press Done when you are happy.",
        ],
        &["Press Ctrl+Shift+P and choose \"Reset Home\" to put the default widgets back."],
        Go::Cmd("CustomizeHome"),
    ),
    t(
        "shortcuts",
        G_VIEW,
        "Change a keyboard shortcut",
        &["keys", "keybinding", "hotkey", "shortcut", "remap"],
        &[
            "Open Settings > Actions.",
            "Under \"Keyboard shortcuts\", click the command.",
            "Press the new keys. Escape cancels.",
            "The arrow next to a changed shortcut puts the default back.",
        ],
        &["If another command has the same shortcut, the page lists it under \"Same shortcut:\"."],
        Go::Page(Page::Actions),
    ),
    t(
        "youractions",
        G_VIEW,
        "Add a command of your own",
        &["custom action", "script", "actions.toml", "own command", "run program"],
        &[
            "Open Settings > Your actions.",
            "Press \"Add action\", or press Add on one of the examples.",
            "In the command, {path} is the first selected item, {files} every selected item, {dir} the current folder and {name} the first item's name.",
            "The action then shows in the right-click menu, the command palette and the shortcut list.",
        ],
        &["\"Open actions.toml\" opens the file the actions are written in."],
        Go::Page(Page::YourActions),
    ),
    // ---------------------------------------------------------------- Panes
    t(
        "dualpane",
        G_PANES,
        "Work in two folders side by side",
        &["dual pane", "two panes", "split", "commander", "side by side", "f5", "f6"],
        &[
            "Press Ctrl+Shift+D to split the tab into two folders. Press it again to join them.",
            "Press F6 to move the keyboard to the other pane.",
            "Select files and press Shift+F5 to copy them to the other pane's folder, or Shift+F6 to move them.",
            "Press Ctrl+Shift+W to close the pane that has the keyboard.",
        ],
        &[
            "Press Ctrl+Shift+P and choose \"Split pane side by side\" or \"Split pane top and bottom\" for other layouts.",
            "Settings > General, \"Open new tabs in dual pane\", starts every new tab with two panes.",
        ],
        Go::No,
    ),
    t(
        "compare",
        G_PANES,
        "Compare two folders",
        &["compare", "diff", "difference", "newer", "older"],
        &[
            "Press Ctrl+Shift+D and open one folder in each pane.",
            "Press Ctrl+Shift+P and choose \"Compare folders\".",
            "What differs is marked: only here, newer, older, or another size.",
        ],
        &[],
        Go::No,
    ),
    // ---------------------------------------------------------------- Tools
    t(
        "terminal",
        G_TOOLS,
        "Open a terminal or an editor in this folder",
        &["terminal", "console", "shell", "powershell", "cmd", "editor", "vscode", "ssh"],
        &[
            "Press Ctrl+Shift+` to open a terminal in the folder.",
            "Press Ctrl+Shift+P and choose \"Open in editor\" to open the folder in your code editor.",
            "In a folder of a server (sftp://), the terminal runs ssh into it.",
        ],
        &["Which terminal and which editor are chosen in Settings > Advanced, under Developer tools."],
        Go::No,
    ),
    t(
        "admin",
        G_TOOLS,
        "Run something as administrator",
        &["admin", "administrator", "elevated", "uac", "access denied", "permission", "retry"],
        &[
            "For a program: right-click the file and choose \"Run as administrator\".",
            "For a terminal (Windows): press Ctrl+Shift+P and choose \"Open in Terminal as administrator\".",
            "When a copy, move or delete is refused with \"access denied\", the question has a \"Retry as administrator\" button. A folder that cannot be read has the same button.",
            "Windows asks for permission. The helper stops after five minutes without work, or at once with \"End now\" in the Status Center.",
        ],
        &[
            "\"Retry as administrator\" works only when leaf is installed for all users (in Program Files). Settings > About says whether it is available here, and why not.",
            "Every retry needs its own click.",
        ],
        Go::Page(Page::About),
    )
    .label("Open Settings > About"),
    t(
        "stuckhelper",
        G_TOOLS,
        "Stop the administrator helper",
        &["helper", "stuck", "end now", "leaf-helper", "elevated", "kill"],
        &[
            "Click the Status Center button in the title bar.",
            "Find the row for the administrator helper.",
            "Press \"End now\". The helper stops at once.",
        ],
        &["Without that, the helper ends by itself after five idle minutes, or when leaf closes."],
        Go::No,
    ),
    t(
        "diskusage",
        G_TOOLS,
        "Find what uses the disk",
        &["disk usage", "storage", "space", "treemap", "sunburst", "big files", "what is taking space", "wiztree", "windirstat"],
        &[
            "Right-click a folder or a drive, or an empty spot in a folder, and choose \"Show disk usage\".",
            "Wait while the folder is counted. The biggest items come first.",
            "Switch between Treemap, Sunburst and Skyline, and between Size and Size on disk.",
            "Right-click an item for Open, Open in new tab and Show in folder.",
        ],
        &["Closing the tab stops the count.", "With the agent layer on, the size can also be an estimate of tokens, or how many agent sessions wrote the file."],
        Go::No,
    )
    .needs(Module::DiskUsage),
    t(
        "junk",
        G_TOOLS,
        "Free space from build folders",
        &["node_modules", "target", "venv", "cache", "build junk", "clean"],
        &[
            "Open the folder that holds your projects.",
            "Press Ctrl+Shift+P and choose \"Find build junk\".",
            "The list shows folders a build makes again, such as node_modules, target and .venv, with their size.",
            "Tick the ones to remove. They are moved to the Trash.",
        ],
        &[],
        Go::No,
    )
    .needs(Module::Dev),
    t(
        "mount",
        G_TOOLS,
        "Open a disc image (.iso)",
        &["iso", "img", "mount", "eject", "disc", "dvd"],
        &[
            "Select the .iso or .img file.",
            "Press Ctrl+Shift+P and choose \"Mount image\". It opens read-only.",
            "It shows under Drives until you choose \"Eject image\".",
        ],
        &[],
        Go::No,
    )
    .needs(Module::Media),
    t(
        "bluray",
        G_TOOLS,
        "Turn a Blu-ray into one video file",
        &["blu-ray", "bluray", "bdmv", "avchd", "m2ts", "mkv", "rip", "convert"],
        &[
            "Select the disc, a BDMV folder, an .iso holding one, an AVCHD card or the .m2ts files.",
            "Press Ctrl+Shift+P and choose \"Convert to video file…\".",
            "Choose the title and the audio and subtitle tracks.",
            "Choose the result: a lossless MKV with chapters, or, with ffmpeg installed, a smaller re-encoded file.",
        ],
        &["Copy-protected discs are not converted."],
        Go::No,
    )
    .needs(Module::Media),
    t(
        "rotate",
        G_TOOLS,
        "Turn pictures a quarter",
        &["rotate", "turn", "photo", "orientation", "image"],
        &[
            "Select the pictures.",
            "Press Ctrl+Shift+P and choose \"Rotate right\" or \"Rotate left\".",
            "In the viewer, press R for a quarter to the right and Shift+R for a quarter to the left.",
        ],
        &[],
        Go::No,
    ),
    t(
        "netdrive",
        G_TOOLS,
        "Give a network folder a drive letter",
        &["map drive", "network drive", "smb", "share", "unc"],
        &[
            "Press Ctrl+Shift+P and choose \"Map network drive…\".",
            "Fill in the share and the drive letter it asks for.",
            "\"Disconnect network drive…\" takes it away again.",
        ],
        &["Windows only."],
        Go::Cmd("MapNetworkDrive"),
    ),
    t(
        "docker",
        G_TOOLS,
        "Look into a Docker container's files",
        &["docker", "container", "volume", "mount"],
        &[
            "Press Ctrl+Shift+P and choose \"Docker containers\".",
            "Open a container. It lists its bind mounts and volumes.",
            "Each one opens as the folder it stands for.",
        ],
        &["It only reads.", "In a folder on an sftp:// host, it asks that host."],
        Go::No,
    )
    .needs(Module::Dev),
    // ---------------------------------------------------------------- The viewer
    t(
        "viewer",
        G_VIEWER,
        "Preview a file without opening it",
        &["space", "quick look", "quicklook", "preview", "peek", "viewer", "look"],
        &[
            "Select a file in the list.",
            "Press Space. The viewer opens at once.",
            "Press the Left and Right arrow keys (or Up and Down) to look at the next and the previous item of the folder.",
            "Press Space or Escape to close it. Enter opens the file with its own program instead.",
            "Click the \"...\" button in the viewer's title bar for the other things it can do and the keys of this kind of file.",
        ],
        &[
            "Pictures, video, PDF, text and code, Markdown, CSV, archives, fonts, e-mail and documents have their own view. Other files show a card with their details, or their first bytes.",
            "The viewer is a separate program: it does not slow leaf down.",
            "Settings > Preview turns it off or sets it up. On Windows, \"Space opens QuickLook\" hands Space to QuickLook if that is installed.",
        ],
        Go::Page(Page::Look),
    )
    .label("Open Settings > Preview"),
    t(
        "viewer-edit",
        G_VIEWER,
        "Edit a text file in the viewer",
        &["edit", "text", "markdown", "csv", "notepad", "save"],
        &[
            "Select a text, Markdown or CSV file and press Space.",
            "Press E. The file opens for editing in the same window.",
            "Press Ctrl+S to save. Ctrl+G goes to a line.",
            "Press Escape to go back to the view. If there is something unsaved, it asks first.",
        ],
        &[
            "A file can be edited when it is a real file of up to 8 MB that reads cleanly as text. For any other, the \"...\" menu has \"Why can't this file be edited?\".",
            "Alt+L shows or hides line numbers, and Alt+Z turns word wrap on or off. Both are also in Settings > Preview.",
            "Alt+F keeps the end of a file that grows, such as a log, in view.",
        ],
        Go::No,
    ),
    t(
        "viewer-pictures",
        G_VIEWER,
        "Look through pictures in the viewer",
        &["image", "photo", "slideshow", "rotate", "exif", "screenshot", "png", "copy picture"],
        &[
            "Select a picture and press Space.",
            "Press the arrow keys to go to the next and the previous picture.",
            "Press R to turn it to the right, Shift+R to the left.",
            "Press S for a slideshow of the folder's pictures. Any other key stops it.",
            "The \"...\" menu has Copy to clipboard, Save as PNG…, Invert colors, EXIF info and Background: dark/light.",
        ],
        &["The slideshow needs other pictures in the folder."],
        Go::No,
    ),
    t(
        "viewer-keys",
        G_VIEWER,
        "Know the keys of the viewer",
        &["keys", "keyboard", "f11", "f5", "reload", "full screen", "enter"],
        &[
            "Space or Escape closes the viewer.",
            "The arrow keys step to the next and the previous item.",
            "Enter opens the file with its own program.",
            "F5 reads the file again. F11 fills the screen.",
            "E edits text. R and Shift+R turn a picture, S starts a slideshow. Alt+L and Alt+Z change the line numbers and the word wrap in text.",
        ],
        &["The \"...\" menu in the viewer's title bar lists the keys for the file that is open."],
        Go::No,
    ),
    // ---------------------------------------------------------------- Servers
    t(
        "server",
        G_SERVERS,
        "Connect to a server",
        &["ftp", "sftp", "ssh", "nas", "webdav", "dav", "smb", "share", "host", "server", "remote", "ftps"],
        &[
            "Open Settings > Hosts and press Add.",
            "Type one line: a name, =, then the address. For example: home = sftp://user@nas.example.com/data",
            "Other kinds are ftps://, ftp://, davs:// and dav:// (WebDAV), and smb:// for a Windows share, such as files = smb://nas.example.com/photos.",
            "Press Save. The host now shows under Hosts in the sidebar.",
            "Click it. leaf asks for the password, or for the key's passphrase, and shows an unknown host key once to check.",
            "Tick Remember in the password question to keep the password in the system's keyring.",
        ],
        &[
            "Passwords are never kept in the list of hosts.",
            "ftp:// and dav:// are not encrypted: the password crosses the network as it is. Use ftps:// and davs:// where the server has them.",
            "\"Import from OpenSSH config\" on that page adds the hosts of your ~/.ssh/config.",
            "The Hosts list in the sidebar shows only while Servers is on in Settings > Features.",
        ],
        Go::Page(Page::Hosts),
    )
    .needs(Module::Servers),
    t(
        "servercopy",
        G_SERVERS,
        "Copy between two servers",
        &["transfer", "two servers", "server to server", "sftp to ftp", "move between hosts"],
        &[
            "Press Ctrl+Shift+D to split the tab into two panes.",
            "Click a host under Hosts in the sidebar, then press F6 and click the other host.",
            "Select the files in one pane and press Shift+F5 to copy them to the other pane's folder.",
            "Follow the job in the Status Center. A file with the same name asks what to do.",
        ],
        &[
            "Between two different servers the data goes through this PC.",
            "A copy inside one SFTP server is done by the server itself when it can.",
            "Right-click on a file of a server also has \"Copy to...\", which asks for the folder.",
        ],
        Go::No,
    )
    .needs(Module::Servers),
    t(
        "serverundo",
        G_SERVERS,
        "Undo a delete on a server",
        &["server trash", "undo", "restore", "leaf-trash", "deleted on server"],
        &[
            "Delete as usual. On an SFTP, FTP, FTPS or WebDAV host the item moves into leaf's trash on that server (the .leaf-trash folder where you log in).",
            "Press Ctrl+Z to put it back.",
            "To empty it for good, right-click the host in the sidebar and choose \"Empty server trash\".",
        ],
        &[
            "Shift+Del deletes for good without the trash.",
            "Settings > Hosts has \"Server trash: on\" for each host. Press it to turn the server trash off for that host.",
        ],
        Go::Page(Page::Hosts),
    )
    .needs(Module::Servers),
    t(
        "serveredit",
        G_SERVERS,
        "Edit a file on a server",
        &["edit remote", "edit sftp", "save to server", "bak"],
        &[
            "Right-click the file on the server and choose Edit.",
            "It opens in your editor from a copy on this PC.",
            "Save in the editor. leaf uploads each save and keeps the old version beside it as name.leaf-bak.",
        ],
        &["If the file on the server changed after you opened it, leaf refuses to overwrite it and offers \"Save as copy on server\"."],
        Go::No,
    )
    .needs(Module::Servers),
    t(
        "serverbookmark",
        G_SERVERS,
        "Bookmark a folder on a server",
        &["bookmark", "favorite", "remote folder"],
        &[
            "Open the folder on the host.",
            "Press Ctrl+Shift+P and choose \"Bookmark remote folder\".",
            "The folder shows under its host in the sidebar.",
            "Choose the command again in the folder to take the bookmark away.",
        ],
        &["The host must be saved in Settings > Hosts."],
        Go::No,
    )
    .needs(Module::Servers),
    t(
        "serverlog",
        G_SERVERS,
        "Watch a log file grow on a server",
        &["tail", "follow", "log", "tail -f"],
        &[
            "Open the folder on the sftp:// host and put the cursor on the log file.",
            "Press Alt+F. leaf reads the end of the file every 2 seconds and shows it in the viewer.",
            "Press Alt+F again to stop.",
        ],
        &["It stops by itself when the tab closes."],
        Go::No,
    )
    .needs(Module::Servers),
    t(
        "forgetpassword",
        G_SERVERS,
        "Make leaf forget a saved password",
        &["password", "keyring", "credentials", "forget", "passphrase"],
        &[
            "Open Settings > Hosts and look under \"Saved passwords\".",
            "Press Forget beside one, or \"Forget all\".",
            "Or right-click the host in the sidebar and choose \"Forget saved password\".",
        ],
        &["leaf asks again the next time it connects."],
        Go::Page(Page::Hosts),
    )
    .needs(Module::Servers),
    // ---------------------------------------------------------------- Downloads
    t(
        "download",
        G_DOWNLOADS,
        "Download a file into this folder",
        &["download", "http", "https", "url", "link", "wget", "curl", "internet"],
        &[
            "Open the folder the file should go to.",
            "Press Ctrl+Shift+P and choose \"Download here…\". An address on the clipboard is already in the box.",
            "Paste or type the address (it must start with http:// or https://) and press Download.",
            "Follow it in the Status Center. There you can pause or cancel it.",
        ],
        &[
            "The file arrives over several connections, and carries on after a cut line or a restart.",
            "To check the file, add its SHA-256 after the address, or tick \"Verify with the .sha256 file beside it\".",
            "From a terminal: leaf download <address> [folder].",
            "Settings > Advanced > Downloads has the download folder, the number of connections and the speed limit.",
        ],
        Go::Page(Page::Advanced),
    )
    .needs(Module::Downloads)
    .label("Open Settings > Advanced"),
    t(
        "downloadmany",
        G_DOWNLOADS,
        "Download many files at once",
        &["batch", "list of links", "pattern", "bulk download", "wget list"],
        &[
            "Open the folder the files should go to.",
            "Press Ctrl+Shift+P and choose \"Download several…\".",
            "Paste the links, one per line. Or write a pattern such as https://example.com/file[001-100].zip or photo[a-f].jpg.",
            "Press Preview to see how many there are.",
            "Press \"Add to the queue\".",
        ],
        &["Lines that are not web addresses are left out and counted."],
        Go::No,
    )
    .needs(Module::Downloads),
    t(
        "downloadmgr",
        G_DOWNLOADS,
        "Manage downloads",
        &["download manager", "downloads page", "download list", "pause", "resume", "speed limit", "queue", "retry", "bandwidth", "cancel", "remove", "priority", "nzb", "paste", "drop"],
        &[
            "Open the Downloads page: choose \"Download list\" in the sidebar, or press Ctrl+Shift+P and choose \"Download list\", or click the title of a download in the Status Center. One list holds every download: web, video, Usenet and from the browser, the running ones on top, then the waiting, then the finished.",
            "To add one: press Add (or Add several) in the toolbar, or paste a link or an NZB file with Ctrl+V, or drop one on the page. It goes into the download folder.",
            "Click a row (Ctrl and Shift add more) and use the buttons above the list or the right-click menu: Pause or Resume, Cancel, Retry (a failed or cancelled one carries on from the part it got), Open, Show in folder and Copy the address.",
            "On a Usenet job, Move up, Move down and High or Low priority change its place in the Usenet queue. Web downloads go in the order they were added.",
            "\"Pause all\" and \"Resume all\" hold or release every download; the button with a speed sets the limit of each download (the same as Settings > Advanced > Downloads).",
            "\"Remove from the list\" (or Delete) takes only the entry off. \"Remove and delete the file\" also moves the finished file to the Trash. \"Clear finished\" takes off all the finished ones.",
        ],
        &["The Status Center keeps its cards as they are; the page lists the same downloads and, after a restart, the last finished and failed ones (the history of transfers)."],
        Go::Cmd("ShowDownloadList"),
    )
    .needs(Module::Downloads)
    .label("Open the Downloads page"),
    t(
        "browserext",
        G_DOWNLOADS,
        "Capture downloads from the browser",
        &["extension", "chrome", "edge", "firefox", "brave", "browser", "add-on", "addon", "capture"],
        &[
            "Open Settings > Advanced > Downloads and press \"Connect browsers\". It needs no administrator rights.",
            "Under it, \"Browser extension, step 2\" names the extension folder. Press \"Open the extension folder\".",
            "In step 3, press \"Copy the address for\" your browser. leaf cannot open that page itself: paste the address into the browser's own address bar.",
            "In Chrome, Edge or Brave: turn on Developer mode, press Load unpacked and choose the chromium folder. In Firefox: press Load Temporary Add-on and choose manifest.json in the firefox folder.",
            "The last line of the card says \"The extension has talked to leaf: Yes\" once it has.",
        ],
        &[
            "Keep \"Capture browser downloads\" on. Files smaller than \"Smallest download to capture\" stay with the browser.",
            "In Firefox a temporary add-on is gone after Firefox restarts.",
            "The extension folder is shipped beside leaf in an installed copy. In a source copy, run extension/build.ps1 to make it.",
        ],
        Go::Page(Page::Advanced),
    )
    .needs(Module::Downloads)
    .label("Open Settings > Advanced"),
    t(
        "video",
        G_DOWNLOADS,
        "Download a video or just its sound from a website",
        &["youtube", "yt-dlp", "ytdlp", "video", "audio", "mp3", "tiktok", "ffmpeg"],
        &[
            "Make sure yt-dlp is installed. leaf does not install it; Settings > Advanced > Downloads, \"Videos from websites\", says whether it was found.",
            "Open the folder the video should go to.",
            "Press Ctrl+Shift+P, choose \"Download here…\" and paste the page address.",
            "In \"Download video or audio\", choose the quality, or Audio only, and press Download.",
        ],
        &[
            "With ffmpeg installed, pictures and sound are merged, and audio can be saved as MP3.",
            "Services that protect their streams are not supported.",
            "Only download what you are allowed to.",
        ],
        Go::Page(Page::Advanced),
    )
    .needs(Module::VideoDl)
    .label("Open Settings > Advanced"),
    t(
        "usenet",
        G_DOWNLOADS,
        "Download from Usenet with an NZB file",
        &["nzb", "usenet", "newsgroup", "nntp", "par2"],
        &[
            "Open Settings > Advanced > Downloads and find \"Usenet servers\". Press Add and enter your server.",
            "Press Test to check it.",
            "Select an .nzb file, press Ctrl+Shift+P and choose \"Download with leaf (NZB)\". Or open the file.",
            "Follow it in the Status Center: it is downloaded, checked, repaired with par2, unpacked and put in the download folder.",
        ],
        &[
            "A job's Queue… menu sets its priority (High, Normal, Low) and moves it up or down.",
            "Press Ctrl+Shift+P and choose \"Pause or resume all Usenet downloads\" to hold every Usenet job.",
            "Every article goes to the primary servers first, then to the backup and fill servers.",
        ],
        Go::Page(Page::Advanced),
    )
    .needs(Module::Usenet)
    .label("Open Settings > Advanced"),
    t(
        "torrent",
        G_DOWNLOADS,
        "Download a torrent through the VPN",
        &["torrent", "magnet", "bittorrent", "p2p", "vpn", "peers"],
        &[
            "Sign in to leaf's VPN (Settings > VPN) and press Connect.",
            "Press Ctrl+Shift+P and choose \"Add a magnet link…\", and paste the link. Or open a .torrent file.",
            "Follow it in the Status Center: its size, its peers and \"through the VPN\". Pause and cancel work as for any download.",
            "When it is complete it is put in the download folder (several files get a folder of their own) and nothing more is uploaded.",
        ],
        &[
            "\"Torrents only while the VPN is on\" (Settings > Advanced > Downloads) is on by default: everything a torrent sends goes through leaf's VPN and nowhere else. While the VPN is off or connecting, torrents wait as \"Waiting for the VPN\" and go on by themselves; when you are signed in to Mullvad, starting a torrent (or leaf starting with one unfinished) connects the VPN for it.",
            "A torrent with several files asks which of them to download.",
            "\"Never upload\" (on by default) means nothing of a torrent is sent to other peers, also while it downloads. Some peers then give less, so a download can be slower.",
            "On the VPN route, trackers and the DHT are asked through the VPN, and leaf is never listed in the DHT. What cannot go through the VPN is off: incoming connections, port forwarding, local discovery and IPv6.",
            "Turn the rule off only if you want torrents on your own connection: every peer and tracker then sees your address. leaf asks before it does that.",
            "A download that was cut off goes on from what is on disk when leaf starts again.",
            "The speed limit of the downloads applies to torrents too.",
        ],
        Go::Page(Page::Advanced),
    )
    .needs(Module::Torrent)
    .label("Open Settings > Advanced"),
    t(
        "vpn",
        G_DOWNLOADS,
        "Send downloads and servers through the VPN",
        &["vpn", "mullvad", "wireguard", "privacy", "kill switch", "socks"],
        &[
            "Open Settings > VPN and press \"Sign in…\". Enter your Mullvad account number.",
            "Press Connect. Pick a Country and City, or leave \"Fastest server\" on.",
            "Under \"What uses the VPN\", turn on \"Downloads through the VPN\". For a server, tick \"Through VPN\" in its entry in Settings > Hosts.",
            "Turn on \"Kill switch for leaf\" so they wait when the VPN is down.",
        ],
        &[
            "Only leaf's own connections use it. Other programs and the rest of the PC do not change, and no administrator rights are needed.",
            "\"Let other programs use the VPN\" opens a SOCKS5 proxy on this PC for a browser or another program.",
            "\"Connect when leaf starts\" connects it for you.",
            "Press Ctrl+Shift+P and choose \"Connect the VPN\" or \"Disconnect the VPN\".",
        ],
        Go::Page(Page::Vpn),
    )
    .needs(Module::Vpn),
    t(
        "nearby",
        G_DOWNLOADS,
        "Send files to a phone or another computer",
        &["localsend", "airdrop", "send", "phone", "share", "nearby", "wifi"],
        &[
            "Select the files.",
            "Right-click and choose \"Send with Nearby\", then the device. Or press Ctrl+Shift+P and choose \"Send to device\".",
            "The other side must run LocalSend or leaf and accept.",
        ],
        &[
            "\"Send to device\" sends to the device used last, or the only one found.",
            "Settings > Nearby is an experimental page. While it is on screen, this PC shows to everyone nearby.",
        ],
        Go::Page(Page::Nearby),
    )
    .needs(Module::Shares),
    t(
        "sync",
        G_TOOLS,
        "Keep two folders the same (sync or backup)",
        &["sync", "backup", "mirror", "two-way", "robocopy", "rsync", "synchronize"],
        &[
            "Open a folder. Press Ctrl+Shift+D and open the other folder in the other pane.",
            "Press Ctrl+Shift+P and choose \"Sync with…\". The other pane's folder is the right side.",
            "Choose Mirror, Backup or Two-way, and the filters.",
            "Press Preview. Every row shows what would happen. Click a row to skip it or to copy the other way.",
            "Press Run. The job shows in the Status Center.",
        ],
        &[
            "Removed and replaced files go to the Trash, and on a server into .leaf-sync-trash.",
            "If one side is empty, leaf asks before it removes files because of that.",
            "The saved jobs, with their last run, are on the Sync jobs page. There you can preview, run, edit or delete a job, and press New job for one from scratch. A job can also run by itself every so many hours or at a time of day.",
        ],
        Go::Cmd("SyncJobs"),
    )
    .needs(Module::Sync)
    .label("Open Sync jobs"),
    t(
        "friendsync",
        G_TOOLS,
        "Keep a folder the same as a friend's",
        &["share", "friend", "invite", "peer", "wireguard", "shared folder"],
        &[
            "Both of you run leaf.",
            "Select the folder, press Ctrl+Shift+P and choose \"Share with a friend…\". leaf makes an invite. Send it to your friend.",
            "Your friend presses Ctrl+Shift+P, chooses \"Join shared folder…\", pastes the invite and picks where the folder goes.",
            "\"Shared folders\" lists them: with whom, whether they are in sync, the last sync.",
        ],
        &[
            "No server is in between. The PCs reach each other by address, over your WireGuard or the local network.",
            "A file changed on both sides keeps both versions. Removed files go to the Trash or .leaf-sync-trash.",
        ],
        Go::Cmd("SharedFolders"),
    )
    .needs(Module::Shares)
    .label("Open Shared folders"),
    // ---------------------------------------------------------------- Coding agents
    t(
        "agents",
        G_AGENTS,
        "See what coding agents are doing",
        &["agent", "claude", "claude code", "ai", "session", "sessions"],
        &[
            "Open Settings > Advanced, under Developer tools, and turn on \"Agent layer\".",
            "Press Ctrl+Shift+P and choose \"Agents\".",
            "The sessions are listed by project folder, with their branch, state and last activity.",
            "Open a session's folder, or a terminal in it, from its row.",
        ],
        &[
            "leaf only reads the agent's own session files. It never changes them.",
            "A small dot shows on the folder and the tab of a running session.",
        ],
        Go::Cmd("ShowAgents"),
    )
    .needs(Module::Agents),
    t(
        "agentfiles",
        G_AGENTS,
        "Let an agent move files through leaf",
        &["mcp", "leaf-mcp", "model context protocol", "claude desktop", "tools", "agent files"],
        &[
            "Open Settings > Advanced, under Developer tools, and turn on \"Let agents move and delete files through leaf\".",
            "Tell the agent to start leaf-mcp.exe, which sits next to leaf.exe. For Claude Code: claude mcp add leaf -- \"C:\\Program Files\\leaf\\leaf-mcp.exe\" (use the path where leaf is installed).",
            "Keep leaf running. The first change of a session asks you in leaf.",
            "Every change shows in the Status Center and can be undone.",
        ],
        &[
            "Off, the agent can only show a file, ask you to pick files, search and list folders.",
            "The agent may work only inside its session's project folder. Nothing is overwritten, and there is no permanent delete.",
        ],
        Go::Page(Page::Advanced),
    )
    .label("Open Settings > Advanced"),
    t(
        "protect",
        G_AGENTS,
        "Keep an agent away from some files",
        &["claude", "permissions", "deny", "settings.local.json", "protect", "allow agents"],
        &[
            "Select the files or folders.",
            "Press Ctrl+Shift+P and choose \"Protect from agents\". leaf shows the lines it would add and the file.",
            "Confirm. It writes .claude/settings.local.json, unless you choose the shared file.",
            "\"Allow agents here\" does the opposite. \"Agent rules\" lists the rules and removes the ones you tick.",
        ],
        &[],
        Go::Cmd("ShowAgentRules"),
    )
    .needs(Module::Agents)
    .label("Open Agent rules"),
    t(
        "checkpoint",
        G_AGENTS,
        "Save a folder's state and go back to it",
        &["checkpoint", "snapshot", "restore", "undo changes", "rollback"],
        &[
            "Open the folder and press Ctrl+Shift+P, then choose \"Checkpoint\".",
            "Later, choose \"Checkpoints\". It lists them with their time, note and size.",
            "Press Restore… on one. leaf shows what the restore would change first.",
        ],
        &[
            "In a git repository a checkpoint is a hidden snapshot inside it. Your files, the index, the branch and the stash stay as they are.",
            "Elsewhere it is a copy in leaf's data folder, with its size shown first.",
            "A restore can be taken back.",
        ],
        Go::Cmd("ShowCheckpoints"),
    )
    .needs(Module::Dev)
    .label("Open Checkpoints"),
    t(
        "review",
        G_AGENTS,
        "Look at what changed in a project",
        &["git", "diff", "changes", "review", "stage", "commit", "revert"],
        &[
            "Open a folder of a git repository.",
            "Press Ctrl+Shift+P and choose \"Show changes\" for every changed, new, deleted and renamed file, with its difference in the preview pane.",
            "Choose \"Review changes\" to accept (stage) or revert a whole file or one change, and then commit what you accepted.",
        ],
        &[
            "\"Review changes\" needs the agent layer on. Reverting asks first and keeps your version in the Trash. leaf never pushes.",
            "\"Show changes\" only reads, apart from \"Revert this file\" on a row.",
        ],
        Go::No,
    )
    .needs(Module::Dev),
    t(
        "lane",
        G_AGENTS,
        "Start a separate lane of work for a task",
        &["worktree", "branch", "parallel", "lane", "git worktree"],
        &[
            "Open a folder of a git repository.",
            "Press Ctrl+Shift+P and choose \"New lane…\".",
            "Pick a name and a base, and optionally a coding agent to start in a terminal there.",
            "The lane opens in a new tab, in a worktree beside the repository.",
            "When you are done, choose \"End lane…\" to review, merge, remove or discard it.",
        ],
        &[
            "A merge happens only when both folders are clean and nothing conflicts. A worktree with work that is not merged is not removed.",
            "Where lanes are put is set in Settings > Advanced, under \"Lane folders\".",
        ],
        Go::No,
    )
    .needs(Module::Agents),
    // ---------------------------------------------------------------- leaf itself
    t(
        "defaultfm",
        G_APP,
        "Make leaf the default file manager",
        &["default", "win+e", "explorer", "replace explorer", "default-fm"],
        &[
            "Open Settings > Advanced and find \"Set as default file manager\" under Experimental.",
            "Turn it on. Folders and Win+E now open with leaf.",
            "Turn it off to give them back to what had them.",
        ],
        &[
            "It applies to this user only.",
            "From a terminal: leaf default-fm status, on or off.",
            "A copy run from a build folder, without leaf-open.exe beside it, is refused.",
        ],
        Go::Page(Page::Advanced),
    )
    .label("Open Settings > Advanced"),
    t(
        "features",
        G_APP,
        "Turn parts of leaf on or off",
        &["modules", "features", "switch off", "hide", "setup", "first run", "tour"],
        &[
            "Open Settings > Features. Each part of leaf has a switch.",
            "A command of a part that is off is not offered. \"What leaf can do\" lists it with a note.",
            "To walk through the choices again, press Ctrl+Shift+P and choose \"Setup guide\".",
        ],
        &["Nothing changes in the setup guide until you reach its end."],
        Go::Page(Page::Modules),
    ),
    t(
        "history",
        G_APP,
        "See the copies and downloads that finished",
        &["transfer history", "log", "history", "speed", "finished"],
        &[
            "Press Ctrl+Shift+P and choose \"Transfer history\".",
            "Each row has what, from, to, size, time, speed and result.",
            "Use \"Clear history\" to empty it, or the switch there to stop recording.",
        ],
        &["It is kept on this PC only."],
        Go::Cmd("ShowTransferHistory"),
    ),
    t(
        "numbers",
        G_APP,
        "Share a picture of your totals",
        &["share card", "stats", "numbers", "picture", "screenshot", "png"],
        &[
            "Open Settings > Your numbers to see your totals.",
            "Press Ctrl+Shift+P and choose \"Share card\".",
            "Tick the lines to put on the picture, then copy or save it.",
        ],
        &["No folder, file or user names are on the picture, and nothing is sent anywhere."],
        Go::Cmd("ShareCard"),
    ),
    t(
        "speed",
        G_APP,
        "See how fast leaf is opening folders",
        &["performance", "fps", "memory", "speed overlay", "hud", "benchmark"],
        &[
            "Press Ctrl+Shift+P and choose \"Speed overlay\".",
            "A small readout shows in the corner: how long the current folder took to open, the last frame and the memory leaf holds.",
            "Choose it again to hide it.",
        ],
        &["It is off by default and costs nothing while it is off. Settings > Advanced has the same switch."],
        Go::Cmd("ToggleSpeedHud"),
    ),
    t(
        "thispc",
        G_APP,
        "See the details of this PC and of leaf",
        &["neofetch", "system info", "specs", "about", "version", "cpu", "gpu"],
        &[
            "Open Settings > About, or press Ctrl+Shift+P and choose \"About this PC and leaf\".",
            "Read the summary. It can be copied as text or as a picture.",
        ],
        &[
            "No computer name, user name, serial number or address is shown.",
            "Settings > About also says whether leaf is installed for all users or for this user only.",
        ],
        Go::Cmd("ShowThisPc"),
    ),
    t(
        "guide",
        G_APP,
        "Find help for a task",
        &["help", "how do i", "guide", "tutorial", "f1", "?"],
        &[
            "Press F1 to open this guide, or choose Guide in the title bar's main menu.",
            "Type in the search box: words from the task, or a word such as ftp, nas, zip or screenshot.",
            "Click a topic to open its steps. \"Do it\" starts it where it can be started.",
        ],
        &[
            "A small ? on some pages (Sync jobs, Disk usage, Hosts, Downloads, Usenet, VPN, Agents and the viewer's menu) opens the topic for that page.",
            "Typing in the command palette (Ctrl+Shift+P) also offers topics, starting with \"How do I:\".",
        ],
        Go::No,
    ),
];

/// The topic named `id`.
pub fn by_id(id: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.id == id)
}
