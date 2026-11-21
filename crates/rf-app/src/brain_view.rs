//! The project brain page (Location::Brain): what an agent reads in a project, in one list: instruction files (`CLAUDE.md`,
//! `AGENTS.md`), the `.claude` folder (settings, skills, agents, commands, rules, memory) and `.mcp.json`, from the folder shown up to
//! the repository root. Selecting a file shows it in the preview pane (Markdown rendered); the box above narrows the list by name.
//! Read-only: nothing here writes.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::format;

use crate::t;
use crate::theme::{self, Palette};

/// Tab title: "Project brain - project".
pub fn title(dir: &Path) -> String {
    format!("{} \u{2014} {}", t!("Project brain"), rf_core::history::display_name(dir))
}

pub enum BrainEvent {
    /// The file now chosen (path, size, modified ms), or none.
    Selected(Option<(PathBuf, u64, i64)>),
    /// Open this file's folder with it selected.
    ShowInFolder(PathBuf),
}

impl EventEmitter<BrainEvent> for BrainView {}

struct Item {
    kind: &'static str,
    /// Relative to the project root, forward slashes.
    rel: String,
    path: PathBuf,
    size: u64,
    modified_ms: i64,
}

pub struct BrainView {
    dir: PathBuf,
    root: PathBuf,
    items: Vec<Item>,
    search: Entity<InputState>,
    selected: Option<PathBuf>,
    hover: Option<usize>,
}

fn meta_of(p: &Path) -> (u64, i64) {
    match std::fs::metadata(p) {
        Ok(m) => (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)),
        Err(_) => (0, 0),
    }
}

const TOP_FILES: &[(&str, &str)] = &[
    ("CLAUDE.md", "Instructions"),
    ("CLAUDE.local.md", "Instructions"),
    ("AGENTS.md", "Instructions"),
    ("GEMINI.md", "Instructions"),
    (".mcp.json", "MCP"),
];

/// What a file under `.claude` is, by the folder it lies in.
fn claude_kind(first: &str, name: &str) -> &'static str {
    match first {
        "skills" => "Skill",
        "agents" => "Agent",
        "commands" => "Command",
        "rules" => "Rule",
        "memory" => "Memory",
        _ if name.starts_with("settings") => "Settings",
        _ if name.ends_with(".md") => "Instructions",
        _ => "Other",
    }
}

fn walk_claude(dir: &Path, base: &Path, depth: usize, out: &mut Vec<(PathBuf, &'static str)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= 600 {
            return;
        }
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < 4 {
                walk_claude(&p, base, depth + 1, out);
            }
        } else {
            let rel = p.strip_prefix(base).unwrap_or(&p);
            let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
            let name = e.file_name().to_string_lossy().into_owned();
            // only text a person or an agent reads: Markdown, JSON, TOML, YAML
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if matches!(ext.as_str(), "md" | "json" | "toml" | "yaml" | "yml" | "txt") {
                out.push((p.clone(), claude_kind(if rel.components().count() > 1 { &first } else { "" }, &name)));
            }
        }
    }
}

/// Finds the files, from `dir` up to the repository root (or `dir` alone outside a repository).
fn scan(dir: &Path) -> (PathBuf, Vec<Item>) {
    let root = rf_core::copy_path::repo_root(dir).unwrap_or_else(|| dir.to_path_buf());
    let mut found: Vec<(PathBuf, &'static str)> = Vec::new();
    for a in dir.ancestors() {
        for (name, kind) in TOP_FILES {
            let p = a.join(name);
            if p.is_file() {
                found.push((p, kind));
            }
        }
        let claude = a.join(".claude");
        if claude.is_dir() {
            walk_claude(&claude, &claude, 0, &mut found);
        }
        if a == root {
            break;
        }
    }
    let mut items: Vec<Item> = found
        .into_iter()
        .map(|(path, kind)| {
            let (size, modified_ms) = meta_of(&path);
            let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            Item { kind, rel, path, size, modified_ms }
        })
        .collect();
    items.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.rel.to_lowercase().cmp(&b.rel.to_lowercase())));
    (root, items)
}

impl BrainView {
    pub fn new(dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (root, items) = scan(&dir);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search by name")));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        Self { dir, root, items, search, selected: None, hover: None }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn status_text(&self) -> String {
        format!("{} {}", self.items.len(), if self.items.len() == 1 { t!("file") } else { t!("files") })
    }

    /// The items whose path holds every word of the box.
    fn shown(&self, cx: &App) -> Vec<usize> {
        let q = self.search.read(cx).value().to_string();
        let words: Vec<String> = q.split_whitespace().map(rf_core::entry::fold_case).collect();
        (0..self.items.len())
            .filter(|&i| {
                let hay = rf_core::entry::fold_case(&format!("{} {}", self.items[i].rel, self.items[i].kind));
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    fn select(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(it) = self.items.get(i) else { return };
        self.selected = Some(it.path.clone());
        cx.emit(BrainEvent::Selected(Some((it.path.clone(), it.size, it.modified_ms))));
        cx.notify();
    }
}

fn icon_of(kind: &str) -> L {
    match kind {
        "Instructions" | "Rule" => L::FileText,
        "Skill" => L::Zap,
        "Agent" | "Memory" => L::Brain,
        "Command" => L::Command,
        "Settings" => L::Settings,
        "MCP" => L::Server,
        _ => L::File,
    }
}

impl Render for BrainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let count = self.shown(cx).len();
        let summary = if self.items.is_empty() {
            t!("No instruction files, .claude folder or .mcp.json from here up to the project root.").to_string()
        } else {
            format!("{}: {} ({})", t!("Project brain"), self.status_text(), self.root.display())
        };
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::Brain).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary))
            .child(
                h_flex()
                    .w(px(220.))
                    .h(px(28.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .bg(p.hover)
                    .border_1()
                    .border_color(p.border)
                    .child(Icon::new(L::Search).size(px(13.)).flex_shrink_0().text_color(p.text_secondary))
                    .child(Input::new(&self.search).appearance(false).cleanable(true)),
            );
        let body = uniform_list(
            "brain-rows",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let p = Palette::for_window(window);
                let shown = this.shown(cx);
                range.filter_map(|r| shown.get(r).map(|&i| this.render_row(i, &p, cx))).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .p_1();
        v_flex().size_full().child(top).child(body)
    }
}

impl BrainView {
    fn render_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let it = &self.items[i];
        let on = self.selected.as_ref() == Some(&it.path);
        let hov = self.hover == Some(i);
        let when = if it.modified_ms > 0 { format::datetime(it.modified_ms) } else { String::new() };
        let p2 = it.path.clone();
        h_flex()
            .id(("brain-row", i))
            .w_full()
            .h(theme::ROW_H)
            .px_2()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .rounded(theme::RADIUS_SM)
            .text_sm()
            .cursor_pointer()
            .when(hov && !on, |d| d.bg(p.hover))
            .when(on, |d| d.bg(p.selection))
            .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                let want = h.then_some(i);
                if *h && v.hover != want || !*h && v.hover == Some(i) {
                    v.hover = want;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |v, e: &ClickEvent, _, cx| {
                v.select(i, cx);
                if e.click_count() >= 2 {
                    if let Some(it) = v.items.get(i) {
                        cx.emit(BrainEvent::ShowInFolder(it.path.clone()));
                    }
                }
            }))
            .child(Icon::new(icon_of(it.kind)).size(px(14.)).text_color(p.text_secondary))
            .child(div().flex_shrink_0().w(px(100.)).text_xs().text_color(p.accent).child(crate::i18n::td(it.kind)))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text).child(it.rel.clone()))
            .child(div().w(px(80.)).flex_shrink_0().text_xs().text_right().text_color(p.text).child(format::size(it.size)))
            .child(div().w(px(120.)).flex_shrink_0().text_xs().text_right().text_color(p.text_secondary).child(when))
            .child(
                div()
                    .id(("brain-open", i))
                    .size(px(24.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme::RADIUS_SM)
                    .hover(|s| s.bg(p.pressed))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("Open location")).build(window, cx))
                    .child(Icon::new(L::FolderSearch).size(px(14.)).text_color(p.text_secondary))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(BrainEvent::ShowInFolder(p2.clone()));
                    })),
            )
            .into_any_element()
    }
}
