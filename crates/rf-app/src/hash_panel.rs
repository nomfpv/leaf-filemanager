//! Hashes panel (Files' properties "Hashes" tab): MD5 / SHA-1 / SHA-256 / SHA-384 / SHA-512 / CRC32 computed
//! on a background thread with progress + cancel. Each row has a copy button and a compare input
//! (paste a hash: green check on match, red X otherwise). Laid out for a ~300 px side panel.
//!
//! Embed: `cx.new(|cx| HashPanel::new(path, cx))`; call `panel.update(cx, |p, cx| p.set_path(path, cx))`
//! to switch files (the running computation is cancelled). Dropping the entity cancels too.
#![allow(dead_code)]

use crate::t;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;
use leaf_sys::hash::{Cancelled, Hashes};

use crate::theme::{self, Palette};

const ALGOS: [&str; 6] = ["MD5", "SHA-1", "SHA-256", "SHA-384", "SHA-512", "CRC32"];
const GREEN: u32 = 0x30D158;

#[derive(Clone, PartialEq)]
enum Phase {
    Running,
    Done,
    Cancelled,
    Failed(String),
}

enum Ev {
    Progress(u64, u64),
    /// The result, whether it was cancelled, and the SHA-256 the `.sha256` file beside the file holds (read off the UI thread).
    Done(Result<Hashes, String>, bool, Option<String>),
}

#[derive(Clone, PartialEq)]
enum HostCheck {
    Idle,
    Running,
    Done,
    Failed(String),
}

pub struct HashPanel {
    path: PathBuf,
    phase: Phase,
    done: u64,
    total: u64,
    hashes: Option<Hashes>,
    cancel: Arc<AtomicBool>,
    compare: Vec<Entity<InputState>>,
    copied: Option<usize>,
    /// The SHA-256 a `.sha256` file beside the file holds, put into the compare field of SHA-256 once.
    fill: Option<String>,
    host_input: Option<Entity<InputState>>,
    host: HostCheck,
    /// What saving the `.sha256` file said.
    saved: Option<String>,
    _subs: Vec<Subscription>,
    _task: Option<Task<()>>,
    _copy_task: Option<Task<()>>,
    _host_task: Option<Task<()>>,
}

impl Drop for HashPanel {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl HashPanel {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path: PathBuf::new(),
            phase: Phase::Running,
            done: 0,
            total: 0,
            hashes: None,
            cancel: Arc::new(AtomicBool::new(false)),
            compare: Vec::new(),
            copied: None,
            fill: None,
            host_input: None,
            host: HostCheck::Idle,
            saved: None,
            _subs: Vec::new(),
            _task: None,
            _copy_task: None,
            _host_task: None,
        };
        this.set_path(path, cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Cancels the running computation (if any) and starts hashing `path`.
    pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.path = path.clone();
        self.phase = Phase::Running;
        self.done = 0;
        self.total = 0; // filled in by the first progress event (no stat on the UI thread)
        self.hashes = None;
        self.copied = None;
        self.fill = None;
        self.host = HostCheck::Idle;
        self._host_task = None;
        self.saved = None;
        // Reset compare fields (created lazily on first render).
        self.compare.clear();
        self._subs.clear();

        let (tx, mut rx) = mpsc::unbounded::<Ev>();
        std::thread::Builder::new()
            .name("rf-hash".into())
            .spawn(move || {
                let last = std::cell::Cell::new(Instant::now() - Duration::from_secs(1));
                let ptx = tx.clone();
                let c2 = cancel.clone();
                let r = leaf_sys::hash::compute(
                    &path,
                    &mut |d, t| {
                        if last.get().elapsed() >= Duration::from_millis(80) || d >= t {
                            last.set(Instant::now());
                            let _ = ptx.unbounded_send(Ev::Progress(d, t));
                        }
                    },
                    &move || c2.load(Ordering::Relaxed),
                );
                let (res, cancelled) = match r {
                    Ok(h) => (Ok(h), false),
                    Err(e) if e.downcast_ref::<Cancelled>().is_some() => (Err(String::new()), true),
                    Err(e) => (Err(format!("{e:#}")), false),
                };
                let side = if res.is_ok() { sidecar_of(&path) } else { None };
                let _ = tx.unbounded_send(Ev::Done(res, cancelled, side));
            })
            .ok();

        self._task = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut evs = vec![first];
                while let Ok(e) = rx.try_recv() {
                    evs.push(e);
                }
                let mut finished = false;
                let alive = this
                    .update(cx, |v, cx| {
                        for e in evs {
                            match e {
                                Ev::Progress(d, t) => {
                                    v.done = d;
                                    v.total = t;
                                }
                                Ev::Done(r, cancelled, side) => {
                                    finished = true;
                                    v.fill = side.or(v.fill.take());
                                    match r {
                                        Ok(h) => {
                                            v.done = h.size;
                                            v.total = h.size;
                                            v.hashes = Some(h);
                                            v.phase = Phase::Done;
                                        }
                                        Err(_) if cancelled => v.phase = Phase::Cancelled,
                                        Err(m) => v.phase = Phase::Failed(m),
                                    }
                                }
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if finished || !alive {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Stops the running computation.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        cx.notify();
    }

    fn values(&self) -> Option<[&str; 6]> {
        self.hashes.as_ref().map(|h| [&*h.md5, &*h.sha1, &*h.sha256, &*h.sha384, &*h.sha512, &*h.crc32])
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.compare.is_empty() {
            return;
        }
        if self.host_input.is_none() {
            self.host_input = Some(cx.new(|cx| InputState::new(window, cx).placeholder(crate::t!("Same file on a server: sftp://host/path"))));
        }
        for _ in 0..ALGOS.len() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(crate::t!("Compare: paste a hash")));
            self._subs.push(cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }));
            self.compare.push(input);
        }
    }

    /// Asks the server for the SHA-256 of the file at the address typed in (`sha256sum` there, over the SSH connection): the
    /// answer goes into the compare field of SHA-256, which shows whether it matches.
    fn check_on_host(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.host_input else { return };
        let text = input.read(cx).value().trim().to_string();
        let addr = match leaf_remote::VPath::parse(&text) {
            Ok(a) if a.scheme() == "sftp" => a,
            _ => {
                self.host = HostCheck::Failed(crate::t!("Type the file's address on the server: sftp://host/path/file").to_string());
                cx.notify();
                return;
            }
        };
        self.host = HostCheck::Running;
        let (tx, rx) = futures::channel::oneshot::channel::<Result<String, String>>();
        std::thread::Builder::new()
            .name("rf-hash-host".into())
            .spawn(move || {
                let r = leaf_remote::remote_sum(&addr, leaf_remote::checksum::Algo::Sha256, &|| false).map_err(|e| e.to_string());
                let _ = tx.send(r);
            })
            .ok();
        self._host_task = Some(cx.spawn(async move |this, cx| {
            let r = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(h) => {
                        v.fill = Some(h);
                        v.host = HostCheck::Done;
                    }
                    Err(m) => v.host = HostCheck::Failed(m),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Writes `<file>.sha256` beside the file (`hash  name`); never replaces one that is there.
    fn save_sidecar(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.hashes.as_ref().map(|h| h.sha256.to_string()) else { return };
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut side = self.path.clone().into_os_string();
        side.push(".sha256");
        let r = std::fs::OpenOptions::new().write(true).create_new(true).open(&side).and_then(|mut f| {
            use std::io::Write;
            f.write_all(leaf_remote::checksum::format_sidecar(&h, &name).as_bytes())
        });
        self.saved = Some(match r {
            Ok(()) => crate::t!("Saved the .sha256 file beside the file.").to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => crate::t!("A .sha256 file is there already.").to_string(),
            Err(e) => e.to_string(),
        });
        cx.notify();
    }

    fn copy(&mut self, i: usize, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(i);
        self._copy_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |v, cx| {
                v.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

/// The SHA-256 in `<file>.sha256` beside the file, when there is such a small file with a line for it.
fn sidecar_of(path: &Path) -> Option<String> {
    let mut side = path.to_path_buf().into_os_string();
    side.push(".sha256");
    let side = PathBuf::from(side);
    if std::fs::metadata(&side).ok()?.len() > 64 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(side).ok()?;
    leaf_remote::checksum::parse_sidecar(&text, &path.file_name()?.to_string_lossy())
}

/// Lowercases and strips whitespace / `0x` so pasted hashes compare cleanly.
fn normalize(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    t.strip_prefix("0x").map(str::to_string).unwrap_or(t)
}

impl Render for HashPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        let p = Palette::for_window(window);
        let green: Hsla = rgb(GREEN).into();
        let name = rf_core::history::display_name(&self.path);

        let frac = if self.total > 0 { (self.done as f32 / self.total as f32).clamp(0., 1.) } else { 0. };
        let running = self.phase == Phase::Running;

        let header = v_flex()
            .gap_0p5()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(name),
            )
            .child(div().text_xs().text_color(p.text_secondary).child(format::size(self.total)));

        let status: AnyElement = match &self.phase {
            Phase::Running => v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded_full()
                                .bg(p.hover)
                                .overflow_hidden()
                                .child(div().h_full().w(relative(frac)).rounded_full().bg(p.accent)),
                        )
                        .child(div().w(px(36.)).text_xs().text_color(p.text_secondary).child(crate::i18n::pct(frac * 100.)))
                        .child(
                            div()
                                .id("hp-cancel")
                                .px_2()
                                .py_0p5()
                                .rounded(theme::RADIUS_SM)
                                .text_xs()
                                .bg(p.hover)
                                .cursor_pointer()
                                .hover(|s| s.bg(p.pressed))
                                .child(t!("Cancel"))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.text_tertiary)
                        .child(format!("{} / {}", format::size(self.done), format::size(self.total))),
                )
                .into_any_element(),
            Phase::Cancelled => div()
                .id("hp-retry")
                .text_xs()
                .text_color(p.text_secondary)
                .child(t!("Cancelled. Click to calculate again."))
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    let path = this.path.clone();
                    this.set_path(path, cx);
                }))
                .into_any_element(),
            Phase::Failed(m) => h_flex()
                .gap_1()
                .items_start()
                .text_xs()
                .text_color(p.danger)
                .child(Icon::new(L::TriangleAlert).size(px(13.)))
                .child(div().flex_1().min_w_0().child(crate::i18n::td(m)))
                .into_any_element(),
            Phase::Done => div().into_any_element(),
        };

        // a hash from the `.sha256` file or from the server goes into the SHA-256 compare field (once)
        if let Some(h) = self.fill.take() {
            self.compare[2].update(cx, |s, cx| s.set_value(h, window, cx));
        }
        let values = self.values();
        let mut rows = v_flex().gap_2();
        for (i, algo) in ALGOS.iter().enumerate() {
            let value = values.map(|v| v[i].to_string());
            let typed = normalize(&self.compare[i].read(cx).value());
            let verdict: Option<bool> = match (&value, typed.is_empty()) {
                (Some(v), false) => Some(*v == typed),
                _ => None,
            };
            let copied = self.copied == Some(i);
            let copy_val = value.clone();
            rows = rows.child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded(theme::RADIUS)
                    .bg(p.hover)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(*algo))
                            .child(
                                div()
                                    .id(("hp-copy", i))
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(theme::RADIUS_SM)
                                    .when(value.is_some(), |d| d.cursor_pointer().hover(|s| s.bg(p.pressed)))
                                    .when(value.is_none(), |d| d.opacity(0.4))
                                    .child(if copied {
                                        Icon::new(L::Check).size(px(14.)).text_color(green)
                                    } else {
                                        Icon::new(L::Copy).size(px(14.)).text_color(p.text_secondary)
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(v) = copy_val.clone() {
                                            this.copy(i, v, cx);
                                        }
                                    })),
                            ),
                    )
                    .child(match &value {
                        Some(v) => div()
                            .w_full()
                            .text_xs()
                            .font_family("Consolas")
                            .text_color(p.text)
                            .child(v.clone())
                            .into_any_element(),
                        None => div()
                            .text_xs()
                            .text_color(p.text_tertiary)
                            .child(if running { t!("Calculating…") } else { "—" })
                            .into_any_element(),
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.compare[i])))
                            .children(verdict.map(|ok| {
                                Icon::new(if ok { L::CircleCheck } else { L::CircleX })
                                    .size(px(18.))
                                    .text_color(if ok { green } else { p.danger })
                            })),
                    ),
            );
        }

        let host_state: AnyElement = match &self.host {
            HostCheck::Idle => div().into_any_element(),
            HostCheck::Running => div().text_xs().text_color(p.text_tertiary).child(t!("Asking the server...")).into_any_element(),
            HostCheck::Done => div().text_xs().text_color(p.text_secondary).child(t!("The server's SHA-256 is in the compare field above.")).into_any_element(),
            HostCheck::Failed(m) => div().text_xs().text_color(p.danger).child(m.clone()).into_any_element(),
        };
        let can_save = self.hashes.is_some();
        let button = |id: &'static str, label: String, on: bool| {
            div().id(id).px_2().py_0p5().rounded(theme::RADIUS_SM).text_xs().bg(p.hover).child(label).when(on, |d| d.cursor_pointer().hover(|s| s.bg(p.pressed))).when(!on, |d| d.opacity(0.4))
        };
        let extras = v_flex()
            .gap_1()
            .p_2()
            .rounded(theme::RADIUS)
            .bg(p.hover)
            .child(h_flex().gap_1().items_center().child(div().flex_1().min_w_0().child(Input::new(self.host_input.as_ref().expect("made in ensure_inputs"))))
                .child(button("hp-host", t!("Check on server").to_string(), true).on_click(cx.listener(|this, _, _, cx| this.check_on_host(cx)))))
            .child(host_state)
            .child(
                h_flex().gap_2().items_center()
                    .child(button("hp-save", t!("Save .sha256").to_string(), can_save).on_click(cx.listener(move |this, _, _, cx| {
                        if can_save {
                            this.save_sidecar(cx);
                        }
                    })))
                    .children(self.saved.clone().map(|m| div().flex_1().min_w_0().text_xs().text_color(p.text_secondary).child(m))),
            );

        v_flex().w_full().gap_3().p_3().child(header).child(status).child(rows).child(extras)
    }
}
