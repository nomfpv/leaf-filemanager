//! A tiny extension: a status-bar item that counts the files deleted in this run, and an overlay that draws a dot in the corner.
//! It shows that an extension compiles against the public API only (`rf_app::ext`). Run it like leaf itself:
//! `cargo run --profile fast -p rf-app --example hello_ext`.

use std::cell::Cell;
use std::rc::Rc;

use rf_app::ext::gpui_kit::prelude::*;
use rf_app::ext::gpui_kit::{div, px};
use rf_app::ext::{Event, Extensions, Side};

fn main() {
    let ext = Extensions::new().add("hello", "Hello example", |e| {
        // events: count the items that were deleted
        let deleted = Rc::new(Cell::new(0usize));
        let counter = deleted.clone();
        e.subscribe(move |ev, cx| {
            if let Event::ItemsDeleted { total, .. } = ev {
                counter.set(counter.get() + total);
                // the status bar is drawn again with the new number
                rf_app::ext::request_frame(cx);
            }
        });
        // a slot: the status bar's right group, nothing while the count is zero
        e.status_item(Side::Right, move |_cx| (deleted.get() > 0).then(|| div().child(format!("{} deleted", deleted.get())).into_any_element()));
        // a layer: a dot in the bottom right corner of every window, in the app's accent colour
        e.overlay(|_window, _cx| {
            let accent = rf_app::ext::theme().accent;
            div().absolute().right(px(12.)).bottom(px(40.)).size(px(8.)).rounded_full().bg(accent).opacity(0.8).into_any_element()
        });
    });
    rf_app::run_with(ext);
}
