//! The file-list context menu (Files style): quick action buttons, our own commands, and the
//! shell's items loaded in the background into "Open with", "New" and "Show more options".
//! Shift+right-click still opens the classic Explorer menu.

use std::path::PathBuf;
use std::rc::Rc;

use futures::channel::oneshot;
use gpui_kit::assets::IconName as L;
use gpui_kit::*;
use rf_core::{Location, SortDir, SortKey};
use leaf_sys::ctxmenu::{MenuOutcome, ShellItem, ShellTarget};
use leaf_sys::fileops::Transfer;

use crate::fluent_menu::{self, Item, Menu, MenuIcon, Pending, Quick, Sub};
use crate::folder_view::{FolderEvent, FolderView, GroupBy, ViewMode};
use crate::menu_model::{Entry, MenuContext, MenuPlace, Slot, Target, plan};
use crate::t;

/// Shell verbs we show as our own commands (or that make no sense here), hidden from
/// "Show more options" — the same idea as Files' filter list.
const HIDDEN_VERBS: &[&str] = &[
    "open",
    "opennewwindow",
    "opennewtab",
    "opennewprocess",
    "explore",
    "cut",
    "copy",
    "paste",
    "pastelink",
    "delete",
    "rename",
    "properties",
    "link",
    "copyaspath",
    "pintohome",
    "pintohomefile",
    "windows.modernshare",
    "windows.share",
    "openas",
    "newfolder",
    // Windows Terminal's "Open in Terminal" (we have our own)
    "{9f156763-7844-4dc4-b2b1-901f640f5155}",
];

type Me = WeakEntity<FolderView>;

/// A menu action that runs on the folder view.
fn act(me: &Me, f: impl Fn(&mut FolderView, &mut Window, &mut Context<FolderView>) + 'static) -> impl Fn(&mut Window, &mut App) + 'static {
    let me = me.clone();
    move |w, cx| {
        let _ = me.update(cx, |v, cx| f(v, w, cx));
    }
}

fn icon(l: L) -> MenuIcon {
    MenuIcon::Lucide(l)
}

fn cursor() -> (i32, i32) {
    let pt = leaf_sys::input::cursor_pos();
    (pt.x, pt.y)
}

/// What the builders of a menu's entries read: the view, the folder the menu acts on, the user's own actions.
struct Build<'a> {
    me: &'a Me,
    v: &'a FolderView,
    cx: &'a App,
    ctx: &'a MenuContext,
    /// The folder the menu acts on: the one of the list, or the selected folder.
    dir: PathBuf,
    /// The selection (empty on the background).
    items: &'a [PathBuf],
    own: Option<Item>,
}

/// The items of a menu: the rows of `menu_model` that this context shows, each made by its builder.
fn generate(b: &Build) -> Vec<Item> {
    plan(b.ctx)
        .into_iter()
        .map(|e| match e {
            Entry::Separator => Item::Separator,
            Entry::Row(p) => {
                let item = build_item(p, b);
                if p.hint.is_empty() { item } else { item.hint(p.hint) }
            }
        })
        .collect()
}

/// The entry of one row: its label and icon from the row, what it does from here.
fn build_item(p: &MenuPlace, b: &Build) -> Item {
    let (me, v, cx) = (b.me, b.v, b.cx);
    let (label, ic) = (p.label(b.ctx), icon(p.icon));
    let d = b.dir.clone();
    match p.slot {
        Slot::Layout => Item::sub(label, ic, layout_items(me, v.view_mode, v.grid_style)),
        Slot::Sort => Item::sub(label, ic, sort_items(me, v.sort, v.sort_offers(cx))),
        Slot::GroupBy => {
            let mut group_menu = group_items(me, v.group);
            group_menu.extend(v.property_group_items(me, cx));
            Item::sub(label, ic, group_menu)
        }
        Slot::Refresh => Item::action(label, ic, act(me, |v, _, cx| v.refresh(cx))),
        // what Ctrl+Z and Ctrl+Y would do, when there is something (issue 55: Explorer's "Undo Delete")
        Slot::Undo => Item::action(label, ic, |_, cx| crate::undo::undo(cx)),
        Slot::Redo => Item::action(label, ic, |_, cx| crate::undo::redo(cx)),
        Slot::New => Item::Sub { label: label.into(), icon: ic, items: Sub::Pending { key: "new", fallback: Some(Rc::new(own_new_items(me))) }, click: None },
        Slot::PasteShortcut => Item::action(label, ic, act(me, |v, w, cx| v.paste_shortcuts(w, cx))).enabled(leaf_sys::clipboard::has_files()),
        Slot::Pin => Item::action(label, ic, act(me, move |v, _, cx| v.pin(d.clone(), cx))),
        Slot::CopyPath => Item::action(label, ic, act(me, |v, _, cx| v.copy_path(cx))),
        Slot::CopyAsContext => Item::action(label, ic, act(me, |v, w, cx| v.copy_as_context(w, cx))),
        Slot::DiskUsage => Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::OpenInNewTab(Location::DiskUsage(d.clone()))))),
        Slot::FindDuplicates => Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::OpenInNewTab(Location::Duplicates(vec![d.clone()]))))),
        Slot::FindBuildJunk => Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::OpenInNewTab(Location::BuildJunk(d.clone()))))),
        Slot::Checkpoint => Item::action(label, ic, act(me, move |_, w, cx| crate::checkpoint_ui::new_checkpoint(d.clone(), cx.entity().downgrade(), w, cx))),
        Slot::SyncWith => Item::action(label, ic, |w, cx| w.dispatch_action(Box::new(crate::actions::SyncWith), cx)),
        Slot::ShowHidden => Item::action(label, ic, act(me, |v, _, cx| v.toggle_hidden(cx))).checked(v.filter.show_hidden),
        Slot::HideIgnored => Item::action(label, ic, |w, cx| w.dispatch_action(Box::new(crate::actions::ToggleHideIgnored), cx))
            .checked(cx.global::<crate::settings::Settings>().hide_ignored),
        Slot::Terminal => Item::action(label, ic, move |_, cx| open_terminal(d.clone(), cx)),
        // the folder organizer: only where the folder has rules
        Slot::OrganizeFolder => Item::action(label, ic, move |w, cx| crate::organizer::organize_folder(d.clone(), w, cx)),
        // Git (Files: initialize / clone from the folder background)
        Slot::Git => {
            let mut git_items = Vec::new();
            if v.git.is_none() {
                let (m, d) = (me.clone(), d.clone());
                git_items.push(Item::action(t!("Initialize Git repository"), icon(L::GitBranchPlus), move |_, cx| crate::git_ui::init_repo(m.clone(), d.clone(), cx)));
            }
            let m = me.clone();
            git_items.push(Item::action(t!("Clone repository…"), icon(L::GitFork), move |w, cx| crate::git_ui::clone_dialog(m.clone(), d.clone(), w, cx)));
            Item::sub(label, ic, git_items)
        }
        Slot::Open => Item::action(label, ic, act(me, |v, _, cx| v.open_selected(cx))),
        Slot::OpenNewTab | Slot::OpenNewWindow => {
            // (one archive that opens like a folder opens itself; a folder, the selected one)
            let opened = if b.ctx.single_archive { b.items[0].clone() } else { d };
            let new_tab = p.slot == Slot::OpenNewTab;
            Item::action(label, ic, act(me, move |_, _, cx| {
                let at = Location::Path(opened.clone());
                cx.emit(if new_tab { FolderEvent::OpenInNewTab(at) } else { FolderEvent::OpenInNewWindow(at) })
            }))
        }
        Slot::OpenOtherPane => Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::OpenInOtherPane(Location::Path(d.clone()))))),
        Slot::OpenWith => Item::Sub { label: label.into(), icon: ic, items: Sub::Pending { key: "openwith", fallback: None }, click: None },
        // Explorer's "Send to" in the first menu (issue 117): its entries come with the shell's items (see `show_context_menu`)
        Slot::SendTo => Item::Sub { label: label.into(), icon: ic, items: Sub::Pending { key: "sendto", fallback: None }, click: None },
        Slot::OpenLocation => {
            let p = b.items[0].clone();
            Item::action(label, ic, act(me, move |v, _, cx| {
                if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
                    v.set_reselect(name.to_string_lossy().into_owned());
                    v.navigate(Location::Path(parent.to_path_buf()), cx);
                }
            }))
        }
        Slot::CopyPathAs => Item::sub(label, ic, copy_path_items(me, b.items)),
        Slot::Duplicate => Item::action(label, ic, act(me, |v, _, cx| v.duplicate_selected(cx))),
        Slot::Shortcut => {
            let link_items = b.items.to_vec();
            Item::action(label, ic, move |_, _| create_shortcuts(link_items.clone()))
        }
        Slot::FolderWithSelection => {
            let sel = b.items.to_vec();
            Item::action(label, ic, act(me, move |v, _, cx| v.folder_with_selection(sel.clone(), cx)))
        }
        Slot::Alias => Item::action(label, ic, move |w, cx| crate::aliases::edit_dialog(d.clone(), w, cx)),
        // leave a folder (a build output, a huge cache) out of the search index
        Slot::IndexOut => {
            let key = d.to_string_lossy().trim_end_matches('\\').to_string();
            let out = cx.global::<crate::settings::Settings>().index_excluded.iter().any(|e| rf_core::pathkey::name_eq(e, &key));
            Item::action(label, ic, move |_, cx| {
                let key = key.clone();
                crate::settings::update(cx, |s| {
                    if out {
                        s.index_excluded.retain(|e| !rf_core::pathkey::name_eq(e, &key));
                    } else {
                        s.index_excluded.push(key);
                    }
                });
            })
            .checked(out)
        }
        Slot::Flatten => Item::action(label, ic, act(me, move |v, w, cx| v.flatten_folder(d.clone(), w, cx))),
        // type-specific actions (Files: rotate, set as background, install font, run as administrator)
        Slot::RotateRight => Item::action(label, ic, act(me, |v, w, cx| v.rotate_selection(true, w, cx))),
        Slot::RotateLeft => Item::action(label, ic, act(me, |v, w, cx| v.rotate_selection(false, w, cx))),
        Slot::Wallpaper => {
            let p = b.items[0].clone();
            Item::action(label, ic, move |_, _| {
                let p = p.clone();
                std::thread::spawn(move || {
                    // A picture inside an archive has no file of its own: Windows was given the path inside the archive, could not
                    // read it and left a plain colour (the owner, 2026-11-18: the background turned purple and pink). It is
                    // extracted first, into the app's own folder (not the temp folder, which is cleaned after two days: the
                    // background must still be there next week).
                    let Some(real) = wallpaper_file(&p) else {
                        log::warn!("wallpaper: the picture could not be extracted from its archive");
                        return;
                    };
                    if let Err(e) = leaf_sys::fileactions::set_wallpaper(&real) {
                        log::warn!("wallpaper: {e:#}");
                    }
                });
            })
        }
        Slot::RunAdmin => {
            let p = b.items[0].clone();
            Item::action(label, ic, move |_, _| {
                let p = p.clone();
                leaf_sys::sta::run(move |owner| {
                    if let Err(e) = leaf_sys::ctxmenu::invoke_verb(owner, &ShellTarget::One(p), "runas", cursor()) {
                        log::warn!("runas: {e:#}");
                    }
                });
            })
        }
        Slot::InstallFont => {
            let fonts = b.items.to_vec();
            Item::action(label, ic, move |w, cx| {
                let fonts = fonts.clone();
                let title = format!("{} {}", t!("Installing font:"), crate::i18n::items(fonts.len()));
                crate::status_center::start_with(cx, title, move |_| {
                    for f in &fonts {
                        // (a font inside an archive is installed from a copy extracted first)
                        let real = if f.exists() { Some(f.clone()) } else { crate::archive_view::real_copy(f) };
                        let real = real.ok_or_else(|| anyhow::anyhow!("{}", t!("Could not extract it from the archive")))?;
                        leaf_sys::fileactions::install_font_for_user(&real)?;
                    }
                    Ok(true)
                }, None);
                let _ = w;
            })
        }
        Slot::Compress => {
            let items = b.items;
            let (zip_items, z7_items, dlg_items) = (items.to_vec(), items.to_vec(), items.to_vec());
            let quick = cx.global::<crate::settings::Settings>().quick_compress;
            let quick_items = items.to_vec();
            let sub_items: Vec<Item> = vec![
                Item::action(t!("Create archive…"), icon(L::Archive), move |w, cx| create_archive_dialog(dlg_items.clone(), w, cx)),
                Item::Separator,
                Item::action(t!("Compress to ZIP"), icon(L::FileArchive), move |_, cx| compress(cx, zip_items.clone(), rf_archive::Format::Zip)),
                Item::action(t!("Compress to 7z"), icon(L::FileArchive), move |_, cx| compress(cx, z7_items.clone(), rf_archive::Format::SevenZ)),
            ]
            .into_iter()
            .chain(crate::archive_view::winrar().is_some().then(|| {
                let rar_items = items.to_vec();
                Item::action(t!("Compress to RAR"), icon(L::FileArchive), move |_, cx| {
                    if let Some((dir, stem)) = archive_stem(&rar_items) {
                        let dest = leaf_sys::fileops::unique_name(&dir, &stem, ".rar");
                        crate::status_center::compress_rar(cx, rar_items.clone(), dest, 5, None);
                    }
                })
            }))
            .collect();
            // (a click on the row compresses at once in the format chosen in Settings; hovering shows the other ways)
            match quick_compress_format(quick) {
                Some(f) => Item::sub_click(label, ic, sub_items, move |_, cx| quick_compress(cx, quick_items.clone(), f)),
                None => Item::sub(label, ic, sub_items),
            }
        }
        Slot::Extract => {
            use crate::status_center::ExtractTo;
            let items = b.items;
            let list = items.to_vec();
            let act = |label: String, ic: L, to: ExtractTo| {
                let list = list.clone();
                Item::action(label, icon(ic), move |_, cx| crate::status_center::extract_to(cx, list.clone(), to.clone()))
            };
            let named = if items.len() > 1 {
                t!("Extract each to its own folder").to_string()
            } else {
                format!("{} {}\\", t!("Extract to folder:"), crate::ops::archive_stem(&items[0]))
            };
            let pick = list.clone();
            Item::sub(
                label,
                ic,
                vec![
                    act(t!("Extract here").to_string(), L::FolderDown, ExtractTo::Here),
                    act(t!("Extract smartly").to_string(), L::Zap, ExtractTo::Smart),
                    act(named, L::FolderInput, ExtractTo::Named),
                    Item::action(t!("Extract to…"), icon(L::FolderSearch), move |_, cx| crate::status_center::extract_pick(cx, pick.clone())),
                ],
            )
        }
        Slot::Tags => Item::sub(label, ic, tag_items(me, b.items, cx)),
        Slot::Shelf => {
            let shelf_items = b.items.to_vec();
            Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::AddToShelf(shelf_items.clone()))))
        }
        Slot::Basket => Item::sub(label, ic, crate::basket::menu_items(b.items, &b.dir)),
        Slot::Nearby => Item::sub(label, ic, crate::nearby::menu_items(b.items, cx)),
        Slot::BulkRename => {
            let items = b.items.to_vec();
            Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::BulkRename(items.clone()))))
        }
        Slot::CopyToPane => {
            let items = b.items.to_vec();
            Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::ToOtherPane(Transfer::Copy, items.clone()))))
        }
        Slot::MoveToPane => {
            let items = b.items.to_vec();
            Item::action(label, ic, act(me, move |_, _, cx| cx.emit(FolderEvent::ToOtherPane(Transfer::Move, items.clone()))))
        }
        Slot::YourActions => b.own.clone().expect("a row for the user's actions is shown only when there are some"),
        Slot::More => Item::Sub { label: label.into(), icon: ic, items: Sub::Pending { key: "more", fallback: None }, click: None },
        // the rows of the Recycle Bin page and of the sidebar are made where those menus are (`recycle_view.rs`, `workspace.rs`)
        Slot::BinRestore | Slot::BinDelete | Slot::BinSettings | Slot::Unpin | Slot::Properties | Slot::HidePlace | Slot::HideDrive => {
            unreachable!("no file list menu shows {:?}", p.slot)
        }
    }
}

impl FolderView {
    /// The menu in a folder on a server (`Location::Remote`): opening, copying down to this PC, and where the place can be
    /// written to: cut, paste (uploads), edit, rename, delete (permanent: a server has no Trash), new folder.
    fn show_remote_menu(&mut self, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let chosen = if background { Vec::new() } else { self.selection_remote() };
        let mut items = Vec::new();
        if !background && self.cursor_entry().is_some() && self.selected.len() == 1 {
            let dir = self.selected_is_single_dir();
            items.push(Item::action(t!("Open"), icon(if dir { L::FolderOpen } else { L::SquareArrowOutUpRight }), act(&me, |v, _, cx| {
                if let Some(ei) = v.cursor_entry() {
                    v.open_entry(ei, cx);
                }
            })));
            items.push(Item::Separator);
        }
        if !chosen.is_empty() {
            let c = chosen.clone();
            items.push(Item::action(t!("Copy"), icon(L::Copy), act(&me, move |_, _, cx| crate::remote::copy_to_clipboard(c.clone(), false, cx))).hint("Ctrl+C"));
            let c = chosen.clone();
            items.push(Item::action(t!("Copy to..."), icon(L::FolderInput), act(&me, move |_, _, cx| cx.emit(FolderEvent::CopyRemoteTo(c.clone())))));
            if self.dual {
                let c = chosen.clone();
                items.push(Item::action(t!("Copy to other pane"), icon(L::CopyPlus), act(&me, move |_, _, cx| cx.emit(FolderEvent::DownloadToOtherPane(c.clone(), false)))).hint("Shift+F5"));
            }
        }
        if self.remote_writable_dir().is_some() {
            let all = if background { Vec::new() } else { self.selection_remote_all() };
            if !all.is_empty() {
                items.push(Item::action(t!("Cut"), icon(L::Scissors), act(&me, |v, window, cx| v.copy_to_clipboard(leaf_sys::fileops::Transfer::Move, window, cx))).hint("Ctrl+X"));
            }
            items.push(Item::action(t!("Paste"), icon(L::Clipboard), act(&me, |v, window, cx| v.paste(window, cx))).hint("Ctrl+V"));
            let one_file = (!background && self.selected.len() == 1).then(|| self.remote_look_target()).flatten();
            if let Some((v, size)) = one_file.filter(|(v, _)| v.scheme() == "sftp") {
                // fetched, opened in the local editor, every save uploaded (refused when the server's copy changed)
                items.push(Item::action(t!("Edit"), icon(L::FilePen), act(&me, move |_, _, cx| crate::remote::edit_file(cx, v.clone(), size))));
            }
            if !background && self.selected.len() == 1 {
                items.push(Item::action(t!("Rename"), icon(L::PencilLine), act(&me, |v, window, cx| v.begin_rename(window, cx))).hint("F2"));
            }
            if !all.is_empty() {
                items.push(Item::action(t!("Delete"), icon(L::Trash), act(&me, |v, window, cx| v.delete_selected(false, window, cx))).hint("Del"));
            }
            items.push(Item::action(t!("New folder"), icon(L::FolderPlus), act(&me, |v, _, cx| v.create_new(true, cx))).hint("Ctrl+Shift+N"));
        } else {
            for (label, l) in [(t!("Cut"), L::Scissors), (t!("Paste"), L::Clipboard), (t!("Rename"), L::PencilLine), (t!("Delete"), L::Trash)] {
                items.push(Item::action(label, icon(l), |_, _| {}).enabled(false));
            }
            items.push(Item::action(crate::i18n::td(crate::remote::NOT_WIRED), icon(L::Info), |_, _| {}).enabled(false));
        }
        items.push(Item::Separator);
        items.push(Item::action(t!("Refresh"), icon(L::RefreshCw), act(&me, |v, _, cx| v.refresh(cx))));
        fluent_menu::show(Menu { quick: Vec::new(), items }, window.mouse_position(), window, cx);
    }

    /// Opens our context menu for the selection (or the folder background) at the mouse.
    pub fn show_context_menu(&mut self, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.location(), rf_core::history::Location::Shell { .. }) {
            if shift_down() {
                self.show_shell_folder_menu(background, cx);
            } else {
                self.show_shell_fluent_menu(background, window, cx);
            }
            return;
        }
        if matches!(self.location(), rf_core::history::Location::Remote(_)) {
            self.show_remote_menu(background, window, cx);
            return;
        }
        let shift = leaf_sys::input::shift_down();
        if shift {
            // power users: the full classic Explorer menu (extended verbs)
            self.show_native_menu(background, cx);
            return;
        }
        let Some(dir) = self.current_dir() else { return };
        let items = if background { Vec::new() } else { self.selection_paths() };
        let background = background || items.is_empty();
        let me = cx.entity().downgrade();
        let menu = if background { self.background_menu(&me, &dir, cx) } else { self.items_menu(&me, &dir, &items, cx) };
        let menu_view = fluent_menu::show(menu, window.mouse_position(), window, cx);

        // Shell items in the background: QueryContextMenu can take a second with slow extensions.
        let target = if background { ShellTarget::Background(dir.clone()) } else { ShellTarget::Items(items.clone()) };
        // The user's own programs for the first selected file's type (Settings > File types): the first rows of "Open with". Their
        // icons are read on the worker, with the shell's items: never on the UI thread.
        let own: Vec<String> = if background { Vec::new() } else { items.first().map(|f| leaf_sys::openrules::programs_for(f)).unwrap_or_default() };
        // "Send to" (issue 117): its entries and their icons are read on the worker too, only when the menu has the row
        let send_to = !background && send_to_fits(&items, cx);
        let drives = if send_to { self.send_to_drives(cx) } else { Vec::new() };
        let send_items = items.clone();
        // the packaged apps' commands of Windows 11's new menu (issue 107), read on the worker with their icons
        let pkg_items = if background { vec![dir.clone()] } else { items.clone() };
        let (tx, rx) = oneshot::channel();
        leaf_sys::sta::run(move |owner| {
            let packaged = packaged_commands(leaf_sys::pkgverbs::for_items(&pkg_items, background));
            let own: Vec<OwnProgram> = own
                .into_iter()
                .map(|program| {
                    let icon = leaf_sys::thumbs::get_image(std::path::Path::new(&program), 32, leaf_sys::thumbs::Kind::Icon).ok();
                    (program, icon)
                })
                .collect();
            let targets: Vec<SendTarget> = if send_to {
                leaf_sys::sendto::targets()
                    .into_iter()
                    .map(|t| {
                        let icon = leaf_sys::thumbs::get_image(&t.path, 32, leaf_sys::thumbs::Kind::Icon).ok();
                        (t, icon)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let _ = tx.send((leaf_sys::ctxmenu::build(owner, &target, false).map_err(|e| format!("{e:#}")), own, targets, packaged));
        });
        let menu_weak = menu_view.downgrade();
        cx.spawn(async move |_, cx| {
            let (result, own, targets, packaged) = rx.await.unwrap_or_else(|_| (Err("cancelled".into()), Vec::new(), Vec::new(), Vec::new()));
            let _ = cx.update(|cx| {
                let (more, open_with, new) = match result {
                    Ok((session, shell)) => split_shell_items(&me, session, shell, background, cx),
                    Err(e) => {
                        log::warn!("shell menu: {e}");
                        (Pending::Failed, Pending::Failed, Pending::Failed)
                    }
                };
                let open_with = with_own_programs(&me, own, open_with);
                let more = with_packaged(packaged, more);
                let inline = !cx.global::<crate::settings::Settings>().menu.shell_submenu;
                if let Some(m) = menu_weak.upgrade() {
                    m.update(cx, |m, cx| {
                        m.set_pending("more", more, cx);
                        m.set_pending("openwith", open_with, cx);
                        m.set_pending("new", new, cx);
                        if send_to {
                            m.set_pending("sendto", send_to_items(targets, drives, send_items), cx);
                        }
                        // Settings: shell extensions listed in the menu itself, not in a submenu
                        if inline {
                            m.inline_pending("more", cx);
                        }
                    });
                }
            });
        })
        .detach();
    }

    fn quick_clipboard(&self, me: &Me, with_items: bool) -> Vec<Quick> {
        let paste_ok = leaf_sys::clipboard::can_paste();
        if !with_items {
            return vec![
                Quick::new(L::ClipboardPaste, t!("Paste  (Ctrl+V)"), paste_ok, act(me, |v, w, cx| v.paste(w, cx))),
                Quick::new(L::Wrench, t!("Properties  (Alt+Enter)"), true, act(me, |v, _, cx| v.folder_properties(cx))),
            ];
        }
        vec![
            Quick::new(L::Scissors, t!("Cut  (Ctrl+X)"), true, act(me, |v, w, cx| v.copy_to_clipboard(Transfer::Move, w, cx))),
            Quick::new(L::Copy, t!("Copy  (Ctrl+C)"), true, act(me, |v, w, cx| v.copy_to_clipboard(Transfer::Copy, w, cx))),
            Quick::new(
                L::ClipboardPaste,
                t!("Paste  (Ctrl+V)"),
                paste_ok,
                act(me, |v, w, cx| if v.selected_is_single_dir() { v.paste_into_selected(w, cx) } else { v.paste(w, cx) }),
            ),
            Quick::new(L::TextCursorInput, t!("Rename  (F2)"), true, act(me, |v, w, cx| v.rename_selection(w, cx))),
            Quick::new(L::Share2, t!("Share"), true, act(me, |v, w, cx| v.share(w, cx))),
            Quick::new(L::Trash, t!("Delete  (Del)"), true, act(me, |v, w, cx| v.delete_selected(false, w, cx))),
            Quick::new(L::Wrench, t!("Properties  (Alt+Enter)"), true, act(me, |v, _, cx| v.properties(cx))),
        ]
    }

    /// The menu of a selection (a file, a folder or several items): made from the rows of `menu_model` for this context.
    fn items_menu(&self, me: &Me, dir: &std::path::Path, items: &[PathBuf], cx: &mut Context<Self>) -> Menu {
        let single_dir = items.len() == 1 && self.selected_is_single_dir();
        let target = MenuContext::target_of(items.len(), single_dir);
        let ctx_dir = if single_dir { items[0].clone() } else { dir.to_path_buf() };
        let own = crate::custom_actions::menu_section(&self.selection_with_kind(), dir, false, cx);
        let in_archive = items.iter().any(|p| crate::archive_view::split(p).is_some() && !p.exists());
        let prefs = cx.global::<crate::settings::Settings>();
        let image = items.len() == 1 && leaf_sys::fileactions::is_image(&items[0]);
        let ctx = MenuContext {
            target,
            count: items.len(),
            single_dir,
            // one archive that opens like a folder: it can open in a new tab or window too (Discord, 2026-11-20)
            single_archive: items.len() == 1 && !single_dir && prefs.browse_archives && items[0].exists() && crate::archive_view::is_browsable(&items[0]),
            has_files: !self.selected_is_single_dir() || items.len() > 1,
            in_archive,
            in_search: self.is_search(),
            dual: self.dual,
            prefs: prefs.menu.clone(),
            show_flatten: prefs.show_flatten,
            name_index: prefs.name_index,
            send_to_fits: send_to_fits_files(items),
            archive_path: crate::archive_view::split(&ctx_dir).is_some(),
            ftp: crate::remote::is_address_path(&ctx_dir),
            organizer: false,
            rotatable: !items.is_empty() && items.iter().all(|p| leaf_sys::rotate::can_rotate(p) && crate::archive_view::split(p).is_none()),
            image,
            runnable: items.len() == 1 && !image && leaf_sys::fileactions::is_runnable(&items[0]),
            fonts: !items.is_empty() && items.iter().all(|p| leaf_sys::fileactions::is_font(p)),
            extractable: !items.is_empty() && items.iter().all(|p| rf_archive::is_supported(p)),
            nearby: crate::nearby::running(cx),
            own_actions: own.is_some(),
            undo: None,
            redo: None,
            alias: if single_dir && crate::archive_view::split(&ctx_dir).is_none() { crate::aliases::name_of(&ctx_dir) } else { None },
            ..Default::default()
        };
        let build = Build { me, v: self, cx, ctx: &ctx, dir: ctx_dir, items, own };
        let mut quick = self.quick_clipboard(me, true);
        if in_archive {
            // (Windows' share sheet needs real files)
            quick.retain(|q| q.icon != L::Share2);
        }
        Menu { quick, items: generate(&build) }
    }

    /// The folder background's menu: made from the rows of `menu_model` for this context.
    fn background_menu(&self, me: &Me, dir: &std::path::Path, cx: &mut Context<Self>) -> Menu {
        let in_search = self.is_search();
        let archive_path = crate::archive_view::is_browsable_path(dir);
        let (undo, redo) = crate::undo::next_labels();
        let own = crate::custom_actions::menu_section(&[], dir, true, cx);
        let ctx = MenuContext {
            target: Target::Background,
            in_archive: crate::archive_view::split(dir).is_some() && !dir.exists(),
            in_search,
            prefs: cx.global::<crate::settings::Settings>().menu.clone(),
            archive_path,
            ftp: crate::remote::is_address_path(dir),
            organizer: !in_search && crate::organizer::has_rules_for(dir, cx),
            own_actions: own.is_some(),
            undo: undo.map(str::to_string),
            redo: redo.map(str::to_string),
            ..Default::default()
        };
        let build = Build { me, v: self, cx, ctx: &ctx, dir: dir.to_path_buf(), items: &[], own };
        Menu { quick: self.quick_clipboard(me, false), items: generate(&build) }
    }

    /// F2 from the menu: bulk rename for several items, inline for one.
    pub(crate) fn rename_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selection_paths().len() > 1 {
            cx.emit(FolderEvent::BulkRename(self.selection_paths()));
        } else {
            self.begin_rename(window, cx);
        }
    }

    /// Windows' share sheet for the selection, over this window (`leaf_sys::share`: the context menu's verb failed, issue 48).
    pub(crate) fn share(&self, window: &Window, cx: &mut App) {
        crate::share::open(crate::look::window::hwnd_of(window), self.selection_paths(), cx);
    }

    /// Properties of the current folder (background menu).
    pub(crate) fn folder_properties(&self, cx: &mut App) {
        if let rf_core::history::Location::Remote(dir) = self.location() {
            crate::properties_view::open_remote(dir.clone(), cx);
            return;
        }
        let Some(dir) = self.current_dir() else { return };
        crate::properties_view::open(vec![dir], self.images.clone(), cx);
    }
}

/// "Geri al: Silme", or plain "Geri al" when there is nothing to undo.
pub(crate) fn undo_text(what: Option<&str>) -> String {
    match what {
        Some(w) => format!("{}: {w}", t!("Undo")),
        None => t!("Undo").to_string(),
    }
}

pub(crate) fn redo_text(what: Option<&str>) -> String {
    match what {
        Some(w) => format!("{}: {w}", t!("Redo")),
        None => t!("Redo").to_string(),
    }
}

fn layout_items(me: &Me, mode: ViewMode, style: crate::folder_view::GridStyle) -> Vec<Item> {
    use crate::folder_view::GridStyle;
    let layout = |label: &str, l: L, m: ViewMode, st: GridStyle, hint: &str| {
        let on = mode == m && (m != ViewMode::Grid || style == st);
        Item::action(label.to_string(), icon(l), act(me, move |v, _, cx| v.set_layout(m, st, cx))).checked(on).hint(hint.to_string())
    };
    vec![
        layout(t!("Details"), L::List, ViewMode::Details, GridStyle::Tiles, "Ctrl+Shift+1"),
        layout(t!("List"), L::TextAlignJustify, ViewMode::Grid, GridStyle::List, "Ctrl+Shift+5"),
        layout(t!("Cards"), L::LayoutList, ViewMode::Grid, GridStyle::Cards, "Ctrl+Shift+4"),
        layout(t!("Grid"), L::LayoutGrid, ViewMode::Grid, GridStyle::Tiles, "Ctrl+Shift+2"),
        layout(t!("Columns"), L::Columns3, ViewMode::Columns, GridStyle::Tiles, "Ctrl+Shift+3"),
    ]
}

fn sort_items(me: &Me, sort: rf_core::SortSpec, offers: crate::folder_view::sort_ext::SortOffers) -> Vec<Item> {
    let by = |label: &str, k: SortKey| {
        Item::action(label.to_string(), MenuIcon::None, act(me, move |v, _, cx| {
            let mut s = v.sort;
            s.key = k;
            v.set_sort_spec(s, cx)
        }))
        .checked(sort.key == k)
    };
    let dir_item = |label: &str, d: SortDir| {
        Item::action(label.to_string(), MenuIcon::None, act(me, move |v, _, cx| {
            let mut s = v.sort;
            s.dir = d;
            v.set_sort_spec(s, cx)
        }))
        .checked(sort.dir == d)
    };
    let mut v = vec![
        by(t!("Name"), SortKey::Name),
        by(t!("Date modified"), SortKey::Modified),
        by(t!("Date created"), SortKey::Created),
        by(t!("Type"), SortKey::Type),
        by(t!("Size"), SortKey::Size),
    ];
    // the keys that need more than the entry: tag, the folder (results from several folders), sync state
    if offers.tag {
        v.push(by(t!("Tag"), SortKey::Tag));
    }
    if offers.path {
        v.push(by(t!("Location"), SortKey::Path));
    }
    if offers.sync {
        v.push(by(t!("Sync status"), SortKey::Sync));
    }
    v.push(Item::Separator);
    v.push(dir_item(t!("Ascending"), SortDir::Asc));
    v.push(dir_item(t!("Descending"), SortDir::Desc));
    v
}

/// Folders first / files first / mixed (Files' three radio items under Sort and Group).
fn order_items(me: &Me, sort: rf_core::SortSpec) -> Vec<Item> {
    let order = |label: &str, dirs: bool, files: bool| {
        Item::action(label.to_string(), MenuIcon::None, act(me, move |v, _, cx| {
            let mut s = v.sort;
            s.dirs_first = dirs;
            s.files_first = files;
            v.set_sort_spec(s, cx)
        }))
        .checked(sort.dirs_first == dirs && (dirs || sort.files_first == files))
    };
    vec![
        order(t!("Sort folders first"), true, false),
        order(t!("Sort files first"), false, true),
        order(t!("Sort files and folders together"), false, false),
    ]
}

fn group_items(me: &Me, group: GroupBy) -> Vec<Item> {
    let grp = |label: &str, g: GroupBy| Item::action(label.to_string(), MenuIcon::None, act(me, move |v, _, cx| v.set_group(g, cx))).checked(group == g);
    vec![
        grp(t!("None"), GroupBy::None),
        grp(t!("Name"), GroupBy::Name),
        grp(t!("Type"), GroupBy::Type),
        grp(t!("Date modified"), GroupBy::Modified),
        grp(t!("Year"), GroupBy::Year),
        grp(t!("Month"), GroupBy::Month),
        grp(t!("Day"), GroupBy::Day),
        grp(t!("Date created"), GroupBy::Created),
        grp(t!("Size"), GroupBy::Size),
        grp(t!("Tag"), GroupBy::Tag),
        grp(t!("Folder"), GroupBy::Folder),
    ]
}

/// Drop-down menus of the command bar.
pub enum BarMenu {
    New,
    Sort,
    /// The "…" button: undo and redo, the selection commands, the folder's properties, Settings (issue 55: as Explorer's "See more"
    /// menu has them).
    More,
    /// Layout + view toggles; the flags are workspace state shown as check marks.
    View { dual: bool, shelf: bool },
}

impl FolderView {
    pub fn show_bar_menu(&mut self, which: BarMenu, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        let items = match which {
            BarMenu::New => {
                let Some(dir) = self.current_dir() else { return };
                let view = fluent_menu::show(Menu { quick: Vec::new(), items: own_new_items(&me) }, pos, window, cx);
                // the shell's templates (Word, Excel…) join as soon as the shell answers
                let (tx, rx) = oneshot::channel();
                leaf_sys::sta::run(move |owner| {
                    let _ = tx.send(leaf_sys::ctxmenu::build(owner, &ShellTarget::Background(dir), false).ok());
                });
                let weak = view.downgrade();
                cx.spawn(async move |_, cx| {
                    let Ok(Some((session, shell))) = rx.await else { return };
                    let _ = cx.update(|cx| {
                        if let (_, _, Pending::Ready(items)) = split_shell_items(&me, session, shell, true, cx) {
                            if let Some(m) = weak.upgrade() {
                                m.update(cx, |m, cx| m.set_root((*items).clone(), cx));
                            }
                        }
                    });
                })
                .detach();
                return;
            }
            BarMenu::More => {
                let (undo, redo) = crate::undo::next_labels();
                let none = self.selected.is_empty();
                // (the commands the tool bar leaves out to stay as short as the mockups': paste, cut by its own name, share)
                let paste_ok = self.location().path().is_some() && leaf_sys::clipboard::can_paste();
                let mut more = vec![
                    Item::action(t!("Paste"), icon(L::ClipboardPaste), act(&me, |v, w, cx| v.paste(w, cx))).hint("Ctrl+V").enabled(paste_ok),
                    Item::action(t!("Cut"), icon(L::Scissors), act(&me, |v, w, cx| v.copy_to_clipboard(leaf_sys::fileops::Transfer::Move, w, cx))).hint("Ctrl+X").enabled(!none),
                    Item::action(t!("Share"), icon(L::Share2), act(&me, |v, w, cx| v.share(w, cx))).enabled(!none),
                    Item::Separator,
                    Item::action(undo_text(undo), icon(L::Undo2), move |_, cx| crate::undo::undo(cx)).hint("Ctrl+Z").enabled(undo.is_some()),
                    Item::action(redo_text(redo), icon(L::Redo2), move |_, cx| crate::undo::redo(cx)).hint("Ctrl+Y").enabled(redo.is_some()),
                    Item::Separator,
                    Item::action(t!("Select all"), icon(L::SquareCheck), act(&me, |v, _, cx| v.select_all_items(cx))).hint("Ctrl+A"),
                    Item::action(t!("Select none"), icon(L::Square), act(&me, |v, _, cx| v.select_no_items(cx))).enabled(!none),
                    Item::action(t!("Invert selection"), icon(L::ArrowLeftRight), act(&me, |v, _, cx| v.invert_selected_items(cx))),
                    Item::Separator,
                    Item::action(t!("Show changes"), icon(L::GitCompare), |w, cx| w.dispatch_action(Box::new(crate::actions::ShowChanges), cx))
                        .enabled(self.current_dir().is_some_and(|d| leaf_dev::gitignore::repo_root(&d).is_some())),
                    Item::action(t!("Checkpoint"), icon(L::FolderClock), |w, cx| w.dispatch_action(Box::new(crate::actions::Checkpoint), cx))
                        .enabled(self.location().path().is_some()),
                    Item::action(t!("Checkpoints"), icon(L::ArchiveRestore), |w, cx| w.dispatch_action(Box::new(crate::actions::ShowCheckpoints), cx))
                        .enabled(self.location().path().is_some()),
                    Item::action(t!("Properties"), icon(L::Wrench), act(&me, |v, _, cx| v.folder_properties(cx))),
                    Item::action(t!("Settings"), icon(L::Settings), |w, cx| w.dispatch_action(Box::new(crate::actions::GoSettings), cx)).hint("Ctrl+,"),
                ];
                // the user's tool bar actions that had no room on the bar
                more.extend(crate::custom_actions::toolbar_overflow(f32::from(window.viewport_size().width), self.selection_paths(), self.current_dir(), cx));
                more
            }
            BarMenu::Sort => {
                // Files' layout: Sort ▸ / Group ▸, then the folder/file order; these apply to
                // this folder only (each folder remembers its own) unless "sync" is on
                let mut v = vec![
                    Item::sub(t!("Sort"), icon(L::ArrowUpDown), sort_items(&me, self.sort, self.sort_offers(cx))),
                    Item::sub(t!("Group by"), icon(L::ListTree), {
                        // (the chosen property columns follow the fixed keys; none chosen: nothing is added)
                        let mut g = group_items(&me, self.group);
                        g.extend(self.property_group_items(&me, cx));
                        g
                    }),
                    Item::Separator,
                ];
                v.extend(order_items(&me, self.sort));
                if !cx.global::<crate::settings::Settings>().sync_layout && matches!(self.location(), Location::Path(_)) {
                    v.push(Item::Separator);
                    v.push(Item::action(t!("Make default for all folders"), icon(L::CopyCheck), act(&me, |v, _, cx| v.make_layout_default(cx))));
                }
                v
            }
            BarMenu::View { dual, shelf } if crate::platform::popup_windows() => {
                // Files' layout flyout (tiles, size slider, switches)
                // opened after this update: its first frame reads this view
                window.defer(cx, move |window, cx| crate::layout_panel::open(me, dual, shelf, pos, window, cx));
                return;
            }
            BarMenu::View { dual, shelf } => {
                let mut v = layout_items(&me, self.view_mode, self.grid_style);
                v.push(Item::Separator);
                v.push(
                    Item::action(t!("Show hidden items"), icon(L::Eye), act(&me, |v, _, cx| v.toggle_hidden(cx)))
                        .checked(self.filter.show_hidden)
                        .hint("Ctrl+H"),
                );
                v.push(
                    Item::action(t!("Hide git-ignored files"), icon(L::EyeOff), |w, cx| w.dispatch_action(Box::new(crate::actions::ToggleHideIgnored), cx))
                        .checked(cx.global::<crate::settings::Settings>().hide_ignored),
                );
                v.push(
                    Item::action(t!("Dual pane"), icon(L::Columns2), |w, cx| w.dispatch_action(Box::new(crate::actions::ToggleDualPane), cx))
                        .checked(dual)
                        .hint("Ctrl+Shift+D"),
                );
                v.push(Item::action(t!("Shelf"), icon(L::Inbox), |w, cx| w.dispatch_action(Box::new(crate::actions::ToggleShelf), cx)).checked(shelf));
                v
            }
        };
        fluent_menu::show(Menu { quick: Vec::new(), items }, pos, window, cx);
    }
}

fn own_new_items(me: &Me) -> Vec<Item> {
    vec![
        Item::action(t!("Folder"), MenuIcon::Tinted(L::Folder, crate::theme::kind_color(crate::icons::Category::Folder, true)), act(me, |v, _, cx| v.create_new(true, cx)))
            .hint("Ctrl+Shift+N"),
        Item::action(t!("Text document"), icon(L::FileText), act(me, |v, _, cx| v.create_new(false, cx))),
        Item::Separator,
        Item::action(t!("Download here…"), icon(L::Download), act(me, |v, w, cx| {
            if let Some(dir) = v.current_dir().filter(|d| crate::downloads::can_save_in(d)) {
                crate::downloads::ask(cx.entity().downgrade(), dir, w, cx);
            }
        })),
        Item::action(t!("Download several…"), icon(L::Download), act(me, |v, w, cx| {
            if let Some(dir) = v.current_dir().filter(|d| crate::downloads::can_save_in(d)) {
                crate::downloads::ask_many(cx.entity().downgrade(), dir, w, cx);
            }
        })),
    ]
}

fn tag_items(me: &Me, items: &[PathBuf], cx: &App) -> Vec<Item> {
    let store = &cx.global::<crate::tags::Tags>().0;
    store
        .defs
        .iter()
        .map(|d| {
            let all = items.iter().all(|p| store.has(p, d.id));
            let (paths, id) = (items.to_vec(), d.id);
            let color = crate::tags::color(d.color);
            Item::action(crate::i18n::td(&d.name), MenuIcon::Dot(color), act(me, move |_, _, cx| {
                crate::tags::update(cx, |t| t.toggle(&paths, id));
                cx.notify();
            }))
            .checked(all)
        })
        .collect()
}

/// Default archive name for `items`: the item's name (one) or the folder's name (several).
fn archive_stem(items: &[PathBuf]) -> Option<(PathBuf, String)> {
    let first = items.first()?;
    let dir = first.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let stem = if items.len() == 1 {
        if first.is_dir() {
            first.file_name().map(|s| s.to_string_lossy().into_owned())
        } else {
            first.file_stem().map(|s| s.to_string_lossy().into_owned())
        }
        .unwrap_or_else(|| "Archive".into())
    } else {
        rf_core::history::display_name(&dir)
    };
    Some((dir, stem))
}

/// What a click on "Sıkıştır" makes: `None` when it only opens the submenu (or RAR is chosen and WinRAR is gone).
fn quick_compress_format(q: crate::settings::QuickCompress) -> Option<Option<rf_archive::Format>> {
    use crate::settings::QuickCompress as Q;
    match q {
        Q::Menu => None,
        Q::Zip => Some(Some(rf_archive::Format::Zip)),
        Q::SevenZ => Some(Some(rf_archive::Format::SevenZ)),
        Q::TarZst => Some(Some(rf_archive::Format::TarZst)),
        Q::TarGz => Some(Some(rf_archive::Format::TarGz)),
        Q::Tar => Some(Some(rf_archive::Format::Tar)),
        // (None inside: RAR, through WinRAR)
        Q::Rar => crate::archive_view::winrar().is_some().then_some(None),
    }
}

fn quick_compress(cx: &mut App, items: Vec<PathBuf>, format: Option<rf_archive::Format>) {
    match format {
        Some(f) => compress(cx, items, f),
        None => {
            if let Some((dir, stem)) = archive_stem(&items) {
                let dest = leaf_sys::fileops::unique_name(&dir, &stem, ".rar");
                crate::status_center::compress_rar(cx, items, dest, 5, None);
            }
        }
    }
}

fn compress(cx: &mut App, items: Vec<PathBuf>, format: rf_archive::Format) {
    let Some((dir, stem)) = archive_stem(&items) else { return };
    let dest = dir.join(format!("{stem}.{}", format.extension()));
    crate::status_center::compress(cx, items, dest, format);
}

/// Files' "Create archive" dialog: name, format and compression level.
fn create_archive_dialog(items: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Choice, Kind};
    let Some((dir, stem)) = archive_stem(&items) else { return };
    let count = items.len();
    let format = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let level = std::rc::Rc::new(std::cell::Cell::new(2usize));
    let protect = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let split = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let dict = std::rc::Rc::new(std::cell::Cell::new(0usize));
    // (None = RAR, WinRAR's format: offered only when its Rar.exe is installed)
    let mut formats = vec![
        ("ZIP", Some(rf_archive::Format::Zip)),
        ("7z", Some(rf_archive::Format::SevenZ)),
        ("tar.zst", Some(rf_archive::Format::TarZst)),
        ("tar.gz", Some(rf_archive::Format::TarGz)),
        ("tar", Some(rf_archive::Format::Tar)),
    ];
    if crate::archive_view::winrar().is_some() {
        formats.push(("RAR", None));
    }
    let choices = vec![
        Choice { label: t!("Format").into(), options: formats.iter().map(|(n, _)| SharedString::from(*n)).collect(), selected: format.clone() },
        Choice {
            label: t!("Compression level").into(),
            options: vec![t!("None").into(), t!("Fast").into(), t!("Normal").into(), t!("Maximum").into()],
            selected: level.clone(),
        },
        // ZIP: AES-256; 7z: AES-256 with encrypted names
        Choice { label: t!("Password").into(), options: vec![t!("None").into(), t!("Set password…").into()], selected: protect.clone() },
        // (parts as 7-Zip names them: name.zip.001, .002, ...)
        Choice {
            label: t!("Split into parts").into(),
            options: std::iter::once(SharedString::from(t!("None"))).chain(SPLIT_SIZES.iter().map(|(n, _)| SharedString::from(*n))).collect(),
            selected: split.clone(),
        },
        Choice {
            label: t!("Dictionary size (7z)").into(),
            options: std::iter::once(SharedString::from(t!("Automatic"))).chain(DICT_SIZES.iter().map(|(n, _)| SharedString::from(*n))).collect(),
            selected: dict.clone(),
        },
    ];
    let create = Button::new(t!("Create"), Kind::Primary, move |name, window, cx| {
        let name = name.unwrap_or_default();
        let name = name.trim();
        if name.is_empty() {
            return Err(t!("Enter a name").into());
        }
        if name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(t!("A name can't contain: \\ / : * ? \" < > |").into());
        }
        let lvl = [0, 1, 5, 9][level.get().min(3)];
        let fmt = formats.get(format.get()).and_then(|f| f.1);
        let volume_size = split.get().checked_sub(1).and_then(|i| SPLIT_SIZES.get(i)).map(|s| s.1);
        let dict_size = dict.get().checked_sub(1).and_then(|i| DICT_SIZES.get(i)).map(|s| s.1);
        let Some(fmt) = fmt else {
            if volume_size.is_some() {
                return Err(t!("RAR can't be split here; choose ZIP or 7z").into());
            }
            let dest = dir.join(format!("{name}.rar"));
            let items = items.clone();
            if protect.get() == 1 {
                let finish: Finish = Rc::new(move |cx, pw| crate::status_center::compress_rar(cx, items.clone(), dest.clone(), lvl, Some(pw)));
                let d = dir.join(format!("{name}.rar"));
                window.defer(cx, move |window, cx| archive_password(d, finish, None, window, cx));
            } else {
                crate::status_center::compress_rar(cx, items, dest, lvl, None);
            }
            return Ok(());
        };
        let dest = dir.join(format!("{name}.{}", fmt.extension()));
        let opts = rf_archive::CompressOptions { password: None, dict_size, volume_size };
        if protect.get() == 1 {
            if !fmt.encrypts() {
                return Err(t!("tar formats don't support passwords; choose ZIP or 7z").into());
            }
            // asked after this dialog closes (one dialog at a time)
            let items = items.clone();
            let d = dest.clone();
            let finish: Finish = Rc::new(move |cx, pw| {
                let opts = rf_archive::CompressOptions { password: Some(pw), ..opts.clone() };
                crate::status_center::compress_opts(cx, items.clone(), dest.clone(), fmt, lvl, opts)
            });
            window.defer(cx, move |window, cx| archive_password(d, finish, None, window, cx));
            return Ok(());
        }
        crate::status_center::compress_opts(cx, items.clone(), dest, fmt, lvl, opts);
        Ok(())
    });
    crate::prompt::show_with(
        t!("Create archive"),
        Some(format!("{} — {}", crate::i18n::items(count),t!("the extension follows the format"))),
        Some((stem, t!("Archive name").to_string())),
        choices,
        vec![create, Button::cancel()],
        window,
        cx,
    );
}

/// "Split into parts" sizes: e-mail attachments, a CD, FAT32's file limit (4 GB less 1 MB).
const SPLIT_SIZES: &[(&str, u64)] = &[("25 MB", 25 << 20), ("100 MB", 100 << 20), ("700 MB", 700 << 20), ("1 GB", 1 << 30), ("4 GB", (4 << 30) - (1 << 20))];
const DICT_SIZES: &[(&str, u64)] = &[("1 MB", 1 << 20), ("16 MB", 16 << 20), ("64 MB", 64 << 20), ("256 MB", 256 << 20)];

type Finish = Rc<dyn Fn(&mut App, String)>;

/// "Create archive" with a password: typed twice (`first` = the first entry), then `finish`.
fn archive_password(dest: PathBuf, finish: Finish, first: Option<String>, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let again = first.is_some();
    let title = if again { t!("Enter the password again") } else { t!("Archive password") };
    let msg = format!("{} — {}", rf_core::history::display_name(&dest), t!("If you forget the password, the archive can't be opened."));
    let label = if again { t!("Create") } else { t!("Next") };
    crate::prompt::show_secret(
        title,
        Some(msg),
        t!("Password").to_string(),
        vec![
            Button::new(label, Kind::Primary, move |text, window, cx| {
                let pw = text.unwrap_or_default();
                if pw.is_empty() {
                    return Err(t!("The password can't be empty").into());
                }
                match &first {
                    None => {
                        let (d, f) = (dest.clone(), finish.clone());
                        window.defer(cx, move |window, cx| archive_password(d, f, Some(pw), window, cx));
                    }
                    Some(p) if *p == pw => finish(cx, pw),
                    Some(_) => return Err(t!("The passwords don't match").into()),
                }
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

fn open_terminal(d: PathBuf, cx: &App) {
    crate::terminal::open(d, cx);
}

/// "<name> - Kısayol.lnk" next to each item (unique name), like Explorer's "Create shortcut".
fn create_shortcuts(items: Vec<PathBuf>) {
    std::thread::spawn(move || {
        leaf_sys::shell::ensure_com();
        for target in items {
            let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else { continue };
            let base = format!("{} - {}", name.to_string_lossy(), t!("Shortcut"));
            let ext = if cfg!(windows) { ".lnk" } else { "" };
            let mut link = parent.join(format!("{base}{ext}"));
            let mut n = 2;
            while link.symlink_metadata().is_ok() {
                link = parent.join(format!("{base} ({n}){ext}"));
                n += 1;
            }
            if let Err(e) = leaf_sys::shortcut::create(&link, &target) {
                log::warn!("shortcut: {e:#}");
            }
        }
    });
}

/// One of the user's own programs for a kind of file, and its icon when the shell gave one.
type OwnProgram = (String, Option<leaf_sys::thumbs::Bgra>);

/// A packaged app's command for the menu, with its icon read on the worker.
struct Packaged {
    id: usize,
    title: String,
    icon: Option<leaf_sys::thumbs::Bgra>,
    enabled: bool,
    sub: Vec<Packaged>,
}

/// The packaged commands with their icons (on the shell STA: the icons are files' own icons).
fn packaged_commands(shown: Vec<leaf_sys::pkgverbs::Shown>) -> Vec<Packaged> {
    shown
        .into_iter()
        .map(|s| Packaged {
            id: s.id,
            // (the menu marks no access keys: "&Open in Terminal" is "Open in Terminal")
            title: s.title.replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&"),
            icon: s.icon.and_then(|p| leaf_sys::thumbs::get_image(&p, 32, leaf_sys::thumbs::Kind::Icon).ok()),
            enabled: s.enabled,
            sub: packaged_commands(s.sub),
        })
        .collect()
}

fn packaged_items(cmds: Vec<Packaged>) -> Vec<Item> {
    cmds.into_iter()
        .map(|c| {
            let picture = c.icon.as_ref().and_then(crate::image_cache::to_render_image).map(MenuIcon::Image).unwrap_or_else(|| icon(L::Package));
            if c.sub.is_empty() {
                let id = c.id;
                Item::action(c.title, picture, move |_, _| {
                    leaf_sys::sta::run(move |_| {
                        if let Err(e) = leaf_sys::pkgverbs::invoke(id) {
                            log::warn!("packaged menu command: {e:#}");
                        }
                    })
                })
                .enabled(c.enabled)
            } else {
                Item::sub(c.title, picture, packaged_items(c.sub))
            }
        })
        .collect()
}

/// "Show more options" with the packaged apps' commands first, as Windows 11's own menu shows them (issue 107: "Edit with
/// Notepad++", NanaZip, Terminal, ...), then the classic menu's items. They are there also when the classic part failed.
fn with_packaged(packaged: Vec<Packaged>, rest: Pending) -> Pending {
    if packaged.is_empty() {
        return rest;
    }
    let mut items = packaged_items(packaged);
    if let Pending::Ready(r) = &rest
        && !r.is_empty()
    {
        items.push(Item::Separator);
        items.extend(r.iter().cloned());
    }
    Pending::Ready(Rc::new(items))
}

/// An entry of the SendTo folder with its icon (read on the shell's worker thread).
type SendTarget = (leaf_sys::sendto::Target, Option<leaf_sys::thumbs::Bgra>);

/// The menu has "Send to" for these items: the setting is on and they are files of one folder on disk (not members of an archive, not
/// on an FTP server: a drop needs the shell's own items of real files).
fn send_to_fits(items: &[PathBuf], cx: &App) -> bool {
    cx.global::<crate::settings::Settings>().menu.send_to && send_to_fits_files(items)
}

/// The same without the setting (the menu's row has the switch itself).
fn send_to_fits_files(items: &[PathBuf]) -> bool {
    let Some(parent) = items.first().and_then(|p| p.parent()) else { return false };
    !crate::remote::is_address_path(parent)
        && items.iter().all(|p| p.parent() == Some(parent) && !(crate::archive_view::split(p).is_some() && !p.exists()))
}

/// The "Send to" submenu: the SendTo folder's entries, then the removable and network drives, as Explorer's menu has them. A click
/// drops the items on the entry (a copy or a link, never a move), on the shell's worker thread.
fn send_to_items(targets: Vec<SendTarget>, drives: Vec<(PathBuf, String, L)>, items: Vec<PathBuf>) -> Pending {
    let send = |target: PathBuf, items: Vec<PathBuf>| {
        move |_: &mut Window, _: &mut App| {
            let (target, items) = (target.clone(), items.clone());
            leaf_sys::sta::run(move |owner| {
                if let Err(e) = leaf_sys::sendto::send(owner, &target, &items) {
                    log::warn!("send to {}: {e:#}", target.display());
                }
            });
        }
    };
    let mut out: Vec<Item> = targets
        .into_iter()
        .map(|(t, picture)| {
            let picture = picture.as_ref().and_then(crate::image_cache::to_render_image).map(MenuIcon::Image).unwrap_or_else(|| icon(L::Send));
            Item::action(t.name, picture, send(t.path, items.clone()))
        })
        .collect();
    if !drives.is_empty() {
        if !out.is_empty() {
            out.push(Item::Separator);
        }
        out.extend(drives.into_iter().map(|(root, name, l)| Item::action(name, icon(l), send(root, items.clone()))));
    }
    if out.is_empty() {
        out.push(Item::action(t!("Empty"), icon(L::Send), |_, _| {}).enabled(false));
    }
    Pending::Ready(Rc::new(out))
}

/// "Open with", with the user's own programs for the file's type first (in their order), then what Windows offers. Without a list
/// for the type the submenu is Windows' alone. The programs are there also when the shell's part failed.
fn with_own_programs(me: &Me, own: Vec<OwnProgram>, rest: Pending) -> Pending {
    if own.is_empty() {
        return rest;
    }
    let mut items: Vec<Item> = own
        .into_iter()
        .map(|(program, picture)| {
            let picture = picture.as_ref().and_then(crate::image_cache::to_render_image).map(MenuIcon::Image).unwrap_or_else(|| icon(L::AppWindow));
            let label = rf_core::openrules::program_label(&program);
            Item::action(label, picture, act(me, move |v, window, cx| v.open_selection_in(program.clone(), window, cx)))
        })
        .collect();
    if let Pending::Ready(more) = &rest
        && !more.is_empty()
    {
        items.push(Item::Separator);
        items.extend(more.iter().cloned());
    }
    Pending::Ready(Rc::new(items))
}

/// Turns the shell menu into our three lazy lists: "Show more options" (what is left after
/// hiding our own commands), "Open with" and "New".
fn split_shell_items(me: &Me, session: u64, shell: Vec<ShellItem>, background: bool, cx: &mut App) -> (Pending, Pending, Pending) {
    let is_hidden = |it: &ShellItem| !it.verb.is_empty() && HIDDEN_VERBS.contains(&it.verb.to_ascii_lowercase().as_str());
    let has_child_verb = |it: &ShellItem, verb: &str| it.children.iter().any(|c| c.verb.eq_ignore_ascii_case(verb));

    let mut more_src = Vec::new();
    let mut open_with: Option<Vec<ShellItem>> = None;
    let mut new: Option<Vec<ShellItem>> = None;
    for it in shell {
        if !it.children.is_empty() && has_child_verb(&it, "openas") {
            open_with = Some(it.children.clone());
            continue;
        }
        if !it.children.is_empty() && has_child_verb(&it, "NewFolder") {
            new = Some(it.children.clone());
            continue;
        }
        if it.verb.eq_ignore_ascii_case("openas") && open_with.is_none() {
            // a single "Open with…" entry: say what it does
            open_with = Some(vec![ShellItem { label: t!("Choose another app").into(), ..it.clone() }]);
        }
        if is_hidden(&it) {
            continue;
        }
        more_src.push(it);
    }
    let more = to_items(me, session, &tidy(more_src), cx);

    let open_with = match open_with {
        Some(v) => Pending::Ready(Rc::new(to_items(me, session, &tidy(v), cx))),
        None => Pending::Ready(Rc::new(vec![Item::action(t!("Choose another app"), icon(L::LayoutGrid), act(me, |v, _, _| v.open_with()))])),
    };
    let new = match new.filter(|_| background) {
        Some(v) => {
            // ours for Folder and Text (inline rename), the shell for every other template
            let rest: Vec<ShellItem> = v
                .into_iter()
                .filter(|c| !c.verb.eq_ignore_ascii_case("NewFolder") && !c.verb.eq_ignore_ascii_case(".txt"))
                .collect();
            let mut items = own_new_items(me);
            let rest = to_items(me, session, &tidy(rest), cx);
            if !rest.is_empty() {
                items.push(Item::Separator);
                items.extend(rest);
            }
            Pending::Ready(Rc::new(items))
        }
        None => Pending::Failed,
    };
    (Pending::Ready(Rc::new(more)), open_with, new)
}

/// No leading/trailing/double separators.
fn tidy(items: Vec<ShellItem>) -> Vec<ShellItem> {
    let mut out: Vec<ShellItem> = Vec::with_capacity(items.len());
    for it in items {
        if it.separator && out.last().is_none_or(|l| l.separator) {
            continue;
        }
        out.push(it);
    }
    while out.last().is_some_and(|l| l.separator) {
        out.pop();
    }
    out
}

/// Fills the "more" submenu of an open menu with the shell's items for `target` (sidebar,
/// drives…); `me` receives "Rename" if the shell asks for it.
pub fn load_shell_more(menu: WeakEntity<fluent_menu::MenuView>, target: ShellTarget, me: Me, cx: &mut App) {
    let (tx, rx) = oneshot::channel();
    leaf_sys::sta::run(move |owner| {
        let _ = tx.send(leaf_sys::ctxmenu::build(owner, &target, false).map_err(|e| format!("{e:#}")));
    });
    cx.spawn(async move |cx| {
        let r = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
        let _ = cx.update(|cx| {
            let pending = match r {
                Ok((session, shell)) => {
                    let is_hidden = |it: &ShellItem| !it.verb.is_empty() && HIDDEN_VERBS.contains(&it.verb.to_ascii_lowercase().as_str());
                    let keep: Vec<ShellItem> = shell.into_iter().filter(|it| !is_hidden(it)).collect();
                    Pending::Ready(Rc::new(to_items(&me, session, &tidy(keep), cx)))
                }
                Err(e) => {
                    log::warn!("shell menu: {e}");
                    Pending::Failed
                }
            };
            if let Some(m) = menu.upgrade() {
                m.update(cx, |m, cx| m.set_pending("more", pending, cx));
            }
        });
    })
    .detach();
}

fn to_items(me: &Me, session: u64, items: &[ShellItem], cx: &mut App) -> Vec<Item> {
    items
        .iter()
        .map(|it| {
            if it.separator {
                return Item::Separator;
            }
            let icon = it.icon.as_ref().and_then(|i| fluent_menu::shell_icon(i, cx)).map(MenuIcon::Image).unwrap_or(MenuIcon::None);
            if !it.children.is_empty() {
                return Item::Sub { label: it.label.clone().into(), icon, items: Sub::Items(Rc::new(to_items(me, session, &tidy(it.children.clone()), cx))), click: None };
            }
            let (id, me) = (it.id, me.clone());
            let mut item = Item::action(it.label.clone(), icon, move |_, cx| invoke_shell(&me, session, id, cx)).enabled(it.enabled);
            if let Some(h) = &it.hint {
                item = item.hint(h.clone());
            }
            item
        })
        .collect()
}

fn invoke_shell(me: &Me, session: u64, id: u32, cx: &mut App) {
    let (tx, rx) = oneshot::channel();
    leaf_sys::sta::run(move |owner| {
        let r = leaf_sys::ctxmenu::invoke(owner, session, id, cursor());
        if let Err(e) = &r {
            log::warn!("shell command: {e:#}");
        }
        let _ = tx.send(r.ok());
    });
    let me = me.clone();
    cx.spawn(async move |cx| {
        let outcome = rx.await;
        let _ = me.update(cx, |v, cx| {
            let shell_folder = matches!(v.location(), Location::Shell { .. });
            match outcome {
                // a phone's folder has no inline rename here
                Ok(Some(MenuOutcome::Rename)) if !shell_folder => {
                    v.request_rename();
                    cx.notify();
                }
                // what the shell did on a device (a deletion, a new folder) isn't watched: list again
                Ok(Some(MenuOutcome::Invoked(_) | MenuOutcome::Rename)) if shell_folder => v.refresh(cx),
                _ => {}
            }
        });
    })
    .detach();
}

fn shift_down() -> bool {
    leaf_sys::input::shift_down()
}

impl FolderView {
    /// Right-click in a shell folder (a phone, Libraries…): our menu, filled with the shell's
    /// own commands for the selection or the folder (found by parsing names, as they have no
    /// path). Shift gives the classic menu instead, like elsewhere.
    fn show_shell_fluent_menu(&mut self, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Location::Shell { parsing, .. } = self.location().clone() else { return };
        let mut sel: Vec<u32> = self.selected.iter().copied().collect();
        sel.sort_unstable();
        let items: Vec<PathBuf> = if background { Vec::new() } else { sel.iter().filter_map(|&i| self.shell_items.get(i as usize)).map(|c| PathBuf::from(&c.parsing)).collect() };
        let background = items.is_empty();
        let me = cx.entity().downgrade();
        let mut menu = Menu::default();
        if !background {
            menu.quick.push(Quick::new(L::Copy, t!("Copy  (Ctrl+C)"), true, act(&me, |v, w, cx| v.copy_to_clipboard(Transfer::Copy, w, cx))));
        }
        menu.items.push(Item::Sub { label: t!("Show more options").into(), icon: icon(L::Ellipsis), items: Sub::Pending { key: "more", fallback: None }, click: None });
        if background {
            menu.items.push(Item::Separator);
            menu.items.push(Item::action(t!("Refresh"), icon(L::RotateCw), act(&me, |v, _, cx| v.refresh(cx))).hint("Ctrl+R"));
        }
        let menu_view = fluent_menu::show(menu, window.mouse_position(), window, cx);
        let target = match items.as_slice() {
            [] => ShellTarget::Background(PathBuf::from(&parsing)),
            [one] => ShellTarget::One(one.clone()),
            _ => ShellTarget::Items(items),
        };
        let (tx, rx) = oneshot::channel();
        leaf_sys::sta::run(move |owner| {
            let _ = tx.send(leaf_sys::ctxmenu::build(owner, &target, false).map_err(|e| format!("{e:#}")));
        });
        let menu_weak = menu_view.downgrade();
        cx.spawn(async move |_, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = cx.update(|cx| {
                let more = match result {
                    Ok((session, shell)) => Pending::Ready(Rc::new(to_items(&me, session, &tidy(shell), cx))),
                    Err(e) => {
                        log::info!("shell folder menu: {e}");
                        Pending::Failed
                    }
                };
                if let Some(m) = menu_weak.upgrade() {
                    m.update(cx, |m, cx| {
                        m.set_pending("more", more, cx);
                        m.inline_pending("more", cx);
                    });
                }
            });
        })
        .detach();
    }
}

/// The file to make the desktop background of `p`: `p` itself, or for a picture inside an archive a copy of it in the app's
/// `wallpapers` folder (one file per name, replaced when the same name is chosen again). Blocks (extraction): a worker thread.
fn wallpaper_file(p: &std::path::Path) -> Option<std::path::PathBuf> {
    if p.exists() || crate::archive_view::split(p).is_none() {
        return Some(p.to_path_buf());
    }
    let extracted = crate::archive_view::real_copy(p)?;
    let dir = crate::settings::app_dir()?.join("wallpapers");
    std::fs::create_dir_all(&dir).ok()?;
    let to = dir.join(p.file_name()?);
    std::fs::copy(&extracted, &to).ok()?;
    Some(to)
}

/// "Copy path as" submenu: the variants of `rf_core::copy_path` for the items (repository one only inside a
/// repository, WSL only on Windows).
fn copy_path_items(me: &Me, items: &[PathBuf]) -> Vec<Item> {
    use rf_core::copy_path::CopyPathKind as K;
    let in_repo = items.first().is_some_and(|p| rf_core::copy_path::repo_root(p).is_some());
    let mut rows: Vec<(String, K)> = vec![
        (t!("Full path").into(), K::Full),
        (t!("Name only").into(), K::Name),
        (t!("Folder path").into(), K::Folder),
        (t!("With forward slashes").into(), K::Slashes),
    ];
    if in_repo {
        rows.push((t!("Relative to repository").into(), K::Relative));
    }
    rows.push(("@path".into(), K::At));
    if cfg!(windows) {
        rows.push((t!("WSL path").into(), K::Wsl));
    }
    rows.push((t!("File URL").into(), K::Url));
    rows.push((t!("Quoted").into(), K::Quoted));
    rows.into_iter()
        .map(|(label, kind)| Item::action(label, icon(L::ClipboardCopy), act(me, move |v, _, cx| v.copy_path_as(kind, cx))))
        .collect()
}
