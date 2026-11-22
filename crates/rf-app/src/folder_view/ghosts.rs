//! Ghost rows (PLAN D5): in a folder of a repository whose project has agent sessions, the files deleted since the last commit show as faint
//! rows under the list, with "Restore" (the file comes back from the last commit or, never committed, from a checkpoint; never over a file that is there) and "Dismiss".
//!
//! They are display only and are not part of the list: they take no part in the selection, copy, delete, rename, the sort, the item
//! count or the size totals. Which session deleted a file is a best guess (a session works in this project); the tooltip says so.
//! Restore writes a file and records it as a created item, so Undo moves the restored file to the Trash again.
//! The content comes from the last commit; a file git never had comes from the newest checkpoint that holds it.

use std::path::PathBuf;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::FolderView;
use crate::t;
use crate::theme::Palette;

/// The most ghost rows shown at once; the rest is a count.
const MAX_ROWS: usize = 6;

/// One deleted file: its path from the repository's root (with `/`) and its full path.
struct Ghost {
    rel: String,
    full: PathBuf,
}

impl FolderView {
    /// The files of this folder that are deleted in the work tree and not dismissed, by name.
    fn ghosts(&self, cx: &App) -> Vec<Ghost> {
        let Some(git) = self.git.as_ref() else { return Vec::new() };
        if !cx.global::<crate::settings::Settings>().ghost_files {
            return Vec::new();
        }
        let Some(dir) = self.location().path() else { return Vec::new() };
        if crate::agents::sessions_over(cx, dir).is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Ghost> = git
            .files
            .iter()
            .filter(|(_, st)| **st == leaf_sys::git::FileState::Deleted)
            .filter_map(|(rel, _)| {
                let full = git.root.join(rel);
                (full.parent() == Some(dir) && !self.ghost_dismissed.contains(&full) && full.symlink_metadata().is_err())
                    .then(|| Ghost { rel: rel.to_string_lossy().replace('\\', "/"), full })
            })
            .collect();
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        out
    }

    /// The faint rows under the list, `None` when there is nothing to show.
    pub(super) fn ghost_strip(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ghosts = self.ghosts(cx);
        if ghosts.is_empty() {
            return None;
        }
        let more = ghosts.len().saturating_sub(MAX_ROWS);
        let root = self.git.as_ref()?.root.clone();
        let tip = t!("Deleted since the last commit; most likely by an agent (a best guess)");
        let mut strip = v_flex().w_full().flex_shrink_0().px(px(9.)).py_1().border_t_1().border_color(p.divider);
        for (i, g) in ghosts.into_iter().take(MAX_ROWS).enumerate() {
            let name = g.full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (rel, full, dismiss_full) = (g.rel.clone(), g.full.clone(), g.full.clone());
            let root = root.clone();
            strip = strip.child(
                h_flex()
                    .id(("ghost-row", i))
                    .h(px(24.))
                    .gap_2()
                    .items_center()
                    .text_size(px(p.fs_small))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().opacity(0.5).line_through().text_color(p.text_secondary).child(name))
                    .child(
                        div()
                            .id(("ghost-restore", i))
                            .px_2()
                            .rounded(crate::theme::RADIUS_SM)
                            .cursor_pointer()
                            .text_color(p.accent)
                            .hover(|s| s.bg(p.hover))
                            .child(t!("Restore"))
                            .on_click(cx.listener(move |this, _, _, cx| this.restore_ghost(root.clone(), rel.clone(), full.clone(), cx))),
                    )
                    .child(
                        div()
                            .id(("ghost-dismiss", i))
                            .px_2()
                            .rounded(crate::theme::RADIUS_SM)
                            .cursor_pointer()
                            .text_color(p.text_tertiary)
                            .hover(|s| s.bg(p.hover))
                            .child(t!("Dismiss"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ghost_dismissed.insert(dismiss_full.clone());
                                this.ghost_note = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if more > 0 {
            strip = strip.child(div().text_size(px(p.fs_small)).text_color(p.text_tertiary).child(format!("+ {more}")));
        }
        if let Some(n) = self.ghost_note.clone() {
            strip = strip.child(div().text_size(px(p.fs_small)).text_color(p.warning).child(n));
        }
        Some(strip.into_any_element())
    }

    /// Writes the file back from the last commit (off the UI thread); on success it counts as a created item for Undo.
    fn restore_ghost(&mut self, root: PathBuf, rel: String, full: PathBuf, cx: &mut Context<Self>) {
        self.ghost_note = None;
        let work = cx.background_executor().spawn(async move {
            match leaf_dev::gitread::restore_deleted(&root, &rel) {
                Ok(p) => Ok(p),
                // git has no such file (never committed): the newest checkpoint that holds it
                Err(e) if e.contains("there again") => Err(e),
                Err(e) => match crate::checkpoint_ui::store_of(&root) {
                    Ok(store) => leaf_dev::checkpoint::restore_missing(&store, &rel).map_err(|e2| if e2.contains("No checkpoint") { e } else { e2 }),
                    Err(_) => Err(e),
                },
            }
        });
        cx.spawn(async move |this, cx| {
            let r = work.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(path) => {
                        crate::undo::push(crate::undo::UndoOp::Created { path });
                        v.ghost_dismissed.insert(full);
                        v.refresh(cx);
                    }
                    Err(e) => v.ghost_note = Some(format!("{} {e}", t!("The file was not restored."))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}
