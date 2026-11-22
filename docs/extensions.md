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

A complete small one is `crates/rf-app/examples/hello_ext.rs` (a status-bar item and an overlay).
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
`OperationStarted/Progress/Finished` (every Status Center job: copy, move, delete, extract, compress, download, upload, sync; with
leaf's own title, items, bytes, speed, current name, result), `ItemsDeleted { names, sizes, permanent, total }`,
`WindowFocus { focused }`, `PeerMessage { .. }`.

```rust
e.subscribe(|ev, _cx| if let Event::FolderOpened { items, .. } = ev { println!("{items} items") });
```

**Layers**: `overlay(|&mut Window, &mut App| -> AnyElement)` above the whole workspace (below dialogs and menus), `underlay(..)` behind
the panes. They fill the window and are pointer-transparent except for elements of your own that have a mouse handler. Redrawn
whenever leaf redraws, or call `ext::request_frame(cx)`.

```rust
e.overlay(|_w, _cx| div().absolute().right(px(8.)).bottom(px(8.)).size(px(6.)).rounded_full().bg(ext::theme().accent).into_any_element());
```

**Slots**:
- `status_item(Side::Left | Side::Right, |&mut App| -> Option<AnyElement>)`: the status bar's two groups.
- `title_item(width, |&mut App| -> Option<AnyElement>)`: the right side of the title bar, before the system buttons; `width` is
  the room it needs (taken from the tabs). Hidden together with leaf's own right-hand controls in a narrow window.
- `sidebar_group(title, |&mut App| -> Vec<SidebarRow>)`: a heading and rows (icon, label, click, right-click menu), above Settings.
- `side_pane(title, width, |&mut Window, &mut App| -> AnyElement)`: a pane docked at the right of the workspace; the command
  "Show or hide <title>" (added in the command search) toggles it.
- `about_section(|&mut App| -> AnyElement)`: your own section under your extension's row in Settings > About.

```rust
e.sidebar_group("Mine", |_cx| vec![SidebarRow::new(IconName::Star, "Hello", |_w, _cx| println!("clicked"))]);
```

**Commands**: `command(Command::new(id, title, |&CmdCtx, &mut App| ..).icon(..).keys("ctrl-alt-j"))` appears in the command search,
and with the key when one is given. `menu_entry(MenuTarget::Files | Folders | Background, Command ..)` adds an entry at the end of
that right-click menu; `.when(|&CmdCtx| bool)` decides per selection whether it is shown (the command search lists a command
always, so check `ctx.selection` in the handler). `CmdCtx` holds the folder shown and the selected paths.

```rust
e.menu_entry(MenuTarget::Files, Command::new("n", "Count", |ctx, _| println!("{}", ctx.selection.len())).when(|c| !c.selection.is_empty()));
```

**Presentation hooks** (display only):
- `display_name(|&Path| -> Option<String>)`: the text shown instead of a name in the file list, tab titles and address crumbs.
- `row_decoration(|&RowInfo| -> Option<AnyElement>)`: an extra element after a row's name.
- `icon_override(|&Path| -> Option<Arc<RenderImage>>)`: a picture instead of the icon.
- `ext::start_scene(ext_id, ms, render, cx)`: takes the whole window content for a short scene. Esc always ends it, it ends by
  itself after `ms` (at most 30 s), leaf's own dialogs and menus are drawn above it, and it takes the mouse while it runs.
  `ext::end_scene(cx)` ends it from code.

```rust
e.display_name(|p| p.extension().is_some_and(|x| x == "log").then(|| "a log".to_string()));
```

**Painting**: `ext::FrameSurface::new(Scaling::Linear | Scaling::Nearest(k))`, then `set_frame(width, height, &rgba)` each frame
and `element()` in a layer or slot. It hands the pixels to the toolkit (`paint_image`) with one copy and no encoding. Linear is the
toolkit's smooth sampling; `Nearest(k)` repeats every pixel `k x k` (1 to 8) before upload for hard edges. Alpha is straight RGBA
alpha. Follow `set_frame` with `request_frame(cx)`.

**Sound**: `ext::play_sound()` plays the notification sound of the user's sound scheme (what leaf plays for its own notices). leaf
has no sound setting of its own, so the scheme is what decides; a scheme without a sound plays nothing.

**Storage and theme**: `ext::data_dir(id)` is a folder of your own under leaf's config folder (`extensions/<id>`); `ext::theme()`
returns the app's colours (read only): `dark`, `window_bg`, `content_bg`, `card_bg`, `text`, `text_secondary`, `text_tertiary`,
`border`, `accent`, `hover`, `selection`.

## Peer channel

On the link a shared folder (leaf-share) already has with a friend's leaf: `ext::peer::friends(cx)` lists the linked friends,
`ext::peer::send(cx, friend_id, kind: u16, payload: &[u8])` sends one message, and `Event::PeerMessage { friend, friend_name, kind,
payload }` is the receive subscription.

- Wire format: one more control-stream notice in the existing length-prefixed JSON frames:
  `{"t":"ext","kind":<u16>,"hex":"<lower-case hex of the payload>"}`. No new stream, no file, no setting.
- Limits, checked by the receiver before anything is acted on: payload at most **64 KiB**; a control frame at most 128 KiB + 256 B
  (checked before the buffer is allocated, a larger frame ends the connection); an odd-length or non-hex text is dropped;
  **5 messages at once, 3 a second** on average per link and direction (the rest are dropped; `send` returns an error).
- Off by default. Messages are passed on only when the **global** switch ("Allow extension messages from friends") and the
  **per-friend** switch ("Allow extension messages") are both on; both are in the Shared folders section (Sync jobs page) and appear only while an
  extension is registered. A message is bytes for the extension; it cannot touch files or settings.
- Compatibility: a leaf older than this seam does not know the notice; its control reader stops at it and the link reconnects. Both
  PCs should run the same version before an extension sends.

## Not provided

Home dashboard widgets, a full page with its own tab, a Settings section (use `about_section`), menu entries for sidebar rows and
tabs, drive-space, transfer-speed-tick and archive-extraction events (operation progress carries the speed), the transition hook
around row removal and folder opening, and replacing a progress bar's visual. Each needs a change to a part of leaf that is not yet
shaped for it; they can be added to this seam one at a time.
