//! A tiny extension that uses one thing from each group of the seam, so that the public API is compile-proven: a status-bar item that
//! counts the files deleted in this run, an overlay with a dot, events (extraction, drive space, friend links), a transition hook for
//! rows that leave the list, geometry, pointer and key input, an animation, a sound file, a page, a Home widget, a Settings section, a
//! left pane, a progress bar visual and the peer channel. Run it like leaf itself:
//! `cargo run --profile fast -p rf-app --example hello_ext`.

use std::cell::Cell;
use std::rc::Rc;

use rf_app::ext::gpui_kit::assets::IconName;
use rf_app::ext::gpui_kit::prelude::*;
use rf_app::ext::gpui_kit::{Bounds, div, point, px, size};
use rf_app::ext::{Event, Extensions, FileKind, OpKind, Side, WidgetSize};

fn main() {
    let ext = Extensions::new().add("hello", "Hello example", |e| {
        // events: count the items that were deleted; look at the newer events with a `_` arm for the rest
        let deleted = Rc::new(Cell::new(0usize));
        let counter = deleted.clone();
        e.subscribe(move |ev, cx| match ev {
            Event::ItemsDeleted { total, .. } => {
                counter.set(counter.get() + total);
                // the status bar is drawn again with the new number
                rf_app::ext::request_frame(cx);
            }
            // an archive is about to be extracted: a short full-window scene while it runs underneath
            Event::ArchiveExtractStarting { info } if info.largest.as_ref().is_some_and(|l| l.1 == FileKind::Video) => {
                rf_app::ext::start_scene("hello", 2_000, |_w, _cx| div().size_full().bg(rf_app::ext::theme().window_bg).opacity(0.5).into_any_element(), cx);
            }
            // a finished download: play a sound file at half volume (nothing plays while the user's switch is off)
            Event::DownloadFinished { .. } => rf_app::ext::play_sound_file(cx, "C:/path/to/ding.wav", 0.5),
            // operations carry a typed kind and an id that matches progress to its start
            Event::OperationStarted { kind: OpKind::Copy, .. } | Event::DriveSpace { .. } | Event::TransferSpeed { .. } | Event::WindowMinimized { .. } => {}
            Event::FriendLink { friend, online, .. } => println!("{friend} online: {online}"),
            _ => {}
        });
        // a slot: the status bar's right group, nothing while the count is zero
        e.status_item(Side::Right, move |_cx| (deleted.get() > 0).then(|| div().child(format!("{} deleted", deleted.get())).into_any_element()));

        // a layer: a dot in the bottom right corner of every window, in the app's accent colour
        e.overlay(|_window, _cx| {
            let accent = rf_app::ext::theme().accent;
            div().absolute().right(px(12.)).bottom(px(40.)).size(px(8.)).rounded_full().bg(accent).opacity(0.8).into_any_element()
        });
        // the dot takes the mouse (a region) and an effect asks for frames for a moment
        e.overlay_regions(|_cx| vec![Bounds { origin: point(px(0.), px(0.)), size: size(px(1.), px(1.)) }]);
        e.on_pointer(|ev, cx| {
            if matches!(ev.kind, rf_app::ext::PointerKind::Down(_)) {
                rf_app::ext::animate(cx, 500);
            }
        });
        // the peer channel: the friends whose leaf takes extension messages
        e.on_key(|key, cx| {
            let can = rf_app::ext::peer::friends(cx).iter().filter(|f| f.can_receive).count();
            println!("key in the scene: {} ({can} friends can receive)", key.key);
        });

        // transition hook: rows that are deleted or moved away stay painted (greyed) for 300 ms while an effect could play;
        // geometry says where things are
        e.on_rows_leave(|leaving, _cx| {
            let _first_rect = leaving.rows.first().and_then(|r| r.rect).or_else(|| rf_app::ext::content_rect());
            Some(300)
        });
        e.on_folder_open(|opening, _cx| println!("opening {:?}, content {:?}", opening.to, opening.content));

        // a full page with its own tab (opened by "Open Hello page" in the command search), a Home widget, a Settings section
        e.page("main", "Hello page", IconName::Star, |_w, _cx| div().p_4().child("Hello from a page").into_any_element());
        e.home_widget("hi", "Hello", IconName::Star, &[WidgetSize::Small, WidgetSize::Medium], 1, |_cx, _size, _width| Some(div().child("Hello widget").into_any_element()));
        e.settings_section("Hello", IconName::Star, |_cx| vec![div().child("Hello settings").into_any_element()]);
        // a pane docked left, under the sidebar's groups
        e.side_pane_left("Hello notes", 120., |_w, _cx| div().p_2().child("notes").into_any_element());
        // the visual of a progress bar: a plain strip for copies, the normal bar for everything else
        e.progress_bar(|info| {
            (info.kind == OpKind::Copy).then(|| div().h(px(6.)).w(px(info.width * info.fraction.unwrap_or(0.))).bg(rf_app::ext::theme().accent).into_any_element())
        });
    });
    rf_app::run_with(ext);
}
