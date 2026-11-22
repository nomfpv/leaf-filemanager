# Extensions

leaf has a small, generic seam for extensions. An extension is a separate program crate that depends on `rf-app` as a library and
starts leaf with its registrations. leaf built normally registers none and behaves exactly as without the seam; nothing in this
repository depends on an extension, and removing an extension is removing its crate.

```rust
fn main() {
    let ext = rf_app::ext::Extensions::new().add("my-ext", "My extension", |e| {
        e.subscribe(|ev, _cx| println!("{ev:?}"));
    });
    rf_app::run_with(ext); // rf_app::run() is run_with(Extensions::new())
}
```

A complete small one is `crates/rf-app/examples/hello_ext.rs`; it uses one thing from every group below, so the whole API is
compile-proven.
`rf_app::ext` re-exports `gpui_kit`, so an extension builds its elements with the same toolkit version as the application.

## Rules

- **Callbacks run on the UI thread and must be quick.** Events are delivered after the code that caused them has finished (never
  while one of leaf's views is being updated), so a callback may read and write through `&mut App`.
- **Display only.** Presentation hooks (display name, row decoration, icon override, layers, slots, scenes) are called from render
  code. Operations read names and paths from leaf's own model and never see what a hook returned: copy, move, delete, rename, copy
  path and properties use the real name. A registered but idle extension costs one atomic load per file row.
- **Switch off.** Settings > About lists the extensions ("Extensions: none" in a normal build) with a switch per extension for the
  session. A switched-off extension gets no events, draws nothing and none of its hooks are called, so an extension cannot lock the
  user out.
- **Panics.** Every callback runs inside `catch_unwind`: in the dev and fast profiles a panic is logged and the callback skipped. The
  release profile is built with `panic = "abort"`, so there a panic in a callback ends leaf like any panic; write callbacks without
  `unwrap` and keep them short.
- An extension has an `id` (short, letters, digits, `-`, `_`; it names its data folder) and a `name` (shown in About).

## Registrations (all optional)

Each is a method of the `Registrar` given to the closure of `Extensions::add`.

**Events**: `subscribe(|&Event, &mut App|)`. `Event` is `#[non_exhaustive]`; match with a `_` arm.
`AppStart`, `AppExit`, `FolderOpened { path, kind, items, load_ms }`, `SelectionChanged { count, total_bytes }`,
`OperationStarted/Progress/Finished` (every Status Center job, with the job `id`, a typed `kind: OpKind` (Copy, Move, Delete,
Rename, Extract, Compress, Download, Upload, Sync, Other), leaf's own title, items, bytes, speed, current name, result; `Rename` and
`Other` are reported as `Other` for jobs that do not say what they are), `ItemsDeleted { names, sizes, permanent, total }`,
`ArchiveExtractStarting { info }` / `ArchiveExtracted { info, result }` (`ExtractInfo`: archive, destination, entry count, total
size, the largest entry's name and `FileKind`; `result` is `Done`, or `Failed("")` when it did not finish: the matching
`OperationFinished` says why), `DriveSpace { drive, free, total }` (when a drive's space moved by at least 1 percent of its size;
drives are read when they change, and every half minute while an extension is registered), `TransferSpeed { id, bytes_per_sec }` (at
most twice a second per job), `DownloadFinished { name, size, kind }`, `WindowFocus { focused }`, `WindowMinimized { minimized }`,
`Frame { dt_ms }` (only while frames were asked for, see Animation), `FriendLink { friend, friend_name, online }`,
`PeerMessage { .. }`, `ItemRenamed { from, to }` (an item of a folder on this PC was renamed in the file list and it worked; not sent for
renames inside an archive or on a server) and `SceneEnded { ext_id, by_escape }` (the scene of that extension ended: `by_escape` when
the user pressed Esc, otherwise its time ran out, `end_scene` was called or another scene replaced it).

```rust
e.subscribe(|ev, _cx| if let Event::FolderOpened { items, .. } = ev { println!("{items} items") });
```

**Layers**: `overlay(|&mut Window, &mut App| -> AnyElement)` above the whole workspace (below dialogs and menus), `underlay(..)` behind
the panes. They fill the window and are pointer-transparent except for elements of your own that have a mouse handler. Redrawn
whenever leaf redraws, or call `ext::request_frame(cx)`.

**Pointer and keys**: `overlay_regions(|&mut App| -> Vec<Bounds<Pixels>>)` declares the regions (window coordinates, asked on every
draw) of the overlay that take the mouse; `on_pointer(|&PointerEvent, &mut App|)` gets moves and button presses/releases inside
them (`region` is the index) and, while this extension's scene is up, anywhere in the window. `on_key(|&KeyPress, &mut App|)` gets
key presses while the scene is up; they do not reach leaf. Esc is never delivered: it always ends the scene first.

**Animation**: `ext::animate(cx, ms)` keeps every window drawing (a frame after a frame) for `ms` (at most a minute) and delivers
`Event::Frame { dt_ms }`, without the extension running a loop of its own; frames stop by themselves when nothing asked.
`ext::want_frames(ext_id, on, cx)` asks until switched off, for a long effect. With no request no frame is asked for.

**Geometry** (window coordinates, recorded only while an extension is registered, from the last frames): `ext::row_rect(path)`
(a visible file row), `ext::content_rect()`, `ext::pane_rects()` (one or two file panes, left to right), `ext::card_rect(job)` and
`ext::progress_rect(job)` (a Status Center card and its bar, by the job id of the operation events), `ext::pointer(&Window)`.

```rust
e.overlay(|_w, _cx| div().absolute().right(px(8.)).bottom(px(8.)).size(px(6.)).rounded_full().bg(ext::theme().accent).into_any_element());
```

**Slots**:
- `status_item(Side::Left | Side::Right, |&mut App| -> Option<AnyElement>)`: the status bar's two groups.
- `title_item(width, |&mut App| -> Option<AnyElement>)`: the right side of the title bar, before the system buttons; `width` is
  the room it needs (taken from the tabs). Hidden together with leaf's own right-hand controls in a narrow window.
- `sidebar_group(title, |&mut App| -> Vec<SidebarRow>)`: a heading and rows (icon, label, click, right-click menu), above Settings.
- `side_pane(title, width, |&mut Window, &mut App| -> AnyElement)`: a pane docked at the right of the workspace; the command
  "Show or hide <title>" (added in the command search) toggles it. An extension may register several and have them open together.
  `side_pane_left(title, height, ..)` docks a block under the sidebar's own groups instead (not shown in the narrow rail).
- `page(id, title, icon, |&mut Window, &mut App| -> AnyElement)`: a full page with its own tab title and icon, like Settings; the
  command "Open <title>" (added) opens it in a tab, or call `ext::open_page(ext_id, id, cx)`. Saved sessions do not keep it.
- `home_widget(id, title, icon, &[WidgetSize], rows, |&mut App, WidgetSize, width| -> Option<AnyElement>)`: a widget in the Home
  table, with the normal Customize hide / show / move / size and a switch in Settings; off until the user adds it.
- `settings_section(title, icon, |&mut App| -> Vec<AnyElement>)`: its own entry in the list of Settings.
- `about_section(|&mut App| -> AnyElement)`: your own section under your extension's row in Settings > About.

```rust
e.sidebar_group("Mine", |_cx| vec![SidebarRow::new(IconName::Star, "Hello", |_w, _cx| println!("clicked"))]);
```

**Commands**: `command(Command::new(id, title, |&CmdCtx, &mut App| ..).icon(..).keys("ctrl-alt-j"))` appears in the command search,
and with the key when one is given. `menu_entry(MenuTarget::Files | Folders | Background, Command ..)` adds an entry at the end of
that right-click menu; `.when(|&CmdCtx| bool)` decides per selection whether it is shown (the command search lists a command
always, so check `ctx.selection` in the handler). `CmdCtx` holds the folder shown and the selected paths. `MenuTarget::SidebarRow`
(the row's path is `selection[0]`) and `MenuTarget::Tab` (the tab's folder is `dir`) add entries to those menus too.

```rust
e.menu_entry(MenuTarget::Files, Command::new("n", "Count", |ctx, _| println!("{}", ctx.selection.len())).when(|c| !c.selection.is_empty()));
```

**Presentation hooks** (display only):
- `display_name(|&Path| -> Option<String>)`: the text shown instead of a name in the file list, tab titles and address crumbs.
- `row_decoration(|&RowInfo| -> Option<AnyElement>)`: an extra element after a row's name.
- `icon_override(|&Path| -> Option<Arc<RenderImage>>)`: a picture instead of the icon.
- `progress_bar(|&ProgressInfo| -> Option<AnyElement>)`: the element drawn in place of a transfer's progress bar in the Status Center
  (fraction, speed, kind, the room `width` x `ext::PROGRESS_SLOT_H`, clipped); `None` keeps the normal bar. The job never sees it.
- `speed_text(|&ProgressInfo| -> Option<String>)`: the text shown for a transfer's speed on its Status Center card (and strip) instead of
  leaf's own ("46.1 MB/s"); `None` keeps leaf's. Only the words change: the progress, the remaining time and the job are not touched.
  `width` and `height` of the `ProgressInfo` are 0 here.
- `on_rows_leave(|&RowsLeaving, &mut App| -> Option<u64>)`: rows are about to leave the list because they are deleted or moved to
  another folder. You get each row's name, kind and on-screen rectangle (the rectangle is `None` for a row that is not on screen) and
  may play an effect in an overlay. Return `Some(ms)` (at most `ext::KEEP_ROWS_MAX_MS`, 1500) to ask the list to keep those rows
  painted, greyed and not clickable, for that long after the files are gone, and then close the gap. A row whose item was not
  removed (a failed delete) stays an ordinary row. Only the details list keeps rows; the grid and the columns close at once. Kept
  rows are not files: nothing that acts on a file can be done to them. The operation starts at once and never waits.
- `on_folder_open(|&FolderOpening, &mut App|)`: a tab is opening a folder (old place, new place, the content area's rectangle).
- an archive being extracted: subscribe to `ArchiveExtractStarting` and call `ext::start_scene(..)`; the extraction has started and
  runs underneath.
- `ext::start_scene(ext_id, ms, render, cx)`: takes the whole window content for a short scene. Esc always ends it, it ends by
  itself after `ms` (at most 30 s), leaf's own dialogs and menus are drawn above it, and it takes the mouse while it runs.
  `ext::end_scene(cx)` ends it from code. `Event::SceneEnded { ext_id, by_escape }` tells when it is over, whichever way.

```rust
e.display_name(|p| p.extension().is_some_and(|x| x == "log").then(|| "a log".to_string()));
```

**View state and actions**:
- `ext::view_state(&App) -> ViewState { dual_pane, preview_open, hidden_shown, filter: Option<String>, vpn_on, sort: SortSpec,
  view_mode: ViewKind }`: a read-only snapshot of the main window's active tab and pane as the last frame saw them (`filter` is the
  quick filter's text, `None` when empty; `vpn_on` is read at the call; `ViewKind` is `Details`, `Grid` or `Columns`). `SortSpec`,
  `SortKey` and `SortDir` are re-exported from `ext`. Without a registered extension it is the default value.
- `ext::delete_selection(permanent: bool, &mut App)`: starts leaf's normal delete of the active tab's selection, by the same code as the
  Delete key. leaf's own confirmation dialog comes first whenever Settings ask for one (always for a permanent delete unless the user
  turned that off), so the user can still say no; then the Status Center job, `ItemsDeleted` and the `on_rows_leave` hooks follow as for
  any delete. It does nothing without a selection.
- `ext::window_snapshot(&Window, &mut App) -> Option<Frame>` (`Frame { width, height, rgba: Arc<Vec<u8>> }`, opaque straight RGBA): the
  window's client area as it is on screen, for effects that distort what is behind them. The copy is made on a thread of its own and the
  call never waits: it returns the newest finished picture (`None` until the first one is ready, and always on a system that does not
  let a program read the screen) and starts the next capture. Layers of the extension that were up at the time are in the picture.

**Painting**: `ext::FrameSurface::new(Scaling::Linear | Scaling::Nearest(k))`, then `set_frame(width, height, &rgba)` each frame
and `element()` in a layer or slot. It hands the pixels to the toolkit (`paint_image`) with one copy and no encoding. Linear is the
toolkit's smooth sampling; `Nearest(k)` repeats every pixel `k x k` (1 to 8) before upload for hard edges. Alpha is straight RGBA
alpha. Follow `set_frame` with `request_frame(cx)`.

**Sound**: `ext::play_sound()` plays the notification sound of the user's sound scheme (what leaf plays for its own notices); a
scheme without a sound plays nothing. `ext::play_sound_file(cx, path, volume)` plays a sound file (wav, ogg, mp3, flac, ...; up to
30 s) at a volume from 0 to 1 through one audio output opened on first use and kept for the session; sounds overlap (up to 16 at
once). It plays nothing while the switch "Sounds from extensions" in Settings > About is off (it is on by default) or while the
system is muted (the output is the system's shared one). A file that cannot be decoded is logged and skipped. Decoding happens on the
sound thread, so a long file delays the voices already playing for a moment.

**Storage and theme**: `ext::data_dir(id)` is a folder of your own under leaf's config folder (`extensions/<id>`); `ext::theme()`
returns the app's colours (read only): `dark`, `window_bg`, `content_bg`, `card_bg`, `text`, `text_secondary`, `text_tertiary`,
`border`, `accent`, `hover`, `selection`.

## Peer channel

On the link a shared folder (leaf-share) already has with a friend's leaf: `ext::peer::friends(cx)` lists the friends folders are
shared with (`Friend { id, name, online, can_receive }`: `id` is the friend's device id, stable across restarts and the same for every
folder shared with them; `can_receive` says their leaf announced it takes extension messages), `ext::peer::online(cx, id)`,
`ext::peer::send(cx, id, kind: u16, payload: &[u8])` sends one message, `Event::PeerMessage { friend, friend_name, kind, payload }` is
the receive subscription and `Event::FriendLink { friend, friend_name, online }` says a friend's link came up or went down.

- Wire format: one more control-stream notice in the existing length-prefixed JSON frames:
  `{"t":"ext","kind":<u16>,"hex":"<lower-case hex of the payload>"}`. No new stream, no file, no setting.
- Limits, checked by the receiver before anything is acted on: payload at most **64 KiB**; a control frame at most 128 KiB + 256 B
  (checked before the buffer is allocated, a larger frame ends the connection); an odd-length or non-hex text is dropped;
  **5 messages at once, 3 a second** on average per link and direction (the rest are dropped; `send` returns an error).
- Off by default. Messages are passed on only when the **global** switch ("Allow extension messages from friends") and the
  **per-friend** switch ("Allow extension messages") are both on; both are in the Shared folders section (Sync jobs page) and appear only while an
  extension is registered. A message is bytes for the extension; it cannot touch files or settings.
- Compatibility: this leaf skips a notice of a kind it does not know (and one with fields it does not understand) instead of
  dropping the connection, and the hello (the `Join` / `Resume` frame and the `Welcome` reply) carries `"ext":true`, which is how
  `can_receive` is known; an older leaf does not send the field, so it reads as "cannot". This cannot fix leaf builds that are
  already installed: a build from before this change stops reading at an `ext` notice and the link reconnects, so send only to
  friends whose `can_receive` is true, and keep both PCs on a recent version.

## Not provided

- A sound mixer of your own: `play_sound_file` plays files only. Sounds are not cut off by the system's volume mixer beyond the
  system mute.
- The kind of a job that does not say what it is (`OpKind::Other`), and `Rename` (renames are not jobs of the Status Center).
- Keeping rows painted after they leave the grid or the columns view, and rows that leave for a reason other than a delete or a move
  to another folder (a rename, the folder changing underneath).
- Pages of an extension are not restored from a saved session (the tab comes back as Home).
- Receiving extension messages is the receiver's choice (two switches); a sender cannot tell whether they are on.
