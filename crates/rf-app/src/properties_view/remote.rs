//! Properties of one item on a server (SFTP, FTP): what the server says about it (size, time, owner, group, a link's target) and, where
//! the place can do it (`Details::can_chmod`: SFTP, FTP with `SITE CHMOD`), the permission grid. The window reads and writes through the
//! place's `Fs` on worker threads; nothing here touches this PC's disks.

use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_sys::perms::{Mode, PERMS, Perm, WHO, Who};
use leaf_vfs::{Details, FsError, Kind, Meta, MetaChange};
use rf_core::format;
use rf_core::vpath::VPath;

use super::{card, link_button, note, row, title_bar_content_width};
use crate::t;
use crate::theme::{self, Palette};

/// Opens the Properties window of the item at `v` (one window per call), on the display of the window in front.
pub fn open(v: VPath, cx: &mut App) {
    let display = cx.active_window().and_then(|w| w.update(cx, |_, w, cx| w.display(cx).map(|d| d.id())).ok().flatten());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(display, size(px(480.), px(560.)), cx))),
        display_id: display,
        window_min_size: Some(size(px(400.), px(380.))),
        window_background: crate::theme::window_material(WindowBackgroundAppearance::MicaBackdrop),
        // Linux: the window draws its own caption buttons, edges and shadow (Windows ignores this)
        window_decorations: Some(gpui_kit::WindowDecorations::Client),
        ..TitleBar::window_options()
    };
    if let Err(e) = gpui_kit::open_window(options, cx, move |window, cx| {
        crate::glass::keep_out_of_capture(crate::look::window::hwnd_of(window));
        window.activate_window();
        cx.new(|cx| RemoteProps::new(v, cx))
    }) {
        log::error!("remote properties window: {e:#}");
    }
}

struct Loaded {
    meta: Meta,
    /// `None`: the place does not give owner and mode.
    details: Option<Details>,
}

pub struct RemoteProps {
    path: VPath,
    loaded: Option<Result<Loaded, String>>,
    /// The grid as edited, and as the server has it.
    mode: Mode,
    on_server: Mode,
    busy: bool,
    msg: Option<String>,
    frame_dark: Option<bool>,
}

fn words(e: &FsError) -> String {
    match e {
        FsError::Unsupported => t!("The server does not allow this here.").to_string(),
        FsError::Denied => t!("The server refused: access denied.").to_string(),
        FsError::NotFound => t!("This item is not on the server any more.").to_string(),
        e => e.to_string(),
    }
}

impl RemoteProps {
    fn new(path: VPath, cx: &mut Context<Self>) -> Self {
        let mut v = RemoteProps { path, loaded: None, mode: Mode::new(0), on_server: Mode::new(0), busy: false, msg: None, frame_dark: None };
        v.load(cx);
        v
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let p = self.path.clone();
        let task = cx.background_spawn(async move {
            let fs = leaf_vfs::fs_for(p.scheme()).map_err(|e| words(&e))?;
            let meta = fs.stat(&p).map_err(|e| words(&e))?;
            let details = fs.details(&p).ok();
            Ok::<_, String>(Loaded { meta, details })
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                if let Ok(l) = &r {
                    let bits = l.details.as_ref().and_then(|d| d.mode).or(l.meta.mode).unwrap_or(0);
                    v.mode = Mode::new(bits);
                    v.on_server = v.mode;
                }
                v.loaded = Some(r);
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.mode == self.on_server {
            return;
        }
        self.busy = true;
        self.msg = None;
        let (p, bits) = (self.path.clone(), self.mode.0);
        let task = cx.background_spawn(async move {
            let fs = leaf_vfs::fs_for(p.scheme()).map_err(|e| words(&e))?;
            fs.set_meta(&p, &MetaChange { modified_ms: None, mode: Some(bits) }).map_err(|e| words(&e))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(()) => {
                        v.on_server = v.mode;
                        // (what the server keeps is read again: it may have refused part of it)
                        v.load(cx);
                    }
                    Err(e) => v.msg = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn grid(&self, can: bool, is_dir: bool, p: &Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let head = |s: &'static str| div().w(px(80.)).flex().justify_center().text_xs().text_color(p.text_secondary).child(s);
        let mut grid = vec![
            h_flex()
                .py(px(5.))
                .child(div().flex_1())
                .child(head(t!("Read")))
                .child(head(t!("Write")))
                .child(head(if is_dir { t!("Enter") } else { t!("Execute") }))
                .into_any_element(),
        ];
        for (wi, who) in WHO.iter().enumerate() {
            let label = match who {
                Who::Owner => t!("Owner"),
                Who::Group => t!("Group"),
                Who::Others => t!("Others"),
            };
            let mut r = h_flex().py(px(4.)).items_center().text_sm().child(div().flex_1().child(label));
            for (pi, perm) in PERMS.iter().enumerate() {
                let (who, perm): (Who, Perm) = (*who, *perm);
                let on = self.mode.get(who, perm);
                r = r.child(
                    div().w(px(80.)).flex().justify_center().child(
                        div()
                            .id(("rperm", wi * 3 + pi))
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .when(on, |d| d.bg(p.accent))
                            .when(!on, |d| d.border_1().border_color(p.text_secondary))
                            .when(can, |d| d.cursor_pointer())
                            .when(!can, |d| d.opacity(0.5))
                            .children(on.then(|| Icon::new(L::Check).size(px(12.)).text_color(p.accent_text)))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                if can && !v.busy {
                                    v.mode = v.mode.toggled(who, perm);
                                    cx.notify();
                                }
                            })),
                    ),
                );
            }
            grid.push(r.into_any_element());
        }
        grid
    }
}

impl Render for RemoteProps {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        if self.frame_dark != Some(p.dark) {
            self.frame_dark = Some(p.dark);
            let h = super::PropsView::hwnd(window);
            if h != 0 {
                leaf_sys::window::set_dark_mode(h, p.dark);
            }
        }
        let name = if self.path.name().is_empty() { self.path.host().to_string() } else { self.path.name().to_string() };
        let title = format!("{name} — {}", t!("Properties"));
        window.set_window_title(&title);

        let mut body: Vec<AnyElement> = Vec::new();
        let mut icon = L::File;
        match &self.loaded {
            None => body.push(note(&p, t!("Loading…"))),
            Some(Err(e)) => body.push(card(&p, vec![row(&p, t!("Error"), e.clone())])),
            Some(Ok(l)) => {
                let (m, d) = (&l.meta, l.details.as_ref());
                icon = match m.kind {
                    Kind::Dir => L::Folder,
                    Kind::Link => L::Link,
                    Kind::File => L::File,
                };
                let kind = match m.kind {
                    Kind::Dir => t!("Folder"),
                    Kind::Link => t!("Link"),
                    Kind::File => t!("File"),
                };
                let mut rows = vec![row(&p, t!("Type"), kind.to_string()), row(&p, t!("Location"), super::breakable(&self.path.to_string()))];
                if let Some(t) = d.and_then(|d| d.link_target.clone()) {
                    rows.push(row(&p, t!("Link target"), t));
                }
                if m.kind == Kind::File {
                    rows.push(row(&p, t!("Size"), format!("{} ({} {})", format::size(m.size), m.size, t!("bytes"))));
                }
                if m.modified_ms > 0 {
                    rows.push(row(&p, t!("Modified"), format::datetime_full(m.modified_ms)));
                }
                body.push(card(&p, rows));
                if let Some(d) = d {
                    let who = |name: &Option<String>, id: Option<u32>| match (name, id) {
                        (Some(n), Some(i)) => format!("{n} ({i})"),
                        (Some(n), None) => n.clone(),
                        (None, Some(i)) => i.to_string(),
                        (None, None) => "—".to_string(),
                    };
                    let mut rows = vec![row(&p, t!("Owner"), who(&d.owner, d.uid)), row(&p, t!("Group"), who(&d.group, d.gid))];
                    if d.mode.is_some() {
                        rows.push(row(&p, t!("Permissions"), format!("{}  ({})", self.mode.symbolic(), self.mode.octal())));
                    }
                    body.push(card(&p, rows));
                    if d.mode.is_some() {
                        let can = d.can_chmod && !self.busy;
                        body.push(card(&p, self.grid(can, m.kind == Kind::Dir, &p, cx)));
                        let special = self.mode.special_words();
                        if !special.is_empty() {
                            body.push(note(&p, &format!("{}: {}", t!("Special bits"), special.join(", "))));
                        }
                        if m.kind == Kind::Link {
                            body.push(note(&p, t!("This is a link; its permissions are those of its target and cannot be changed here.")));
                        } else if !d.can_chmod {
                            body.push(note(&p, t!("This server does not let leaf change permissions.")));
                        } else if self.mode != self.on_server {
                            let apply = link_button(&p, "rperm-apply", L::Check, t!("Apply permissions")).on_click(cx.listener(|v, _, _, cx| v.apply(cx)));
                            let revert = link_button(&p, "rperm-reset", L::X, t!("Revert")).on_click(cx.listener(|v, _, _, cx| {
                                v.mode = v.on_server;
                                cx.notify();
                            }));
                            body.push(card(&p, vec![apply.into_any_element(), revert.into_any_element()]));
                        }
                    }
                }
            }
        }
        if let Some(m) = &self.msg {
            body.push(note(&p, m));
        }
        let bar_w = title_bar_content_width(f32::from(window.viewport_size().width));
        v_flex()
            .size_full()
            .bg(p.window_bg)
            .text_color(p.text)
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
                        .child(Icon::new(L::Info).size(px(14.)).flex_shrink_0())
                        .child(div().flex_1().min_w_0().truncate().child(title.clone())),
                ),
            )
            .child(
                v_flex()
                    .id("rprops-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_2()
                    .pb_4()
                    .gap_4()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(Icon::new(icon).size(px(40.)).text_color(p.text_secondary))
                            .child(div().flex_1().min_w_0().text_base().font_weight(FontWeight::SEMIBOLD).child(name)),
                    )
                    .children(body),
            )
    }
}
