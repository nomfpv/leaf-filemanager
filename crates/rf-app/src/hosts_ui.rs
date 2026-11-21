//! Saved hosts in the app (W1b): the list from `hosts.toml` in the config folder, shared by the Settings page "Hosts", the
//! sidebar section and the Home widget (both only while there are hosts). Opening a host goes to its address as a remote place
//! (SFTP through `leaf-remote`); a scheme with no backend yet says that connecting is not built yet.
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
    cx.global::<Store>().0.hosts.clone()
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

/// Where opening the host goes: its start folder as a remote place; an SFTP host without one opens in the user's home
/// folder on the server (`/~`).
pub fn location(h: &Host) -> Option<Location> {
    let v = VPath::parse(&h.address()).ok()?;
    let v = if h.start_path.is_none() && h.scheme.eq_ignore_ascii_case("sftp") { v.with_path("/~") } else { v };
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

/// The add / edit dialog: one line (`Host::to_line`). `old`: the name of the host being edited.
fn edit_dialog(old: Option<Host>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let initial = old.as_ref().map(Host::to_line).unwrap_or_default();
    let old_name = old.map(|h| h.name);
    let title = if old_name.is_some() { t!("Edit host") } else { t!("Add a host") };
    let message = format!("{}\n{EXAMPLE}\n{}", t!("One line: a name, =, the address, and if needed the key file and a jump host."), t!("Passwords are never saved in this list."));
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
        card(p, L::Plus, t!("Add a host"), Some(t!("A server you reach over SFTP: its address, user, port, start folder, key file and jump host. No passwords are kept here.")), button("hosts-add", p, t!("Add"), |w, cx| edit_dialog(None, w, cx))),
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
                "Servers are checked against ~/.ssh/known_hosts: a new one is asked about, one whose key changed is refused. Logging in tries your SSH agent, the key file, then asks for a password; passphrases and passwords stay in memory until leaf closes. Browsing, opening and copying to this PC work; uploading, renaming and deleting on a server come next."
            )))
            .into_any_element(),
    );
    v
}

/// The Home widget: one card per host (none: the widget is left out).
pub fn widget(_: &mut crate::folder_view::FolderView, w: &crate::home::WidgetCx, cx: &mut Context<crate::folder_view::FolderView>) -> Option<AnyElement> {
    let hosts = all(cx);
    if hosts.is_empty() {
        return None;
    }
    let p = &w.p;
    let mut grid = div().grid().grid_cols(crate::folder_view::tile_cols(w.width, 240., 340., 12.)).gap_3();
    for (i, h) in hosts.into_iter().enumerate() {
        let loc = location(&h);
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
                                .child(div().text_xs().text_color(p.text_secondary).overflow_hidden().text_ellipsis().whitespace_nowrap().child(subtitle(&h))),
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
