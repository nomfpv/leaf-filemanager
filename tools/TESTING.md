# Testing leaf without disturbing the owner

Start the app for a check with three environment variables, so it uses a throwaway config,
runs next to the owner's own copy and never shows up on his screen:

```powershell
$cfg = Join-Path $env:TEMP "leaf-check-cfg"
New-Item -ItemType Directory -Force $cfg | Out-Null
$env:LEAF_CONFIG_DIR = $cfg      # throwaway settings, never the real ones
$env:LEAF_MULTI_INSTANCE = "1"   # own instance, no hand-over to a running copy
$env:LEAF_OFFSCREEN = "1"        # window parked right of every monitor, no focus, no taskbar button
$p = Start-Process target\release\leaf.exe -ArgumentList "C:\some\folder" -PassThru
```

For a capture with a selection, `$env:LEAF_CHECK_SELECT = "a.txt|b.txt|c"` selects those names in the first
folder the window lists (no click needed).

In this mode the window is still shown and rendered (not minimised, not hidden), it is just
outside the virtual screen. Saved window placement is neither read nor written.

Capture it (works off-screen; `CopyFromScreen` does not):

```powershell
powershell -ExecutionPolicy Bypass -File tools\capture-window.ps1 -ProcessId $p.Id -Out $env:TEMP\check.png
```

Exit 0 = PNG written, 1 = no window, 2 = blank picture. Read the PNG to check the result.

Close only what you started, with the window's own close, never by process name:

```powershell
[void]$p.CloseMainWindow(); if (-not $p.WaitForExit(5000)) { Stop-Process -Id $p.Id }
```

`openbench` (`crates/rf-win/examples/openbench.rs`) already runs the app this way; set
`OPENBENCH_ON_SCREEN=1` for an on-screen run.

## Scripted drive (`LEAF_CHECK_SCRIPT`)

`LEAF_CHECK_SCRIPT=<file>` runs the file's lines in order on the UI thread once the first folder is listed (off and free
when unset; code in `crates/rf-app/src/check_script.rs`). One line per step, `#` starts a comment:

- `select a|b|c`: select these names in the focused pane's folder
- `run <CommandId>`: any id from `registry.rs` (`NewFolder`, `RenameItem`, ...), dispatched like the key press would be
- `type <text>`, `key enter|escape|tab|...`: keystrokes into whatever has the focus (a text field, a dialog)
- `wait <ms>`, `wait-idle` (no file operation running, no folder loading)
- `goto <path|home>`, `usage <path>` (the storage usage view), `pane left|right` (dual pane), `bin restore` (the Recycle Bin
  page: restores the selected rows; select them with `run SelectAll`), `bulk apply` (the open bulk rename dialog: its Rename button),
  `settings <general|appearance|layout|files|filetypes|actions|tags|preview|search|nearby|advanced|speed|organizer|features|about>`
- `changes <path>` (the change feed page of the git repository the folder is in), `changes-pick <part of a path>` (selects that row; its diff shows in the preview pane)
- `dups newest` (the duplicate finder page: tick all but the newest copy of every group), `dups delete` (its Delete ticked button, opens the question; `key enter` answers it)
- `compare-contents [all]` (folder compare on: the Compare contents chip; `all` also checks same-size, same-time pairs)
- `tag <name>` (toggles that tag on the selection), `gototag <name>` (the tag's page)
- `shot <name>`: prints `LEAF_SHOT <name>` on stdout; if `LEAF_CHECK_SHOTDIR` is set it waits (15 s at most) for
  `<dir>/<name>.png` to appear, else 1.5 s. The wrapper takes the picture.
- `quit`: leaves (after running operations)

An unknown line is logged (`check script: unknown line`) and skipped. On Linux, `tools/linux-headless-run.sh` takes
`SCRIPT=<file>` and `SHOTS=<dir>` and does the picture part (see its header); `$FOLDER`, `$OTHER`, `$DATA` and `$BASE` in the
script are replaced by the temp folders it made.

## Still checked on screen (with the owner's agreement)

- Glass / Mica backdrop look (an off-screen window is not composed like a visible one).
- Drag and drop.
- Focus and keyboard behaviour (what takes the focus, tab order, shortcuts, the tray).
- The official end-of-phase benchmark (`OPENBENCH_ON_SCREEN=1`, or the Speed test in Settings).
