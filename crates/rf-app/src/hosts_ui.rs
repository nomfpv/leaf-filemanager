//! Saved hosts in the app (W1b): the list from `hosts.toml` in the config folder, shared by the Settings page "Hosts", the
//! sidebar section and the Home widget (both only while there are hosts). Opening a host goes to its address as a remote place
//! (SFTP, FTP and FTPS through `leaf-remote`); a scheme with no backend yet says that connecting is not built yet.
//!
//! The file never holds a password or another secret (see `rf_core::hosts`).

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::*;
use rf_core::Location;
use rf_core::hosts::{Host, Hosts};
use rf_core::vpath::VPath;

use crate::settings_view::{button, card, section};
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
    let v = VPath::parse(&h.address()).ok()?;
    let login_folder = ["sftp", "ftp", "ftps"].iter().any(|s| h.scheme.eq_ignore_ascii_case(s));
    let v = if h.start_path.is_none() && login_folder { v.with_path("/~") } else { v };
    Some(Location::Remote(v))
}

/// A bookmark of the host as a place.
pub fn bookmark_location(h: &Host, path: &str) -> Option<Location> {
    let v = VPath::parse(&h.address()).ok()?;
    Some(Location::Remote(v.with_path(path)))
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
        s.push_str(&format!("  ({} {j}: {})", t!("via"), t!("jump hosts are not supported yet")));
    }
    s
}

const EXAMPLE: &str = "name = sftp://user@example.com:22/folder key=~/.ssh/id_ed25519 jump=bastion";
const FTP_EXAMPLE: &str = "nas = ftps://user@nas.local/share";

/// The add / edit dialog: one line (`Host::to_line`). `old`: the name of the host being edited.
fn edit_dialog(old: Option<Host>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let initial = old.as_ref().map(Host::to_line).unwrap_or_default();
    let old_name = old.map(|h| h.name);
    let title = if old_name.is_some() { t!("Edit host") } else { t!("Add a host") };
    let message = format!(
        "{}\n{EXAMPLE}\n{FTP_EXAMPLE}\n{}\n{}",
        t!("One line: a name, =, the address, and if needed the key file and a jump host."),
        t!("Passwords are never saved in this list."),
        t!("ftp:// is not encrypted (the password crosses the network as it is); use ftps:// where the server has it.")
    );
    crate::prompt::show(
        title,
        Some(message),
        Some((initial, t!("name = sftp://user@host/folder").to_string())),
        vec![
            Button::new(t!("Save"), Kind::Primary, move |text, _, cx| {
                let h = Host::from_line(text.as_deref().unwrap_or_default()).map_err(|e| crate::i18n::td(&e))?;
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
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            crate::prompt::notice(t!("Import from OpenSSH config"), format!("{}: {e}", path.display()), window, cx);
            return;
        }
    };
    let r = rf_core::hosts::parse_ssh_config(&text);
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
        card(p, L::Plus, t!("Add a host"), Some(t!("A server you reach over SFTP, FTPS or FTP: its address, user, port, start folder, key file and jump host. No passwords are kept here.")), button("hosts-add", p, t!("Add"), |w, cx| edit_dialog(None, w, cx))),
        card(
            p,
            L::Download,
            t!("Import from OpenSSH config"),
            Some(t!("Adds the hosts of your ~/.ssh/config (Host, HostName, User, Port, IdentityFile, ProxyJump). Include lines and wildcards are left out.")),
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
            .into_any_element();
        let mut desc = subtitle(&h);
        if let Some(k) = &h.key_file {
            desc.push_str(&format!("  ·  {} {k}", t!("key")));
        }
        v.push(card(p, L::Server, &h.name, Some(&desc), controls));
    }
    v.push(
        v_flex()
            .pt_3()
            .child(div().text_xs().text_color(p.text_tertiary).child(t!(
                "SFTP servers are checked against ~/.ssh/known_hosts: a new one is asked about, one whose key changed is refused. Logging in tries your SSH agent, the key file, then asks for a password; passphrases and passwords stay in memory until leaf closes. An FTPS certificate this PC does not trust is asked about; one you trust is kept by its fingerprint. Plain FTP is not encrypted."
            )))
            .into_any_element(),
    );
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
}

#[derive(Default)]
struct Probes(std::collections::HashMap<String, Probe>);
impl Global for Probes {}

/// The probe is looked at again after this long, and only while a tile is drawn.
const PROBE_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

fn probe_host(h: &Host) -> Probe {
    use std::net::ToSocketAddrs;
    let port = h.port.unwrap_or(if h.scheme.to_ascii_lowercase().starts_with("ftp") { 21 } else { 22 });
    let started = std::time::Instant::now();
    let online = (h.host.as_str(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.find_map(|a| std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(2)).ok()))
        .is_some();
    let ping_ms = online.then(|| started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32);
    let free = if online { VPath::parse(&h.address()).ok().and_then(|v| leaf_remote::free_if_connected(&v)) } else { None };
    Probe { online: Some(online), ping_ms, free, at: Some(std::time::Instant::now()), running: false }
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
            (Some(true), Some(ms)) => (p.success, format!("{} \u{b7} {ms} ms", t!("Online"))),
            (Some(true), None) => (p.success, t!("Online").to_string()),
            (Some(false), _) => (p.danger, t!("Not reachable").to_string()),
            (None, _) => (p.text_tertiary, t!("Checking...").to_string()),
        };
        let state = match probe.free {
            Some(f) => format!("{state} \u{b7} {} {}", rf_core::format::size(f), t!("free")),
            None => state,
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
                        .child(Icon::new(L::Server).size(px(28.)).text_color(p.text_secondary))
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
                                ),
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
