//! Editing a text file in the viewer (E in the text viewer): the toolkit's code editor over the file's text, Ctrl+S saves.
//!
//! A save never damages the file: it is written in the encoding, byte order mark and line breaks it had (`rf_core::textedit`,
//! where a file that would not come back byte for byte is not offered at all), into a temp file that is swapped in whole
//! (`leaf_sys::fileops::replace_contents`). A file changed on disk since it was opened, a character its encoding cannot hold, and
//! unsaved edits when the editor is left are all asked about; nothing is saved or thrown away without the user saying so.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::input::{
    Editor, EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputEvent, InputHighlighter, InputHighlighterFactory, Position, Rope, RopeExt as _,
    TabSize,
};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::textedit::{Breaks, TextFormat, Unrepresentable};

use crate::look::config::LookSettings;
use crate::prompt::{self, Button, Kind};
use crate::t;

/// Bigger files stay read-only (the editor keeps the whole text in memory and lays it out).
pub const EDIT_MAX: u64 = 8 << 20;
/// Syntax colours up to this size: the whole text is coloured again after each pause in typing (off the UI thread), which for a
/// bigger file would lag behind the typing.
const HIGHLIGHT_MAX: usize = 1 << 20;

/// Whether the shown file can be edited, and if not, why.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Offer {
    #[default]
    Checking,
    Yes,
    No(SharedString),
}

/// The file's size and modified time when it was read: a save first looks whether it is still the same file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp(u64, Option<SystemTime>);

fn stamp_of(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().map(|m| Stamp(m.len(), m.modified().ok()))
}

/// What keeps a file from being edited before its content is looked at: only a real file on this PC or a mounted drive, not a
/// stream, a shell item or leaf's own copy of an archive's or a server's file (saving there would not reach the original), not
/// bigger than [`EDIT_MAX`], and not read-only.
fn place_refusal(path: &Path) -> Option<SharedString> {
    let s = path.to_string_lossy();
    let copy: SharedString = t!("Only a file on this PC or on a mounted drive can be edited here").into();
    if s.starts_with("::") || crate::look::stream::parse(path).is_some() {
        return Some(copy);
    }
    // leaf's caches (archive items, server files) are folders "leaf-…" in the temp folder
    let temp = std::env::temp_dir().to_string_lossy().to_lowercase();
    let low = s.to_lowercase();
    if let Some(rest) = low.strip_prefix(temp.trim_end_matches(['\\', '/'])) {
        if rest.trim_start_matches(['\\', '/']).starts_with("leaf-") {
            return Some(copy);
        }
    }
    let Ok(m) = std::fs::metadata(path) else { return Some(copy) };
    if !m.is_file() {
        return Some(copy);
    }
    if m.len() > EDIT_MAX {
        return Some(t!("Files over 8 MB are not edited here (Open with… opens another program)").into());
    }
    if m.permissions().readonly() {
        return Some(t!("The file is read-only").into());
    }
    None
}

fn content_refusal(r: rf_core::textedit::Refusal) -> SharedString {
    use rf_core::textedit::Refusal::*;
    match r {
        NotText => t!("The file has binary content"),
        Lossy => t!("The file does not decode cleanly as text, so it could not be saved back exactly as it is"),
        LoneCr => t!("The file has old Mac line breaks (CR only), which the editor would change"),
    }
    .into()
}

/// Off the UI thread when a text file is shown: may it be edited? (The content is read and decoded; nothing is kept.)
pub fn check(path: &Path) -> Offer {
    if let Some(why) = place_refusal(path) {
        return Offer::No(why);
    }
    // (a file on a network drive is not read a second time just for this: E reads it and says then if it can't be edited)
    if leaf_sys::drives::slow_to_read(path) {
        return Offer::Yes;
    }
    match std::fs::read(path) {
        Ok(bytes) => match rf_core::textedit::open(&bytes) {
            Ok(_) => Offer::Yes,
            Err(r) => Offer::No(content_refusal(r)),
        },
        Err(e) => Offer::No(e.to_string().into()),
    }
}

/// The file read for editing.
pub struct Loaded {
    text: String,
    format: TextFormat,
    stamp: Stamp,
}

/// Reads the file for editing (on E, with every test again: the file may have changed since it was shown). Opening it for writing
/// tells at once whether Windows lets this user change it, and whether another program holds it.
pub fn load(path: &Path) -> Result<Loaded, SharedString> {
    if let Some(why) = place_refusal(path) {
        return Err(why);
    }
    // (a file this user may not change can still be edited when leaf can save it as administrator: the save asks first)
    if let Err(e) = std::fs::OpenOptions::new().write(true).open(path).or_else(|e| if crate::admin::denied_io(&e) { Ok(std::fs::File::open(path)?) } else { Err(e) }) {
        return Err(match e.kind() {
            std::io::ErrorKind::PermissionDenied => t!("You don't have permission to change this file").into(),
            _ if e.raw_os_error() == Some(32) => t!("Another program is using the file").into(),
            _ => e.to_string().into(),
        });
    }
    let stamp = stamp_of(path).ok_or_else(|| SharedString::from(t!("The file is no longer there")))?;
    let bytes = std::fs::read(path).map_err(|e| SharedString::from(e.to_string()))?;
    let (text, format) = rf_core::textedit::open(&bytes).map_err(content_refusal)?;
    Ok(Loaded { text, format, stamp })
}

/// Sent when editing is over (left with Esc, the edits saved or thrown away): the text viewer shows the file read-only again.
pub struct Leave;

type Then = Box<dyn FnOnce(&mut Window, &mut App)>;

pub struct EditView {
    path: PathBuf,
    state: Entity<EditorState>,
    format: TextFormat,
    stamp: Stamp,
    /// The text on disk, to tell whether there are unsaved edits (undoing every edit makes the file clean again).
    saved: Rope,
    dirty: bool,
    /// A short note in the status line ("Saved") and the task that takes it away.
    note: Option<SharedString>,
    _note_task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Leave> for EditView {}

fn is_dark(cx: &App) -> bool {
    matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

impl EditView {
    pub fn new(path: PathBuf, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = cx.global::<LookSettings>();
        let (numbers, wrap) = (s.look_text_line_numbers, s.look_text_wrap);
        // a file indented with tabs gets tabs from the Tab key, the others four spaces
        let hard_tabs = loaded.text.lines().take(2000).any(|l| l.starts_with('\t'));
        let colours = loaded.text.len() <= HIGHLIGHT_MAX;
        let (dark, hl_path) = (is_dark(cx), path.clone());
        let ext = crate::look::viewer::Probe::ext_of(&path);
        let text = loaded.text;
        let state = cx.new(|cx| {
            let mut st = EditorState::new(window, cx)
                .language(if ext.is_empty() { "text".to_string() } else { ext })
                .line_number(numbers)
                .soft_wrap(wrap)
                .folding(false)
                .auto_close(false)
                .tab_size(TabSize { tab_size: 4, hard_tabs });
            if colours {
                st.set_highlighter_factory(highlighter_factory(hl_path, dark), cx);
            }
            st.set_value(text, window, cx);
            st
        });
        let saved = state.read(cx).text().clone();
        let subs = vec![
            cx.subscribe_in(&state, window, |this: &mut Self, state, ev: &InputEvent, window, cx| {
                if let InputEvent::Change = ev {
                    let dirty = {
                        let text = state.read(cx).text();
                        text.len() != this.saved.len() || *text != this.saved
                    };
                    this.set_dirty(dirty, window, cx);
                }
            }),
            // the status line's line and column follow the cursor
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        state.update(cx, |s, cx| s.focus(window, cx));
        Self { path, state, format: loaded.format, stamp: loaded.stamp, saved, dirty: false, note: None, _note_task: None, _subs: subs }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The window's bar shows the unsaved-edits dot too, so the whole window is drawn again when that changes.
    fn set_dirty(&mut self, dirty: bool, window: &mut Window, cx: &mut Context<Self>) {
        if dirty != self.dirty {
            self.dirty = dirty;
            window.refresh();
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Alt+L / Alt+Z changed the viewer's settings: the editor follows them.
    pub fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = cx.global::<LookSettings>();
        let (numbers, wrap) = (s.look_text_line_numbers, s.look_text_wrap);
        self.state.update(cx, |st, cx| {
            st.set_line_number(numbers, window, cx);
            st.set_soft_wrap(wrap, window, cx);
        });
    }

    fn name(&self) -> String {
        super::file_name(&self.path)
    }

    fn say(&mut self, note: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.note = Some(note.into());
        self._note_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |v, cx| {
                v.note = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Esc: back to the read-only view, asking first when there are unsaved edits.
    pub fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        self.ask_to_leave(
            Box::new(move |_, cx| {
                let _ = me.update(cx, |_, cx| cx.emit(Leave));
            }),
            window,
            cx,
        );
    }

    /// Unsaved edits: Save, Don't save, or Cancel. `go` runs after a save that worked or when the edits are given up; Cancel (or
    /// a save that failed) runs nothing.
    pub fn ask_to_leave(&mut self, go: Then, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            return window.defer(cx, go);
        }
        let go = Rc::new(Cell::new(Some(go)));
        let me = cx.weak_entity();
        let save = {
            let (me, go) = (me.clone(), go.clone());
            move |_: Option<String>, window: &mut Window, cx: &mut App| {
                // (after the dialog has closed: the save may ask a question of its own)
                let (me, go) = (me.clone(), go.clone());
                window.defer(cx, move |window, cx| {
                    let _ = me.update(cx, |v, cx| v.save_then(go.take(), window, cx));
                });
                Ok(())
            }
        };
        let discard = move |_: Option<String>, window: &mut Window, cx: &mut App| {
            let _ = me.update(cx, |v, cx| v.set_dirty(false, window, cx));
            if let Some(go) = go.take() {
                window.defer(cx, go);
            }
            Ok(())
        };
        prompt::show(
            t!("Save the changes?"),
            Some(format!("\"{}\" {}", self.name(), t!("has changes that are not saved."))),
            None,
            vec![Button::new(t!("Save"), Kind::Primary, save), Button::new(t!("Don't save"), Kind::Danger, discard), Button::cancel()],
            window,
            cx,
        );
    }

    /// Ctrl+S.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_then(None, window, cx);
    }

    fn save_then(&mut self, then: Option<Then>, window: &mut Window, cx: &mut Context<Self>) {
        // changed on disk by someone else since it was read (or deleted): asked, and Cancel is the key's answer
        if stamp_of(&self.path) != Some(self.stamp) {
            let me = cx.weak_entity();
            let then = Rc::new(Cell::new(then));
            let overwrite = {
                let me = me.clone();
                move |_: Option<String>, window: &mut Window, cx: &mut App| {
                    let (me, then) = (me.clone(), then.clone());
                    window.defer(cx, move |window, cx| {
                        let _ = me.update(cx, |v, cx| v.encode_and_write(then.take(), window, cx));
                    });
                    Ok(())
                }
            };
            let reload = move |_: Option<String>, window: &mut Window, cx: &mut App| {
                let me = me.clone();
                window.defer(cx, move |window, cx| {
                    let _ = me.update(cx, |v, cx| v.reload_from_disk(window, cx));
                });
                Ok(())
            };
            return prompt::show(
                t!("The file changed on disk"),
                Some(format!("\"{}\" {}", self.name(), t!("was changed (or removed) by something else after it was opened here. Overwrite it with your version, or reload it and lose your edits?"))),
                None,
                vec![Button::cancel(), Button::new(t!("Reload"), Kind::Normal, reload), Button::new(t!("Overwrite"), Kind::Normal, overwrite)],
                window,
                cx,
            );
        }
        self.encode_and_write(then, window, cx);
    }

    fn encode_and_write(&mut self, then: Option<Then>, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.state.read(cx).value();
        match self.format.encode(&text) {
            Ok((bytes, next)) => self.write(bytes, next, then, window, cx),
            Err(Unrepresentable(c)) => {
                let me = cx.weak_entity();
                let then = Rc::new(Cell::new(then));
                let as_utf8 = move |_: Option<String>, window: &mut Window, cx: &mut App| {
                    let (me, then) = (me.clone(), then.clone());
                    window.defer(cx, move |window, cx| {
                        let _ = me.update(cx, |v, cx| {
                            let text = v.state.read(cx).value();
                            let (bytes, next) = v.format.encode_utf8(&text);
                            v.write(bytes, next, then.take(), window, cx);
                        });
                    });
                    Ok(())
                };
                prompt::show(
                    t!("Save as UTF-8?"),
                    Some(format!(
                        "{} {} (\"{c}\", U+{:04X}). {}",
                        self.format.encoding_label(),
                        t!("cannot hold a character in the text"),
                        c as u32,
                        t!("Save the file as UTF-8 instead, or cancel and change the text.")
                    )),
                    None,
                    vec![Button::new(t!("Save as UTF-8"), Kind::Primary, as_utf8), Button::cancel()],
                    window,
                    cx,
                );
            }
        }
    }

    fn write(&mut self, bytes: Vec<u8>, next: TextFormat, then: Option<Then>, window: &mut Window, cx: &mut Context<Self>) {
        let r = if self.path.exists() {
            if std::fs::metadata(&self.path).is_ok_and(|m| m.permissions().readonly()) {
                return prompt::notice(t!("Not saved"), format!("\"{}\" {}", self.name(), t!("is read-only. Nothing was changed.")), window, cx);
            }
            leaf_sys::fileops::replace_contents(&self.path, &bytes)
        } else {
            // removed meanwhile and the user said overwrite: made again
            (|| {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&self.path)?;
                f.write_all(&bytes)?;
                f.sync_all()
            })()
        };
        if let Err(e) = r {
            // refused with "access denied": the same bytes can be saved again as administrator (after the click and the prompt)
            if crate::admin::denied_io(&e) {
                return self.offer_admin_save(bytes, next, then, window, cx);
            }
            let why = match e.kind() {
                std::io::ErrorKind::PermissionDenied => t!("Access was denied (no permission, or another program holds the file).").to_string(),
                _ => e.to_string(),
            };
            log::warn!("look edit: save {}: {e}", self.path.display());
            return prompt::notice(t!("Not saved"), format!("{why} {}", t!("The file on disk was not changed.")), window, cx);
        }
        self.saved_now(bytes.len(), next, then, window, cx);
    }

    fn saved_now(&mut self, len: usize, next: TextFormat, then: Option<Then>, window: &mut Window, cx: &mut Context<Self>) {
        self.format = next;
        self.stamp = stamp_of(&self.path).unwrap_or(Stamp(len as u64, None));
        self.saved = self.state.read(cx).text().clone();
        self.set_dirty(false, window, cx);
        self.say(t!("Saved"), cx);
        if let Some(then) = then {
            window.defer(cx, then);
        }
    }

    /// "Retry as administrator" for a save Windows refused: the bytes go through leaf's administrator helper.
    fn offer_admin_save(&mut self, bytes: Vec<u8>, next: TextFormat, then: Option<Then>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.weak_entity();
        let path = self.path.clone();
        let bytes = std::sync::Arc::new(bytes);
        let then = Rc::new(Cell::new(then));
        let handle = window.window_handle();
        let what = format!("\"{}\": {}", self.name(), t!("access was denied. The file on disk was not changed."));
        crate::admin::offer(
            t!("Not saved").to_string(),
            what,
            move |cx| {
                let (me, path, bytes, then, next) = (me.clone(), path.clone(), bytes.clone(), then.clone(), next.clone());
                let b = bytes.clone();
                let task = cx.background_spawn(async move { crate::admin::write_file(&path, &b) });
                cx.spawn(async move |cx| {
                    let r = task.await;
                    let _ = handle.update(cx, |_, window, cx| match r {
                        Ok(()) => {
                            let _ = me.update(cx, |v, cx| v.saved_now(bytes.len(), next, then.take(), window, cx));
                        }
                        Err(e) => prompt::notice(t!("Not saved"), format!("{e:#}
{}", t!("The file on disk was not changed.")), window, cx),
                    });
                })
                .detach();
            },
            window,
            cx,
        );
    }

    /// "Reload" after the file changed on disk: its new content replaces the edits (the user chose that).
    fn reload_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match load(&self.path) {
            Ok(l) => {
                self.state.update(cx, |s, cx| s.set_value(l.text, window, cx));
                self.format = l.format;
                self.stamp = l.stamp;
                self.saved = self.state.read(cx).text().clone();
                self.set_dirty(false, window, cx);
                self.say(t!("Reloaded"), cx);
            }
            Err(why) => {
                // (no longer editable: back to the read-only view, which says what is there now)
                self.set_dirty(false, window, cx);
                prompt::notice(t!("Can't edit the file now"), why.to_string(), window, cx);
                cx.emit(Leave);
            }
        }
    }

    /// Ctrl+G: asks for a line and puts the cursor there.
    pub fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let lines = self.state.read(cx).text().lines_len().max(1);
        let go = move |v: Option<String>, window: &mut Window, cx: &mut App| {
            let n: usize = v.as_deref().map(str::trim).and_then(|s| s.parse().ok()).filter(|n| (1..=lines).contains(n)).ok_or_else(|| format!("{} 1–{lines}", t!("A line number:")))?;
            let state = state.clone();
            window.defer(cx, move |window, cx| state.update(cx, |s, cx| s.set_cursor_position(Position::new(n as u32 - 1, 0), window, cx)));
            Ok(())
        };
        prompt::show(t!("Go to line"), None, Some((String::new(), format!("1–{lines}"))), vec![Button::new(t!("Go"), Kind::Primary, go), Button::cancel()], window, cx);
    }
}

impl Render for EditView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::theme::Palette::for_window(window);
        let s = cx.global::<LookSettings>();
        let font_size = s.look_text_font_size.clamp(6.0, 72.0);
        let font: SharedString = if s.look_text_font.trim().is_empty() { crate::platform::fonts::mono().into() } else { s.look_text_font.clone().into() };
        let pos = self.state.read(cx).cursor_position();
        let breaks = match self.format.breaks() {
            Breaks::CrLf => "CRLF",
            Breaks::Lf => "LF",
            Breaks::Mixed => t!("Mixed line breaks"),
            Breaks::None => if cfg!(windows) { "CRLF" } else { "LF" },
        };
        let dirty = self.dirty;
        v_flex()
            .size_full()
            .bg(p.content_bg)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Editor::new(&self.state).appearance(false).bordered(false).h(relative(1.)).font_family(font).text_size(px(font_size))),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(p.border)
                    .bg(p.status_bg)
                    .text_xs()
                    .text_color(p.status_text_secondary)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_color(if dirty { p.accent } else { p.status_text_secondary })
                            .child(div().size(px(7.)).rounded_full().when(dirty, |d| d.bg(p.accent)).when(!dirty, |d| d.border_1().border_color(p.status_text_secondary)))
                            .child(if dirty { t!("Modified") } else { t!("Editing") }),
                    )
                    .when_some(self.note.clone(), |d, n| d.child(div().text_color(p.status_text).child(n)))
                    .child(div().flex_1())
                    .child(format!("{} {}, {} {}", t!("Ln"), pos.line + 1, t!("Col"), pos.character + 1))
                    .child(self.format.encoding_label())
                    .child(breaks)
                    .child(div().text_color(p.text_tertiary).child(t!("Ctrl+S save · Ctrl+F find · Ctrl+H replace · Ctrl+G line · Esc done"))),
            )
    }
}

// ----- syntax colours: the read-only view's syntect colouring, fed to the editor -----

/// The editor's highlighter for `path`: the same colours as the read-only view (`text::syntax_highlight`), worked out off the UI
/// thread after a short pause in typing; until then the colours of the text that did not change move with it.
fn highlighter_factory(path: PathBuf, dark: bool) -> InputHighlighterFactory {
    Rc::new(move |_language: &str| {
        Some(Box::new(LookHighlighter { path: path.clone(), dark, spans: Rc::default(), task: None }) as Box<dyn InputHighlighter>)
    })
}

struct LookHighlighter {
    path: PathBuf,
    dark: bool,
    /// Colour runs over the whole text (byte ranges, ordered, not overlapping; 0xRRGGBB).
    spans: Rc<RefCell<Vec<(Range<usize>, u32)>>>,
    task: Option<Task<()>>,
}

/// The read-only view's per-line colours as ranges over the whole text.
fn whole_text_spans(path: &Path, text: &str, dark: bool) -> Option<Vec<(Range<usize>, u32)>> {
    let lines = super::text::syntax_highlight(path, text, dark)?;
    let mut out = Vec::new();
    let mut at = 0;
    for (line, spans) in text.split_inclusive('\n').zip(lines) {
        out.extend(spans.into_iter().map(|(r, c)| (at + r.start..at + r.end, c)));
        at += line.len();
    }
    Some(out)
}

impl InputHighlighter for LookHighlighter {
    fn language(&self) -> SharedString {
        "look".into()
    }

    fn update(&mut self, edit: Option<InputEdit>, text: &Rope, _folding: bool, window: &mut Window, cx: &mut Context<EditorState>) {
        if let Some(e) = edit {
            // the runs after the edit move by its length, the ones it touched go until the new colours come
            let delta = e.new_end_byte as isize - e.old_end_byte as isize;
            self.spans.borrow_mut().retain_mut(|(r, _)| {
                if r.end <= e.start_byte {
                    true
                } else if r.start >= e.old_end_byte {
                    r.start = r.start.saturating_add_signed(delta);
                    r.end = r.end.saturating_add_signed(delta);
                    true
                } else {
                    false
                }
            });
        }
        let (path, dark, spans, text) = (self.path.clone(), self.dark, self.spans.clone(), text.clone());
        let pause = edit.is_some();
        // (a new edit replaces the task, which drops the colouring still waiting)
        self.task = Some(cx.spawn_in(window, async move |state, cx| {
            if pause {
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }
            let done = cx.background_executor().spawn(async move { whole_text_spans(&path, &text.to_string(), dark) }).await;
            if let Some(done) = done {
                *spans.borrow_mut() = done;
                let _ = state.update(cx, |_, cx| cx.notify());
            }
        }));
    }

    fn styles(&self, range: &Range<usize>, _resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        let spans = self.spans.borrow();
        let first = spans.partition_point(|(r, _)| r.end <= range.start);
        let mut runs = Vec::new();
        let mut at = range.start;
        for (r, c) in &spans[first..] {
            if r.start >= range.end {
                break;
            }
            let (start, end) = (r.start.max(at), r.end.min(range.end));
            if start >= end {
                continue;
            }
            if at < start {
                runs.push((at..start, HighlightStyle::default()));
            }
            runs.push((start..end, HighlightStyle { color: Some(rgb(*c).into()), ..Default::default() }));
            at = end;
        }
        if at < range.end {
            runs.push((at..range.end, HighlightStyle::default()));
        }
        runs
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}
