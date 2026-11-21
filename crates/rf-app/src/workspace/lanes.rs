//! Lanes in the window (PLAN F1, D6): the commands "New lane…" and "End lane…", the Lanes section of the sidebar and the
//! right-click menu of a lane row. The records are `crate::lanes`, the git side `leaf_dev::lanes`.
//!
//! New lane: a name, a base (the current head or another branch) and an agent to start (none, Claude Code, or one of the user's
//! own terminal actions); then `git worktree add` beside the repository, a new tab on it, and the agent started there through the
//! custom-action path (a program in a terminal, never text typed for the user).
//! End lane: what is in the lane first (commits ahead, uncommitted files), then Review, Merge, Remove or Discard.

use std::path::{Path, PathBuf};

use leaf_dev::gitread;
use leaf_dev::lanes::{LaneError, Merged};
use rf_core::actions::CustomAction;

use super::*;
use crate::lanes::Lane;

/// How many lines of a list a dialog shows before "and N more".
const SHOWN: usize = 8;

fn listed(v: &[String]) -> String {
    let mut s: String = v.iter().take(SHOWN).map(|l| format!("\n  {l}")).collect();
    if v.len() > SHOWN {
        s.push_str(&format!("\n  {} {} {}", t!("and"), v.len() - SHOWN, t!("more")));
    }
    s
}

/// A lane error in leaf's words; git's own text (or the list) after it.
pub(crate) fn error_text(e: &LaneError) -> String {
    match e {
        LaneError::Uncommitted(v) => format!("{}{}", t!("The lane has files that are not committed. Commit them in the lane, or discard the lane."), listed(v)),
        LaneError::BaseUncommitted(v) => format!("{}{}", t!("The folder of the base branch has changes that are not committed. Commit or put them away there first."), listed(v)),
        LaneError::Unmerged(v) => format!("{}{}", t!("The lane has commits its base branch does not have. Merge the lane first, or discard it."), listed(v)),
        LaneError::Conflicts(v) => format!("{}{}", t!("Merging would conflict in these files. Nothing was changed. Merge by hand, or bring the lane up to date with its base first."), listed(v)),
        LaneError::BaseNotCheckedOut(b) => format!("{}\n  {b}", t!("The base branch is not checked out in any folder of the repository, so there is nowhere to merge. Check it out first.")),
        LaneError::NotALane(s) => format!("{}\n  {s}", t!("This folder is not a worktree of the lane's repository. Nothing was touched.")),
        LaneError::Git { what, detail } => format!("{}\n  {what}: {detail}", t!("git refused.")),
    }
}

/// The agents a new lane can start: none, Claude Code, and the user's own actions that run in a terminal and need no selection.
fn agent_presets(cx: &App) -> Vec<(String, Option<CustomAction>)> {
    let claude = CustomAction { name: "Claude Code".into(), command: "claude".into(), terminal: true, icon: Some("terminal".into()), ..Default::default() };
    let mut v = vec![(t!("No agent").to_string(), None), (claude.name.clone(), Some(claude))];
    for a in crate::custom_actions::list(cx) {
        if v.len() >= 5 {
            break;
        }
        if a.terminal && !a.command.contains("{path}") && !a.command.contains("{files}") && !v.iter().any(|(n, _)| *n == a.name) {
            v.push((a.name.clone(), Some(a)));
        }
    }
    v
}

fn off_notice(window: &mut Window, cx: &mut App) {
    crate::prompt::notice(t!("Lanes are off"), t!("Lanes are part of the agent layer. Turn it on in Settings under Developer.").to_string(), window, cx);
}

impl Workspace {
    /// Review a lane's work: the Review side (F3) of the lane folder's change feed page, in a new tab.
    pub(super) fn open_review(&mut self, dir: PathBuf, _window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab().downgrade();
        crate::review_view::show(dir, tab, cx);
    }

    /// The command "New lane…".
    pub(super) fn new_lane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::prompt::{Button, Choice, Kind};
        if !crate::lanes::on(cx) {
            return off_notice(window, cx);
        }
        let dir = self.active_tab().read(cx).current_dir();
        let Some(root) = dir.as_deref().and_then(leaf_dev::gitignore::repo_root) else {
            crate::prompt::notice(t!("Not in a git repository"), t!("A lane is a new worktree of a git repository. This folder is not in one.").to_string(), window, cx);
            return;
        };
        let list = gitread::worktrees(&root).unwrap_or_default();
        let main = list.first().map_or_else(|| root.clone(), |w| w.path.clone());
        // the head of the folder shown: its branch, else its commit
        let head = gitread::current_branch(&root).or_else(|| list.iter().find(|w| leaf_dev::lanes::same_path(&w.path, &root)).map(|w| w.head.clone())).unwrap_or_else(|| "HEAD".into());
        let presets = agent_presets(cx);
        let pattern = cx.global::<crate::settings::Settings>().lane_folder.clone();
        let base_sel = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let agent_sel = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let head_label: String = if head.len() == 40 { head.chars().take(7).collect() } else { head.clone() };
        let choices = vec![
            Choice { label: t!("Base").into(), options: vec![format!("{} ({head_label})", t!("Current head")).into(), t!("Other branch…").into()], selected: base_sel.clone() },
            Choice { label: t!("Agent").into(), options: presets.iter().map(|(n, _)| n.clone().into()).collect(), selected: agent_sel.clone() },
        ];
        let message = format!(
            "{}\n{} {}",
            t!("A lane is a new worktree on a branch of its own, for one piece of work. Nothing that exists is changed."),
            t!("Folder:"),
            pattern.replace("<repo>", &main.to_string_lossy())
        );
        let me = cx.entity().downgrade();
        let create = Button::new(t!("Create"), Kind::Primary, move |text, window, cx| {
            let name = text.unwrap_or_default().trim().to_string();
            if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) {
                return Err(t!("Enter a name without spaces; it is also the branch name").to_string());
            }
            if !leaf_dev::lanes::valid_branch(&main, &name) {
                return Err(t!("git does not accept this as a branch name").to_string());
            }
            if gitread::local_branches(&main).unwrap_or_default().iter().any(|b| *b == name) {
                return Err(t!("A branch with this name exists already").to_string());
            }
            let folder = crate::lanes::folder_for(&pattern, &main, &name);
            if folder.exists() && folder.read_dir().map(|mut d| d.next().is_some()).unwrap_or(true) {
                return Err(format!("{} {}", t!("The folder exists and is not empty:"), folder.display()));
            }
            let agent = presets.get(agent_sel.get()).and_then(|(_, a)| a.clone());
            let (main, me, head) = (main.clone(), me.clone(), head.clone());
            if base_sel.get() == 1 {
                window.defer(cx, move |window, cx| base_dialog(main, name, folder, agent, me, window, cx));
            } else {
                create_lane(main, name, head, folder, agent, me, window, cx);
            }
            Ok(())
        });
        crate::prompt::show_with(t!("New lane"), Some(message), Some((String::new(), t!("Lane name").to_string())), choices, vec![create, Button::cancel()], window, cx);
    }

    /// The command "End lane…": for the lane the shown folder is in.
    pub(super) fn end_lane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::lanes::on(cx) {
            return off_notice(window, cx);
        }
        let dir = self.active_tab().read(cx).current_dir();
        match dir.as_deref().and_then(|d| crate::lanes::of_dir(cx, d)) {
            Some(lane) => self.end_lane_for(lane, window, cx),
            None => crate::prompt::notice(t!("Not in a lane"), t!("End lane works in a lane's folder. Open one from Lanes in the sidebar, or start one with New lane.").to_string(), window, cx),
        }
    }

    /// Reads what ending `lane` would meet (off the UI thread), then shows the choices.
    pub(super) fn end_lane_for(&mut self, lane: Lane, window: &mut Window, cx: &mut Context<Self>) {
        let l = lane.clone();
        let work = cx.background_spawn(async move { leaf_dev::lanes::end_info(&l.repo, &l.path, &l.base) });
        let me = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let info = work.await;
                let _ = cx.update(|window, cx| end_dialog(lane, info, me, window, cx));
            })
            .detach();
    }

    /// The Lanes section of the sidebar: one row per lane with its colour, branch, progress and agent state.
    pub(super) fn lanes_sidebar(&mut self, mut col: Stateful<Div>, head: Div, cur: &Location, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let lanes = crate::lanes::list(cx);
        if lanes.is_empty() {
            return col;
        }
        crate::lanes::poll(cx);
        col = col.child(
            head.child(div().flex_1()).child(
                div()
                    .id("sb-lane-add")
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .hover(|s| s.bg(p.hover).text_color(p.text))
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(t!("New lane…")).build(window, cx))
                    .child(Icon::new(L::Plus).size(px(14.)))
                    .on_click(cx.listener(|_, _, window, cx| {
                        cx.stop_propagation();
                        window.dispatch_action(Box::new(crate::actions::NewLane), cx);
                    })),
            ),
        );
        for (i, l) in lanes.iter().enumerate() {
            let agent = crate::agents::tab_state(cx, &l.path);
            let mut parts: Vec<String> = Vec::new();
            if l.branch != l.name {
                parts.push(l.branch.clone());
            }
            if let Some(st) = agent {
                parts.push(crate::agents::state_text(st).to_lowercase());
            }
            match crate::lanes::progress(cx, &l.path) {
                Some(Some(pr)) => {
                    if pr.ahead > 0 || pr.behind > 0 {
                        parts.push(format!("\u{2191}{} \u{2193}{}", pr.ahead, pr.behind));
                    }
                    if pr.changed > 0 {
                        parts.push(format!("{} {}", pr.changed, if pr.changed == 1 { t!("file changed") } else { t!("files changed") }));
                    } else if parts.is_empty() {
                        parts.push(t!("clean").to_string());
                    }
                }
                Some(None) => parts.push(t!("folder missing").to_string()),
                None => {}
            }
            let second = h_flex()
                .gap(px(5.))
                .items_center()
                .min_w_0()
                .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(crate::lanes::color(l.color, p)))
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(p.fs_xs))
                        .line_height(px(p.fs_xs * 1.25))
                        .text_color(p.text_tertiary)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(parts.join(" \u{b7} ")),
                )
                .into_any_element();
            let loc = Location::Path(l.path.clone());
            let active = *cur == loc;
            let item = self.sidebar_item(("sb-lane", i).into(), L::GitBranch, p.text_secondary, l.name.clone(), loc, active, Some(second), p, cx);
            col = col.child(item.when_some(agent, |d, st| d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(crate::agents::state_color(st, p)))));
        }
        col
    }

    /// The right-click menu of a lane row.
    pub(super) fn lane_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        use crate::fluent_menu::{Item, Menu, MenuIcon};
        let Some(lane) = crate::lanes::list(cx).into_iter().find(|l| l.path == path) else {
            return self.sidebar_menu(path, window, cx);
        };
        let me = cx.entity().downgrade();
        let with = |f: fn(&mut Workspace, Lane, &mut Window, &mut Context<Workspace>)| {
            let (me, lane) = (me.clone(), lane.clone());
            move |w: &mut Window, cx: &mut App| {
                let l = lane.clone();
                let _ = me.update(cx, |ws, cx| f(ws, l, w, cx));
            }
        };
        let items = vec![
            Item::action(t!("Open in new tab"), MenuIcon::Lucide(L::SquarePlus), with(|ws, l, w, cx| ws.open_tab(Location::Path(l.path), w, cx))),
            Item::action(t!("Open terminal here"), MenuIcon::Lucide(L::SquareTerminal), with(|_, l, _, cx| crate::terminal::open(l.path, cx))),
            Item::Separator,
            Item::action(t!("Review changes"), MenuIcon::Lucide(L::GitCompare), with(|ws, l, w, cx| ws.open_review(l.path, w, cx))),
            Item::action(t!("End lane…"), MenuIcon::Lucide(L::GitMerge), with(|ws, l, w, cx| ws.end_lane_for(l, w, cx))),
            Item::action(t!("Discard lane…"), MenuIcon::Lucide(L::Trash), with(|_, l, w, cx| {
                let me = cx.entity().downgrade();
                discard_dialog(l, me, w, cx);
            })),
        ];
        crate::fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }
}

/// "Other branch…": the base to start the lane from.
fn base_dialog(main: PathBuf, name: String, folder: PathBuf, agent: Option<CustomAction>, me: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let branches = gitread::local_branches(&main).unwrap_or_default();
    let shown: Vec<&str> = branches.iter().take(12).map(String::as_str).collect();
    let message = format!("{}{}", t!("Local branches: "), if branches.is_empty() { "-".to_string() } else { format!("{}{}", shown.join(", "), if branches.len() > 12 { ", ..." } else { "" }) });
    let go = Button::new(t!("Create"), Kind::Primary, move |text, window, cx| {
        let base = text.unwrap_or_default().trim().to_string();
        if !branches.iter().any(|b| *b == base) {
            return Err(t!("There is no local branch with this name").to_string());
        }
        create_lane(main.clone(), name.clone(), base, folder.clone(), agent.clone(), me.clone(), window, cx);
        Ok(())
    });
    crate::prompt::show(t!("Base of the new lane"), Some(message), Some((String::new(), t!("Branch name").to_string())), vec![go, Button::cancel()], window, cx);
}

/// `git worktree add` off the interface's thread; then the record, a tab on the lane, and the agent.
#[allow(clippy::too_many_arguments)]
fn create_lane(main: PathBuf, name: String, base: String, folder: PathBuf, agent: Option<CustomAction>, me: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let (m, n, b, f) = (main.clone(), name.clone(), base.clone(), folder.clone());
    let work = cx.background_spawn(async move { leaf_dev::lanes::add(&m, &n, &b, &f) });
    window
        .spawn(cx, async move |cx| {
            let res = work.await;
            let _ = cx.update(|window, cx| match res {
                Ok(()) => {
                    let lane = Lane {
                        name: name.clone(),
                        repo: main,
                        path: folder.clone(),
                        branch: name,
                        base,
                        color: crate::lanes::next_color(cx),
                        created_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64),
                    };
                    crate::lanes::add(lane, cx);
                    let _ = me.update(cx, |ws, cx| {
                        ws.wt_forget();
                        ws.open_tab(Location::Path(folder.clone()), window, cx);
                    });
                    if let Some(a) = agent {
                        crate::custom_actions::run(a, Vec::new(), Some(folder), window, cx);
                    }
                }
                Err(e) => crate::prompt::notice(t!("The lane could not be made"), error_text(&e), window, cx),
            });
        })
        .detach();
}

/// What ending the lane meets, and the four ways to end it.
fn end_dialog(lane: Lane, info: leaf_dev::lanes::EndInfo, me: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let mut msg = format!("{} {} \u{2192} {}\n{}", t!("Branch"), lane.branch, lane.base, lane.path.display());
    if !info.is_lane {
        msg.push_str(&format!("\n\n{}", t!("This folder is no longer a worktree of the repository. Discard forgets the lane.")));
    }
    if info.ahead.is_empty() {
        msg.push_str(&format!("\n\n{}", t!("No commits that the base does not have.")));
    } else {
        msg.push_str(&format!("\n\n{} ({}):{}", t!("Commits not in the base"), info.ahead.len(), listed(&info.ahead)));
    }
    if info.behind > 0 {
        msg.push_str(&format!("\n{} {}", info.behind, t!("commits of the base are not in the lane.")));
    }
    if info.uncommitted.is_empty() {
        msg.push_str(&format!("\n{}", t!("No uncommitted files.")));
    } else {
        msg.push_str(&format!("\n\n{} ({}):{}", t!("Uncommitted files"), info.uncommitted.len(), listed(&info.uncommitted)));
    }
    let review = Button::new(t!("Review"), Kind::Normal, {
        let (me, path) = (me.clone(), lane.path.clone());
        move |_, window, cx| {
            let _ = me.update(cx, |ws, cx| ws.open_review(path.clone(), window, cx));
            Ok(())
        }
    });
    let merge = Button::new(t!("Merge into base"), Kind::Primary, {
        let (lane, info) = (lane.clone(), info.clone());
        move |_, window, cx| {
            if !info.uncommitted.is_empty() {
                return Err(t!("Commit or discard the lane's uncommitted files first.").to_string());
            }
            if info.ahead.is_empty() {
                return Err(t!("Nothing to merge: the base has every commit of the lane.").to_string());
            }
            if info.base_dir.is_none() {
                return Err(t!("The base branch is not checked out in any folder of the repository.").to_string());
            }
            if !info.base_dirty.is_empty() {
                return Err(t!("The base branch's folder has uncommitted changes.").to_string());
            }
            let l = lane.clone();
            let work = cx.background_spawn(async move { leaf_dev::lanes::merge(&l.repo, &l.path, &l.branch, &l.base) });
            let base = lane.base.clone();
            window
                .spawn(cx, async move |cx| {
                    let res = work.await;
                    let _ = cx.update(|window, cx| {
                        crate::lanes::refresh(cx);
                        match res {
                            Ok(how) => {
                                let how = match how {
                                    Merged::FastForward => t!("fast-forward"),
                                    Merged::MergeCommit => t!("with a merge commit"),
                                    Merged::Nothing => t!("nothing was new"),
                                };
                                crate::prompt::notice(t!("Lane merged"), format!("{} {base} ({how}). {}", t!("Merged into"), t!("The lane stays until you remove it.")), window, cx);
                            }
                            Err(e) => crate::prompt::notice(t!("The lane was not merged"), error_text(&e), window, cx),
                        }
                    });
                })
                .detach();
            Ok(())
        }
    });
    let remove = Button::new(t!("Remove"), Kind::Normal, {
        let (lane, info) = (lane.clone(), info.clone());
        move |_, window, cx| {
            if !info.uncommitted.is_empty() {
                return Err(format!("{}{}", t!("Remove keeps work safe: these files are not committed."), listed(&info.uncommitted)));
            }
            if !info.ahead.is_empty() {
                return Err(format!("{}{}", t!("Remove keeps work safe: these commits are not in the base. Merge first, or discard the lane."), listed(&info.ahead)));
            }
            let l = lane.clone();
            let work = cx.background_spawn(async move { leaf_dev::lanes::remove(&l.repo, &l.path, &l.branch, &l.base) });
            let (path, branch) = (lane.path.clone(), lane.branch.clone());
            window
                .spawn(cx, async move |cx| {
                    let res = work.await;
                    let _ = cx.update(|window, cx| match res {
                        Ok(deleted) => {
                            crate::lanes::forget(&path, cx);
                            let b = if deleted { t!("Its branch was merged and is deleted:") } else { t!("Its branch is kept:") };
                            crate::prompt::notice(t!("Lane removed"), format!("{} {branch}", b), window, cx);
                        }
                        Err(e) => crate::prompt::notice(t!("The lane was not removed"), error_text(&e), window, cx),
                    });
                })
                .detach();
            Ok(())
        }
    });
    let discard = Button::new(t!("Discard…"), Kind::Danger, {
        let (lane, me) = (lane.clone(), me.clone());
        move |_, window, cx| {
            let (lane, me) = (lane.clone(), me.clone());
            window.defer(cx, move |window, cx| discard_dialog(lane, me, window, cx));
            Ok(())
        }
    });
    crate::prompt::show(format!("{} {}", t!("End lane"), lane.name), Some(msg), None, vec![review, merge, remove, discard, Button::cancel()], window, cx);
}

/// "Discard lane": asks for the lane's name typed, then a Status Center job moves the worktree folder to the Trash and prunes.
fn discard_dialog(lane: Lane, _me: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let msg = format!(
        "{}\n  {}\n{} {}\n\n{}",
        t!("Everything in the lane's folder goes to the Trash, also what is not committed:"),
        lane.path.display(),
        t!("Its branch is kept unless it is merged:"),
        lane.branch,
        t!("Type the lane's name to discard it."),
    );
    let name = lane.name.clone();
    let go = Button::new(t!("Discard"), Kind::Danger, move |text, _, cx| {
        if text.unwrap_or_default().trim() != lane.name {
            return Err(t!("Type the lane's name exactly").to_string());
        }
        let l = lane.clone();
        let path = lane.path.clone();
        crate::status_center::start_with(
            cx,
            format!("{} {}", t!("Discarding lane:"), lane.name),
            move |t| {
                let put_away = |p: &Path| -> Result<(), String> {
                    match leaf_sys::fileops::delete_tracked(&[p.to_path_buf()], false, t.clone()) {
                        Ok(true) => Ok(()),
                        Ok(false) => Err(t!("stopped").to_string()),
                        Err(e) => Err(format!("{e:#}")),
                    }
                };
                match leaf_dev::lanes::discard(&l.repo, &l.path, &l.branch, &put_away) {
                    Ok(_) => Ok(true),
                    // the folder is gone and git does not know it any more: only the record is left to forget
                    Err(LaneError::NotALane(_)) if std::fs::symlink_metadata(&l.path).is_err() => Ok(true),
                    Err(e) => Err(anyhow::anyhow!("{}: {}", t!("The lane was not discarded"), error_text(&e))),
                }
            },
            Some(Box::new(move |ok, cx| {
                if ok {
                    crate::lanes::forget(&path, cx);
                }
            })),
        );
        Ok(())
    });
    crate::prompt::show(t!("Discard lane"), Some(msg), Some((String::new(), name)), vec![go, Button::cancel()], window, cx);
}
