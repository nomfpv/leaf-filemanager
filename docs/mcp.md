# leaf-mcp: leaf for AI assistants

`leaf-mcp` is a small program that speaks the Model Context Protocol (MCP) over standard input and output. An assistant that
starts it can work with the leaf that is running, and nothing else:

| Tool | What it does |
|---|---|
| `show_file(path)` | leaf opens the file's folder with the file selected (the same as `leaf reveal <file>`). |
| `pick_files(prompt)` | leaf asks you to choose files (the open dialog, several files can be chosen, the prompt is its title) and returns the chosen paths; an empty list when you cancel. |
| `search(query, contents?, max?)` | Names inside the project folder, from leaf's name index (a bounded walk where the index does not cover the folder). `contents: true` asks Windows Search for names and contents where it indexes the folder (not on Linux). Paths only, at most 200 (default 50). |
| `list_folder(path)` | The entries of a folder inside the project folder: name, kind (folder, file, link), size; at most 1000. |
| `copy(sources, destination)` | Copies into a folder. At most 200 sources. Only with the switch on (below). |
| `move(sources, destination)` | Moves into a folder. At most 200 sources. Only with the switch on. |
| `rename(path, new_name)` | A new name in the same folder. Only with the switch on. |
| `create_folder(path)` | One new folder; its parent must exist. Only with the switch on. |
| `trash(paths)` | To the Recycle Bin / Trash, at most 50 items per call, never the project folder itself. Only with the switch on. |

`show_file`, `pick_files`, `search` and `list_folder` change nothing. There is no permanent delete, no overwrite, no tool that runs a
program and none that reads what is in a file.

## File operations (off by default)

Settings > Developer > "Let agents move and delete files through leaf". Off, the five changing tools are not listed at all, and leaf
refuses them if they are called anyway. On:

- **The project folder.** A session's folder is `--root <folder>` on leaf-mcp's command line, else the `LEAF_MCP_ROOT` environment
  variable, else the folder leaf-mcp was started in (an agent starts it in its project). leaf resolves it once, at the session's
  first request, and keeps it: a later request of the same session with another folder is refused. It may not be a drive root, the
  user folder itself or a folder that holds it, the Windows, Program Files, ProgramData or AppData folders, or leaf's own folders.
  `search` and `list_folder` are kept inside it too.
- **Every path is checked in leaf** (`leaf-mcp` only shapes requests; leaf treats them as untrusted input). A path is absolute or
  relative to the project folder; `..` is refused. Each source and each target is compared on its real path: the folder it is in
  is resolved (links, junctions, `subst` drives followed) and must be inside the project folder; an item that is itself a link is
  refused. Refusals about a place outside never name it.
- **Asked once.** The first change of a session shows a dialog in leaf with the agent's name (as it gives it), the folder and the
  kind of operation: Allow for this session / Allow once / Refuse. Refuse is the default; closing the dialog refuses, and so does
  no answer within 60 seconds. A call that comes while the dialog is open waits for its answer. Nothing is remembered when leaf
  closes. With no leaf window open (only the notification area icon) nothing can be asked, so it is refused.
- **Nothing is overwritten.** A target that exists fails the call and names it; the agent can choose another name. Copy and move
  use leaf's own engine, which creates every file without replacing; a conflict that appears after the check stops the job.
- **Like your own operations.** Each change is a Status Center job titled with the agent's name ("Agent claude-code: Copying..."),
  can be cancelled there, and goes on the undo list (Ctrl+Z, or the card's undo button).
- **Agent rules.** The deny rules of Claude Code's settings (`.claude/settings.json` and `settings.local.json` of the project and of
  its Git root, and the user's `settings.json`) that name files (`Read`, `Edit`, `Write`, `MultiEdit`, `NotebookEdit`) protect
  what they match, as "Protect from agents" writes them: a change to a protected item, into a protected folder, or of a folder
  that holds a protected item is refused, and `search` and `list_folder` leave protected items out. A rule with no pattern
  (`Edit`), a rule leaf cannot understand, or a settings file that cannot be read refuses every change. A folder with more than
  50,000 items inside cannot be checked against the rules and is refused when rules exist.

### Where a check and the act are not one step

The checks are made just before the job starts (and again after the consent dialog, which may have stood for a minute), not under
a lock. What can change in between:

- A folder on the way (the destination, or the folder a source is in) replaced by a link or junction to a place outside after the
  check: the job then follows it. Closing this needs handle-relative operations in the engines; not done.
- A source replaced by a different item (or a link) of the same name after the check: the job acts on what is there.
- A target that appears after the check: copy and move stop at the engine's conflict step, or the engine's no-replace create
  fails for that file; `rename` (a no-replace rename) and `create_folder` (a plain create) fail. Nothing is overwritten in any case.
- A protected item put inside a folder after the folder was checked against the agent rules: it is moved or trashed with it.
- The Recycle Bin: on Windows, an item the bin cannot take (too big, or a drive without a bin) makes Windows ask you whether to
  delete it for good; leaf never says yes for you. On Linux a drive without a Trash fails the call.
- Two calls of one session at once are checked one by one; each job's own no-replace creates are what keeps them apart.

`leaf-mcp` talks to leaf through the same channel a second launch uses to hand a folder to the running leaf. It never starts leaf:
when leaf is not running, the tools answer an error ("leaf is not running"). Starting leaf first is up to you.

On Linux the hand-over is there, `show_file` works, and `pick_files` answers an empty list until leaf has a file dialog there.

## Setting it up

`leaf-mcp.exe` sits next to `leaf.exe`. Tell the assistant to start it.

Claude Code, in `.mcp.json` (or `claude mcp add leaf -- "C:\Program Files\leaf\leaf-mcp.exe"`):

```json
{
  "mcpServers": {
    "leaf": {
      "command": "C:\Program Files\leaf\leaf-mcp.exe",
      "args": []
    }
  }
}
```

Claude Desktop, in `claude_desktop_config.json`, the same block under `mcpServers`. Use the path where leaf is installed.

## Notes

- One JSON message per line, as MCP's stdio transport says. Protocol: `initialize`, `ping`, `tools/list`, `tools/call`.
- `pick_files` waits up to ten minutes for you; `show_file` up to fifteen seconds; `search` and `list_folder` thirty seconds; a
  change up to thirty minutes (the answer comes when its job ends; past that the job may still be running in the Status Center).
- leaf-mcp asks leaf whether the switch is on when the agent lists the tools. Started before leaf (or with the switch turned on
  later), the agent sees the changing tools only after it lists them again (most agents do so when they reconnect).
- The instance it talks to is the one named by the `LEAF_INSTANCE` environment variable (unset: your normal leaf).
