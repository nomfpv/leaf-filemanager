//! The git worktree panel of the sidebar (A5): the worktrees of the open repository by branch, the quick list (the command
//! "Worktrees") and the dialog that adds one (the command "New worktree…"; it runs `git worktree add`, the one git command of leaf
//! that writes). No remove, no prune, no force.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use leaf_dev::gitread::{self, Worktree};

use super::*;

/// How long a read list is trusted while the user stays inside one repository.
const STALE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct WtState {
    /// The folder the list was last asked for (so a folder outside any repository is not asked again and again).
    for_dir: Option<PathBuf>,
    pending: bool,
    at: Option<Instant>,
    /// The repository the list belongs to, and its worktrees (the main one first).
    root: Option<PathBuf>,
    list: Vec<Worktree>,
    /// The section is folded.
    folded: bool,
}

/// `a` is `b` or inside it (both as git and Windows write them: slashes and case do not matter).
fn within(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase().replace('/', "\\").trim_end_matches('\\'));
    norm(a).starts_with(norm(b))
}

/// The index of the worktree `dir` is in: the deepest one that holds it.
fn current_of(list: &[Worktree], dir: &Path) -> Option<usize> {
    list.iter().enumerate().filter(|(_, w)| within(dir, &w.path)).max_by_key(|(_, w)| w.path.as_os_str().len()).map(|(i, _)| i)
}

/// `<repo parent>\<repo name>-<branch>` (a `/` in the branch becomes `-`).
fn default_folder(main: &Path, branch: &str) -> PathBuf {
    let name = main.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let branch: String = branch.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c }).collect();
    main.parent().unwrap_or(main).join(format!("{name}-{branch}"))
}

impl Workspace {
    /// Asks, off the interface's thread, for the worktrees of the repository the shown folder is in, when that is not known yet.
    fn wt_poll(&mut self, cur: &Location, cx: &mut Context<Self>) {
        if !crate::modules::enabled(cx, crate::modules::Module::Dev) {
            return;
        }
        let Some(dir) = cur.path().map(Path::to_path_buf) else { return };
        let fresh = self.wt.at.is_some_and(|t| t.elapsed() < STALE);
        let known = self.wt.root.as_ref().is_some_and(|r| within(&dir, r));
        if self.wt.pending || ((known || self.wt.for_dir.as_deref() == Some(dir.as_path())) && fresh) {
            return;
        }
        self.wt.for_dir = Some(dir.clone());
        // outside a repository there is nothing to ask (a cheap look for `.git`, no process)
        if leaf_dev::gitignore::repo_root(&dir).is_none() {
            self.wt.root = None;
            self.wt.list.clear();
            self.wt.at = Some(Instant::now());
            return;
        }
        self.wt.pending = true;
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(async move {
                    let root = gitread::repo_root(&dir)?;
                    let list = gitread::worktrees(&root)?;
                    Some((root, list))
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                w.wt.pending = false;
                w.wt.at = Some(Instant::now());
                match got {
                    Some((root, list)) => {
                        w.wt.root = Some(root);
                        w.wt.list = list;
                    }
                    None => {
                        w.wt.root = None;
                        w.wt.list.clear();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Forgets the list so the next draw reads it again (after a worktree was added).
    pub(super) fn wt_forget(&mut self) {
        self.wt.at = None;
        self.wt.for_dir = None;
        self.wt.root = None;
    }

    /// The "Worktrees" section of the sidebar: only inside a repository that has more than one worktree.
    pub(super) fn wt_sidebar(&mut self, mut col: Stateful<Div>, cur: &Location, rail: bool, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        self.wt_poll(cur, cx);
        if rail || self.wt.list.len() < 2 || !crate::modules::enabled(cx, crate::modules::Module::Dev) {
            return col;
        }
        let list = self.wt.list.clone();
        let here = cur.path().and_then(|d| current_of(&list, d));
        let folded = self.wt.folded;
        col = col.child(
            h_flex()
                .id("sb-wt-head")
                .h(px(28.))
                .mt(px(10.))
                .px_2()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .text_size(px(p.fs_xs))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text_tertiary)
                .hover(|s| s.text_color(p.text_secondary))
                .child(t!("Worktrees"))
                .child(Icon::new(if folded { L::ChevronRight } else { L::ChevronDown }).size(px(12.)).flex_shrink_0())
                .when(folded, |d| d.child(div().text_color(p.text_tertiary).child(list.len().to_string())))
                .child(div().flex_1())
                .child(
                    div()
                        .id("sb-wt-add")
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .hover(|s| s.bg(p.hover).text_color(p.text))
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("New worktree…")).build(window, cx))
                        .child(Icon::new(L::Plus).size(px(14.)))
                        .on_click(cx.listener(|_, _, window, cx| {
                            cx.stop_propagation();
                            window.dispatch_action(Box::new(crate::actions::NewWorktree), cx);
                        })),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.wt.folded = !this.wt.folded;
                    cx.notify();
                })),
        );
        if folded {
            return col;
        }
        for (i, w) in list.iter().enumerate() {
            let label = if w.locked { format!("{} ({})", w.label(), t!("locked")) } else { w.label() };
            col = col.child(self.sidebar_item(
                ("sb-wt", i).into(),
                L::GitBranch,
                p.text_secondary,
                label,
                Location::Path(w.path.clone()),
                here == Some(i),
                None,
                p,
                cx,
            ));
        }
        col
    }

    /// The right-click menu of a worktree row.
    pub(super) fn wt_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let me = cx.entity().downgrade();
        let with = |f: fn(&mut Workspace, PathBuf, &mut Window, &mut Context<Workspace>)| {
            let (me, path) = (me.clone(), path.clone());
            move |w: &mut Window, cx: &mut App| {
                let p = path.clone();
                let _ = me.update(cx, |ws, cx| f(ws, p, w, cx));
            }
        };
        let split = self.tabs[self.active].right.is_some();
        let items = vec![
            Item::action(t!("Open"), MenuIcon::Lucide(L::SquareArrowOutUpRight), with(|ws, p, w, cx| ws.navigate(Location::Path(p), w, cx))),
            Item::action(t!("Open in new tab"), MenuIcon::Lucide(L::SquarePlus), with(|ws, p, w, cx| {
                let keep = ws.active;
                ws.open_tab(Location::Path(p), w, cx);
                if !cx.global::<crate::settings::Settings>().focus_new_tab {
                    ws.activate(keep, w, cx);
                }
            })),
            Item::action(
                t!("Open in other pane"),
                MenuIcon::Lucide(L::Columns2),
                with(|ws, p, _, cx| {
                    let t = &ws.tabs[ws.active];
                    let other = if t.right_active { Some(t.left.clone()) } else { t.right.clone() };
                    if let Some(o) = other {
                        o.update(cx, |v, cx| v.navigate(Location::Path(p), cx));
                    }
                }),
            )
            .enabled(split),
            Item::action(t!("Copy path"), MenuIcon::Lucide(L::ClipboardCopy), with(|_, p, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string())))),
            Item::Separator,
            Item::action(t!("Show changes"), MenuIcon::Lucide(L::GitCompare), with(|ws, p, _, cx| {
                ws.active_tab().clone().update(cx, |_, cx| cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(Location::Changes(p))));
            })),
        ];
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    /// The command "Worktrees": the quick list.
    pub(super) fn show_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.active_tab().read(cx).current_dir();
        let Some(root) = dir.as_deref().and_then(leaf_dev::gitignore::repo_root) else {
            crate::prompt::notice(t!("Not in a git repository"), t!("Worktrees lists the worktrees of a git repository. This folder is not in one.").to_string(), window, cx);
            return;
        };
        if self.wt.root.as_ref().is_none_or(|r| !within(&root, r)) || self.wt.list.is_empty() {
            self.wt.list = gitread::worktrees(&root).unwrap_or_default();
            self.wt.root = Some(root);
            self.wt.at = Some(Instant::now());
        }
        self.toggle_palette(crate::command_palette::PaletteMode::Worktrees, window, cx);
    }

    /// The rows of the quick list.
    pub(super) fn wt_entries(&self) -> Vec<crate::command_palette::TabEntry> {
        self.wt
            .list
            .iter()
            .enumerate()
            .map(|(i, w)| crate::command_palette::TabEntry { label: w.label(), detail: w.path.to_string_lossy().into_owned(), closed: false, id: i })
            .collect()
    }

    /// The command "New worktree…": the branch first, then the folder, then `git worktree add` off the interface's thread.
    pub(super) fn new_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::prompt::{Button, Choice, Kind};
        let dir = self.active_tab().read(cx).current_dir();
        let Some(root) = dir.as_deref().and_then(leaf_dev::gitignore::repo_root) else {
            crate::prompt::notice(t!("Not in a git repository"), t!("New worktree adds a working folder to a git repository. This folder is not in one.").to_string(), window, cx);
            return;
        };
        let list = gitread::worktrees(&root).unwrap_or_default();
        let branches = gitread::local_branches(&root).unwrap_or_default();
        let main = list.first().map_or_else(|| root.clone(), |w| w.path.clone());
        let me = cx.entity().downgrade();
        let mode = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let shown: Vec<&str> = branches.iter().take(12).map(String::as_str).collect();
        let message = format!(
            "{}\n{}{}",
            t!("A new branch starts at the current head. The new worktree is a second working folder; nothing that exists is changed."),
            t!("Local branches: "),
            if branches.is_empty() { "-".to_string() } else { format!("{}{}", shown.join(", "), if branches.len() > 12 { ", ..." } else { "" }) },
        );
        let choice = Choice { label: t!("Branch").into(), options: vec![t!("New branch").into(), t!("Existing branch").into()], selected: mode.clone() };
        let next = Button::new(t!("Next"), Kind::Primary, {
            let (root, main, list, branches, me) = (root.clone(), main.clone(), list.clone(), branches.clone(), me.clone());
            move |text, window, cx| {
                let name = text.unwrap_or_default().trim().to_string();
                let existing = mode.get() == 1;
                if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) {
                    return Err(t!("Enter a branch name without spaces").to_string());
                }
                let exists = branches.iter().any(|b| *b == name);
                if existing && !exists {
                    return Err(t!("There is no local branch with this name").to_string());
                }
                if !existing && exists {
                    return Err(t!("A branch with this name exists: choose Existing branch to check it out").to_string());
                }
                if existing && let Some(w) = list.iter().find(|w| w.branch.as_deref() == Some(name.as_str())) {
                    return Err(format!("{} {}", t!("This branch is already checked out in"), w.path.display()));
                }
                let (root, main, me) = (root.clone(), main.clone(), me.clone());
                window.defer(cx, move |window, cx| folder_dialog(root, main, name, existing, me, window, cx));
                Ok(())
            }
        });
        crate::prompt::show_with(t!("New worktree"), Some(message), Some((String::new(), t!("Branch name").to_string())), vec![choice], vec![next, Button::cancel()], window, cx);
    }
}

fn folder_dialog(root: PathBuf, main: PathBuf, branch: String, existing: bool, me: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let start = default_folder(&main, &branch);
    let go = Button::new(t!("Create"), Kind::Primary, move |text, window, cx| {
        let folder = PathBuf::from(text.unwrap_or_default().trim().trim_matches('"'));
        if folder.as_os_str().is_empty() || folder.is_relative() {
            return Err(t!("Enter the full path of the new folder").to_string());
        }
        if folder.exists() && folder.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
            return Err(t!("The folder exists and is not empty. Choose a folder that does not exist yet.").to_string());
        }
        let (root, branch, me, f2) = (root.clone(), branch.clone(), me.clone(), folder.clone());
        let work = cx.background_spawn(async move { gitread::add_worktree(&root, &branch, !existing, &f2) });
        window
            .spawn(cx, async move |cx| {
                let res = work.await;
                let _ = cx.update(|window, cx| match res {
                    Ok(()) => {
                        let _ = me.update(cx, |ws, cx| {
                            ws.wt_forget();
                            ws.open_tab(Location::Path(folder), window, cx);
                        });
                    }
                    Err(text) => crate::prompt::notice(t!("The worktree could not be added"), text, window, cx),
                });
            })
            .detach();
        Ok(())
    });
    crate::prompt::show(
        t!("New worktree"),
        Some(t!("The folder for the new worktree. It must not exist yet, or be empty.").to_string()),
        Some((start.display().to_string(), String::new())),
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::{Worktree, current_of, default_folder};
    use std::path::{Path, PathBuf};

    #[test]
    fn folder_and_membership() {
        let main = Path::new("D:\\work\\app");
        assert_eq!(default_folder(main, "feature/x"), Path::new("D:\\work").join("app-feature-x"));
        let wt = |p: &str| Worktree { path: PathBuf::from(p), branch: None, head: String::new(), bare: false, locked: false, prunable: false };
        let list = vec![wt("D:/work/app"), wt("D:/work/app/sub-wt")];
        assert_eq!(current_of(&list, Path::new("d:\\work\\app\\src")), Some(0));
        assert_eq!(current_of(&list, Path::new("D:\\work\\app\\sub-wt\\x")), Some(1));
        assert_eq!(current_of(&list, Path::new("D:\\other")), None);
    }
}
