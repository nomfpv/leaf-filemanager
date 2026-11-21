# What leaf can do

Generated from the command registry (`crates/rf-app/src/registry.rs`); do not edit by hand.
The same list is in leaf under Settings > What leaf can do, and in the command palette (Ctrl+Shift+P).
To write it again: `LEAF_WRITE_FEATURES=1 cargo test -p rf-app features_md`.

## Tabs and windows

| Command | What it does | Shortcut |
|---|---|---|
| New tab | Opens a new tab next to the current one. | `Ctrl+T` |
| New window | Opens another leaf window. | `Ctrl+N` |
| Close tab | Closes the current tab. | `Ctrl+W` |
| Duplicate tab | Opens the current folder again in a new tab. | `Ctrl+Shift+K` |
| Close other tabs | Closes every tab but the current one. |  |
| Close all tabs | Closes every tab of the window. |  |
| Reopen closed tab | Opens the tab closed last again. | `Ctrl+Shift+T` |
| Full screen | Fills the screen with the window. | `F11` |
| Always on top | Keeps the window above other windows. |  |
| Compact overlay | Turns the window into a small one that stays on top. |  |
| Search tabs | Finds an open tab or one closed recently. | `Ctrl+Shift+A` |
| Open in new window | Opens the selected folder in a new window. | `Ctrl+Alt+Enter` |
| Next tab | Goes to the tab on the right. | `Ctrl+Tab` |
| Previous tab | Goes to the tab on the left. | `Ctrl+Shift+Tab` |

## Going places

| Command | What it does | Shortcut |
|---|---|---|
| Refresh | Reads the folder again. | `F5`, `Ctrl+R` |
| Back | Goes back to the folder before. | `Alt+←`, `Backspace` |
| Forward | Goes forward again after going back. | `Alt+→` |
| Up one level | Opens the folder this one is in. | `Alt+↑` |
| Home | Opens the Home page. | `Alt+Home` |
| Recycle Bin | Opens the Recycle Bin. |  |
| Focus address bar | Puts the keyboard in the address bar to type a path. | `Ctrl+L`, `Alt+D` |
| Search | Searches this folder. | `Ctrl+F`, `Ctrl+E` |
| Go to folder alias | Opens a folder by its short name: type @ and the name in the address bar. | `@` |
| Search past folders | Finds a folder you went to before. | `Ctrl+P` |
| Open | Opens the selected file or folder. | `Enter` |
| Next part of the window | Moves the keyboard to the next part: address bar, folder tree, file list. | `Tab` |
| Previous part of the window | Moves the keyboard to the part before. | `Shift+Tab`, `Shift+Tab` |

## Selecting

| Command | What it does | Shortcut |
|---|---|---|
| Select all | Selects every item in the folder. | `Ctrl+A` |
| Invert selection | Selects what is not selected, and the other way round. |  |
| Toggle item selection | Adds the item under the cursor to the selection or takes it out. | `Ctrl+Space` |
| Select next item | Moves the selection down by one. | `↓` |
| Select previous item | Moves the selection up by one. | `↑` |
| Select item to the left | Moves the selection left in the grid, or out of a folder in the columns view. | `←` |
| Select item to the right | Moves the selection right in the grid, or into a folder in the columns view. | `→` |
| Select first item | Selects the first item of the folder. | `Home` |
| Select last item | Selects the last item of the folder. | `End` |
| Extend selection down | Adds the next item to the selection. | `Shift+↓` |
| Extend selection up | Adds the item above to the selection. | `Shift+↑` |
| Page down | Moves the selection down by a screenful. | `Pagedown` |
| Page up | Moves the selection up by a screenful. | `Pageup` |
| Clear selection | Selects nothing. | `Escape` |

## Files and folders

| Command | What it does | Shortcut |
|---|---|---|
| New folder | Makes a new folder here and lets you name it. | `Ctrl+Shift+N` |
| New text document | Makes a new empty text file here. |  |
| Add to shelf | Puts the selected items on the shelf to drop them somewhere else later. | `Ctrl+Shift+S` |
| Copy | Copies the selected items to paste them somewhere else. | `Ctrl+C` |
| Cut | Takes the selected items to paste them somewhere else. | `Ctrl+X` |
| Paste | Puts the copied or cut items into this folder. | `Ctrl+V` |
| Paste into selected folder | Puts the copied items into the selected folder. | `Ctrl+Shift+V` |
| Paste as shortcut | Makes shortcuts to the copied items here. |  |
| Move to Recycle Bin | Moves the selected items to the Recycle Bin. | `Del`, `Ctrl+D` |
| Delete permanently | Deletes the selected items without the Recycle Bin. | `Shift+Del` |
| Rename | Renames the selected item. | `F2` |
| Duplicate | Makes a copy of the selected items next to them. | `Ctrl+Alt+D` |
| Undo | Takes back the last copy, move, rename or delete. | `Ctrl+Z` |
| Open with | Opens the selected file in a program you choose. |  |
| Properties | Shows the details of the selected items. | `Alt+Enter` |
| Context menu | Opens the right-click menu for the selected items. | `Shift+F10`, `Menu` |
| Redo | Does again what Undo took back. | `Ctrl+Y` |

## Copying paths

| Command | What it does | Shortcut |
|---|---|---|
| Copy path | Copies the full path of the selected items. | `Ctrl+Shift+C` |
| Copy path as quoted | Copies the paths in quotes, ready for a command line. | `Ctrl+Alt+C` |
| Copy path as name only | Copies just the names of the selected items. |  |
| Copy path as folder path | Copies the path of the folder the items are in. |  |
| Copy path with forward slashes | Copies the paths written with / instead of \. |  |
| Copy path relative to repository | Copies the paths from the root of their Git repository. |  |
| Copy path as @path | Copies the paths with an @ in front, as coding assistants take them. |  |
| Copy path as WSL path | Copies the paths as Linux under WSL sees them (/mnt/c/...). (Windows only) |  |
| Copy path as file URL | Copies the paths as file:/// addresses. |  |
| Copy as context | Copies the selected files (or this folder) as one text block for a chat: a tree, then each text file under its path. Leaves out .gitignore'd, binary, big and secret files. |  |

## View

| Command | What it does | Shortcut |
|---|---|---|
| Customize Home | Opens Home with buttons to add, remove, resize and reorder its widgets. |  |
| Reset Home | Puts the Home page back to its default widgets. |  |
| Show/hide hidden items | Shows or hides hidden files and folders. | `Ctrl+H` |
| Hide git-ignored files | Inside a git repository, leaves out the files and folders .gitignore ignores (node_modules, target, logs...). The status line says how many are hidden. |  |
| Details view | Shows the folder as a list with columns. | `Ctrl+Shift+1` |
| Grid view | Shows the folder as large icons. | `Ctrl+Shift+2` |
| Columns view | Shows the folder and the ones above it side by side. | `Ctrl+Shift+3` |
| Cards view | Shows each item as a card with its details. | `Ctrl+Shift+4` |
| List view | Shows the folder as a compact list of names. | `Ctrl+Shift+5` |
| Zoom in | Makes the items larger. | `Ctrl+=`, `Ctrl++` |
| Show/hide shelf | Shows or hides the shelf. |  |
| Show full path in address bar | Shows the path as text instead of the clickable parts. |  |
| Zoom out | Makes the items smaller. | `Ctrl+-` |
| Preview pane | Shows or hides the preview of the selected file. | `Alt+P` |
| Sidebar | Shows or hides the sidebar. | `Ctrl+B` |
| Light / dark theme | Switches between the light and the dark theme. | `Ctrl+Alt+T` |
| Tool bar | Shows or hides the tool bar. | `Ctrl+Shift+B` |

## Panes

| Command | What it does | Shortcut |
|---|---|---|
| Toggle dual pane | Splits the tab into two folders side by side, or joins them again. | `Ctrl+Shift+D` |
| Switch pane | Moves the keyboard to the other pane. | `F6` |
| Split pane side by side | Splits the current pane into two, left and right. |  |
| Split pane top and bottom | Splits the current pane into two, one above the other. |  |
| Close active pane | Closes the pane that has the keyboard. | `Ctrl+Shift+W` |
| Copy to other pane | Copies the selected items into the other pane's folder. | `Shift+F5` |
| Move to other pane | Moves the selected items into the other pane's folder. | `Shift+F6` |

## Tools

| Command | What it does | Shortcut |
|---|---|---|
| Map network drive | Gives a shared network folder a drive letter. (Windows only) |  |
| Disconnect network drive | Removes a mapped network drive. (Windows only) |  |
| Rotate right | Turns the selected pictures a quarter to the right. |  |
| Rotate left | Turns the selected pictures a quarter to the left. |  |
| Open in terminal | Opens a terminal in this folder. | `` Ctrl+Shift+` `` |
| Open in Terminal as administrator | Opens an elevated terminal in this folder. (Windows only) |  |
| Open in editor | Opens this folder in your code editor. |  |
| Compare folders | Marks what differs between the two panes' folders: only here, newer, older, other size. |  |
| Find duplicates | Looks for files with identical content in this folder or the selected folders. |  |
| Find build junk | Looks in this folder for node_modules, target, .venv, caches and other folders a build makes again, with their size, and moves the ones you tick to the Recycle Bin. |  |
| Show changes | Lists every changed, new, deleted and renamed file of the git repository this folder is in, grouped by folder, with the difference of each in the preview pane. It only reads: nothing in the repository is changed. |  |
| Worktrees | Lists the worktrees of the git repository this folder is in, by branch, to jump to one. It only reads. |  |
| New worktree | Adds a second working folder to the git repository this folder is in, on a new branch from the current head or on an existing branch, and opens it in a new tab. Nothing that exists is changed. |  |
| Organize folder | Sorts the folder's files into folders by kind. |  |

## leaf itself

| Command | What it does | Shortcut |
|---|---|---|
| Restart leaf | Closes leaf and starts it again with the same tabs. | `Ctrl+Alt+Shift+R` |
| Save session | Saves the open tabs as a session to open later. |  |
| Settings | Opens leaf's settings. | `Ctrl+,` |
| What leaf can do | Lists every command with what it does and its shortcut. |  |
| Command palette | Finds any command by typing a few letters of its name, or a path to go to. | `Ctrl+Shift+P`, `F1` |
