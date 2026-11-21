# leaf on the command line

    leaf [--new-tab] <folder> [<folder> ...]
    leaf [--new-tab] reveal <file>
    leaf --new-tab
    leaf download <address> [<folder>]

- `leaf <folder>` opens the folder. When leaf is already running, the running window takes it over as a new tab (or as a new
  window when "open folders from other programs in a new window" is on). A relative path is read from the folder the command
  was typed in. Several folders become several tabs; one that is gone is passed over.
- `leaf reveal <file>` opens the file's folder with the file selected (the same as Explorer's `/select,<file>`, which also works).
- `--new-tab` always opens the folder as a tab of the running window, whatever that setting says. With no window running, the
  folder opens in a new one.
- `leaf download <address> [<folder>]` downloads an http or https address into the folder (Settings > Advanced > Downloads names the default
  folder when none is given). It is a job in the Status Center like a copy: several connections to the file when the server allows byte
  ranges, pause and cancel there, and the same address into the same folder carries on from the partial file (`name.leaf-part`) after a
  cut line or a restart. The window shows the folder and selects the finished file. When leaf is already running, that window takes it.
- Also understood: `rclone://remote/path` (needs rclone installed; WebDAV and other storage come through an rclone remote), `docker://local/` (containers and their mounts), `sftp://`, `ftp://` addresses, `shell:` names and `::{...}` namespaces, `--settings=<page>`.
