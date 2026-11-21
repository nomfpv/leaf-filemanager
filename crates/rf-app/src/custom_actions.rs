//! Custom actions (B2): the user's own commands from `actions.toml` in the config folder. The file's format, the placeholders and
//! where an action is offered are `rf_core::actions` (pure); this file is the rest: reading the file at start and when it
//! changes (looked at every two seconds), the rows in the command registry (`registry::custom`), the keys, the "Your actions"
//! part of the right-click menu, and running one.
//!
//! A file that cannot be read is logged and ignored (the actions already loaded stay); it is kept aside as `actions.toml.corrupt-<ms>`
//! only when the settings page saves over it, so what the user wrote is never lost silently.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::assets::IconName as L;
use gpui_kit::*;
use rf_core::actions::{self, Context, CustomAction, Place};

use crate::fluent_menu::{Item, MenuIcon};
use crate::i18n::tx;
use crate::registry::CommandSpec;

/// The registry id of an action is `action:` and its name; the shortcut editor stores a user's own keys under it.
pub const ID_PREFIX: &str = "action:";

#[derive(Default)]
pub struct State {
    pub actions: Vec<CustomAction>,
    /// The file is there but cannot be read: what is shown is what was read before.
    pub broken: bool,
    stamp: Option<(SystemTime, u64)>,
    /// (name, keys) already bound this run: bindings stack, so each is made once.
    bound: Vec<(String, String)>,
}
impl Global for State {}

pub fn path() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("actions.toml"))
}

pub fn list(cx: &App) -> Vec<CustomAction> {
    cx.try_global::<State>().map(|s| s.actions.clone()).unwrap_or_default()
}

pub fn broken(cx: &App) -> bool {
    cx.try_global::<State>().is_some_and(|s| s.broken)
}

fn stamp_of(p: &Path) -> Option<(SystemTime, u64)> {
    std::fs::metadata(p).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

/// Reads the file at start and then looks at it every two seconds.
pub fn init(cx: &mut App) {
    cx.set_global(State::default());
    reload(cx);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = cx.update(|cx| {
                let now = path().and_then(|p| stamp_of(&p));
                if cx.global::<State>().stamp != now {
                    reload(cx);
                }
            });
        }
    })
    .detach();
}

fn reload(cx: &mut App) {
    let Some(p) = path() else { return };
    let stamp = stamp_of(&p);
    let read = match rf_core::read_config(&p) {
        Ok(text) => actions::parse(&text).map(Some).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    };
    match read {
        Ok(list) => {
            cx.global_mut::<State>().broken = false;
            apply(list.unwrap_or_default(), cx);
        }
        Err(e) => {
            log::warn!("actions: {} cannot be read, ignored: {e}", p.display());
            cx.global_mut::<State>().broken = true;
            cx.refresh_windows();
        }
    }
    cx.global_mut::<State>().stamp = stamp;
}

pub fn icon_of(a: &CustomAction) -> L {
    match a.icon.as_deref().unwrap_or("").to_lowercase().as_str() {
        "code" | "editor" => L::Code,
        "terminal" | "square-terminal" => L::SquareTerminal,
        "folder" => L::Folder,
        "hash" => L::Hash,
        "play" => L::Play,
        _ => L::Zap,
    }
}

/// Makes the rows, binds the shortcuts, tells the windows.
fn apply(list: Vec<CustomAction>, cx: &mut App) {
    let rows: Vec<&'static CommandSpec> = list
        .iter()
        .map(|a| {
            let desc = format!("Runs {}.", a.command);
            let keys = a.shortcut.as_deref();
            crate::registry::custom_row(&a.name, &desc, keys, icon_of(a), a.offered_in(Place::Palette), a.offered_in(Place::Toolbar))
        })
        .collect();
    crate::registry::set_custom(rows);
    for a in &list {
        bind(a, cx);
    }
    cx.global_mut::<State>().actions = list;
    cx.refresh_windows();
}

/// The other command that has `keys` already: a built-in default, or a key the user set. Compared the way the shortcut editor does.
pub fn clash(keys: &str, own_id: &str, cx: &App) -> Option<String> {
    let want = crate::keymap_editor::canon(&crate::keymap_editor::pretty(keys));
    for c in crate::registry::COMMANDS {
        if c.default_keys.iter().any(|k| crate::keymap_editor::canon(&crate::registry::shown(k.keys)) == want) {
            return Some(c.title.to_string());
        }
    }
    for (k, a) in &cx.global::<crate::settings::Settings>().keybindings {
        if a != own_id && crate::keymap_editor::canon(&crate::keymap_editor::pretty(k)) == want {
            return Some(a.strip_prefix(ID_PREFIX).unwrap_or(a).to_string());
        }
    }
    None
}

/// Binds the `shortcut` of the file. Never over a key a built-in command has (or one the user set): that is logged and left
/// unbound, and the shortcut editor shows both rows as having the same shortcut.
fn bind(a: &CustomAction, cx: &mut App) {
    let Some(keys) = a.shortcut.clone() else { return };
    let id = format!("{ID_PREFIX}{}", a.name);
    // a key the user set in the shortcut editor replaces the file's
    if crate::keymap_editor::custom_keys(&id, cx).is_some() {
        return;
    }
    if let Some(other) = clash(&keys, &id, cx) {
        log::warn!("actions: {keys} of {:?} is {other}'s shortcut, not bound", a.name);
        return;
    }
    if cx.global::<State>().bound.iter().any(|(n, k)| *n == a.name && *k == keys) {
        return;
    }
    match binding(&a.name, &keys) {
        Some(b) => {
            cx.bind_keys([b]);
            cx.global_mut::<State>().bound.push((a.name.clone(), keys));
        }
        None => log::warn!("actions: shortcut {keys:?} of {:?} is not a key", a.name),
    }
}

/// The key binding of an action's shortcut: in the file list's key context, the one the built-in file commands (Copy path ...)
/// are bound in. `None` when `keys` is not a valid keystroke.
pub fn binding(name: &str, keys: &str) -> Option<KeyBinding> {
    // (the binding wants its keys for good; one is made per bound shortcut)
    let kept: &'static str = Box::leak(keys.to_string().into_boxed_str());
    if kept.split(' ').any(|k| Keystroke::parse(k).is_err()) {
        return None;
    }
    Some(KeyBinding::new(kept, crate::actions::RunCustomAction { name: name.to_string(), keys: keys.to_string() }, Some(crate::actions::FOLDER_CTX)))
}

/// Whether a key press that reached the binding made for `keys` is still the shortcut of the action `name` (the user's own, else
/// the file's). Palette and menu runs carry no keys and always run.
fn current(name: &str, keys: &str, cx: &App) -> bool {
    if keys.is_empty() {
        return true;
    }
    let id = format!("{ID_PREFIX}{name}");
    crate::keymap_editor::custom_keys(&id, cx).or_else(|| file_shortcut(&id, cx)).is_some_and(|k| k.eq_ignore_ascii_case(keys))
}

/// The file's shortcut of the action with registry id `id` (for the shortcut editor's list of clashes).
pub fn file_shortcut(id: &str, cx: &App) -> Option<String> {
    let name = id.strip_prefix(ID_PREFIX)?;
    cx.try_global::<State>()?.actions.iter().find(|a| a.name == name).and_then(|a| a.shortcut.clone())
}

/// Writes the list to `actions.toml` (the file the user may also edit by hand) and reads it again.
pub fn save(list: Vec<CustomAction>, cx: &mut App) -> anyhow::Result<()> {
    let p = path().ok_or_else(|| anyhow::anyhow!("no config folder"))?;
    if cx.global::<State>().broken && p.exists() {
        rf_core::backup_corrupt(&p);
    }
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, actions::to_toml(&list))?;
    std::fs::rename(&tmp, &p)?;
    reload(cx);
    Ok(())
}

/// Opens `actions.toml` in the program the system has for it (made empty first when it is not there yet).
pub fn open_file(cx: &mut App) {
    let Some(p) = path() else { return };
    if !p.exists() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, "# Your actions. See Settings > Your actions for the fields.\n");
    }
    let _ = cx;
    std::thread::spawn(move || {
        if let Err(e) = leaf_sys::shell::open(&p) {
            log::warn!("actions: cannot open {}: {e:#}", p.display());
        }
    });
}

/// The "Your actions" submenu for what is selected (`items`: path and whether it is a folder), or for the folder background.
pub fn menu_section(items: &[(PathBuf, bool)], dir: &Path, background: bool, cx: &App) -> Option<Item> {
    let own: Vec<CustomAction> = list(cx)
        .into_iter()
        .filter(|a| if background { a.matches_background() } else { a.matches_items(items) })
        .collect();
    if own.is_empty() {
        return None;
    }
    let files: Vec<PathBuf> = if background { Vec::new() } else { items.iter().map(|(p, _)| p.clone()).collect() };
    let rows = own
        .into_iter()
        .map(|a| {
            let (files, dir) = (files.clone(), dir.to_path_buf());
            let shown = crate::keymap_editor::custom_keys(&format!("{ID_PREFIX}{}", a.name), cx).or_else(|| a.shortcut.clone());
            let item = Item::action(a.name.clone(), MenuIcon::Lucide(icon_of(&a)), move |w, cx| run(a.clone(), files.clone(), Some(dir.clone()), w, cx));
            match shown {
                Some(k) => item.hint(crate::registry::shown(&k)),
                None => item,
            }
        })
        .collect();
    Some(Item::sub(crate::i18n::t("Your actions"), MenuIcon::Lucide(L::Zap), rows))
}

/// Runs the action called `name` on what the file list has selected (a key, the palette).
pub fn run_named(name: &str, keys: &str, files: Vec<PathBuf>, dir: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    if !current(name, keys, cx) {
        return;
    }
    if let Some(a) = list(cx).into_iter().find(|a| a.name == name) {
        run(a, files, dir, window, cx);
    }
}

/// Runs `a`: expands the command for the selection, asks first when the action says so, starts it off the UI thread. A failure is
/// a notice, never silence.
pub fn run(a: CustomAction, files: Vec<PathBuf>, dir: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let ctx = Context { files: &files, dir: dir.as_deref() };
    let argv = match actions::expand(&a.command, &ctx) {
        Ok(v) => v,
        Err(e) => return failed(&a.name, tx(match e {
            actions::ExpandError::NeedsSelection => "Select a file or folder first.",
            actions::ExpandError::NeedsFolder => "Open a folder first.",
            actions::ExpandError::Unclosed => "A quote in the command is not closed.",
            actions::ExpandError::Empty => "The command is empty.",
        }).to_string(), window, cx),
    };
    let env = actions::environment(&ctx);
    let question = a.confirm.then(|| format!("{}

{}", a.name, a.command));
    let (name, terminal) = (a.name.clone(), a.terminal.then(|| cx.global::<crate::settings::Settings>().terminal));
    let go = move |window: &mut Window, cx: &mut App| start(name.clone(), terminal, argv.clone(), env.clone(), dir.clone(), window, cx);
    if let Some(shown) = question {
        let handle = window.window_handle();
        let rx = crate::prompt::ask(tx("Run this action?"), shown, window, cx);
        cx.spawn(async move |cx| {
            if rx.await.unwrap_or(false) {
                let _ = handle.update(cx, |_, window, cx| go(window, cx));
            }
        })
        .detach();
    } else {
        go(window, cx);
    }
}

fn failed(name: &str, message: String, window: &mut Window, cx: &mut App) {
    crate::prompt::notice(format!("{} {name}", tx("Your action failed:")), message, window, cx);
}

fn start(name: String, terminal: Option<rf_core::terminal::Terminal>, argv: Vec<OsString>, env: Vec<(&'static str, OsString)>, dir: Option<PathBuf>, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let (send, receive) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = send.send(launch(argv, env, dir, terminal));
    });
    cx.spawn(async move |cx| {
        if let Ok(Err(e)) = receive.await {
            let _ = handle.update(cx, |_, window, cx| failed(&name, format!("{e:#}"), window, cx));
        }
    })
    .detach();
}

/// The program as a path to start: an absolute one as it is, a bare name the way the Run box finds it.
fn resolve(program: &OsString, dir: Option<&Path>) -> anyhow::Result<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        anyhow::ensure!(path.exists(), "{}: {}", tx("Program not found."), path.display());
        return Ok(path.to_path_buf());
    }
    #[cfg(windows)]
    {
        let line = rf_core::addrcmd::CommandLine { program: program.to_string_lossy().into_owned(), args: String::new() };
        return leaf_sys::runfind::find(&[line], dir.unwrap_or(Path::new("")))
            .map(|f| f.exe)
            .ok_or_else(|| anyhow::anyhow!("{}: {}", tx("Program not found."), path.display()));
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Ok(path.to_path_buf())
    }
}

/// Starts the program with the arguments as a list (no shell, nothing glued together). Off the UI thread. Hidden: waits for the
/// program to end and reports a failure (its exit code and the end of what it wrote to stderr). In a terminal: runs inside the
/// terminal chosen in Settings, in the folder, and the window stays open after the program ends; not waited for.
fn launch(argv: Vec<OsString>, env: Vec<(&'static str, OsString)>, dir: Option<PathBuf>, terminal: Option<rf_core::terminal::Terminal>) -> anyhow::Result<()> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut it = argv.into_iter();
    let program = it.next().ok_or_else(|| anyhow::anyhow!("{}", tx("The command is empty.")))?;
    let args: Vec<OsString> = it.collect();
    let work = dir.filter(|d| d.is_dir());
    let exe = resolve(&program, work.as_deref())?;
    if let Some(kind) = terminal {
        let at = work.as_deref().and_then(crate::terminal::target).or_else(|| dirs::home_dir()).unwrap_or_else(std::env::temp_dir);
        return leaf_sys::shell::open_terminal_running(kind, &at, &exe, &args, &env);
    }
    let mut cmd = Command::new(&exe);
    cmd.args(&args).envs(env);
    if let Some(w) = &work {
        cmd.current_dir(w);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    // stderr is read on a thread of its own: a program that starts another and leaves holds the pipe open long after it ended
    let (tx_err, rx_err) = std::sync::mpsc::channel::<String>();
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.by_ref().take(4096).read_to_end(&mut buf);
            let _ = tx_err.send(String::from_utf8_lossy(&buf).trim().to_string());
        });
    }
    let status = child.wait()?;
    if status.success() {
        return Ok(());
    }
    let said = rx_err.recv_timeout(Duration::from_millis(300)).unwrap_or_default();
    let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
    if said.is_empty() {
        anyhow::bail!("{} {code}", tx("The program ended with the code"));
    }
    anyhow::bail!("{} {code}\n{said}", tx("The program ended with the code"));
}

/// The actions with `where = toolbar`, in file order.
pub fn toolbar_actions(cx: &App) -> Vec<CustomAction> {
    list(cx).into_iter().filter(|a| a.offered_in(Place::Toolbar)).collect()
}

/// How many of the tool bar's actions have room as buttons at this window width (logical px); the rest go to the More menu.
pub fn toolbar_fit(width: f32) -> usize {
    (((width - 820.0) / 150.0).floor().max(0.0)) as usize
}

/// The tool bar actions that did not fit, as items for the More menu (with a separator before them).
pub fn toolbar_overflow(width: f32, files: Vec<PathBuf>, dir: Option<PathBuf>, cx: &App) -> Vec<Item> {
    let rest: Vec<CustomAction> = toolbar_actions(cx).into_iter().skip(toolbar_fit(width)).collect();
    if rest.is_empty() {
        return Vec::new();
    }
    let mut v = vec![Item::Separator];
    v.extend(rest.into_iter().map(|a| {
        let (files, dir) = (files.clone(), dir.clone());
        Item::action(a.name.clone(), MenuIcon::Lucide(icon_of(&a)), move |w, cx| run(a.clone(), files.clone(), dir.clone(), w, cx))
    }));
    v
}
