//! "Copy as context": the files of a selection (or a folder) as one text block for pasting into a chat. The reading is done by
//! `leaf_dev::context` off the UI thread (with a Cancel button); a dialog then shows the counts, the token estimate and what was
//! left out, with the switches for the tree, line numbers and "include flagged files anyway"; Copy puts the text on the clipboard.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::*;
use leaf_dev::context::{self, Built, Options, Prepared};
use rf_core::format;

use crate::i18n::tx;
use crate::prompt::{self, Button, Check, Kind};

/// How many left-out files the dialog names (the copied text names them all).
const SHOWN_LEFT_OUT: usize = 8;

pub fn start(base: PathBuf, roots: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    let (max_file_kb, max_total_kb) = {
        let s = cx.global::<crate::settings::Settings>();
        (s.context_max_file_kb, s.context_max_total_kb)
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    prompt::show(
        tx("Copy as context"),
        Some(tx("Reading the files…").to_string()),
        None,
        vec![Button::new(crate::t!("Cancel"), Kind::Normal, move |_, _, _| {
            stop.store(true, Ordering::Relaxed);
            Ok(())
        })],
        window,
        cx,
    );
    let Some(waiting) = prompt::current(window, cx).map(|d| d.entity_id()) else { return };
    let (send, receive) = futures::channel::oneshot::channel();
    let flag = cancel.clone();
    std::thread::spawn(move || {
        let _ = send.send(context::prepare(&base, &roots, u64::from(max_file_kb) * 1024, max_total_kb as usize * 1024, &flag));
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Some(prepared)) = receive.await else { return };
        let _ = handle.update(cx, |_, window, cx| {
            // (Escape put the waiting dialog away, or another one took its place: nothing to answer)
            if prompt::current(window, cx).map(|d| d.entity_id()) != Some(waiting) || cancel.load(Ordering::Relaxed) {
                return;
            }
            prompt::close(window, cx);
            show_result(prepared, window, cx);
        });
    })
    .detach();
}

fn show_result(prepared: Prepared, window: &mut Window, cx: &mut App) {
    if prepared.files.is_empty() && prepared.fixed_out.is_empty() {
        return prompt::notice(tx("Copy as context"), tx("There are no files to copy here.").to_string(), window, cx);
    }
    let prepared = Rc::new(prepared);
    let (tree, lines, anyway) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let mut checks = vec![
        Check { label: tx("Tree of the files").into(), note: "".into(), on: tree.clone() },
        Check { label: tx("Line numbers").into(), note: "".into(), on: lines.clone() },
    ];
    if prepared.has_flagged() {
        checks.push(Check { label: tx("Include files that may hold a secret anyway").into(), note: "".into(), on: anyway.clone() });
    }
    // the text is made again only when a switch changed
    let cache: Rc<RefCell<Option<((bool, bool, bool), Rc<Built>)>>> = Rc::new(RefCell::new(None));
    let built = {
        let (prepared, cache) = (prepared.clone(), cache.clone());
        let (tree, lines, anyway) = (tree.clone(), lines.clone(), anyway.clone());
        move || -> Rc<Built> {
            let key = (tree.get(), lines.get(), anyway.get());
            if let Some((k, b)) = cache.borrow().as_ref() {
                if *k == key {
                    return b.clone();
                }
            }
            let opt = Options { tree: key.0, line_numbers: key.1, include_anyway: key.2, ..Options::default() };
            let b = Rc::new(prepared.assemble(&opt));
            *cache.borrow_mut() = Some((key, b.clone()));
            b
        }
    };
    let live: Rc<dyn Fn() -> String> = {
        let built = built.clone();
        Rc::new(move || message(&built()))
    };
    let copy = Button::new(tx("Copy"), Kind::Primary, move |_, _, cx| {
        let b = built();
        if b.included == 0 {
            return Err(tx("Nothing to copy: every file was left out.").to_string());
        }
        cx.write_to_clipboard(ClipboardItem::new_string(b.text.clone()));
        Ok(())
    });
    prompt::show_live(tx("Copy as context"), live, checks, vec![copy, Button::cancel()], window, cx);
}

/// The dialog's text: the counts and the left-out files.
fn message(b: &Built) -> String {
    let mut m = format!("{} {}, {}\n{}", b.included, if b.included == 1 { tx("file") } else { tx("files") }, format::size(b.text.len() as u64), context::tokens_phrase(b.tokens()));
    if let Some(c) = &b.capped {
        m.push_str(&format!("\n\n{c}"));
    }
    if !b.left_out.is_empty() {
        m.push_str(&format!("\n\n{} ({}):", tx("Left out"), b.left_out.len()));
        for l in b.left_out.iter().take(SHOWN_LEFT_OUT) {
            m.push_str(&format!("\n{}: {}", l.rel, l.reason));
        }
        if b.left_out.len() > SHOWN_LEFT_OUT {
            m.push_str(&format!("\n… {} {}", b.left_out.len() - SHOWN_LEFT_OUT, tx("more, named in the copied text")));
        }
    }
    m
}
