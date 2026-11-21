# leaf on the command line

    leaf [--new-tab] <folder> [<folder> ...]
    leaf [--new-tab] reveal <file>
    leaf --new-tab

- `leaf <folder>` opens the folder. When leaf is already running, the running window takes it over as a new tab (or as a new
  window when "open folders from other programs in a new window" is on). A relative path is read from the folder the command
  was typed in. Several folders become several tabs; one that is gone is passed over.
- `leaf reveal <file>` opens the file's folder with the file selected (the same as Explorer's `/select,<file>`, which also works).
- `--new-tab` always opens the folder as a tab of the running window, whatever that setting says. With no window running, the
  folder opens in a new one.
- Also understood: `sftp://`, `ftp://` addresses, `shell:` names and `::{...}` namespaces, `--settings=<page>`.
