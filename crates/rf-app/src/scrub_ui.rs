//! "Remove metadata": a window that lists the selected files, can show what each one carries without changing anything ("Show
//! metadata"), and removes it into cleaned copies beside the originals (`name.clean.ext`, the default) or in place of the originals
//! (they go to the Recycle Bin first, as one undoable operation). The removal itself is `leaf_scrub`; this file is the face: the
//! options, the Status Center job (progress, cancel) and the result sheet (what was removed per file, what could not be and why).

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::t;
use crate::theme::{self, Palette};

/// Kinds of file the right-click menu offers the command for (the command itself takes any file and says what it cannot do).
const KINDS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "tif", "tiff", "heic", "heif", "avif", "gif", "pdf", "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "odt", "ods", "odp",
    "mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "mp4", "m4v", "mov", "mkv", "avi", "webm", "svg", "bmp", "ico", "dng", "cr2", "cr3", "nef", "arw",
];

/// The selection holds a file this command is for (cheap: for the right-click menu).
pub fn offers(items: &[PathBuf]) -> bool {
    items.iter().take(200).any(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| KINDS.iter().any(|k| k.eq_ignore_ascii_case(e))) && p.is_file())
}

#[derive(Clone, Copy, Default)]
struct RunOpts {
    remove_profile: bool,
    keep_cover: bool,
    reset_times: bool,
    neutral: bool,
    replace: bool,
    remove_signatures: bool,
    keep_subtitles: bool,
    keep_previews: bool,
}

impl RunOpts {
    fn lib(&self) -> leaf_scrub::Options {
        leaf_scrub::Options {
            keep_color_profile: !self.remove_profile,
            keep_cover_art: self.keep_cover,
            reset_times: self.reset_times,
            remove_signatures: self.remove_signatures,
            keep_subtitles: self.keep_subtitles,
            keep_previews: self.keep_previews,
        }
    }
}

/// What the sheet says about one file.
#[derive(Clone)]
struct FileResult {
    name: String,
    format: String,
    /// The file that was written (a cleaned copy, or the file now in the original's place).
    output: Option<PathBuf>,
    removed: Vec<(String, String)>,
    kept: Vec<(String, String)>,
    notes: Vec<String>,
    /// Why it was not cleaned (leaf's wording) and the technical reason.
    error: Option<(String, String)>,
    /// "Show metadata": nothing was changed.
    dry: bool,
    /// The file name itself looks like it carries a date or a device name (the content cleaning cannot change it).
    name_note: Option<String>,
    /// The contents were read and cleaned (false: a kind of file leaf does not read, only its alternate streams went). Only
    /// these count as "cleaned".
    contents_read: bool,
}

/// The sheet's note about a file NAME, unless the neutral-name option is ticked already.
fn name_note(p: &Path, o: &RunOpts) -> Option<String> {
    let n = p.file_name()?.to_string_lossy();
    (!o.neutral && leaf_scrub::name_looks_personal(&n)).then(|| t!("The file name looks like it carries a date or a device name. Tick “Give the files a neutral name” to replace it.").to_string())
}

/// What the "Verify" button found in one cleaned file: what the lister still finds (empty: nothing left).
#[derive(Clone)]
struct Checked {
    name: String,
    left: Vec<(String, String)>,
    error: Option<String>,
}

fn verify_one(r: &FileResult, o: &RunOpts) -> Option<Checked> {
    let out = r.output.as_ref().filter(|_| r.error.is_none())?;
    let name = name_of(out);
    // the lister, and the structural walk of the second look (anything not on the format's allow-list)
    let listed = leaf_scrub::inspect(out, &o.lib()).map(|i| pairs(&i.to_remove().cloned().collect::<Vec<_>>()));
    let walked = leaf_scrub::leftovers(out, &o.lib());
    Some(match (listed, walked) {
        (Ok(mut left), Ok(more)) => {
            left.extend(more.into_iter().map(|m| (t!("Not on the list of what the format needs").to_string(), m)));
            Checked { name, left, error: None }
        }
        (Err(e), _) | (_, Err(e)) => Checked { name, left: vec![], error: Some(e.to_string()) },
    })
}

fn pairs(v: &[leaf_scrub::Finding]) -> Vec<(String, String)> {
    v.iter().map(|f| (f.label.clone(), f.value.clone())).collect()
}

fn name_of(p: &Path) -> String {
    rf_core::history::display_name(p)
}

fn dry_result(p: &Path, o: &RunOpts) -> FileResult {
    let name = name_of(p);
    match leaf_scrub::inspect(p, &o.lib()) {
        Ok(i) => {
            let refusal = (!i.can_clean).then(|| (t!("This kind of file cannot be cleaned").to_string(), i.refusal.clone().unwrap_or_default()));
            let (removed, kept): (Vec<_>, Vec<_>) = i.findings.iter().cloned().partition(|f| f.remove);
            FileResult { name, format: i.format, output: None, removed: pairs(&removed), kept: pairs(&kept), notes: i.notes, error: refusal, dry: true, name_note: name_note(p, o), contents_read: false }
        }
        Err(e) => FileResult { name, format: String::new(), output: None, removed: vec![], kept: vec![], notes: vec![], error: Some((e.to_string(), e.details())), dry: true, name_note: name_note(p, o), contents_read: false },
    }
}

/// One file of a run. `put_away` moves an original out of its place (replace mode); it gives back where it went.
fn run_one(
    src: &Path,
    index: usize,
    o: &RunOpts,
    cancel: &dyn Fn() -> bool,
    put_away: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<leaf_scrub::Outcome, leaf_scrub::ScrubError> {
    let dir = src.parent().unwrap_or(Path::new("."));
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let neutral = || leaf_scrub::neutral_name(dir, &ext, index + 1);
    let lo = o.lib();
    if o.replace && !crate::sync_ui::bin_takes(src) {
        // a drive without a Recycle Bin / Trash (stick, card, network share), a bin that is off or too small for the file: the
        // original could not be kept, and the system would ask whether to delete it for good. It is never asked: the cleaned
        // copy is saved beside the original instead, and the sheet says why.
        let dst = if o.neutral { neutral() } else { leaf_scrub::clean_copy_name(src) };
        let mut oc = leaf_scrub::clean_to(src, &dst, &lo, cancel)?;
        oc.notes.insert(0, t!("Not replaced: this drive has no Recycle Bin that can take the file, so the original could not be kept. The cleaned copy is saved beside it.").to_string());
        oc.undoable = false;
        return Ok(oc);
    }
    if o.replace {
        let target = if o.neutral { neutral() } else { src.to_path_buf() };
        leaf_scrub::replace(src, &target, &lo, cancel, put_away)
    } else {
        let dst = if o.neutral { neutral() } else { leaf_scrub::clean_copy_name(src) };
        leaf_scrub::clean_to(src, &dst, &lo, cancel)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Setup,
    Inspecting,
    Running,
    Done,
}

pub struct ScrubView {
    files: Vec<PathBuf>,
    opts: RunOpts,
    phase: Phase,
    results: Vec<FileResult>,
    /// The sheet shows what a run did (not a "Show metadata" listing).
    ran: bool,
    /// The options of the run that produced `results` (the Verify button reads the cleaned files with the same ones).
    ran_opts: RunOpts,
    /// What "Verify" found, per cleaned file; `None` until the button is pressed.
    verified: Option<Vec<Checked>>,
    verifying: bool,
    focus: FocusHandle,
    frame_dark: Option<bool>,
}

/// Opens the window for `files` (one window per call), on the display of the window in front.
pub fn open(files: Vec<PathBuf>, cx: &mut App) {
    let files: Vec<PathBuf> = files.into_iter().filter(|p| p.is_file()).collect();
    if files.is_empty() {
        return;
    }
    // on pictures and videos the modified time is often the time the photo was taken: "set the file times to now" starts ticked
    const TIMED: &[&str] = &["jpg", "jpeg", "png", "webp", "tif", "tiff", "heic", "heif", "avif", "gif", "mp4", "m4v", "mov", "mkv", "avi", "webm"];
    let timed = files.iter().any(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| TIMED.iter().any(|k| k.eq_ignore_ascii_case(e))));
    let display = cx.active_window().and_then(|w| w.update(cx, |_, w, cx| w.display(cx).map(|d| d.id())).ok().flatten());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(display, size(px(560.), px(680.)), cx))),
        display_id: display,
        window_min_size: Some(size(px(440.), px(420.))),
        window_background: crate::theme::window_material(WindowBackgroundAppearance::MicaBackdrop),
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    if let Err(e) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::glass::keep_out_of_capture(crate::look::window::hwnd_of(window));
        cx.new(|cx| {
            let v = ScrubView {
                files,
                opts: RunOpts { reset_times: timed, ..RunOpts::default() },
                phase: Phase::Setup,
                results: Vec::new(),
                ran: false,
                ran_opts: RunOpts::default(),
                verified: None,
                verifying: false,
                focus: cx.focus_handle(),
                frame_dark: None,
            };
            window.focus(&v.focus, cx);
            v
        })
    }) {
        log::error!("remove metadata window: {e:#}");
    }
}

impl ScrubView {
    /// "Show metadata": reads every file and lists what it carries; nothing is changed.
    fn show_metadata(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Inspecting || self.phase == Phase::Running {
            return;
        }
        self.phase = Phase::Inspecting;
        let (files, o) = (self.files.clone(), self.opts);
        let task = cx.background_spawn(async move { files.iter().map(|p| dry_result(p, &o)).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.results = r;
                v.ran = false;
                v.verified = None;
                v.phase = Phase::Done;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Verify": reads every cleaned file again with the lister, the way "Show metadata" does, and shows what is left per file.
    fn verify(&mut self, cx: &mut Context<Self>) {
        if self.verifying || !self.ran || self.phase != Phase::Done {
            return;
        }
        self.verifying = true;
        let (results, o) = (self.results.clone(), self.ran_opts);
        let task = cx.background_spawn(async move { results.iter().filter_map(|r| verify_one(r, &o)).collect::<Vec<_>>() });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.verified = Some(r);
                v.verifying = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Remove metadata": a Status Center job (progress, cancel); the result lands in this window.
    fn run(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Inspecting || self.phase == Phase::Running {
            return;
        }
        self.phase = Phase::Running;
        let (files, o) = (self.files.clone(), self.opts);
        let view = cx.entity().downgrade();
        let results: Arc<Mutex<Vec<FileResult>>> = Arc::default();
        let results_out = results.clone();
        let title = format!("{} {}", t!("Removing metadata:"), crate::i18n::items(files.len()));
        let on_done: crate::status_center::OnDone = Box::new(move |_, cx| {
            let r = std::mem::take(&mut *results_out.lock().unwrap());
            let _ = view.update(cx, |v, cx| {
                v.results = r;
                v.ran = true;
                v.ran_opts = o;
                v.verified = None;
                v.phase = Phase::Done;
                cx.notify();
            });
        });
        let id = crate::status_center::start_with_id(cx, title, move |t| work(&files, &o, &t, &results), Some(on_done));
        if id == 0 {
            self.phase = Phase::Setup;
        }
        cx.notify();
    }
}

/// The job: every file in turn; what happened to each goes into `out`. `Ok(false)` when it was cancelled.
fn work(files: &[PathBuf], o: &RunOpts, t: &Arc<leaf_sys::fileops::Tracker>, out: &Arc<Mutex<Vec<FileResult>>>) -> anyhow::Result<bool> {
    let cancelled = || t.cancel.load(Ordering::Relaxed);
    let mut undo = Vec::new();
    let total = files.len() as u32;
    let mut stopped = false;
    for (i, src) in files.iter().enumerate() {
        if cancelled() {
            stopped = true;
            break;
        }
        (t.on_progress)(total, i as u32, Some(name_of(src)));
        let bin: Mutex<Option<PathBuf>> = Mutex::new(None);
        let put_away = |p: &Path| -> Result<(), String> {
            // (the tracker keeps what each earlier file's delete reported: this file's pair is the only one asked for)
            t.results.lock().unwrap().clear();
            let pairs = crate::status_center::trash_for_revert(p, t)?;
            *bin.lock().unwrap() = pairs.first().map(|(_, b)| b.clone());
            Ok(())
        };
        let name = name_of(src);
        let r = match run_one(src, i, o, &cancelled, &put_away) {
            Ok(oc) => {
                if o.replace && oc.undoable {
                    if let Some(b) = bin.lock().unwrap().clone() {
                        undo.push((oc.output.clone(), b, leaf_dev::review::Stamp::of_file(&oc.output)));
                    }
                }
                FileResult { name, format: oc.format, output: Some(oc.output), removed: pairs(&oc.removed), kept: pairs(&oc.kept), notes: oc.notes, error: None, dry: false, name_note: name_note(src, o), contents_read: oc.contents_read }
            }
            Err(leaf_scrub::ScrubError::Cancelled) => {
                stopped = true;
                FileResult { name, format: String::new(), output: None, removed: vec![], kept: vec![], notes: vec![], error: Some((t!("Cancelled, left untouched").to_string(), String::new())), dry: false, name_note: None, contents_read: false }
            }
            Err(e) => FileResult { name, format: String::new(), output: None, removed: vec![], kept: vec![], notes: vec![], error: Some((e.to_string(), e.details())), dry: false, name_note: None, contents_read: false },
        };
        out.lock().unwrap().push(r);
        if stopped {
            break;
        }
    }
    (t.on_progress)(total, total, None);
    if !undo.is_empty() {
        crate::undo::push(crate::undo::UndoOp::Scrubbed { items: undo });
    }
    Ok(!stopped)
}

fn check(p: &Palette, id: &'static str, label: &'static str, on: bool, click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    h_flex()
        .id(id)
        .gap_3()
        .py(px(5.))
        .items_center()
        .text_sm()
        .cursor_pointer()
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .when(on, |d| d.bg(p.accent))
                .when(!on, |d| d.border_1().border_color(p.text_secondary))
                .when(on, |d| d.child(Icon::new(L::Check).size(px(12.)).text_color(p.accent_text))),
        )
        .child(div().flex_1().min_w_0().child(label))
        .on_click(click)
        .into_any_element()
}

fn radio(p: &Palette, id: &'static str, label: &'static str, on: bool, click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
    h_flex()
        .id(id)
        .gap_3()
        .py(px(5.))
        .items_center()
        .text_sm()
        .cursor_pointer()
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(if on { p.accent } else { p.text_secondary })
                .when(on, |d| d.child(div().size(px(10.)).rounded_full().bg(p.accent))),
        )
        .child(div().flex_1().min_w_0().child(label))
        .on_click(click)
        .into_any_element()
}

fn card(p: &Palette, rows: Vec<AnyElement>) -> AnyElement {
    v_flex().w_full().px_4().py_2().rounded(theme::RADIUS).bg(p.card_bg).border_1().border_color(p.border).children(rows).into_any_element()
}

fn small(p: &Palette, text: String) -> AnyElement {
    div().text_xs().text_color(p.text_secondary).child(text).into_any_element()
}

/// How many findings one file's list shows before it says "and N more".
const SHOWN: usize = 60;

fn finding_rows(p: &Palette, items: &[(String, String)], out: &mut Vec<AnyElement>) {
    for (k, v) in items.iter().take(SHOWN) {
        out.push(
            h_flex()
                .w_full()
                .gap_3()
                .items_start()
                .text_xs()
                .child(div().w(px(170.)).flex_shrink_0().overflow_hidden().text_ellipsis().text_color(p.text_secondary).child(k.clone()))
                .child(div().flex_1().min_w_0().child(v.clone()))
                .into_any_element(),
        );
    }
    if items.len() > SHOWN {
        out.push(small(p, format!("{} {}", t!("… and"), items.len() - SHOWN)));
    }
}

/// The result of "Verify": per cleaned file, "nothing left" or what the lister still finds.
fn verify_card(p: &Palette, checked: &[Checked]) -> AnyElement {
    let mut rows: Vec<AnyElement> = vec![div().pt_1().pb(px(2.)).text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Check of the cleaned files")).into_any_element()];
    if checked.is_empty() {
        rows.push(small(p, t!("No cleaned file to check.").to_string()));
    }
    for c in checked {
        let (icon, color, text) = match (&c.error, c.left.is_empty()) {
            (Some(e), _) => (L::CircleAlert, p.danger, format!("{}: {e}", t!("could not be read"))),
            (None, true) => (L::CircleCheck, p.accent, t!("Read again: nothing leaf looks for was found").to_string()),
            (None, false) => (L::CircleAlert, p.danger, format!("{}: {}", t!("still found"), c.left.len())),
        };
        rows.push(
            h_flex()
                .gap_2()
                .items_center()
                .text_sm()
                .child(Icon::new(icon).size(px(14.)).text_color(color).flex_shrink_0())
                .child(div().flex_1().min_w_0().truncate().child(c.name.clone()))
                .child(div().text_xs().text_color(color).child(text))
                .into_any_element(),
        );
        finding_rows(p, &c.left, &mut rows);
    }
    card(p, rows)
}

fn file_card(p: &Palette, r: &FileResult, ran: bool) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    let head = if r.format.is_empty() { r.name.clone() } else { format!("{} · {}", r.name, r.format) };
    let (icon, color) = match (&r.error, r.dry) {
        (Some(_), _) => (L::CircleAlert, p.danger),
        (None, true) => (L::Info, p.text_secondary),
        (None, false) if !r.contents_read => (L::Info, p.text_secondary),
        (None, false) => (L::CircleCheck, p.accent),
    };
    rows.push(
        h_flex()
            .gap_2()
            .items_center()
            .pt_1()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child(Icon::new(icon).size(px(14.)).text_color(color).flex_shrink_0())
            .child(div().flex_1().min_w_0().truncate().child(head))
            .into_any_element(),
    );
    if let Some((msg, why)) = &r.error {
        rows.push(div().text_xs().text_color(p.danger).child(msg.clone()).into_any_element());
        if !why.is_empty() && why != msg {
            rows.push(small(p, why.clone()));
        }
    } else if let Some(out) = &r.output {
        let line = match (ran, out.file_name()) {
            (true, Some(n)) => format!("{} {}", t!("Written as"), n.to_string_lossy()),
            _ => String::new(),
        };
        if !line.is_empty() {
            rows.push(small(p, line));
        }
    }
    if r.dry && r.removed.is_empty() && r.error.is_none() {
        rows.push(small(p, t!("Nothing from the list below was found. This is not a promise that the file holds no metadata.").to_string()));
    }
    if !r.removed.is_empty() {
        let title = if r.dry { t!("Found (a run removes it)") } else { t!("Removed") };
        rows.push(div().pt_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(format!("{title}: {}", r.removed.len())).into_any_element());
        finding_rows(p, &r.removed, &mut rows);
    }
    if !r.kept.is_empty() {
        rows.push(div().pt_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Kept on purpose")).into_any_element());
        finding_rows(p, &r.kept, &mut rows);
    }
    for n in &r.notes {
        rows.push(small(p, n.clone()));
    }
    if let Some(n) = &r.name_note {
        rows.push(small(p, n.clone()));
    }
    card(p, rows)
}

impl Render for ScrubView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        if self.frame_dark != Some(p.dark) {
            self.frame_dark = Some(p.dark);
            let h = crate::look::window::hwnd_of(window);
            if h != 0 {
                leaf_sys::window::set_dark_mode(h, p.dark);
            }
        }
        window.set_window_title(t!("Remove metadata"));
        let busy = matches!(self.phase, Phase::Inspecting | Phase::Running);
        let o = self.opts;
        let mut body: Vec<AnyElement> = Vec::new();
        // the files
        let mut list: Vec<AnyElement> = vec![div().pt_1().pb(px(2.)).text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(format!("{} ({})", t!("Files"), self.files.len())).into_any_element()];
        for f in self.files.iter().take(8) {
            list.push(div().text_sm().truncate().child(name_of(f)).into_any_element());
        }
        if self.files.len() > 8 {
            list.push(small(&p, format!("{} {}", t!("… and"), self.files.len() - 8)));
        }
        body.push(card(&p, list));

        if self.phase == Phase::Setup || self.phase == Phase::Done {
            body.push(card(
                &p,
                vec![
                    div().pt_1().pb(px(2.)).text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(t!("Options")).into_any_element(),
                    check(&p, "keep-cover", t!("Keep the cover art of music files"), o.keep_cover, cx.listener_toggle(|v| v.opts.keep_cover = !v.opts.keep_cover)),
                    check(&p, "no-profile", t!("Remove the colour profile of pictures too (they may look different)"), o.remove_profile, cx.listener_toggle(|v| v.opts.remove_profile = !v.opts.remove_profile)),
                    check(&p, "keep-subs", t!("Keep plain subtitle tracks of videos"), o.keep_subtitles, cx.listener_toggle(|v| v.opts.keep_subtitles = !v.opts.keep_subtitles)),
                    check(&p, "keep-previews", t!("Keep the small preview pictures inside TIFF files (they can show the picture before it was cropped)"), o.keep_previews, cx.listener_toggle(|v| v.opts.keep_previews = !v.opts.keep_previews)),
                    check(&p, "no-sigs", t!("Remove digital signatures too (signed documents are refused otherwise; the cleaned file is no longer signed)"), o.remove_signatures, cx.listener_toggle(|v| v.opts.remove_signatures = !v.opts.remove_signatures)),
                    check(&p, "reset-times", t!("Set the file times to now"), o.reset_times, cx.listener_toggle(|v| v.opts.reset_times = !v.opts.reset_times)),
                    check(&p, "neutral", t!("Give the files a neutral name (file-1.jpg, file-2.jpg …)"), o.neutral, cx.listener_toggle(|v| v.opts.neutral = !v.opts.neutral)),
                    div().h(px(1.)).my_1().bg(p.divider).into_any_element(),
                    radio(&p, "mode-copy", t!("Save cleaned copies beside the originals (name.clean.ext)"), !o.replace, cx.listener_toggle(|v| v.opts.replace = false)),
                    radio(&p, "mode-replace", t!("Replace the originals (they go to the Recycle Bin first and Ctrl+Z puts them back; on a drive without a Recycle Bin a cleaned copy is saved beside the original instead)"), o.replace, cx.listener_toggle(|v| v.opts.replace = true)),
                ],
            ));
        }
        match self.phase {
            Phase::Inspecting => body.push(small(&p, t!("Reading the files…").to_string())),
            Phase::Running => body.push(small(&p, t!("Running in the Status Center. This window shows the result when it is done.").to_string())),
            Phase::Done => {
                let bad = self.results.iter().filter(|r| r.error.is_some()).count();
                if self.ran {
                    // "cleaned" counts only files whose contents were read and passed the check; a kind leaf does not read
                    // had only its streams removed and is counted apart
                    let ok = self.results.iter().filter(|r| r.error.is_none() && r.contents_read).count();
                    let streams_only = self.results.iter().filter(|r| r.error.is_none() && !r.contents_read).count();
                    let mut line = format!("{ok} {} · {bad} {}", t!("cleaned"), t!("not cleaned"));
                    if streams_only > 0 {
                        line.push_str(&format!(" · {streams_only} {}", t!("only the download marker removed")));
                    }
                    body.push(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(line).into_any_element());
                }
                for r in &self.results {
                    body.push(file_card(&p, r, self.ran));
                }
                if let Some(checked) = &self.verified {
                    body.push(verify_card(&p, checked));
                } else if self.verifying {
                    body.push(small(&p, t!("Reading the cleaned files again…").to_string()));
                }
                body.push(small(&p, leaf_scrub::NOT_REMOVED_LINE.to_string()));
                body.push(small(&p, leaf_scrub::REFUSED_LINE.to_string()));
            }
            Phase::Setup => {
                body.push(small(&p, leaf_scrub::NOT_REMOVED_LINE.to_string()));
                body.push(small(&p, leaf_scrub::REFUSED_LINE.to_string()));
            }
        }

        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .min_w(px(88.))
                .h(px(32.))
                .px_4()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme::RADIUS_SM)
                .text_sm()
                .when(primary, |d| d.bg(p.accent).text_color(p.accent_text))
                .when(!primary, |d| d.bg(p.hover).border_1().border_color(p.border))
                .when(busy, |d| d.opacity(0.5))
                .hover(|s| s.opacity(0.9))
                .child(label)
        };
        let bar_w = (f32::from(window.viewport_size().width) - 130.).max(80.);
        v_flex()
            .size_full()
            .bg(p.window_bg)
            .text_color(p.text)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, window, _| {
                if ev.keystroke.key == "escape" {
                    window.remove_window();
                }
            }))
            .child(
                TitleBar::new().h(theme::TITLEBAR_H).bg(p.titlebar_bg).border_color(gpui_kit::transparent_black()).child(
                    h_flex()
                        .w(px(bar_w))
                        .min_w_0()
                        .overflow_hidden()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .child(Icon::new(L::Eraser).size(px(14.)).flex_shrink_0())
                        .child(div().flex_1().min_w_0().truncate().child(t!("Remove metadata"))),
                ),
            )
            .child(v_flex().id("scrub-body").flex_1().min_h_0().overflow_y_scroll().px_5().pt_2().pb_4().gap_3().children(body))
            .child(
                h_flex()
                    .px_5()
                    .py_3()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(p.divider)
                    .child(div().flex_1())
                    .when(self.ran && self.phase == Phase::Done && self.results.iter().any(|r| r.output.is_some()), |d| {
                        d.child(button("verify", t!("Verify"), false).on_click(cx.listener(|v, _, _, cx| v.verify(cx))))
                    })
                    .child(button("show", t!("Show metadata"), false).on_click(cx.listener(|v, _, _, cx| v.show_metadata(cx))))
                    .child(button("run", t!("Remove metadata"), true).on_click(cx.listener(|v, _, _, cx| v.run(cx))))
                    .child(button("close", t!("Close"), false).on_click(|_, window, _| window.remove_window())),
            )
    }
}

/// A click handler that changes the view and redraws it (the option rows).
trait ListenerToggle {
    fn listener_toggle(&self, f: impl Fn(&mut ScrubView) + 'static) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static;
}

impl ListenerToggle for Context<'_, ScrubView> {
    fn listener_toggle(&self, f: impl Fn(&mut ScrubView) + 'static) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let weak = self.entity().downgrade();
        move |_, _, cx| {
            let _ = weak.update(cx, |v, cx| {
                if matches!(v.phase, Phase::Setup | Phase::Done) {
                    f(v);
                    cx.notify();
                }
            });
        }
    }
}
