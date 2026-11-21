# leaf browser capture (extension)

Downloads you start in Chrome, Edge, Brave or Firefox go to leaf instead: a job in leaf's Status Center, saved in leaf's download
folder, with the browser's cookies and sign-in for that one download (kept in memory only, never written or logged). The browser's own
copy is paused while leaf answers and cancelled only when leaf says it took the download; otherwise the browser carries on.

How it fits together: the extension (`src/background.js`) sends one message per download to the native host `dev.leaf.nmhost`, which is
`leaf-nmhost(.exe)` next to leaf. The host hands it to the running leaf (or starts leaf) and answers ok or no. Code: `crates/leaf-nmhost`,
`crates/rf-app/src/capture.rs`.

## 1. Connect the browsers (once)

In leaf: Settings > Advanced > Downloads > **Connect browsers**, or on the command line:

    leaf nmhost --register

For this user only, no admin. Windows: keys under `HKCU\Software\...\NativeMessagingHosts\dev.leaf.nmhost` for Chrome, Edge, Brave and
Firefox, pointing at manifests in `%APPDATA%\leaf\native_messaging\`. Linux: `dev.leaf.nmhost.json` in each installed browser's
`NativeMessagingHosts` (Firefox: `~/.mozilla/native-messaging-hosts`). `leaf nmhost --unregister` takes it back.

Snap and Flatpak browsers on Linux cannot start a program outside their sandbox; leaf lists them as "sandboxed" and they will not work.

## 2. Build the loadable folders

No tools needed (plain JavaScript):

    powershell -ExecutionPolicy Bypass -File extension\build.ps1     (Windows)
    sh extension/build.sh                                             (Linux)

This writes `extension/dist/chromium` and `extension/dist/firefox`.

## 3. Load it unpacked

- **Chrome**: `chrome://extensions`, turn on Developer mode, Load unpacked, choose `extension/dist/chromium`.
- **Edge**: `edge://extensions`, turn on Developer mode, Load unpacked, choose `extension/dist/chromium`.
- **Brave**: `brave://extensions`, turn on Developer mode, Load unpacked, choose `extension/dist/chromium`.
- **Firefox**: `about:debugging#/runtime/this-firefox`, Load Temporary Add-on, choose `extension/dist/firefox/manifest.json`. A temporary
  add-on is gone after Firefox restarts; a lasting install needs the add-on signed by addons.mozilla.org.

The Chromium build pins its id with the `key` in `manifest.chromium.json` (`opbdlmjnfgldjmlacejhcflbdpimolpi`); the Firefox id is
`browser-capture@leaf-filemanager`. The host manifests leaf writes allow only these ids (`CHROMIUM_IDS` and `FIREFOX_ID` in
`crates/rf-app/src/capture.rs`). A store listing gets its own Chromium id: add it there and connect the browsers again.

Open the extension's options to check that leaf can be reached, to turn the capture off, to keep cookies to the browser, or to list
sites whose downloads the browser keeps.

## What is captured

Only new http/https GET downloads the page requested just now. Left to the browser: blob:, data:, file: downloads; POST downloads;
downloads started by an extension; the browser's own retries; excluded sites; files smaller than leaf's "Smallest download to capture"
(Settings > Advanced > Downloads, 1 MB by default, when the size is known); everything while "Capture browser downloads" is off in leaf.

A download that needed the browser's sign-in is not resumed by leaf after leaf restarts (the sign-in is not kept): it fails with a
message, and starting it again from the browser carries on from the part already downloaded.

## Browse through Mullvad

The toolbar button's popup has **Browse through Mullvad**: the browser's proxy is set to leaf's SOCKS5 proxy on 127.0.0.1 (in leaf:
Settings > VPN > Let other programs use the VPN), so pages go through leaf's Mullvad VPN and names are looked up through it (Chromium:
`chrome.proxy` fixed_servers socks5; Firefox: `proxy.settings` with `proxyDNS: true`, or per request through `proxy.onRequest` when
leaf's proxy asks for a password). Addresses of this PC and the local network stay direct.

Firefox: the switch turns on only when the add-on is allowed in private windows (about:addons > leaf browser capture > Run in Private
Windows > Allow), so private windows go through Mullvad too. With leaf's proxy password the proxy can only be given per request, and
then Firefox's own sites (addons.mozilla.org, accounts.firefox.com, ...) and its background requests do not go through Mullvad; the
popup says so. After a Firefox restart the per-request proxy is set up from storage before the first request, without the password
until leaf answers (leaf refuses meanwhile: pages fail, never direct).
While it is on, WebRTC is kept to the proxy (`webRTCIPHandlingPolicy` = `disable_non_proxied_udp`) so it does not show the real
address; turning it off puts the browser's own settings back.

The extension asks leaf (through the native host, message `{"v":1,"type":"vpn"}`, leaf is not started for it) for the VPN's state and
the proxy's address every half minute. The switch never turns itself off: while leaf's VPN is off, connecting or failed, while leaf is
not running or does not answer, and at the browser's start before leaf runs, the proxy stays pointed at leaf's (which refuses, or is
closed), so pages fail rather than load directly; the badge turns red and one notice says so. Only the popup's "Turn off (browse
without VPN)" puts the browser's own settings back. A new port or password in leaf is taken over.
Chromium browsers cannot give a SOCKS proxy a password: with leaf's "Ask for a password" on, the switch works in Firefox only.

New permissions for it:

| Permission | Why |
|---|---|
| `proxy` | set the browser's proxy to leaf's (and back) |
| `privacy` | keep WebRTC to the proxy while it is on |
| `alarms` | ask leaf for the VPN's state every half minute |
| `notifications` | the notice when the switch turned itself off |
| `ws://*/*`, `wss://*/*` (Firefox) | WebSocket connections get the per-request proxy too |

## Licence

`src/background.js` adapts code from AB Download Manager browser integration (Apache-2.0); see `NOTICE` and `LICENSE-APACHE-2.0`.
The rest is leaf's (MIT).
