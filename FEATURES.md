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
| Trash | Opens the Trash. |  |
| Focus address bar | Puts the keyboard in the address bar to type a path. | `Ctrl+L`, `Alt+D` |
| Search | Searches this folder. | `Ctrl+F`, `Ctrl+E` |
| Search in file contents | Searches the text inside the files of this folder and its subfolders (plain text, or a regular expression with regex:); text files only, big and binary files are skipped; hits stream in with the matching line. |  |
| Bookmark remote folder | Adds the folder shown on a saved host to that host's bookmarks, shown under it in the sidebar; run it again in a bookmarked folder to take the bookmark away. |  |
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
| Download here | Downloads a web address (http or https) into this folder: several connections at once, resumed after a cut line or a restart, shown in the Status Center with pause and cancel. An address on the clipboard is already in the box. From a terminal: leaf download <address> [folder]. |  |
| Download with leaf (NZB) | Downloads what the selected NZB files list from your Usenet servers (Settings > Advanced > Downloads > Usenet servers): checked and repaired with par2, unpacked and put in the download folder, shown in the Status Center. |  |
| Pause or resume all Usenet downloads | Holds every Usenet job (or lets them go on again). |  |
| Add a magnet link | Downloads a torrent from its magnet link into this folder (the download folder when this is not a folder on this PC). With "Torrents only while the VPN is on" (Settings > Advanced > Downloads, on by default) everything it sends goes through leaf's VPN and nowhere else, and it waits while the VPN is not connected. A link on the clipboard is already in the box. From a terminal: leaf download <magnet link> [folder]. |  |
| Download with leaf (torrent) | Downloads what the selected .torrent files describe into the download folder, shown in the Status Center with pause and cancel. Through leaf's VPN only, unless "Torrents only while the VPN is on" is turned off. Nothing is uploaded once a torrent is complete. |  |
| Pause all | Holds every running and waiting copy, move, sync, download (web, video, Usenet), upload and server-to-server copy until you choose Resume all. The Status Center and the status bar list what is held and for how long. A job that cannot pause says so and finishes. Not the VPN. A restart starts without it. |  |
| Resume all | Lets the jobs that Pause all held go on again; jobs you paused yourself stay paused. |  |
| Download list | Opens the Downloads page: one list of every download (web addresses, videos, Usenet, the browser's) with its state, progress, size, speed, time left and where it was saved. Pause, cancel, retry, open the file, show it in its folder, copy the address, take it off the list. Drop or paste a link or an NZB file on it to add one. |  |
| Download several | Downloads many web addresses into this folder: paste links (one per line) or a pattern such as file[001-100].zip or photo[a-f].jpg. A list shows how many there are before they are added to the download queue. |  |
| Send to device | Sends the selected items to a device nearby: a phone or computer running LocalSend, or another leaf. With several devices in reach, the right-click menu (Send with Nearby) lets you pick; this sends to the one used last, or to the only one found. |  |
| Add to shelf | Puts the selected items on the shelf to drop them somewhere else later. | `Ctrl+Shift+S` |
| Add to basket | Puts the selected items in this project's context basket, a named set of files to hand to an assistant later. |  |
| Remove from basket | Takes the selected items out of this project's context basket. |  |
| New basket | Starts a named context basket for this project; the selected items go in it. |  |
| Copy | Copies the selected items to paste them somewhere else. | `Ctrl+C` |
| Cut | Takes the selected items to paste them somewhere else. | `Ctrl+X` |
| Paste | Puts the copied or cut items into this folder. | `Ctrl+V` |
| Paste into selected folder | Puts the copied items into the selected folder. | `Ctrl+Shift+V` |
| Paste as shortcut | Makes shortcuts to the copied items here. |  |
| Move to Trash | Moves the selected items to the Trash. | `Del`, `Ctrl+D` |
| Delete permanently | Deletes the selected items without the Trash. | `Shift+Del` |
| Rename | Renames the selected item. | `F2` |
| Duplicate | Makes a copy of the selected items next to them. | `Ctrl+Alt+D` |
| Undo | Takes back the last copy, move, rename or delete. | `Ctrl+Z` |
| Protect from agents | Adds rules to Claude Code's settings so it may not read or change the selected items. Shows the lines and the file first, writes .claude/settings.local.json unless you choose the shared file. |  |
| Allow agents here | Adds rules to Claude Code's settings so it may read and change the selected items without asking. Shows the lines and the file first, writes .claude/settings.local.json unless you choose the shared file. |  |
| Open with | Opens the selected file in a program you choose. |  |
| Properties | Shows the details of the selected items. | `Alt+Enter` |
| Context menu | Opens the right-click menu for the selected items. | `Shift+F10`, `Menu` |
| Redo | Does again what Undo took back. | `Ctrl+Y` |

## Copying paths

| Command | What it does | Shortcut |
|---|---|---|
| Copy basket as context | Copies the files of this project's context basket as one text block, with a token estimate. |  |
| Copy basket as @paths | Copies the files of this project's context basket as @path mentions, one per line. |  |
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
| Share card | Draws a picture of some of your own totals (files moved, data copied, fastest transfer, days in a row) in leaf's look with the leaf logo, to copy or save. A list first shows every line with its value and a tick box; no folder, file or user names are on the picture, and nothing is sent anywhere. |  |
| Speed overlay | Shows or hides a small readout in the corner of the window: how long the current folder took to open (and its item count), how long the last frame took, and how much memory leaf holds. Off by default; it costs nothing while off. |  |
| Transfer history | Lists the copies, moves, downloads and uploads that finished (what, from, to, size, time, speed, result), with Clear history and the switch that turns recording off. Kept on this PC only. |  |
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
| Show project brain | Lists what an agent reads in this project: CLAUDE.md, AGENTS.md, the .claude folder (skills, settings, memory) and .mcp.json, with a search by name and the file shown in the preview pane. |  |
| Rotate right | Turns the selected pictures a quarter to the right. |  |
| Rotate left | Turns the selected pictures a quarter to the left. |  |
| Convert to video file | Turns an unprotected Blu-ray (a disc, a BDMV folder, an .iso holding one), an AVCHD card or .m2ts files into one normal video file: the title, audio and subtitle tracks you choose, with chapters, without losing quality (MKV); with ffmpeg installed also a smaller re-encoded file. Copy-protected discs are not converted. |  |
| Remove metadata | Removes what a picture (JPEG, PNG, WebP, HEIC, AVIF, TIFF, GIF, SVG), PDF, Office or OpenDocument file, or a music or video file, says about who made it, with what, where and when (EXIF and GPS, XMP, IPTC, comments and their authors, tracked-change authors, document properties, tags, thumbnails, every alternate data stream and extended attribute). Tracks of a video that are not picture or sound (GPS telemetry, timecode, text) are emptied and named. Signed documents are refused unless you tick that the signatures go too. Camera RAW, Matroska and AVI files, old or password-protected Office files, Photoshop, RTF and other kinds without a remover, and archives are refused and left untouched, with the reason; for a kind leaf does not read only the download marker is removed and it is not counted as cleaned. It writes cleaned copies beside the originals, or replaces them (the originals go to the Recycle Bin and Ctrl+Z puts them back; on a drive without a Recycle Bin a cleaned copy is saved beside the original instead), and checks each result by reading it back and by a second, independent check of its structure and bytes. Show metadata lists what is there and changes nothing. Files it cannot clean safely are refused and left as they are. What the content itself shows (faces, text in a picture, watermarks) is not touched. |  |
| Mount image | Mounts the selected disc image (.iso, .img) read-only and opens it; it shows under Drives until it is ejected. On Linux without root, through udisks (or gio, or fuseiso). |  |
| Eject image | Unmounts the selected disc image that was mounted. |  |
| Mount discs in drives | Mounts the discs in the optical drives that the desktop has not mounted, through udisks (or gio). (Linux only) |  |
| Open in terminal | Opens a terminal in this folder; in a server (sftp://) folder it runs ssh into it. | `` Ctrl+Shift+` `` |
| Open in Terminal as administrator | Opens an elevated terminal in this folder. (Windows only) |  |
| Open in editor | Opens this folder in your code editor. |  |
| Compare folders | Marks what differs between the two panes' folders: only here, newer, older, other size. |  |
| Find duplicates | Looks for files with identical content in this folder or the selected folders. |  |
| Find build junk | Looks in this folder for node_modules, target, .venv, caches and other folders a build makes again, with their size, and moves the ones you tick to the Trash. |  |
| Follow file on a server | In a server (sftp://) folder: keeps reading the end of the file under the cursor every 2 seconds and shows it in leaf-look (a log that grows). Press it again to stop; it stops when the tab closes. | `Alt+F` |
| Show changes | Lists every changed, new, deleted and renamed file of the git repository this folder is in, grouped by folder, with the difference of each in the preview pane. It only reads, apart from "Revert this file" on a row, which asks first and keeps your version in the Trash. |  |
| Checkpoint | Saves how this folder is now, to put it back later with one restore. In a git repository it is a hidden snapshot inside the repository (your files, the index, the branch and the stash stay as they are; ignored files are left out); elsewhere a copy in leaf's data folder, with its size shown first. |  |
| Checkpoints | Lists the checkpoints of this folder (or of its git repository) with their time, note and size; each can be restored after a look at what it would change, and the restore can be taken back. |  |
| Review changes | Every changed file of this folder's repository in one list, with its difference inline or side by side: accept (stage) or revert a whole file or one change, then commit what you accepted. Outside a repository it compares with a checkpoint. Reverting asks first and keeps your version in the Trash; leaf never pushes. Needs the agent layer on (Settings). |  |
| Agents | Lists the coding agent sessions running now and the recent ones (Claude Code), grouped by project folder, with their branch, state (running, waiting for you, idle, ended) and last activity; open a session's folder or a terminal in it. Read from the agent's own session files; needs the agent layer on (Settings). |  |
| Sync jobs | Lists the saved sync and backup jobs (Mirror, Backup, Two-way; between folders on this computer or with a server) with their last run; each can be previewed row by row, run in the Status Center, edited or deleted. Removed and replaced files go to the Trash, on a server into .leaf-sync-trash. |  |
| Connect the VPN | Brings leaf's VPN up (Mullvad, Settings > VPN). Only leaf's own connections use it: the downloads and hosts set to go through it; nothing else on the PC changes. |  |
| Disconnect the VPN | Takes leaf's VPN down. With the kill switch on, what is set to use it waits until it is connected again. |  |
| Sync with | Starts a sync job from this folder (or the selected folder), with the other pane's folder as the right side: choose Mirror, Backup or Two-way and the filters, then preview what it would do before anything is copied. |  |
| Share with a friend | Keeps this folder (or the selected folder) the same on your PC and a friend's, who runs leaf too: makes an invite to send them. No server in between; the PCs reach each other by address (your WireGuard or local network). Removed files go to the Trash or .leaf-sync-trash, a file changed on both sides keeps both. |  |
| Join shared folder | Joins a folder a friend shared with you from their leaf: paste their invite and choose where the folder goes on this PC. |  |
| Shared folders | Lists the folders shared with a friend's leaf (on the Sync jobs page): with whom, whether they are in sync, the last sync; pause, leave, the other PC's address, whether the friend may change files. |  |
| Docker containers | Lists the Docker containers (running or stopped) as folders; a container lists its bind mounts and volumes, which open as the folders they stand for. In a folder on an sftp:// host it asks that host. It only reads. |  |
| Worktrees | Lists the worktrees of the git repository this folder is in, by branch, to jump to one. It only reads. |  |
| New worktree | Adds a second working folder to the git repository this folder is in, on a new branch from the current head or on an existing branch, and opens it in a new tab. Nothing that exists is changed. |  |
| New lane | Starts a lane for one piece of work in the git repository this folder is in: a new worktree beside the repository on a new branch from the current head (or a base you choose), opened in a new tab, with a coding agent started in a terminal there if you pick one. Needs the agent layer on (Settings). |  |
| End lane | Ends the lane this folder is in, after showing its commits and uncommitted files: review its changes, merge it into its base branch (only when both folders are clean and nothing conflicts), remove its worktree (refused while it has work that is not merged), or discard it to the Trash. |  |
| Organize folder | Sorts the folder's files into folders by kind. |  |
| Agent rules | Lists the permission rules Claude Code reads for this project (allow, ask, deny; user, project and local file) and removes the ones you tick. |  |

## The viewer (Space)

| Command | What it does | Shortcut |
|---|---|---|
| Edit in the viewer | In the Space viewer, edits a text, Markdown or CSV file in place and saves it. | `E` |
| Rotate a picture in the viewer | In the Space viewer, turns the picture a quarter to the right (Shift+R: to the left). | `R` |
| Slideshow in the viewer | In the Space viewer, steps through the pictures of the folder by itself; press it again to stop. | `S` |
| Camera RAW files developed | A camera RAW file with a small embedded picture is developed from the sensor data in the background and swapped in when ready. |  |
| First bytes in the preview pane | A file of an unknown kind shows its first 512 bytes as hex rows with the text beside them, as the Space viewer's hex view does. |  |

## leaf itself

| Command | What it does | Shortcut |
|---|---|---|
| Restart leaf | Closes leaf and starts it again with the same tabs. | `Ctrl+Alt+Shift+R` |
| Save session | Saves the open tabs as a session to open later. |  |
| About this PC and leaf | Opens a summary in the style of a terminal's system information: the leaf as text art, and beside it the system, processor, memory, graphics, drives (sizes only), display, leaf's version and build, how long the last folder took to open and how many files are indexed. Copy it as text or as a picture. No computer name, user name, serial number or address is shown. |  |
| Settings | Opens leaf's settings. | `Ctrl+,` |
| What leaf can do | Lists every command with what it does and its shortcut. |  |
| Guide | Short steps for things you may want to do ("How do I ..."), searchable: connect to a server, preview a file, capture downloads from the browser and more. | `F1` |
| Setup guide | Walks through the look, what you use leaf for and which parts of leaf are on. Nothing changes until you finish it. |  |
| Command palette | Finds any command by typing a few letters of its name, or a path to go to. | `Ctrl+Shift+P` |
