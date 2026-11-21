//! Context baskets in the app: add the selection to the project's current basket, take it out again, start a named one, and
//! hand a basket over as one block ("Copy as context", the dialog that exists) or as `@path` mentions. The sets live in
//! `baskets.json` (`rf_core::basket`); the file is read and written around each action, it is small.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use rf_core::basket::{Baskets, project_of};

use crate::fluent_menu::{Item, MenuIcon};
use crate::prompt::{self, Button, Kind};
use crate::t;
use gpui_kit::assets::IconName as L;

fn file() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("baskets.json"))
}

fn load() -> Baskets {
    file().map(|f| Baskets::load(&f)).unwrap_or_default()
}

fn save(b: &Baskets) {
    if let Some(f) = file() {
        b.save(&f);
    }
}

/// Adds the items to the active basket of their project.
pub fn add(items: &[PathBuf]) {
    let Some(first) = items.first() else { return };
    let project = project_of(first);
    let mut all = load();
    let name = all.active_name(&project);
    all.add(&project, &name, items);
    all.set_active(&project, &name);
    save(&all);
}

/// Baskets follow renames and moves (nothing is read when no basket was ever made).
pub fn follow(pairs: &[(PathBuf, PathBuf)]) {
    let Some(f) = file().filter(|f| f.exists()) else { return };
    let mut all = Baskets::load(&f);
    all.moved(pairs);
    all.save(&f);
}

/// Takes the items out of every basket of their project that holds them.
pub fn remove(items: &[PathBuf]) {
    let Some(first) = items.first() else { return };
    let project = project_of(first);
    let mut all = load();
    for n in all.names(&project) {
        all.remove(&project, &n, items);
    }
    save(&all);
}

/// Asks for a name, makes that basket the project's current one and puts the items (if any) in it.
pub fn new_named(project: PathBuf, items: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    let save_btn = Button::new(t!("Create"), Kind::Primary, move |text, _, _| {
        let name = text.unwrap_or_default().trim().to_string();
        if name.is_empty() {
            return Err(t!("The name can't be empty").to_string());
        }
        let mut all = load();
        all.add(&project, &name, &items);
        all.set_active(&project, &name);
        save(&all);
        Ok(())
    });
    prompt::show(t!("New basket"), Some(t!("A named set of files for this project.").to_string()), Some((String::new(), t!("name").to_string())), vec![save_btn, Button::cancel()], window, cx);
}

/// The project's current basket and the project, for a folder or file path.
fn current(at: &Path) -> (PathBuf, String, Vec<PathBuf>) {
    let project = project_of(at);
    let all = load();
    let name = all.active_name(&project);
    let paths = all.get(&project, &name).map(|b| b.paths.clone()).unwrap_or_default();
    (project, name, paths)
}

/// The basket as one text block, through the "Copy as context" dialog.
pub fn copy_as_context(at: &Path, window: &mut Window, cx: &mut App) {
    let (project, _, paths) = current(at);
    if paths.is_empty() {
        return prompt::notice(t!("Basket"), t!("The basket is empty.").to_string(), window, cx);
    }
    crate::context_copy::start(project, paths, window, cx);
}

/// The basket as `@path` mentions, one per line.
pub fn copy_mentions(at: &Path, cx: &mut App) {
    let project = project_of(at);
    let all = load();
    let name = all.active_name(&project);
    if let Some(b) = all.get(&project, &name).filter(|b| !b.paths.is_empty()) {
        cx.write_to_clipboard(ClipboardItem::new_string(b.mentions()));
    }
}

/// "about 3,200 tokens (estimate)" of the files in the basket (folders are not counted: their size is only known when read).
fn tokens(paths: &[PathBuf]) -> String {
    let bytes: u64 = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
    leaf_dev::context::tokens_phrase(leaf_dev::context::estimate_tokens(bytes))
}

/// The "Basket" submenu of the right-click menu (`items` are the selected paths, `dir` the folder when nothing is selected).
pub fn menu_items(items: &[PathBuf], dir: &Path) -> Vec<Item> {
    let at = items.first().map(PathBuf::as_path).unwrap_or(dir);
    let project = project_of(at);
    let all = load();
    let name = all.active_name(&project);
    let held = all.get(&project, &name).map(|b| b.paths.clone()).unwrap_or_default();
    let icon = |l: L| MenuIcon::Lucide(l);
    let mut v = Vec::new();
    if !items.is_empty() {
        let it = items.to_vec();
        v.push(Item::action(format!("{} \"{name}\"", t!("Add to")), icon(L::Plus), move |_, _| add(&it)));
        let it = items.to_vec();
        v.push(Item::action(t!("Remove from basket"), icon(L::Minus), move |_, _| remove(&it)).enabled(!held.is_empty()));
        let (p, it) = (project.clone(), items.to_vec());
        v.push(Item::action(t!("New basket…"), icon(L::FolderPlus), move |w, cx| new_named(p.clone(), it.clone(), w, cx)));
    } else {
        let p = project.clone();
        v.push(Item::action(t!("New basket…"), icon(L::FolderPlus), move |w, cx| new_named(p.clone(), Vec::new(), w, cx)));
    }
    for other in all.names(&project).into_iter().filter(|n| *n != name) {
        let p = project.clone();
        let label = format!("{} \"{other}\"", t!("Switch to"));
        v.push(Item::action(label, icon(L::ArrowLeftRight), move |_, _| {
            let mut all = load();
            all.set_active(&p, &other);
            save(&all);
        }));
    }
    let n = held.len();
    let (a, b) = (at.to_path_buf(), at.to_path_buf());
    v.push(Item::action(format!("{} ({n}, {})", t!("Copy basket as context"), tokens(&held)), icon(L::FileText), move |w, cx| copy_as_context(&a, w, cx)).enabled(n > 0));
    v.push(Item::action(t!("Copy basket as @paths"), icon(L::AtSign), move |_, cx| copy_mentions(&b, cx)).enabled(n > 0));
    v
}
