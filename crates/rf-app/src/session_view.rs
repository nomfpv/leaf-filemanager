//! One agent session's page (PLAN F4), inside the Agents page: the transcript (your prompts, the agent's text, each tool call folded to a
//! line of its name and file path), the files the session's edit and write calls touched, and a search across the sessions of the
//! project. Everything is read only, from the agent's own file through `leaf_agents::transcript`: the transcript is read in chunks
//! on a background thread, the last part first, with "Load earlier" above it; nothing is copied into leaf's files or logs.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_agents::transcript::{self, Chunk, Hit, Item, Kind};
use leaf_agents::{Session, State, folder_key};

use crate::t;
use crate::theme::{self, Palette};

/// Bytes read for one page of the transcript.
const CHUNK: u64 = 512 * 1024;
/// A first load goes on reading earlier chunks until this many items are in.
const ENOUGH: usize = 40;
const MAX_HITS: usize = 200;
const HITS_PER_SESSION: usize = 20;

pub enum SessionEvent {
    Back,
    OpenFolder(PathBuf),
    /// Open the Review side of this folder.
    Review(PathBuf),
    /// A search hit in another session.
    OpenSession(Session, u64),
}

impl EventEmitter<SessionEvent> for SessionView {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Transcript,
    Files,
    Search,
}

struct SearchHit {
    session: Session,
    title: String,
    hit: Hit,
}

pub struct SessionView {
    session: Session,
    tab: Tab,
    file: Option<PathBuf>,
    missing: bool,
    items: Vec<Item>,
    list: ListState,
    /// The bytes `[start, end)` of the file are on the page; `len` is the file's length when last looked at.
    start: u64,
    end: u64,
    len: u64,
    loading: bool,
    epoch: u64,
    /// `None` until the Files tab asks; then what the edit and write calls named.
    files: Option<Vec<String>>,
    files_cancel: Arc<AtomicBool>,
    query: Entity<InputState>,
    hits: Vec<SearchHit>,
    searching: bool,
    searched: bool,
    search_cancel: Arc<AtomicBool>,
    _subs: Vec<Subscription>,
}

impl Drop for SessionView {
    fn drop(&mut self) {
        self.files_cancel.store(true, Ordering::Relaxed);
        self.search_cancel.store(true, Ordering::Relaxed);
    }
}

fn file_of(s: &Session) -> Option<PathBuf> {
    leaf_agents::adapters().iter().find(|a| a.id() == s.agent).and_then(|a| a.session_file(&s.id))
}

/// The chunk that ends the file, grown backwards until it holds enough items.
fn read_end(path: &std::path::Path) -> std::io::Result<Chunk> {
    let len = std::fs::metadata(path)?.len();
    let mut out = Chunk { items: Vec::new(), start: len, end: len };
    for _ in 0..8 {
        let c = transcript::read_back(path, out.start, CHUNK)?;
        let mut items = c.items;
        items.append(&mut out.items);
        out = Chunk { items, start: c.start, end: out.end };
        if out.start == 0 || out.items.len() >= ENOUGH {
            break;
        }
    }
    Ok(out)
}

impl SessionView {
    pub fn new(session: Session, jump: Option<u64>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search the sessions of this project")));
        let sub = cx.subscribe_in(&query, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.start_search(cx);
            }
        });
        let mut v = Self {
            session,
            tab: Tab::Transcript,
            file: None,
            missing: false,
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(800.)),
            start: 0,
            end: 0,
            len: 0,
            loading: false,
            epoch: 0,
            files: None,
            files_cancel: Arc::new(AtomicBool::new(false)),
            query,
            hits: Vec::new(),
            searching: false,
            searched: false,
            search_cancel: Arc::new(AtomicBool::new(false)),
            _subs: vec![sub],
        };
        v.load(jump, cx);
        v
    }

    /// What the status line says on this page.
    pub fn status_text(&self) -> String {
        match self.tab {
            Tab::Files => self.files.as_ref().map(|f| format!("{} {}", f.len(), if f.len() == 1 { t!("file touched") } else { t!("files touched") })).unwrap_or_default(),
            _ => self.session.folder.to_string_lossy().into_owned(),
        }
    }

    /// Reads the page: the end of the file, or (`jump`) from a line on.
    fn load(&mut self, jump: Option<u64>, cx: &mut Context<Self>) {
        self.epoch += 1;
        let epoch = self.epoch;
        self.loading = true;
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let path = file_of(&session)?;
                    let len = std::fs::metadata(&path).ok()?.len();
                    let chunk = match jump {
                        Some(off) => transcript::read_forward(&path, off, CHUNK).ok()?,
                        None => read_end(&path).ok()?,
                    };
                    Some((path, len, chunk))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.epoch != epoch {
                    return;
                }
                v.loading = false;
                match got {
                    Some((path, len, c)) => {
                        v.file = Some(path);
                        v.len = len;
                        v.start = c.start;
                        v.end = c.end;
                        v.items = c.items;
                        v.list.reset(v.items.len());
                        if jump.is_some() {
                            v.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.) });
                        } else {
                            v.list.scroll_to_end();
                        }
                    }
                    None => v.missing = true,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_earlier(&mut self, cx: &mut Context<Self>) {
        let (Some(path), true) = (self.file.clone(), !self.loading && self.start > 0) else { return };
        self.loading = true;
        let epoch = self.epoch;
        let from = self.start;
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let mut items = Vec::new();
                    let mut start = from;
                    for _ in 0..8 {
                        let c = transcript::read_back(&path, start, CHUNK).ok()?;
                        let mut more = c.items;
                        more.append(&mut items);
                        items = more;
                        start = c.start;
                        if start == 0 || items.len() >= ENOUGH / 2 {
                            break;
                        }
                    }
                    Some((start, items))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.epoch != epoch {
                    return;
                }
                v.loading = false;
                if let Some((start, mut items)) = got {
                    let n = items.len();
                    v.start = start;
                    items.append(&mut v.items);
                    v.items = items;
                    v.list.splice(0..0, n);
                    v.list.scroll_to(ListOffset { item_ix: n, offset_in_item: px(0.) });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_later(&mut self, cx: &mut Context<Self>) {
        let (Some(path), true) = (self.file.clone(), !self.loading) else { return };
        self.loading = true;
        let epoch = self.epoch;
        let from = self.end;
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let len = std::fs::metadata(&path).ok()?.len();
                    Some((len, transcript::read_forward(&path, from, CHUNK).ok()?))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.epoch != epoch {
                    return;
                }
                v.loading = false;
                if let Some((len, mut c)) = got {
                    let old = v.items.len();
                    v.len = len;
                    v.end = c.end;
                    let n = c.items.len();
                    v.items.append(&mut c.items);
                    v.list.splice(old..old, n);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_files(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::Files;
        cx.notify();
        if self.files.is_some() {
            return;
        }
        let session = self.session.clone();
        let cancel = self.files_cancel.clone();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move {
                    let path = file_of(&session)?;
                    Some(transcript::files_touched(&path, &cancel))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.files = Some(got.unwrap_or_default());
                cx.notify();
            });
        })
        .detach();
    }

    /// Searches the sessions of this project, one file after the other; the hits are listed as they come.
    fn start_search(&mut self, cx: &mut Context<Self>) {
        let q = self.query.read(cx).value().trim().to_string();
        self.search_cancel.store(true, Ordering::Relaxed);
        if q.is_empty() {
            self.searching = false;
            cx.notify();
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.search_cancel = cancel.clone();
        self.hits.clear();
        self.searching = true;
        self.searched = true;
        cx.notify();
        // the folders of the project: every folder a session of this group works in
        let known = crate::agents::sessions(cx);
        let group = self.session.group().to_path_buf();
        let mut folders: Vec<PathBuf> = vec![group.clone()];
        for s in known.iter().filter(|s| s.group() == group.as_path()) {
            if !folders.iter().any(|f| folder_key(f) == folder_key(&s.folder)) {
                folders.push(s.folder.clone());
            }
        }
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    for a in leaf_agents::adapters() {
                        for f in &folders {
                            out.extend(a.folder_sessions(f).into_iter().map(|sf| (a.id(), f.clone(), sf)));
                        }
                    }
                    out.sort_by(|a, b| b.2.modified_ms.cmp(&a.2.modified_ms));
                    out
                })
                .await;
            let mut total = 0;
            for (agent, folder, sf) in files {
                if cancel.load(Ordering::Relaxed) || total >= MAX_HITS {
                    break;
                }
                let (c, q) = (cancel.clone(), q.clone());
                let hits = cx
                    .background_executor()
                    .spawn(async move {
                        let mut v = Vec::new();
                        transcript::search(&sf.path, &q, HITS_PER_SESSION, &c, |h| v.push(h));
                        (sf, v)
                    })
                    .await;
                let (sf, hits) = hits;
                if hits.is_empty() {
                    continue;
                }
                total += hits.len();
                let known_s = known.iter().find(|s| s.id == sf.id).cloned();
                let _ = this.update(cx, |v, cx| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let session = known_s.unwrap_or(Session {
                        agent,
                        id: sf.id.clone(),
                        folder: folder.clone(),
                        branch: None,
                        repo: leaf_agents::worktree_repo(&folder),
                        state: State::Ended,
                        last_activity_ms: sf.modified_ms,
                        title: String::new(),
                    });
                    let title = if session.title.is_empty() { format!("{} {}", t!("Session of"), crate::agents::ago(session.last_activity_ms)) } else { session.title.clone() };
                    for hit in hits {
                        v.hits.push(SearchHit { session: session.clone(), title: title.clone(), hit });
                    }
                    cx.notify();
                });
            }
            let _ = this.update(cx, |v, cx| {
                if !cancel.load(Ordering::Relaxed) {
                    v.searching = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn go_to(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(h) = self.hits.get(i) else { return };
        let (session, off) = (h.session.clone(), h.hit.offset);
        if session.id == self.session.id {
            self.tab = Tab::Transcript;
            self.load(Some(off), cx);
            cx.notify();
        } else {
            cx.emit(SessionEvent::OpenSession(session, off));
        }
    }
}

fn small(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div().id(id).px_1p5().py_0p5().rounded(theme::RADIUS_SM).text_xs().whitespace_nowrap().cursor_pointer().text_color(p.text_secondary).hover(|s| s.bg(p.pressed).text_color(p.text)).child(label.into())
}

fn seg(id: &'static str, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .text_xs()
        .whitespace_nowrap()
        .cursor_pointer()
        .rounded(theme::RADIUS_SM)
        .when(on, |d| d.bg(p.selection).text_color(p.text))
        .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
        .child(label)
}

fn kind_text(k: Kind) -> &'static str {
    match k {
        Kind::Prompt => t!("You"),
        Kind::Text => t!("Agent"),
        Kind::Tool => t!("Tool"),
    }
}

fn item_row(it: &Item, p: &Palette) -> AnyElement {
    match it.kind {
        Kind::Prompt => v_flex()
            .w_full()
            .mt_2()
            .px_3()
            .py_2()
            .gap_1()
            .border_l_2()
            .border_color(p.accent)
            .bg(p.hover)
            .child(div().text_xs().text_color(p.accent).child(t!("You")))
            .child(div().text_sm().text_color(p.text).child(it.text.clone()))
            .into_any_element(),
        Kind::Text => div().w_full().px_3().py_1p5().text_sm().text_color(p.text).child(it.text.clone()).into_any_element(),
        Kind::Tool => h_flex()
            .w_full()
            .px_3()
            .py_0p5()
            .gap_2()
            .text_xs()
            .child(Icon::new(L::GitCompare).size(px(11.)).text_color(p.text_tertiary).flex_shrink_0())
            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).text_color(p.text_secondary).child(it.text.clone()))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text_tertiary).child(it.target.clone()))
            .into_any_element(),
    }
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let title = if self.session.title.is_empty() { t!("(no title)").to_string() } else { self.session.title.clone() };
        let ended = self.session.state == State::Ended;
        let resumable = ended && crate::agents::can_resume(&self.session);
        let (folder_a, folder_b, folder_c, s_resume) = (self.session.folder.clone(), self.session.folder.clone(), self.session.folder.clone(), self.session.clone());
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(crate::agents_view::icon_button(("session-back", 0), L::ArrowLeft, t!("Back to the sessions"), &p).on_click(cx.listener(|_, _, _, cx| cx.emit(SessionEvent::Back))))
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(crate::agents::state_color(self.session.state, &p)))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(title))
            .child(div().flex_shrink_0().text_xs().text_color(p.text_secondary).child(crate::agents::state_text(self.session.state)))
            .child(crate::agents_view::icon_button(("session-folder", 0), L::FolderSearch, t!("Open folder"), &p).on_click(cx.listener(move |_, _, _, cx| cx.emit(SessionEvent::OpenFolder(folder_a.clone())))))
            .child(crate::agents_view::icon_button(("session-term", 0), L::SquareTerminal, t!("Open terminal here"), &p).on_click(move |_, _, cx| crate::terminal::open(folder_b.clone(), cx)))
            .child(crate::agents_view::icon_button(("session-review", 0), L::GitCompare, t!("Review changes"), &p).on_click(cx.listener(move |_, _, _, cx| cx.emit(SessionEvent::Review(folder_c.clone())))))
            .when(resumable, |d| d.child(crate::agents_view::icon_button(("session-resume", 0), L::Play, t!("Resume in terminal"), &p).on_click(move |_, _, cx| crate::agents::resume(&s_resume, cx))));
        let tabs = h_flex()
            .px_3()
            .py_1()
            .gap_0p5()
            .child(seg("session-tab-transcript", t!("Transcript"), self.tab == Tab::Transcript, &p).on_click(cx.listener(|v, _, _, cx| {
                v.tab = Tab::Transcript;
                cx.notify();
            })))
            .child(seg("session-tab-files", t!("Files touched"), self.tab == Tab::Files, &p).on_click(cx.listener(|v, _, _, cx| v.show_files(cx))))
            .child(seg("session-tab-search", t!("Search"), self.tab == Tab::Search, &p).on_click(cx.listener(|v, _, _, cx| {
                v.tab = Tab::Search;
                cx.notify();
            })));
        let body = match self.tab {
            Tab::Transcript => self.render_transcript(&p, cx),
            Tab::Files => self.render_files(&p, cx),
            Tab::Search => self.render_search(&p, cx),
        };
        v_flex().size_full().child(top).child(tabs).child(div().flex_1().min_h_0().child(body))
    }
}

fn note(text: impl Into<SharedString>, p: &Palette) -> AnyElement {
    div().w_full().px_3().py_3().text_sm().text_color(p.text_secondary).child(text.into()).into_any_element()
}

impl SessionView {
    fn render_transcript(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.missing {
            return note(t!("The session file is gone."), p);
        }
        if self.items.is_empty() {
            return if self.loading { note(t!("Reading the session\u{2026}"), p) } else if self.start > 0 {
                v_flex().child(small("session-earlier", t!("Load earlier"), p).on_click(cx.listener(|v, _, _, cx| v.load_earlier(cx)))).into_any_element()
            } else {
                note(t!("Nothing to show in this session yet."), p)
            };
        }
        let more_before = self.start > 0;
        let more_after = self.end < self.len.max(self.end);
        let list = list(
            self.list.clone(),
            cx.processor(|this, ix: usize, window, _| match this.items.get(ix) {
                Some(it) => item_row(it, &Palette::for_window(window)),
                None => div().into_any_element(),
            }),
        )
        .size_full();
        v_flex()
            .size_full()
            .when(more_before, |d| {
                d.child(
                    h_flex().px_3().py_1().child(small("session-earlier", if self.loading { t!("Loading\u{2026}") } else { t!("Load earlier") }, p).on_click(cx.listener(|v, _, _, cx| v.load_earlier(cx)))),
                )
            })
            .child(div().flex_1().min_h_0().child(list))
            .when(more_after, |d| d.child(h_flex().px_3().py_1().child(small("session-later", t!("Load later"), p).on_click(cx.listener(|v, _, _, cx| v.load_later(cx))))))
            .into_any_element()
    }

    fn render_files(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(files) = &self.files else { return note(t!("Reading the session\u{2026}"), p) };
        if files.is_empty() {
            return note(t!("No file was written by this session's tools."), p);
        }
        let rows: Vec<AnyElement> = files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let path = PathBuf::from(f);
                let a = path.clone();
                let folder = path.parent().map(|d| d.to_path_buf());
                let review = self.session.folder.clone();
                h_flex()
                    .id(("session-file", i))
                    .w_full()
                    .h(theme::ROW_H)
                    .px_3()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .hover(|s| s.bg(p.hover))
                    .child(Icon::new(L::File).size(px(13.)).text_color(p.text_tertiary).flex_shrink_0())
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(f.clone()))
                    .child(small(("session-file-folder", i), t!("Open folder"), p).on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(d) = folder.clone() {
                            cx.emit(SessionEvent::OpenFolder(d));
                        }
                    })))
                    .child(small(("session-file-open", i), t!("Open file"), p).on_click(move |_, _, _| {
                        let p = a.clone();
                        std::thread::spawn(move || {
                            if p.exists() {
                                let _ = leaf_sys::shell::open(&p);
                            }
                        });
                    }))
                    .child(small(("session-file-review", i), t!("Review"), p).on_click(cx.listener(move |_, _, _, cx| cx.emit(SessionEvent::Review(review.clone())))))
                    .into_any_element()
            })
            .collect();
        div().id("session-files").size_full().overflow_y_scroll().p_1().child(v_flex().children(rows)).into_any_element()
    }

    fn render_search(&mut self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let bar = h_flex()
            .px_3()
            .py_1()
            .gap_2()
            .items_center()
            .child(div().flex_1().min_w_0().child(Input::new(&self.query)))
            .child(if self.searching {
                small("session-search-stop", t!("Stop"), p).on_click(cx.listener(|v, _, _, cx| {
                    v.search_cancel.store(true, Ordering::Relaxed);
                    v.searching = false;
                    cx.notify();
                }))
            } else {
                small("session-search-go", t!("Search"), p).on_click(cx.listener(|v, _, _, cx| v.start_search(cx)))
            });
        let status = if self.searching {
            format!("{} {}", self.hits.len(), t!("found so far\u{2026}"))
        } else if self.searched {
            if self.hits.is_empty() { t!("Nothing found.").to_string() } else { format!("{} {}", self.hits.len(), if self.hits.len() == 1 { t!("hit") } else { t!("hits") }) }
        } else {
            t!("Plain text, capital letters do not matter. Press Enter.").to_string()
        };
        let rows: Vec<AnyElement> = self
            .hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                v_flex()
                    .id(("session-hit", i))
                    .w_full()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .on_click(cx.listener(move |v, _, _, cx| v.go_to(i, cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text_secondary).child(h.title.clone()))
                            .child(div().flex_shrink_0().text_color(p.text_tertiary).child(kind_text(h.hit.kind))),
                    )
                    .child(div().w_full().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(h.hit.snippet.clone()))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .size_full()
            .child(bar)
            .child(div().px_3().pb_1().text_xs().text_color(p.text_tertiary).child(status))
            .child(div().id("session-hits").flex_1().min_h_0().overflow_y_scroll().child(v_flex().children(rows)))
            .into_any_element()
    }
}
