use gpui_kit::*;

gpui_kit::actions!(
    leaf,
    [
        GoBack,
        GoForward,
        LeaveTree,
        FocusNextPane,
        FocusPrevPane,
        DialogNext,
        DialogPrev,
        GoUp,
        GoHome,
        Refresh,
        OpenSelected,
        SelectNext,
        SelectPrev,
        SelectFirst,
        SelectLast,
        SelectAll,
        ExtendNext,
        ExtendPrev,
        PageDown,
        PageUp,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        ReopenClosedTab,
        FocusAddress,
        FocusSearch,
        SearchInContents,
        ToggleHidden,
        OpenTerminal,
        Properties,
        ClearSelection,
        CopyItems,
        CutItems,
        PasteItems,
        PasteIntoSelected,
        PasteShortcut,
        DeleteItems,
        DeletePermanently,
        DuplicateItems,
        CopyToOtherPane,
        MoveToOtherPane,
        RenameItem,
        NewFolder,
        NewTextFile,
        ShowContextMenu,
        CopyPath,
        OpenWith,
        SelectLeft,
        SelectRight,
        ViewDetails,
        ViewGrid,
        TogglePreview,
        ToggleSidebar,
        ToggleDualPane,
        SwitchPane,
        ToggleCommandPalette,
        SearchFolderHistory,
        SearchTabs,
        GoSettings,
        GoRecycleBin,
        ViewColumns,
        ViewCards,
        ViewList,
        Undo,
        MapNetworkDrive,
        RestartApp,
        SaveSession,
        DisconnectNetworkDrive,
        NewWindow,
        ZoomIn,
        ZoomOut,
        AddToShelf,
        InvertSelection,
        ToggleSelection,
        ToggleShelf,
        ToggleFullscreen,
        ToggleAlwaysOnTop,
        ToggleCompactOverlay,
        ToggleAddressFullPath,
        GoToFolderAlias,
        RotateRight,
        RotateLeft,
        ConvertToVideo,
        MountImage,
        EjectImage,
        MountDiscs,
        ToggleToolbar,
        Redo,
        DuplicateTab,
        CloseOtherTabs,
        CopyPathQuoted,
        CopyPathName,
        CopyPathFolder,
        CopyPathSlashes,
        CopyPathRelative,
        CopyPathAt,
        CopyPathWsl,
        CopyPathUrl,
        OpenInIde,
        ToggleTheme,
        CloseAllTabs,
        SplitPaneSideBySide,
        SplitPaneStacked,
        CloseActivePane,
        OpenInNewWindow,
        OpenTerminalAdmin,
        OrganizeFolder,
        CompareFolders,
        FindDuplicates,
        ShowFeatures,
        CustomizeHome,
        ResetHome,
        ShowTransferHistory,
        CopyAsContext,
        ToggleHideIgnored,
        FindBuildJunk,
        ShowChanges,
        ReviewChanges,
        FollowRemoteFile,
        Checkpoint,
        ShowCheckpoints,
        SyncJobs,
        ShowAgents,
        SyncWith,
        ShareFolder,
        JoinSharedFolder,
        SharedFolders,
        VpnConnect,
        VpnDisconnect,
        ShowWorktrees,
        OpenDocker,
        NewWorktree,
        NewLane,
        EndLane,
        ShowProjectBrain,
        BookmarkRemoteFolder,
        DownloadHere,
        DownloadMany,
        DownloadNzb,
        UsenetPauseAll,
        SendToDevice,
        AddToBasket,
        RemoveFromBasket,
        NewBasket,
        CopyBasketAsContext,
        CopyBasketAsMentions,
    ]
);

/// Runs one of the user's own actions (`actions.toml`) by name. Not one of the registry's fixed rows: those of the user's are made
/// when the file is read (`custom_actions`).
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = leaf, no_json)]
pub struct RunCustomAction {
    pub name: String,
    /// The keys this binding was made for; empty when it comes from the palette or a menu. A key that is no longer the action's
    /// shortcut (it was changed) still has its old binding until restart, which does nothing.
    pub keys: String,
}

pub const FOLDER_CTX: &str = "FolderView";
/// The controls of a Home widget in Customize mode. Left and Right are bound to the file list's selection moves in
/// [`FOLDER_CTX`], and a binding is matched before any key listener is asked, so the controls' own key handler (Left / Right move the
/// widget) never saw them. `NoAction` in this nearer context lets the keys through to it.
pub const HOME_CONTROLS_CTX: &str = "HomeControls";
/// The folder tree's key context. Tab is bound in it (to [`LeaveTree`]) because the component library binds Tab at the window's
/// root to "focus the next tab stop": a binding is matched before any key listener is asked, so the tree's own key handler never
/// saw Tab, and the keyboard stayed in the tree (issue 38: "TAB seems to be buggy, mine is not going anywhere"). A binding in a
/// context nearer to the focus wins over the root's.
pub const TREE_CTX: &str = "FolderTree";
/// The question dialog's key context: Tab and Shift+Tab move between its buttons there (bound here for the same reason as the
/// tree's Tab: the root's Tab binding would take the key first).
pub const DIALOG_CTX: &str = "Dialog";

/// User overrides from settings.toml `[keybindings]`. Invalid entries are logged and skipped.
pub fn bind_user(cx: &mut App) {
    let map = cx.global::<crate::settings::Settings>().keybindings.clone();
    let mut bindings = Vec::new();
    for (keys, action) in map {
        if let Some(n) = action.strip_prefix(crate::custom_actions::ID_PREFIX) {
            match crate::custom_actions::binding(n, &keys) {
                Some(b) => bindings.push(b),
                None => log::warn!("keybinding {keys:?}: not a key"),
            }
            continue;
        }
        let name = if action.contains("::") { action.clone() } else { format!("leaf::{action}") };
        // the command's own default keys stop working once the user gave it another shortcut
        bindings.extend(crate::registry::unbind_defaults(&action, &keys));
        match cx.build_action(&name, None) {
            Ok(a) => match KeyBinding::load(&keys, a, None, false, None, cx) {
                Ok(b) => bindings.push(b),
                Err(e) => log::warn!("keybinding {keys:?}: {e}"),
            },
            Err(e) => log::warn!("keybinding {keys:?} -> {action:?}: {e}"),
        }
    }
    cx.bind_keys(bindings);
}

/// The default key bindings: every registry row's `default_keys` (`registry.rs`). Tab is bound in the tree's, the list's and the
/// dialog's own contexts there (issue 38): a binding in a context nearer to the focus wins over the component library's root Tab.
pub fn bind(cx: &mut App) {
    cx.bind_keys(crate::registry::default_bindings());
    cx.bind_keys(["left", "right", "alt-left", "alt-right"].map(|k| KeyBinding::new(k, gpui_kit::NoAction, Some(HOME_CONTROLS_CTX))));
    bind_layout_shifted(cx);
}
/// GPUI reports Shift+digit as the layout's shifted character without Shift (Turkish Q:
/// Ctrl+Shift+2 arrives as `ctrl-'`), so `ctrl-shift-2` alone never fires. Bind the
/// layout-specific form too.
fn bind_layout_shifted(cx: &mut App) {
    // (virtual key, action)
    let wanted: [(u16, Box<dyn Fn() -> Box<dyn Action>>); 6] = [
        (0x31, Box::new(|| Box::new(ViewDetails))),
        (0x32, Box::new(|| Box::new(ViewGrid))),
        (0x33, Box::new(|| Box::new(ViewColumns))),
        (0x34, Box::new(|| Box::new(ViewCards))),
        (0x35, Box::new(|| Box::new(ViewList))),
        (0xC0, Box::new(|| Box::new(OpenTerminal))), // VK_OEM_3 (` on US layouts)
    ];
    let mut bindings = Vec::new();
    for (vk, action) in wanted {
        let Some(ch) = leaf_sys::keys::shifted_char(vk) else { continue };
        // AZERTY: Shift+& gives "1"; binding ctrl-1 would steal a plain ctrl+digit shortcut
        if ch.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        // Turkish Q: Shift+4 is "+" — Ctrl++ stays zoom
        if matches!(ch.as_str(), "+" | "-" | "=") {
            continue;
        }
        let keys = format!("ctrl-{ch}");
        match KeyBinding::load(&keys, action(), None, false, None, cx) {
            Ok(b) => bindings.push(b),
            Err(e) => log::debug!("layout binding {keys:?}: {e}"),
        }
    }
    cx.bind_keys(bindings);
}
