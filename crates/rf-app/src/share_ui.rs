//! Shared folders with a friend's leaf, in the app: "Share with a friend..." on a folder (the invite to send), "Join shared
//! folder..." (paste the invite, choose where the folder goes), the code to confirm when the two PCs first meet, the "Shared
//! folders" list on the Sync jobs page, the port in Settings > Nearby, a notice in the Status Center when a share stops by
//! itself, and the badge on a shared folder. The engine is `crates/leaf-share` (on Nearby's identity and leaf-sync's planner);
//! it runs in the main process only, and listens only while a folder is shared or joined.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::*;
use leaf_share::{Config, Invite, Role, ShareInfo, ShareListener, ShareService, State};
use rf_core::format;

use crate::prompt::{Button, Kind};
use crate::settings::{self, Settings};
use crate::t;
use crate::theme::{self, Palette};

enum Ev {
    Changed,
    Code { share: String, peer: String, code: String },
    Joined { result: Result<(), String> },
    Paused { name: String, reason: String },
}

struct Listener(mpsc::UnboundedSender<Ev>);

impl ShareListener for Listener {
    fn changed(&self) {
        let _ = self.0.unbounded_send(Ev::Changed);
    }
    fn pairing_code(&self, share: &str, peer: &str, code: &str) {
        let _ = self.0.unbounded_send(Ev::Code { share: share.into(), peer: peer.into(), code: code.into() });
    }
    fn joined(&self, _share: &str, result: Result<(), String>) {
        let _ = self.0.unbounded_send(Ev::Joined { result });
    }
    fn paused(&self, _share: &str, name: &str, reason: &str) {
        let _ = self.0.unbounded_send(Ev::Paused { name: name.into(), reason: reason.into() });
    }
}

#[derive(Default)]
pub struct ShareUi {
    service: Option<Arc<ShareService>>,
    /// The device name and port it was started with: a change of either starts it anew.
    started: Option<(String, u16)>,
    shares: Vec<ShareInfo>,
    /// The shared folders, lower-cased on Windows (for the badge).
    folders: Vec<String>,
    problem: Option<String>,
    events: Option<mpsc::UnboundedSender<Ev>>,
}

impl Global for ShareUi {}

fn state_dir() -> Option<PathBuf> {
    settings::app_dir().map(|d| d.join("share"))
}

fn key_of(p: &Path) -> String {
    let s = p.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// At start, in the main process only. Nothing listens and no runtime starts unless a folder is shared or joined already.
pub fn init(cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<Ev>();
    cx.set_global(ShareUi { events: Some(tx), ..Default::default() });
    cx.spawn(async move |cx| {
        while let Some(ev) = rx.next().await {
            cx.update(|cx| on_event(ev, cx));
        }
    })
    .detach();
    let any = state_dir().is_some_and(|d| !leaf_share::store::Store::load(&d).shares.is_empty());
    if any {
        ensure(cx);
    }
}

/// Follows the settings: a running service is started anew when the device name or the port changed.
pub fn apply(cx: &mut App) {
    let Some(ui) = cx.try_global::<ShareUi>() else { return };
    if ui.service.is_none() {
        return;
    }
    let s = cx.global::<Settings>();
    let want = (crate::nearby::device_name(s), s.share_port);
    if ui.started.as_ref() != Some(&want) {
        stop(cx);
        ensure(cx);
    }
}

fn stop(cx: &mut App) {
    let ui = cx.global_mut::<ShareUi>();
    ui.started = None;
    if let Some(s) = ui.service.take() {
        // (stopping waits a moment for the connections: not on the UI thread)
        std::thread::spawn(move || s.stop());
    }
}

/// The running service, started now when it is not.
fn ensure(cx: &mut App) -> Option<Arc<ShareService>> {
    let ui = cx.try_global::<ShareUi>()?;
    if let Some(s) = ui.service.clone() {
        return Some(s);
    }
    let tx = ui.events.clone()?;
    let (dir, app) = (state_dir()?, settings::app_dir()?);
    let s = cx.global::<Settings>();
    let (name, port) = (crate::nearby::device_name(s), s.share_port);
    let mut cfg = Config::new(&name, dir, app.join("nearby"), port);
    cfg.trash_local = Arc::new(|p: &[PathBuf]| crate::checkpoint_ui::to_trash(p));
    cfg.bin_takes = Arc::new(|p: &Path| crate::sync_ui::bin_takes(p));
    cfg.runtime = Some(leaf_remote::runtime_handle());
    match ShareService::start(cfg, Arc::new(Listener(tx))) {
        Ok(svc) => {
            let ui = cx.global_mut::<ShareUi>();
            ui.service = Some(svc.clone());
            ui.started = Some((name, port));
            refresh(cx);
            Some(svc)
        }
        Err(e) => {
            log::warn!("share: cannot start: {e}");
            cx.global_mut::<ShareUi>().problem = Some(e);
            None
        }
    }
}

fn refresh(cx: &mut App) {
    let ui = cx.global_mut::<ShareUi>();
    if let Some(s) = ui.service.clone() {
        ui.shares = s.list();
        ui.folders = s.shared_folders().iter().map(|p| key_of(p)).collect();
        ui.problem = s.problem();
    }
}

fn service(cx: &App) -> Option<Arc<ShareService>> {
    cx.try_global::<ShareUi>().and_then(|u| u.service.clone())
}

/// Whether `path` is a shared folder (the badge in the list).
pub fn is_shared(path: &Path, cx: &App) -> bool {
    cx.try_global::<ShareUi>().is_some_and(|u| !u.folders.is_empty() && u.folders.contains(&key_of(path)))
}

fn in_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App)) {
    if let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        let _ = w.update(cx, |_, window, cx| f(window, cx));
    }
}

/// Opens a dialog after the one now closing (a button's run cannot open the next one itself).
fn next(cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
    cx.spawn(async move |cx| {
        let _ = cx.update(|cx| in_window(cx, f));
    })
    .detach();
}

fn on_event(ev: Ev, cx: &mut App) {
    match ev {
        Ev::Changed => {
            refresh(cx);
            cx.refresh_windows();
        }
        Ev::Code { share, peer, code } => in_window(cx, move |window, cx| {
            let (s1, s2) = (share.clone(), share);
            let yes = Button::new(t!("The codes are the same"), Kind::Primary, move |_, _, cx| {
                if let Some(s) = service(cx) {
                    s.confirm_pairing(&s1, true);
                }
                Ok(())
            });
            let no = Button::new(t!("They differ"), Kind::Normal, move |_, _, cx| {
                if let Some(s) = service(cx) {
                    s.confirm_pairing(&s2, false);
                }
                Ok(())
            });
            let shown = format!("{} {}", &code[..code.len().min(3)], &code[code.len().min(3)..]);
            crate::prompt::show(
                t!("Confirm the code"),
                Some(format!(
                    "{}\n\n{shown}\n\n{} ({peer})",
                    t!("This PC and your friend's meet for the first time. Both show a code: check with your friend that it is the same on both screens."),
                    t!("Only when it is the same are the folders shared")
                )),
                None,
                vec![yes, no],
                window,
                cx,
            );
        }),
        Ev::Joined { result } => {
            refresh(cx);
            in_window(cx, move |window, cx| match result {
                Ok(()) => crate::prompt::notice(
                    t!("Shared folder joined"),
                    t!("The folder fills from your friend's PC now, and stays the same on both from here on. It is listed on the Sync jobs page under Shared folders.").to_string(),
                    window,
                    cx,
                ),
                Err(e) => crate::prompt::notice(t!("The shared folder could not be joined"), e, window, cx),
            });
        }
        Ev::Paused { name, reason } => {
            refresh(cx);
            crate::status_center::start_with_id(cx, format!("{} {name}", t!("Shared folder paused:")), move |_| Err(anyhow::anyhow!(reason)), None);
        }
    }
}

// ------------------------------------------------------------------------------------------------------------------ commands

/// "Share with a friend...": shares `folder` and makes an invite to send (asking for the address the friend reaches this PC at).
pub fn share_folder(folder: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let Some(folder) = folder.filter(|f| f.is_dir()) else {
        crate::prompt::notice(t!("Share with a friend"), t!("Choose a folder on this computer first (a server's or an archive's folder cannot be shared).").to_string(), window, cx);
        return;
    };
    let Some(svc) = ensure(cx) else {
        let why = cx.global::<ShareUi>().problem.clone().unwrap_or_default();
        crate::prompt::notice(t!("Shared folders cannot start"), why, window, cx);
        return;
    };
    let id = match svc.create(&folder) {
        Ok(id) => id,
        Err(e) => {
            if let Some(why) = svc.problem() {
                crate::prompt::notice(t!("Shared folders cannot start"), why, window, cx);
            } else {
                crate::prompt::notice(t!("This folder cannot be shared"), e, window, cx);
            }
            return;
        }
    };
    refresh(cx);
    invite_dialog(id, window, cx);
}

fn invite_dialog(id: String, window: &mut Window, cx: &mut App) {
    let Some(svc) = service(cx) else { return };
    if let Some(s) = svc.list().into_iter().find(|s| s.id == id)
        && !s.peer_name.is_empty()
    {
        crate::prompt::notice(t!("Share with a friend"), format!("{} {}.", t!("This folder is shared with"), s.peer_name), window, cx);
        return;
    }
    let guess = leaf_share::own_addresses().into_iter().next().unwrap_or_default();
    let make = Button::new(t!("Make the invite"), Kind::Primary, move |text, _, cx| {
        let Some(svc) = service(cx) else { return Err(t!("Shared folders are not running").into()) };
        let invite = svc.invite(&id, &text.unwrap_or_default())?;
        cx.write_to_clipboard(ClipboardItem::new_string(invite.clone()));
        next(cx, move |window, cx| show_invite(invite, window, cx));
        Ok(())
    });
    crate::prompt::show(
        t!("Share with a friend"),
        Some(format!(
            "{}\n\n{} {}.",
            t!("Your friend needs leaf too. Type the address your friend's PC reaches this one at: its address in your WireGuard network, or on your local network."),
            t!("The port is added by itself:"),
            svc.port()
        )),
        Some((guess, "10.0.0.2".into())),
        vec![make, Button::cancel()],
        window,
        cx,
    );
}

fn show_invite(invite: String, window: &mut Window, cx: &mut App) {
    let again = invite.clone();
    let copy = Button::new(t!("Copy again"), Kind::Normal, move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(again.clone()));
        Ok(())
    });
    crate::prompt::show(
        t!("The invite is copied"),
        Some(format!(
            "{}\n\n{invite}\n\n{}",
            t!("Send this line to your friend (a chat, a mail). In leaf they choose Join shared folder... and paste it."),
            t!("It works once, for 24 hours. When your PCs meet for the first time, both show a code to compare.")
        )),
        None,
        vec![Button::new(t!("Done"), Kind::Primary, |_, _, _| Ok(())), copy],
        window,
        cx,
    );
}

/// "Join shared folder...": the invite, then where the folder goes.
pub fn join_dialog(window: &mut Window, cx: &mut App) {
    let go = Button::new(t!("Next"), Kind::Primary, |text, _, cx| {
        let text = text.unwrap_or_default();
        let inv = Invite::parse(&text)?;
        let name = leaf_vfs::names::safe_local_name(if inv.name.trim().is_empty() { "Shared folder" } else { inv.name.trim() });
        let (tx, rx) = futures::channel::oneshot::channel::<Option<PathBuf>>();
        let title = t!("Choose where the shared folder goes");
        leaf_sys::sta::run(move |owner| {
            let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
        });
        cx.spawn(async move |cx| {
            let Ok(Some(parent)) = rx.await else { return };
            let _ = cx.update(|cx| {
                let folder = parent.join(&name);
                let r = ensure(cx).ok_or_else(|| cx.global::<ShareUi>().problem.clone().unwrap_or_default()).and_then(|s| s.join(&text, &folder));
                refresh(cx);
                cx.refresh_windows();
                if let Err(e) = r {
                    in_window(cx, move |window, cx| crate::prompt::notice(t!("The shared folder could not be joined"), e, window, cx));
                }
            });
        })
        .detach();
        Ok(())
    });
    crate::prompt::show(
        t!("Join shared folder"),
        Some(t!("Paste the invite your friend sent (one line that starts with leaf-share1). Then choose where the folder goes on this PC: a folder with the shared folder's name is made there.").to_string()),
        Some((String::new(), "leaf-share1 ...".into())),
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

// ------------------------------------------------------------------------------------------------------------------ the list

fn state_text(s: &ShareInfo) -> String {
    match &s.state {
        State::InSync => t!("In sync").to_string(),
        State::Syncing { files } if *files > 0 => format!("{} {files} {}", t!("Syncing"), if *files == 1 { t!("file") } else { t!("files") }),
        State::Syncing { .. } => t!("Comparing\u{2026}").to_string(),
        State::Waiting { address } if address.is_empty() || s.role == Role::Host => t!("Waiting for the other PC").to_string(),
        State::Waiting { address } => format!("{} {address}", t!("Waiting for the other PC at")),
        State::NoFriend => t!("Waiting for your friend to join (send the invite)").to_string(),
        State::Joining => t!("Joining\u{2026}").to_string(),
        State::Paused { reason } if reason.is_empty() => t!("Paused").to_string(),
        State::Paused { reason } => format!("{}: {reason}", t!("Paused")),
    }
}

fn leave(s: &ShareInfo, window: &mut Window, cx: &mut App) {
    let id = s.id.clone();
    let go = Button::new(t!("Leave"), Kind::Danger, move |_, _, cx| {
        if let Some(svc) = service(cx) {
            svc.leave(&id);
        }
        refresh(cx);
        Ok(())
    });
    crate::prompt::show(
        t!("Stop sharing this folder?"),
        Some(format!("{}\n\n{}", s.folder.display(), t!("The files stay on both PCs; they are just not kept the same any more. The other PC is told."))),
        None,
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

fn set_address(s: &ShareInfo, window: &mut Window, cx: &mut App) {
    let id = s.id.clone();
    let ok = Button::new(t!("Save"), Kind::Primary, move |text, _, cx| {
        let text = text.unwrap_or_default().trim().to_string();
        if text.is_empty() || text.contains(char::is_whitespace) {
            return Err(t!("Type an address like 10.0.0.2:53318").into());
        }
        if let Some(svc) = service(cx) {
            svc.set_address(&id, &text);
        }
        refresh(cx);
        Ok(())
    });
    crate::prompt::show(
        t!("The other PC's address"),
        Some(t!("Where this PC reaches your friend's: host or address and port, like 10.0.0.2:53318. Change it when their address changed.").to_string()),
        Some((s.address.clone(), "10.0.0.2:53318".into())),
        vec![ok, Button::cancel()],
        window,
        cx,
    );
}

/// The "Shared folders" part of the Sync jobs page (nothing when no folder is shared or joined and the service never ran).
pub fn section(p: &Palette, cx: &mut App) -> Option<Div> {
    let ui = cx.try_global::<ShareUi>()?;
    let (shares, problem) = (ui.shares.clone(), ui.problem.clone());
    let top = h_flex()
        .px_3()
        .py_2()
        .gap_2()
        .items_center()
        .border_b_1()
        .border_color(p.divider)
        .child(Icon::new(L::Users).size(px(16.)).text_color(p.accent))
        .child(div().flex_1().min_w_0().text_sm().text_color(p.text).child(t!("Shared folders")))
        .child(crate::sync_ui::small_button("share-join", t!("Join shared folder\u{2026}"), p).on_click(|_, window, cx| join_dialog(window, cx)));
    let mut rows = v_flex().p_1();
    if let Some(problem) = problem {
        rows = rows.child(div().px_3().py_1().text_xs().text_color(p.danger).child(problem));
    }
    if shares.is_empty() {
        rows = rows.child(div().px_3().py_2().text_sm().text_color(p.text_secondary).child(t!(
            "A shared folder stays the same on your PC and a friend's (who runs leaf too), with no server in between: right-click a folder and choose Share with a friend\u{2026}, or join one a friend shared with you."
        )));
    }
    for (i, s) in shares.into_iter().enumerate() {
        let who = if s.peer_name.is_empty() { t!("nobody yet").to_string() } else { s.peer_name.clone() };
        let mut line = state_text(&s);
        if s.last_sync_ms > 0 {
            line = format!("{line} \u{b7} {} {} \u{2014} {}", t!("Last sync"), format::datetime(s.last_sync_ms), s.last_result);
        }
        let paused = matches!(s.state, State::Paused { .. });
        let host = s.role == Role::Host;
        let (s1, s2, s3, s4, s5) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        let mut buttons = h_flex().gap_2().items_center().flex_shrink_0();
        if host && !s.peer_name.is_empty() {
            let can = s.friend_can_write;
            buttons = buttons.child(div().text_xs().text_color(p.text_secondary).child(t!("They can change files"))).child(crate::settings_view::switch(
                ("share-write", i),
                p,
                can,
                move |_, cx| {
                    if let Some(svc) = service(cx) {
                        svc.set_friend_can_write(&s1.id, !can);
                    }
                    refresh(cx);
                },
            ));
        }
        if host && s.peer_name.is_empty() {
            buttons = buttons.child(crate::sync_ui::small_button(("share-invite", i), t!("Invite\u{2026}"), p).on_click(move |_, window, cx| invite_dialog(s2.id.clone(), window, cx)));
        }
        if host && !paused {
            buttons = buttons.child(crate::sync_ui::small_button(("share-now", i), t!("Sync now"), p).on_click(move |_, _, cx| {
                if let Some(svc) = service(cx) {
                    svc.sync_now(&s3.id);
                }
            }));
        }
        if !host {
            buttons = buttons.child(crate::sync_ui::small_button(("share-addr", i), t!("Address\u{2026}"), p).on_click(move |_, window, cx| set_address(&s4, window, cx)));
        }
        let pid = s.id.clone();
        buttons = buttons
            .child(crate::sync_ui::small_button(("share-pause", i), if paused { t!("Resume") } else { t!("Pause") }, p).on_click(move |_, _, cx| {
                if let Some(svc) = service(cx) {
                    svc.set_paused(&pid, !paused);
                }
                refresh(cx);
            }))
            .child(crate::sync_ui::small_button(("share-leave", i), t!("Leave"), p).on_click(move |_, window, cx| leave(&s5, window, cx)));
        rows = rows.child(
            h_flex()
                .id(("share-row", i))
                .w_full()
                .px_3()
                .py_2()
                .gap_3()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .hover(|d| d.bg(p.hover))
                .child(Icon::new(L::Users).size(px(16.)).flex_shrink_0().text_color(if paused { p.warning } else { p.text_secondary }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().text_color(p.text).truncate().child(format!("{} \u{b7} {} {who}", s.name, t!("with"))))
                        .child(div().text_xs().text_color(p.text_secondary).truncate().child(s.folder.display().to_string()))
                        .child(div().text_xs().text_color(if paused { p.warning } else { p.text_tertiary }).truncate().child(line)),
                )
                .child(buttons),
        );
    }
    Some(v_flex().border_b_1().border_color(p.divider).child(top).child(rows))
}

// ------------------------------------------------------------------------------------------------------------------ settings

/// Settings > Nearby, the shared folders part: the port.
pub(crate) fn settings_cards(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings_view::{button, card, section};
    let port = s.share_port;
    let listening = service(cx).map_or(0, |svc| svc.port());
    let desc = if listening > 0 {
        format!("{} {port}. {}", t!("Shared folders listen on UDP port"), t!("Only while a folder is shared or joined, and only devices stored for a shared folder are answered."))
    } else {
        format!("{} {port} {}", t!("Shared folders use UDP port"), t!("while a folder is shared or joined (nothing listens now). Only devices stored for a shared folder are answered."))
    };
    vec![
        section(p, t!("Shared folders")),
        card(
            p,
            L::Users,
            t!("Port for shared folders"),
            Some(desc.as_str()),
            button("share-port", p, t!("Change\u{2026}"), move |window, cx| {
                let ok = Button::new(t!("Save"), Kind::Primary, |text, _, cx| {
                    let n: u16 = text.unwrap_or_default().trim().parse().map_err(|_| t!("A port is a number from 1024 to 65535").to_string())?;
                    if n < 1024 {
                        return Err(t!("A port is a number from 1024 to 65535").into());
                    }
                    settings::update(cx, |s| s.share_port = n);
                    Ok(())
                });
                crate::prompt::show(
                    t!("Port for shared folders"),
                    Some(format!("{} {}.", t!("Your friend's invite and their saved address carry this port: after a change, tell them the new address. The default is"), leaf_share::DEFAULT_PORT)),
                    Some((port.to_string(), leaf_share::DEFAULT_PORT.to_string())),
                    vec![ok, Button::cancel()],
                    window,
                    cx,
                );
            }),
        ),
    ]
}
