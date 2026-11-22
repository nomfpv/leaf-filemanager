# Packaging leaf for Windows

## Build and package
```powershell
tools\package.ps1                 # release build, stages dist\stage, packs dist\release when vpk is installed
tools\package.ps1 -NoBuild        # stage and pack what target\release already holds
tools\package.ps1 -Version 0.9.5  # another version than the workspace's (Cargo.toml)
tools\package.ps1 -Sign <thumbprint>   # sign with that certificate from the Windows store
```
The script builds `cargo build --release -p rf-app -p leaf-mcp -p leaf-nmhost`, copies the files below into `dist\stage\`, and when
`vpk` (the Velopack tool: `dotnet tool install -g vpk`, needs the .NET SDK) is on PATH runs `vpk pack` with pack id `leaf`,
the main exe `leaf.exe` and the leaf icon into `dist\release\`. It never uploads anything.

Pack id `leaf` makes the installed shortcuts carry the AppUserModelID `velopack.leaf`, which is `installer::APP_ID`.

## Install without Velopack (a stable local copy)
```powershell
tools\package.ps1                 # once, or -NoBuild when target\release is current
tools\install-local.ps1           # copies dist\stage to %LOCALAPPDATA%\Programs\leaf, Start menu shortcut "leaf"
tools\install-local.ps1 -Uninstall
```
Install stops only a leaf that runs from that folder, never the copy in `target\`. Run it again after a new `package.ps1` to
replace the files. Uninstall removes the folder and the shortcut; it refuses while the default-file-manager switch still points
at that folder (turn it off in Settings first, or pass `-Force`). Settings in `%APPDATA%` stay. This kind of install is not a
Velopack install and does not update itself.

## Install with the Velopack installer
`dist\release\leaf-win-Setup.exe` installs to `%LOCALAPPDATA%\leaf` (program in `current\`, `Update.exe` beside it), with
shortcuts. Uninstalling from Windows' Apps list runs leaf's hook, which turns off the default file manager, the start-with-Windows
entries and the browser hand-over registration. The installer is **not signed** unless `-Sign` was given, so Windows SmartScreen
warns the first time ("unknown publisher"; More info, Run anyway).

## What a release contains
| File | Why |
|---|---|
| `leaf.exe` | the file manager |
| `leaf-look.exe` | the Space viewer, started beside leaf.exe |
| `leaf-open.exe` | the folder-handler launcher: the default-file-manager switch registers it (copied to `%LOCALAPPDATA%\leaf-default-fm`) so Win+E and folder links still work if leaf is gone; without it beside `leaf.exe` an installed leaf could only register `leaf.exe` itself |
| `leaf-nmhost.exe` | the browser hand-over host; "Connect browsers" registers the one beside leaf.exe |
| `leaf-mcp.exe` | the MCP server for assistants (docs\mcp.md); started by the assistant, so it only has to exist |
| `LICENSE`, `THIRD-PARTY.md` | licences |

The browser extension (`extension\`) is built separately and is not part of the package.

## Updates
Off by default, and leaf contains no feed address. Settings, About: "Check for leaf updates" (off) and "Release feed" (empty).
Only with the switch on **and** an address written (https, or a folder on this PC) does the button "Check for updates" do
anything; otherwise no code that could contact a server runs. leaf checks only when that button is pressed, never on its own.
A found update is downloaded and checked by Velopack against the SHA1 / SHA256 in the feed, then "Restart and update" applies it.
Only a Velopack install updates; `install-local.ps1` copies and a build in `target\` report "not installed with the setup".

To publish a feed: put the contents of `dist\release\` on any static https host (or a folder) and write its address into
the setting. Uploading is up to you; the scripts never do it.
