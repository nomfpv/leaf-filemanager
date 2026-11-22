//! Saved hosts in the app (W1b): the list from `hosts.toml` in the config folder, shared by the Settings page "Hosts", the
//! sidebar section and the Home widget (both only while there are hosts). Opening a host goes to its address as a remote place
//! (SFTP, FTP and FTPS through `leaf-remote`); a scheme with no backend yet says that connecting is not built yet.
//!
//! The file never holds a password or another secret (see `rf_core::hosts`).

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::Location;
use rf_core::hosts::{FileAccess, Host, HostKind, Hosts};
use std::rc::Rc;
use rf_core::vpath::VPath;

use crate::settings_view::{button, card, card_body_with, section, shell};
use crate::t;
use crate::theme::Palette;

struct Store(Hosts);
impl Global for Store {}

/// The saved hosts (read from the config folder at the first use).
pub fn all(cx: &mut App) -> Vec<Host> {
    if !cx.has_global::<Store>() {
        let hosts = crate::settings::app_dir().map(|d| Hosts::load(&d)).unwrap_or_default();
        cx.set_global(Store(hosts));
    }
    let mut hosts = cx.global::<Store>().0.hosts.clone();
    // the remotes rclone knows (when it is installed): listed beside the saved hosts, never written to hosts.toml
    for r in leaf_remote::rclone::remotes_cached() {
        if !hosts.iter().any(|h| h.scheme == "rclone" && h.host == r) {
            let mut h = Host::new(&format!("rclone: {r}"), &r);
            h.scheme = "rclone".into();
            hosts.push(h);
        }
    }
    hosts
}

fn change(cx: &mut App, f: impl FnOnce(&mut Hosts)) {
    all(cx);
    let store = cx.global_mut::<Store>();
    f(&mut store.0);
    let hosts = store.0.clone();
    if let Some(dir) = crate::settings::app_dir() {
        // (a few hundred bytes; written on a worker all the same)
        std::thread::spawn(move || {
            if let Err(e) = hosts.save(&dir) {
                log::warn!("hosts: could not save {}: {e}", dir.join(rf_core::hosts::FILE_NAME).display());
            }
        });
    }
    cx.refresh_windows();
}

/// Where opening the host goes: its start folder as a remote place; an SFTP or FTP host without one opens in the folder the
/// server logs the user into (`/~`).
pub fn location(h: &Host) -> Option<Location> {
    // an SMB host on Windows: `\\host\share` (or `\\host`, its list of shares) through the system's own SMB
    if cfg!(windows)
        && h.scheme.eq_ignore_ascii_case("smb")
        && let Some(unc) = leaf_remote::smb::unc_of(&h.address())
    {
        leaf_remote::smb::mark_opened_here(&unc);
        return Some(Location::Path(unc));
    }
    let v = VPath::parse(&h.address()).ok()?;
    let login_folder = ["sftp", "ftp", "ftps"].iter().any(|s| h.scheme.eq_ignore_ascii_case(s));
    let v = if h.start_path.is_none() && login_folder { v.with_path("/~") } else { v };
    Some(Location::Remote(v))
}

/// A bookmark of the host as a place.
pub fn bookmark_location(h: &Host, path: &str) -> Option<Location> {
    let v = VPath::parse(&h.address()).ok()?;
    // (an SMB host's bookmark on Windows is a folder of the share, `\\host\share\dir`)
    Some(crate::remote::place_of(v.with_path(path)))
}

/// "Bookmark this folder" in a folder of a Windows share (`\\host\share\dir`): the bookmark of its saved `smb://` host. `None`: no
/// saved host is this server (or the path is no share folder).
pub fn toggle_unc_bookmark(p: &std::path::Path, cx: &mut App) -> Option<bool> {
    let rest = p.to_str()?.strip_prefix(r"\\")?;
    let (host, share_path) = rest.split_once('\\')?;
    if host.is_empty() || host.starts_with(['?', '.']) || share_path.trim_matches('\\').is_empty() {
        return None;
    }
    let path = format!("/{}", share_path.trim_matches('\\').replace('\\', "/"));
    all(cx);
    let mut done = None;
    change(cx, |hs| done = hs.toggle_smb_bookmark(host, &path));
    done
}

/// "Bookmark this folder": adds the remote folder to its saved host, or removes the bookmark. Returns what was done (`None`: no
/// saved host is this server).
pub fn toggle_bookmark(v: &VPath, cx: &mut App) -> Option<bool> {
    all(cx);
    let mut done = None;
    change(cx, |hs| done = hs.toggle_bookmark(v));
    done
}

/// The second line of a host's row: `user@host:port` and its start folder.
pub fn subtitle(h: &Host) -> String {
    let mut s = h.address();
    if let Some(j) = &h.jump {
        s.push_str(&format!("  ({} {j})", t!("via")));
    }
    if h.vpn {
        s.push_str(&format!("  ·  {}", t!("through VPN")));
    }
    s
}

const EXAMPLE: &str = "name = sftp://user@example.com:22/folder key=~/.ssh/id_ed25519 jump=bastion";
const FTP_EXAMPLE: &str = "nas = ftps://user@nas.local/share";
/// (a WebDAV server: `davs://` is https, `dav://` is not encrypted)
const DAV_EXAMPLE: &str = "cloud = davs://user@example.com/remote.php/dav/files/user";
/// (a Windows share: `smb://server` lists its shares; the password is asked when the share wants one)
const SMB_EXAMPLE: &str = "files = smb://nas.local/photos";

/// The add / edit dialog: one line (`Host::to_line`). `old`: the name of the host being edited.
fn edit_dialog(old: Option<Host>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let initial = old.as_ref().map(Host::to_line).unwrap_or_default();
    let old_kind = old.as_ref().map(|h| h.kind);
    let old_access = old.as_ref().map(|h| h.access).unwrap_or_default();
    let vpn = Rc::new(std::cell::Cell::new(old.as_ref().is_some_and(|h| h.vpn)));
    let vpn_on = vpn.clone();
    let old_name = old.map(|h| h.name);
    let title = if old_name.is_some() { t!("Edit host") } else { t!("Add a host") };
    let message = format!(
        "{}\n{EXAMPLE}\n{FTP_EXAMPLE}\n{DAV_EXAMPLE}\n{SMB_EXAMPLE}\n{}\n{}",
        t!("One line: a name, =, the address, and if needed the key file and a jump host."),
        t!("Passwords are never saved in this list."),
        t!("ftp:// and dav:// are not encrypted (the password crosses the network as it is); use ftps:// and davs:// where the server has it.")
    );
    // the kind of machine: "Guess" (from the name and port when it is saved), else one of the kinds; an edited host starts on its own
    let selected = Rc::new(std::cell::Cell::new(old_kind.map_or(0, |k| 1 + HostKind::ALL.iter().position(|x| *x == k).unwrap_or(0))));
    let kind_choice = crate::prompt::Choice {
        label: t!("Kind").into(),
        options: std::iter::once(SharedString::from(t!("Guess"))).chain(HostKind::ALL.iter().map(|k| SharedString::from(crate::i18n::td(k.label())))).collect(),
        selected: selected.clone(),
    };
    // "File access" of an SFTP host (H1): SFTP with shell commands when the server has none, or always one of the two
    let access = Rc::new(std::cell::Cell::new(FileAccess::ALL.iter().position(|a| *a == old_access).unwrap_or(0)));
    let access_choice = crate::prompt::Choice {
        label: t!("File access").into(),
        options: FileAccess::ALL.iter().map(|a| SharedString::from(crate::i18n::td(a.label()))).collect(),
        selected: access.clone(),
    };
    // "Through VPN": its connections go through leaf's VPN (Settings > VPN); never for an address on the local network
    let vpn_check = crate::prompt::Check {
        label: t!("Through VPN").into(),
        note: t!("SFTP, FTP and FTPS hosts; Settings > VPN").into(),
        on: vpn,
        enabled: true,
    };
    crate::prompt::show_with_checks(
        title,
        Some(message),
        Some((initial, t!("name = sftp://user@host/folder").to_string())),
        vec![kind_choice, access_choice],
        vec![vpn_check],
        vec![
            Button::new(t!("Save"), Kind::Primary, move |text, _, cx| {
                let mut h = Host::from_line(text.as_deref().unwrap_or_default()).map_err(|e| crate::i18n::td(&e))?;
                h.vpn = vpn_on.get();
                h.access = FileAccess::ALL.get(access.get()).copied().unwrap_or_default();
                if h.access != FileAccess::Auto && !h.scheme.eq_ignore_ascii_case("sftp") {
                    return Err(t!("File access applies to SFTP hosts only. Set it to Automatic for this host.").to_string());
                }
                // (only these connections are made by leaf itself and can go through its VPN; a docker, smb, nfs or rclone host
                // would go direct with the tick on)
                if h.vpn && !matches!(h.scheme.to_ascii_lowercase().as_str(), "sftp" | "ftp" | "ftps") {
                    return Err(t!("Through VPN works for SFTP, FTP and FTPS hosts only. Untick it for this host.").to_string());
                }
                h.kind = match selected.get() {
                    0 => HostKind::guess(&h),
                    i => HostKind::ALL.get(i - 1).copied().unwrap_or_default(),
                };
                let taken = all(cx).iter().any(|o| o.name.eq_ignore_ascii_case(&h.name) && old_name.as_deref().is_none_or(|n| !n.eq_ignore_ascii_case(&o.name)));
                if taken {
                    return Err(t!("Another host has this name").to_string());
                }
                let old_name = old_name.clone();
                change(cx, move |hs| match old_name.and_then(|n| hs.hosts.iter().position(|o| o.name == n)) {
                    Some(i) => {
                        let mut h = h;
                        h.bookmarks = std::mem::take(&mut hs.hosts[i].bookmarks); // (the line has no bookmarks)
                        hs.hosts[i] = h;
                    }
                    None => hs.hosts.push(h),
                });
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

/// "Import from OpenSSH config": the hosts of `~/.ssh/config` that are not in the list yet; says what was added and left out.
fn import(window: &mut Window, cx: &mut App) {
    let Some(path) = rf_core::hosts::ssh_config_path() else { return };
    // (with the files its Include lines name)
    let r = match rf_core::hosts::parse_ssh_config_file(&path) {
        Ok(r) => r,
        Err(e) => {
            crate::prompt::notice(t!("Import from OpenSSH config"), format!("{}: {e}", path.display()), window, cx);
            return;
        }
    };
    let mut added = 0;
    change(cx, |hs| added = hs.merge(r.hosts));
    let mut msg = format!("{} {added}", t!("Hosts added:"));
    if !r.skipped.is_empty() {
        msg.push_str(&format!("\n\n{}\n{}", t!("Left out:"), r.skipped.join("\n")));
    }
    crate::prompt::notice(t!("Import from OpenSSH config"), msg, window, cx);
}

/// Settings > Hosts.
pub fn page(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let hosts = all(cx);
    let mut v = vec![
        card(p, L::Plus, t!("Add a host"), Some(t!("A server you reach over SFTP, FTPS, FTP or WebDAV: its address, user, port, start folder, key file and jump host. No passwords are kept here.")), button("hosts-add", p, t!("Add"), |w, cx| edit_dialog(None, w, cx))),
        card(
            p,
            L::Download,
            t!("Import from OpenSSH config"),
            Some(t!("Adds the hosts of your ~/.ssh/config and the files its Include lines name (Host, HostName, User, Port, IdentityFile, ProxyJump). Wildcard hosts and Match blocks are left out.")),
            button("hosts-import", p, t!("Import"), import),
        ),
        section(p, t!("Saved hosts")),
    ];
    if hosts.is_empty() {
        v.push(div().text_sm().text_color(p.text_secondary).child(t!("No hosts saved yet.")).into_any_element());
    }
    for (i, h) in hosts.into_iter().enumerate() {
        let (edit, remove) = (h.clone(), h.name.clone());
        let controls = h_flex()
            .gap_2()
            .child(button(("host-edit", i), p, t!("Edit"), move |w, cx| edit_dialog(Some(edit.clone()), w, cx)))
            .child(button(("host-remove", i), p, t!("Remove"), move |_, cx| {
                let name = remove.clone();
                change(cx, move |hs| hs.hosts.retain(|o| o.name != name));
            }))
            // B7: deletes into leaf's trash on the server, or permanent (per host)
            .when(matches!(h.scheme.as_str(), "sftp" | "ftp" | "ftps" | "dav" | "davs"), |d| {
                let name = h.name.clone();
                let label = if h.no_trash { t!("Server trash: off") } else { t!("Server trash: on") };
                d.child(button(("host-trash", i), p, label, move |_, cx| {
                    let name = name.clone();
                    change(cx, move |hs| {
                        if let Some(o) = hs.hosts.iter_mut().find(|o| o.name == name) {
                            o.no_trash = !o.no_trash;
                        }
                    });
                }))
            })
            // an SMB host: ends the system's session with the server (Windows) or lets go of the mount (Linux), never forced
            .when(h.scheme.eq_ignore_ascii_case("smb") || h.scheme.eq_ignore_ascii_case("nfs"), |d| {
                let address = h.address();
                d.child(button(("host-disconnect", i), p, t!("Disconnect"), move |_, cx| {
                    if let Ok(v) = VPath::parse(&address) {
                        crate::remote::disconnect(&v, cx);
                    }
                }))
            })
            .into_any_element();
        let mut desc = subtitle(&h);
        match h.access {
            FileAccess::Auto => {}
            FileAccess::Sftp => desc.push_str(&format!("  ·  {}", t!("File access: SFTP"))),
            FileAccess::Shell => desc.push_str(&format!("  ·  {}", t!("File access: Shell"))),
        }
        if let Some(k) = &h.key_file {
            desc.push_str(&format!("  ·  {} {k}", t!("key")));
        }
        let icon = device_icon(h.kind, 20., p);
        v.push(shell(p).child(card_body_with(p, icon, h.name.clone().into(), Some(desc.into()), Some(controls))).into_any_element());
    }
    v.extend(saved_section(p, cx));
    v.push(
        v_flex()
            .pt_3()
            .child(div().text_xs().text_color(p.text_tertiary).child(t!(
                "SFTP servers are checked against ~/.ssh/known_hosts: a new one is asked about, one whose key changed is refused. Logging in tries your SSH agent, the key file, then asks for a password; passphrases and passwords stay in memory until leaf closes, or in the system's keyring when you tick Remember. An FTPS certificate this PC does not trust is asked about; one you trust is kept by its fingerprint. Plain FTP is not encrypted."
            )))
            .into_any_element(),
    );
    v
}

// ------------------------------------------------------------------------------------------------ saved passwords (keyring)

/// What the keyring holds for leaf (ids only, never a secret), read on a worker; `None` until the first read is back.
static SAVED: std::sync::Mutex<Option<(u64, Result<Vec<leaf_remote::SecretId>, String>)>> = std::sync::Mutex::new(None);
static READING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The list as last read, while nothing was saved or deleted since.
fn saved_now() -> Option<Result<Vec<leaf_remote::SecretId>, String>> {
    let g = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().filter(|(at, _)| *at == leaf_remote::secrets::changes()).map(|(_, r)| r.clone())
}

/// Reads the list again from the keyring (off the UI thread), then redraws.
fn reload_saved(cx: &mut App) {
    if READING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let work = cx.background_executor().spawn(async { (leaf_remote::secrets::changes(), leaf_remote::saved_secrets()) });
    cx.spawn(async move |cx| {
        let r = work.await;
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        READING.store(false, std::sync::atomic::Ordering::Release);
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// Deletes these saved secrets from the keyring (and the passwords from this run's memory); a failure is told in a dialog.
fn forget(ids: Vec<leaf_remote::SecretId>, cx: &mut App) {
    let work = cx.background_executor().spawn(async move { ids.iter().filter_map(|id| leaf_remote::forget_saved(id).err()).collect::<Vec<String>>() });
    cx.spawn(async move |cx| {
        let errors = work.await;
        cx.update(|cx| {
            reload_saved(cx);
            if let Some(e) = errors.first() {
                notice_anywhere(t!("Saved passwords"), format!("{}\n\n{}", t!("A saved password could not be deleted."), crate::i18n::td(e)), cx);
            }
        });
    })
    .detach();
}

fn notice_anywhere(title: &'static str, text: String, cx: &mut App) {
    if let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        let _ = w.update(cx, |_, window, cx| crate::prompt::notice(title, text, window, cx));
    }
}

/// "Forget saved password" on a host's menu: every password saved for that server (its scheme, server, port and user), read
/// from the keyring on a worker; says what was done.
pub fn forget_for(v: &VPath, cx: &mut App) {
    let (scheme, host, port, user) = (v.scheme().to_string(), v.host().to_string(), v.port(), v.user().map(str::to_string));
    let work = cx.background_executor().spawn(async move {
        let ids: Vec<_> = leaf_remote::saved_secrets()?.into_iter().filter(|id| id.is_for(&scheme, &host, port, user.as_deref())).collect();
        let n = ids.len();
        for id in &ids {
            leaf_remote::forget_saved(id)?;
        }
        Ok::<usize, String>(n)
    });
    cx.spawn(async move |cx| {
        let r = work.await;
        cx.update(|cx| {
            reload_saved(cx);
            let text = match r {
                Ok(0) => t!("No password is saved for this host.").to_string(),
                Ok(_) => t!("The saved password was deleted from the keyring. leaf asks for it at the next connection.").to_string(),
                Err(e) => format!("{}\n\n{}", t!("A saved password could not be deleted."), crate::i18n::td(&e)),
            };
            notice_anywhere(t!("Forget saved password"), text, cx);
        });
    })
    .detach();
}

/// Settings > Hosts > "Saved passwords": the servers (and key files) whose secret is in the keyring, by name only, with
/// Forget and Forget all.
fn saved_section(p: &Palette, cx: &mut App) -> Vec<AnyElement> {
    let mut v = vec![section(p, t!("Saved passwords"))];
    let quiet = |text: String| div().text_sm().text_color(p.text_secondary).child(text).into_any_element();
    match saved_now() {
        None => {
            reload_saved(cx);
            v.push(quiet(t!("Reading the keyring…").to_string()));
        }
        Some(Err(why)) => v.push(quiet(crate::i18n::td(&why))),
        Some(Ok(ids)) if ids.is_empty() => v.push(quiet(t!("No passwords saved. Tick Remember in the password dialog to save one in the system's keyring.").to_string())),
        Some(Ok(ids)) => {
            let all = ids.clone();
            v.push(card(
                p,
                L::Lock,
                t!("Forget all"),
                Some(t!("Deletes every password and passphrase leaf saved in the system's keyring.")),
                button("saved-forget-all", p, t!("Forget all"), move |_, cx| forget(all.clone(), cx)),
            ));
            for (i, id) in ids.into_iter().enumerate() {
                let label = id.label();
                v.push(card(p, L::Lock, &label, None, button(("saved-forget", i), p, t!("Forget"), move |_, cx| forget(vec![id.clone()], cx))));
            }
        }
    }
    v
}

/// What the tile of a host shows about it: found by a short TCP connect to its port (no login, no session), and the free space
/// only while a connection to the host is open already.
#[derive(Clone, Default)]
struct Probe {
    online: Option<bool>,
    ping_ms: Option<u32>,
    free: Option<u64>,
    at: Option<std::time::Instant>,
    running: bool,
    /// Set to use leaf's VPN, which is down: not probed.
    waits_vpn: bool,
    /// The open connection reaches the files through shell commands (no SFTP there); `Some(false)`: names only from `ls`.
    shell: Option<bool>,
}

#[derive(Default)]
struct Probes(std::collections::HashMap<String, Probe>);
impl Global for Probes {}

/// The probe is looked at again after this long, and only while a tile is drawn.
const PROBE_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// How a host's tile is probed: a host set to use leaf's VPN (not on the local network) only through the tunnel, and not at
/// all while it is down (a direct probe would show the server this PC's own address, and the PC's resolver its name).
#[derive(Debug, PartialEq, Eq)]
enum ProbeWay {
    Direct,
    Tunnel,
    /// It waits for the VPN.
    None,
}

fn probe_way(h: &Host, tunnel_up: bool) -> ProbeWay {
    if !h.vpn || leaf_vpn::route::is_lan_host(&h.host) {
        ProbeWay::Direct
    } else if tunnel_up {
        ProbeWay::Tunnel
    } else {
        ProbeWay::None
    }
}

fn probe_host(h: &Host) -> Probe {
    use std::net::ToSocketAddrs;
    let scheme = h.scheme.to_ascii_lowercase();
    let port = h.port.unwrap_or(match scheme.as_str() {
        "ftp" | "ftps" => 21,
        "smb" => 445,
        "dav" => 80,
        "davs" => 443,
        _ => 22,
    });
    let started = std::time::Instant::now();
    let limit = std::time::Duration::from_secs(2);
    let online = match probe_way(h, leaf_vpn::tunnel().is_some()) {
        ProbeWay::None => return Probe { waits_vpn: true, at: Some(std::time::Instant::now()), ..Probe::default() },
        ProbeWay::Tunnel => leaf_vpn::dial_blocking(&h.host, port, limit).is_ok_and(|s| s.through_tunnel()),
        ProbeWay::Direct => (h.host.as_str(), port).to_socket_addrs().ok().and_then(|mut a| a.find_map(|a| std::net::TcpStream::connect_timeout(&a, limit).ok())).is_some(),
    };
    let ping_ms = online.then(|| started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32);
    let free = if online { VPath::parse(&h.address()).ok().and_then(|v| leaf_remote::free_if_connected(&v)) } else { None };
    // (never connects and runs nothing on the host: it looks at the connection that is open)
    let shell = VPath::parse(&h.address()).ok().and_then(|v| leaf_remote::shell_access(&v));
    Probe { online: Some(online), ping_ms, free, at: Some(std::time::Instant::now()), running: false, waits_vpn: false, shell }
}

/// Starts the probes of the hosts whose answer is old (the page is redrawn when one comes back).
fn refresh_probes(hosts: &[Host], cx: &mut Context<crate::folder_view::FolderView>) {
    if !cx.has_global::<Probes>() {
        cx.set_global(Probes::default());
    }
    for h in hosts {
        let stale = {
            let p = cx.global_mut::<Probes>().0.entry(h.name.clone()).or_default();
            let stale = !p.running && p.at.is_none_or(|t| t.elapsed() > PROBE_EVERY);
            if stale {
                p.running = true;
            }
            stale
        };
        if !stale {
            continue;
        }
        let h = h.clone();
        cx.spawn(async move |this, cx| {
            let name = h.name.clone();
            let got = cx.background_spawn(async move { probe_host(&h) }).await;
            let _ = cx.update(|cx| {
                if cx.has_global::<Probes>() {
                    cx.global_mut::<Probes>().0.insert(name, got);
                }
            });
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}

/// The Home widget: one card per host (none: the widget is left out).
pub fn widget(_: &mut crate::folder_view::FolderView, w: &crate::home::WidgetCx, cx: &mut Context<crate::folder_view::FolderView>) -> Option<AnyElement> {
    let hosts = all(cx);
    if hosts.is_empty() {
        return None;
    }
    let p = &w.p;
    refresh_probes(&hosts, cx);
    let mut grid = div().grid().grid_cols(crate::folder_view::tile_cols(w.width, 240., 340., 12.)).gap_3();
    for (i, h) in hosts.into_iter().enumerate() {
        let loc = location(&h);
        let probe = cx.global::<Probes>().0.get(&h.name).cloned().unwrap_or_default();
        let (dot, state) = match (probe.online, probe.ping_ms) {
            _ if probe.waits_vpn => (p.text_tertiary, t!("Waits for the VPN").to_string()),
            (Some(true), Some(ms)) => (p.success, format!("{} \u{b7} {ms} ms", t!("Online"))),
            (Some(true), None) => (p.success, t!("Online").to_string()),
            (Some(false), _) => (p.danger, t!("Not reachable").to_string()),
            (None, _) => (p.text_tertiary, t!("Checking...").to_string()),
        };
        let state = match probe.free {
            Some(f) => format!("{state} \u{b7} {} {}", rf_core::format::size(f), t!("free")),
            None => state,
        };
        // H1: a host without SFTP, reached through shell commands
        let shell_note = match probe.shell {
            Some(true) => Some(t!("Shell access: no SFTP on this host; names with unusual characters may be limited")),
            Some(false) => Some(t!("Shell access (ls only): no SFTP on this host; items with unusual names may be shown wrong and are not deleted, moved or changed")),
            None => None,
        };
        grid = grid.child(
            v_flex()
                .id(("home-host", i))
                .p_3()
                .gap_2()
                .min_w_0()
                .rounded(px(p.tile_radius))
                .bg(p.tile_bg)
                .border_1()
                .border_color(p.border)
                .cursor_pointer()
                .hover(|s| s.bg(p.tile_hover))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(device_icon(h.kind, 28., p))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(h.name.clone()))
                                .child(div().text_xs().text_color(p.text_secondary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(subtitle(&h)))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .text_xs()
                                        .text_color(p.text_secondary)
                                        .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
                                        .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(state)),
                                )
                                .when_some(shell_note, |d, note| d.child(div().text_xs().text_color(p.text_tertiary).child(note))),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(l) = loc.clone() {
                        this.navigate(l, cx);
                    }
                })),
        );
    }
    Some(grid.into_any_element())
}

#[cfg(test)]
mod vpn_probe_tests {
    // (not `super::*`: that brings gpui's own `test` attribute in with the toolkit's prelude)
    use super::{ProbeWay, probe_way};
    use rf_core::hosts::Host;

    #[test]
    fn a_vpn_host_with_the_tunnel_down_is_never_probed() {
        let mut h = Host::new("box", "files.example.com");
        h.vpn = true;
        assert_eq!(probe_way(&h, false), ProbeWay::None, "no direct probe while the VPN is down");
        assert_eq!(probe_way(&h, true), ProbeWay::Tunnel);
        // the same host without the tick, and a ticked host on the local network: probed as before
        h.vpn = false;
        assert_eq!(probe_way(&h, false), ProbeWay::Direct);
        let mut nas = Host::new("nas", "nas.local");
        nas.vpn = true;
        assert_eq!(probe_way(&nas, false), ProbeWay::Direct);
    }
}

/// A host kind's device icon; the line server icon if the picture cannot be made.
fn device_icon(kind: HostKind, size: f32, p: &Palette) -> AnyElement {
    crate::host_icons::element(kind, size, p.text_secondary, p.accent)
        .unwrap_or_else(|| Icon::new(L::Server).size(px(size)).text_color(p.text_secondary).into_any_element())
}

/// Check scripts (`demo home`): two made-up hosts in the list (in memory only, never saved) with their tiles' answers already in, so
/// nothing is dialled.
pub fn demo_hosts(cx: &mut App) {
    let _ = all(cx);
    let mut nas = Host::new("Home NAS", "nas.example.com");
    nas.kind = HostKind::Nas;
    nas.user = Some("user".into());
    let mut box_ = Host::new("Test box", "192.0.2.10");
    box_.user = Some("user".into());
    let hosts = vec![nas, box_];
    let now = Some(std::time::Instant::now());
    let mut probes = Probes::default();
    probes.0.insert("Home NAS".into(), Probe { online: Some(true), ping_ms: Some(4), free: Some(1_800_000_000_000), at: now, ..Probe::default() });
    probes.0.insert("Test box".into(), Probe { online: Some(false), at: now, ..Probe::default() });
    cx.set_global(probes);
    cx.global_mut::<Store>().0.hosts = hosts;
}
