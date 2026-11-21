# leaf-mcp: leaf for AI assistants

`leaf-mcp` is a small program that speaks the Model Context Protocol (MCP) over standard input and output. An assistant that
starts it can do two things in the leaf that is running, and nothing else:

| Tool | What it does |
|---|---|
| `show_file(path)` | leaf opens the file's folder with the file selected (the same as `leaf reveal <file>`). |
| `pick_files(prompt)` | leaf asks you to choose files (the open dialog, several files can be chosen, the prompt is its title) and returns the chosen paths; an empty list when you cancel. |

Both are read-only: nothing is changed, moved or deleted, and `pick_files` only returns paths you chose yourself.

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
- `pick_files` waits up to ten minutes for you; `show_file` up to fifteen seconds.
- The instance it talks to is the one named by the `LEAF_INSTANCE` environment variable (unset: your normal leaf).
