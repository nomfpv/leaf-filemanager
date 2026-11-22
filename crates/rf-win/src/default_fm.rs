//! "Set as default file manager" (Files' Advanced setting), per-user (HKCU only, no admin):
//! folders and Win+E open in leaf. Same keys as Files' SetFilesAsDefault.reg. Archives
//! are offered as an app registration (see `app_writes`): the user picks leaf for them
//! in Windows' Default apps.
//!
//! The app must hand non-folder arguments (e.g. `::{CLSID}` shell namespaces like Control Panel)
//! back to explorer.exe — see `rf-app` main.
//!
//! What it takes over, and why each:
//! - `Folder\shell\open` and `Folder\shell\explore`: every folder the shell opens. Also "Show in folder" in browsers, editors and
//!   Task Manager (`SHOpenFolderAndSelectItems`: Windows opens the folder through this verb and names the file to a window it finds
//!   in its window list, see `shell_window`), and `explorer.exe <folder>` started by another program (what leaf hands to Explorer
//!   comes back through this verb, hence the loop guard below).
//! - `CLSID\{52205fd8-…}\shell\opennewwindow`: Win+E. Empty `DelegateExecute` so the command wins over HKLM's handler.
//!
//! Everything it writes is recorded first (`default_fm_journal.rs`), so turning it off puts back exactly what was there.

use windows_registry::CURRENT_USER;
#[cfg(test)]
use windows_registry::Key;

#[path = "default_fm_journal.rs"]
mod journal;
use journal::Write;

pub const CLASSES: &str = r"Software\Classes";
const WIN_E_CLSID: &str = r"CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";
/// zip (CompressedFolder) and 7z, rar, tar, gz… (ArchiveFolder): their `open` verbs. Earlier
/// versions pointed these at leaf; no more: "Open with" then merged Windows' own handler
/// ("File Explorer") with ours into one entry with Explorer's name. Ours are removed.
const ARCHIVE_VERBS: [&str; 2] = [r"CompressedFolder\shell\open", r"ArchiveFolder\shell\open"];

#[cfg(test)]
fn set_command(base: &Key, sub: &str, command: &str) -> anyhow::Result<()> {
    let k = base.create(sub)?;
    k.set_string("", command)?;
    k.set_string("DelegateExecute", "")?;
    Ok(())
}

/// The verbs' `command` keys (under HKCU): whether they start leaf tells whether a journal is still ours.
fn command_keys(classes_root: &str) -> Vec<String> {
    [r"Folder\shell\open\command".to_string(), r"Folder\shell\explore\command".to_string(), format!(r"{WIN_E_CLSID}\shell\opennewwindow\command")]
        .into_iter()
        .map(|k| format!(r"{classes_root}\{k}"))
        .collect()
}

/// The launcher (crate `leaf-open`): what the verbs start when this leaf has it beside itself. It starts the leaf.exe its path
/// file names, or the real Explorer when that leaf is gone, so a rebuilt, cleaned or deleted leaf never leaves Windows without a
/// folder handler.
const LAUNCHER_STEM: &str = "leaf-open";
/// The launcher's path file (`leaf_open::PATH_FILE`, kept the same by hand: rf-win does not depend on the launcher).
const LAUNCHER_PATH_FILE: &str = "leaf-exe.txt";

/// The launcher shipped next to `exe` (built next to leaf.exe, installed next to it).
fn launcher_source(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let p = exe.with_file_name(crate::exe_name(LAUNCHER_STEM));
    p.is_file().then_some(p)
}

/// Whether this leaf has the launcher beside it (then even a copy in a build folder is safe to register).
pub fn launcher_beside(exe: &std::path::Path) -> bool {
    launcher_source(exe).is_some()
}

fn is_launcher_path(p: &std::path::Path) -> bool {
    p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(LAUNCHER_STEM))
}

/// The leaf.exe a registered launcher starts (its path file).
fn launcher_target(launcher: &std::path::Path) -> Option<std::path::PathBuf> {
    let t = std::fs::read_to_string(launcher.with_file_name(LAUNCHER_PATH_FILE)).ok()?;
    Some(std::path::PathBuf::from(t.trim())).filter(|p| !p.as_os_str().is_empty())
}

/// Puts the launcher in place for `exe`: copied into the state folder (when it differs from the one there) and its path file
/// naming `exe`. Its path; None when this leaf has none beside it or it could not be put there (leaf.exe is registered then).
fn install_launcher(exe: &str, classes_root: &str) -> Option<std::path::PathBuf> {
    let src = launcher_source(std::path::Path::new(exe))?;
    let dir = journal::state_dir(classes_root);
    std::fs::create_dir_all(&dir).ok()?;
    let dest = dir.join(crate::exe_name(LAUNCHER_STEM));
    let same = matches!((std::fs::read(&src), std::fs::read(&dest)), (Ok(a), Ok(b)) if a == b);
    if !same {
        // a temp copy renamed over: a launcher that is running at that moment keeps the old one, which works as well
        let tmp = dir.join(format!("{LAUNCHER_STEM}.tmp"));
        let copied = std::fs::copy(&src, &tmp).is_ok() && std::fs::rename(&tmp, &dest).is_ok();
        let _ = std::fs::remove_file(&tmp);
        if !copied && !dest.is_file() {
            return None;
        }
    }
    let file = dir.join(LAUNCHER_PATH_FILE);
    let tmp = dir.join(format!("{LAUNCHER_PATH_FILE}.tmp"));
    (std::fs::write(&tmp, exe).is_ok() && std::fs::rename(&tmp, &file).is_ok()).then_some(dest)
}

/// The launcher copy and its path file go (when turned off).
fn remove_launcher(classes_root: &str) {
    let dir = journal::state_dir(classes_root);
    let _ = std::fs::remove_file(dir.join(crate::exe_name(LAUNCHER_STEM)));
    let _ = std::fs::remove_file(dir.join(LAUNCHER_PATH_FILE));
}

/// Registers `exe` under `classes_root` (normally `CLASSES`; tests pass a scratch key). What was there before is recorded first
/// (see `default_fm_journal.rs`); already on, the record from the first time stays. All or nothing.
///
/// The verbs start the launcher when this leaf has one beside it (see [`LAUNCHER_STEM`]), else `exe` itself. The archive
/// registration always names `exe`: an archive is not something Windows needs to open to work.
pub fn enable(exe: &str, classes_root: &str) -> anyhow::Result<()> {
    let was_on = is_enabled(classes_root);
    let launcher = install_launcher(exe, classes_root);
    let target = launcher.as_ref().map_or_else(|| exe.to_string(), |l| l.display().to_string());
    let target = target.as_str();
    let cmd = format!("\"{target}\" \"%1\"");
    let keys = command_keys(classes_root);
    let mut writes = Vec::new();
    for (key, cmd) in [(&keys[0], cmd.clone()), (&keys[1], cmd), (&keys[2], format!("\"{target}\""))] {
        writes.push(Write::sz(key.as_str(), "", cmd));
        writes.push(Write::sz(key.as_str(), "DelegateExecute", ""));
    }
    writes.extend(app_writes(exe, classes_root));
    let r = journal::record_and_apply(target, classes_root, &keys, &writes, || {
        // leaf's own older keys (not recorded: they were never anyone else's)
        remove_archive_verbs(classes_root);
        // a plain command: `DelegateExecute` (needed above to beat HKLM's) would make Windows list this handler as File Explorer
        if let Ok(base) = CURRENT_USER.open(classes_root) {
            let _ = base.remove_tree(format!(r"{PROGID}\shell"));
        }
    });
    if r.is_err() && !was_on {
        remove_launcher(classes_root);
    }
    r
}

/// Archive types leaf browses, offered in Windows' Default apps and "Open with". Which
/// app opens them is the user's choice (Windows keeps it signed; no program may set it).
pub const ARCHIVE_EXTS: [&str; 9] = [".zip", ".7z", ".rar", ".tar", ".tgz", ".txz", ".tzst", ".tbz2", ".tbz"];
const PROGID: &str = "leaf.Archive";
/// Name under RegisteredApplications (`ms-settings:defaultapps?registeredAppUser=leaf`).
pub const APP_NAME: &str = "leaf";

/// `Software` for `Software\Classes` (tests use a scratch root).
fn software_root(classes_root: &str) -> &str {
    classes_root.strip_suffix(r"\Classes").unwrap_or(classes_root)
}

/// Registers leaf as an app that opens archives (Default Programs capabilities): it
/// can then be picked in Settings → Default apps or "Open with → Always". Changes no default.
/// The values, for `enable` to record and write.
fn app_writes(exe: &str, classes_root: &str) -> Vec<Write> {
    let c = classes_root;
    let mut w = vec![
        Write::sz(format!(r"{c}\{PROGID}"), "", "Compressed archive"),
        // the zip folder icon, not ours: archives must still look like archives
        Write::expand_sz(format!(r"{c}\{PROGID}\DefaultIcon"), "", r"%SystemRoot%\system32\zipfldr.dll"),
        Write::sz(format!(r"{c}\{PROGID}\shell\open\command"), "", format!("\"{exe}\" \"%1\"")),
        // the name and icon "Open with" / Default apps show for this handler
        Write::sz(format!(r"{c}\{PROGID}\Application"), "ApplicationName", APP_NAME),
        Write::sz(format!(r"{c}\{PROGID}\Application"), "ApplicationIcon", format!("\"{exe}\",0")),
        Write::sz(format!(r"{c}\{PROGID}\Application"), "ApplicationCompany", APP_NAME),
    ];
    for e in ARCHIVE_EXTS {
        w.push(Write::sz(format!(r"{c}\{e}\OpenWithProgids"), PROGID, ""));
    }
    let root = software_root(classes_root);
    let caps_path = format!(r"{root}\{APP_NAME}\Capabilities");
    w.push(Write::sz(caps_path.as_str(), "ApplicationName", APP_NAME));
    w.push(Write::sz(caps_path.as_str(), "ApplicationDescription", "Fast file manager"));
    for e in ARCHIVE_EXTS {
        w.push(Write::sz(format!(r"{caps_path}\FileAssociations"), e, PROGID));
    }
    w.push(Write::sz(format!(r"{root}\RegisteredApplications"), APP_NAME, caps_path.as_str()));
    w
}

fn unregister_app(classes_root: &str) {
    let Ok(base) = CURRENT_USER.create(classes_root) else { return };
    let _ = base.remove_tree(PROGID);
    for e in ARCHIVE_EXTS {
        if let Ok(k) = base.open(format!(r"{e}\OpenWithProgids")) {
            let _ = k.remove_value(PROGID);
        }
    }
    let root = software_root(classes_root);
    let _ = CURRENT_USER.remove_tree(format!(r"{root}\{APP_NAME}"));
    if let Ok(k) = CURRENT_USER.open(format!(r"{root}\RegisteredApplications")) {
        let _ = k.remove_value(APP_NAME);
    }
}

/// True when the archive app registration points at `exe`.
fn app_registered(exe: &str, classes_root: &str) -> bool {
    verb_command(classes_root, &format!(r"{PROGID}\shell\open")).is_some_and(|c| c == format!("\"{exe}\" \"%1\""))
        && CURRENT_USER.open(format!(r"{}\RegisteredApplications", software_root(classes_root))).and_then(|k| k.get_string(APP_NAME)).is_ok()
        // registrations before the name fix had DelegateExecute and no Application key
        && CURRENT_USER.open(format!(r"{classes_root}\{PROGID}\Application")).and_then(|k| k.get_string("ApplicationName")).is_ok()
        && CURRENT_USER.open(format!(r"{classes_root}\{PROGID}\shell\open\command")).and_then(|k| k.get_string("DelegateExecute")).is_err()
}

fn verb_command(classes_root: &str, verb: &str) -> Option<String> {
    CURRENT_USER.open(format!(r"{classes_root}\{verb}\command")).and_then(|k| k.get_string("")).ok()
}

fn ours(classes_root: &str, verb: &str) -> bool {
    verb_command(classes_root, verb).is_some_and(|v| is_our_command(&v))
}

/// The program a registry command line starts: `"<dir>/leaf.exe" "%1"` -> `<dir>/leaf.exe` (quoted, or up to the first space).
fn command_exe(command: &str) -> Option<&str> {
    let v = command.trim();
    match v.strip_prefix('"') {
        Some(rest) => rest.split('"').next(),
        None => v.split(' ').next(),
    }
    .filter(|e| !e.is_empty())
}

/// Whether `exe` is this app: its file name is `leaf.exe` or the launcher's `leaf-open.exe` (any case). A file-name test, not a
/// substring one: a path such as `D:/leaf-filemanager/other.exe` or a program called `leafpad.exe` is somebody else's.
fn is_our_exe(exe: &str) -> bool {
    let p = std::path::Path::new(exe);
    p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(crate::APP_EXE_STEM) || s.to_string_lossy().eq_ignore_ascii_case(LAUNCHER_STEM))
        && p.extension().is_none_or(|e| e.eq_ignore_ascii_case("exe"))
}

/// Whether a registry command line starts this app.
fn is_our_command(command: &str) -> bool {
    command_exe(command).is_some_and(is_our_exe)
}

/// Our old archive verb overrides go (another app's own ones stay).
fn remove_archive_verbs(classes_root: &str) -> bool {
    let Ok(base) = CURRENT_USER.create(classes_root) else { return false };
    let mut removed = false;
    for v in ARCHIVE_VERBS {
        if ours(classes_root, v) {
            removed |= base.remove_tree(v).is_ok();
        }
    }
    removed
}

/// Brings an older registration up to date: the archive app registration added, the old archive
/// verb overrides removed. True when it changed something.
pub fn add_archives(exe: &str, classes_root: &str) -> bool {
    if !is_enabled(classes_root) {
        return false;
    }
    // written again whole, through the journal (the state from the first time stays recorded)
    let stale = ARCHIVE_VERBS.iter().any(|v| ours(classes_root, v)) || !app_registered(exe, classes_root);
    stale && enable(exe, classes_root).is_ok()
}

/// Turns it off: what the journal recorded comes back exactly (a key another program took since stays that program's). A
/// registration from before the journal: our overrides are removed (the system defaults under HKLM take over again).
pub fn disable(classes_root: &str) -> anyhow::Result<()> {
    if let Some(r) = journal::restore_recorded(classes_root) {
        remove_archive_verbs(classes_root);
        if r.is_ok() {
            remove_launcher(classes_root);
        }
        return r;
    }
    let base = CURRENT_USER.create(classes_root)?;
    for sub in [r"Folder\shell\open", r"Folder\shell\explore", WIN_E_CLSID] {
        // missing keys are fine
        let _ = base.remove_tree(sub);
    }
    remove_archive_verbs(classes_root);
    unregister_app(classes_root);
    remove_launcher(classes_root);
    Ok(())
}

pub fn is_enabled(classes_root: &str) -> bool {
    CURRENT_USER
        .open(format!(r"{classes_root}\Folder\shell\open\command"))
        .and_then(|k| k.get_string(""))
        .is_ok_and(|v| is_our_command(&v))
}

/// The exe our `Folder\shell\open` command points at (None when not enabled).
pub fn registered_exe(classes_root: &str) -> Option<std::path::PathBuf> {
    let v = CURRENT_USER.open(format!(r"{classes_root}\Folder\shell\open\command")).and_then(|k| k.get_string("")).ok()?;
    let exe = command_exe(&v)?;
    is_our_exe(exe).then(|| exe.into())
}

/// The leaf.exe the registration ends in: the program the launcher's path file names when the launcher is registered, else the
/// registered exe itself (None when not enabled).
pub fn registered_leaf(classes_root: &str) -> Option<std::path::PathBuf> {
    let reg = registered_exe(classes_root)?;
    if is_launcher_path(&reg) { launcher_target(&reg) } else { Some(reg) }
}

/// While we are the default, a registered exe that no longer exists (moved, rebuilt elsewhere,
/// its folder cleaned) would leave Windows unable to open any folder: point it at `current`. With the launcher registered, the
/// same for the leaf its path file names (the launcher opens Explorer meanwhile).
/// Returns true when it re-registered.
///
/// Also a registration that lost its `open` verb while its `explore` verb is still ours (issue 97: on a reporter's PC `Folder\shell\open`
/// was an empty key, so "Show in folder" from a browser went to Explorer, or to nothing, while every other folder opened here): it is
/// written again whole.
///
/// First, a journal whose verbs are no longer ours (another program took them, the keys were removed) is dropped, keys untouched.
pub fn heal(current: &str, classes_root: &str) -> bool {
    if journal::drop_if_stale(classes_root, &command_keys(classes_root)) {
        log::warn!("default file manager: its record was dropped, the keys are not ours any more");
    }
    if !std::path::Path::new(current).exists() {
        return false;
    }
    match registered_exe(classes_root) {
        // the launcher: it, or the leaf its path file names, is gone (a moved or deleted leaf: Explorer opens folders meanwhile)
        Some(l) if is_launcher_path(&l) => (!l.exists() || launcher_target(&l).is_none_or(|t| !t.exists())) && enable(current, classes_root).is_ok(),
        Some(old) if !old.exists() => enable(current, classes_root).is_ok(),
        // this leaf registered itself before it had the launcher: the launcher takes its place
        Some(old) if launcher_beside(&old) && old.to_string_lossy().eq_ignore_ascii_case(current) => enable(current, classes_root).is_ok(),
        Some(_) => false,
        None if verb_command(classes_root, r"Folder\shell\explore").is_some_and(|c| is_our_command(&c)) => {
            enable(current, classes_root).is_ok()
        }
        None => false,
    }
}

/// True when the argument should be handed back to Explorer (shell namespaces, non-folders).
pub fn should_forward_to_explorer(arg: &str) -> bool {
    let a = arg.trim().trim_matches('"');
    !a.is_empty() && (a.starts_with("::") || a.starts_with("shell:") || !std::path::Path::new(a).is_dir())
}

/// Opens `target` (a folder, a server root, a `::{CLSID}` or `shell:` namespace) in the real Explorer, through HKLM's `Folder`
/// class (Explorer's own handler) and not the merged one, so our HKCU verb cannot send it back to leaf. False when the shell
/// refused; the caller then starts explorer.exe.
pub fn open_in_explorer(target: &str) -> bool {
    use windows::Win32::System::Registry::HKEY;
    use windows::Win32::UI::Shell::{SEE_MASK_CLASSKEY, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};
    let Ok(class) = windows_registry::LOCAL_MACHINE.open(r"Software\Classes\Folder") else { return false };
    crate::shell::ensure_com();
    let file = crate::wide::wide(std::ffi::OsStr::new(target));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_CLASSKEY | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        hkeyClass: HKEY(class.as_raw()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.is_ok()
}

/// The real Explorer on `file`'s folder with `file` selected, in a way our verb cannot catch: not `SHOpenFolderAndSelectItems` or
/// `explorer.exe /select` (both open the folder through the verb we take over), but [`open_in_explorer`] on the folder, then the
/// new window's view (found in the shell's window list, as "Show in folder" finds a window) is asked to select the file. The
/// selecting waits for the window on a thread of its own (5 s at most). False when the folder did not open.
pub fn reveal_in_explorer(file: &std::path::Path) -> bool {
    let Some(dir) = file.parent().map(std::path::Path::to_path_buf) else { return false };
    if !open_in_explorer(&dir.to_string_lossy()) {
        return false;
    }
    let file = file.to_path_buf();
    std::thread::spawn(move || select_in_explorer(&dir, &file));
    true
}

fn select_in_explorer(dir: &std::path::Path, file: &std::path::Path) {
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IServiceProvider};
    use windows::Win32::System::Variant::{InitVariantFromBuffer, VARIANT};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        ILFindLastID, ILFree, ILGetSize, IShellBrowser, IShellWindows, SHParseDisplayName, SID_STopLevelBrowser, SVSI_DESELECTOTHERS, SVSI_ENSUREVISIBLE,
        SVSI_FOCUSED, SVSI_SELECT, SWC_BROWSER, SWFO_NEEDDISPATCH, ShellWindows,
    };
    use windows::core::{HSTRING, Interface};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(windows) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) else { return };
        let mut folder: *mut ITEMIDLIST = std::ptr::null_mut();
        let mut item: *mut ITEMIDLIST = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(dir.as_os_str()), None, &mut folder, 0, None).is_err() {
            return;
        }
        if SHParseDisplayName(&HSTRING::from(file.as_os_str()), None, &mut item, 0, None).is_err() {
            ILFree(Some(folder));
            return;
        }
        if let Ok(loc) = InitVariantFromBuffer(folder as *const _, ILGetSize(Some(folder))) {
            let empty = VARIANT::default();
            let flags = (SVSI_SELECT.0 | SVSI_DESELECTOTHERS.0 | SVSI_ENSUREVISIBLE.0 | SVSI_FOCUSED.0) as u32;
            for _ in 0..100 {
                let mut hwnd = 0i32;
                let done = windows
                    .FindWindowSW(&loc, &empty, SWC_BROWSER, &mut hwnd, SWFO_NEEDDISPATCH)
                    .and_then(|disp| disp.cast::<IServiceProvider>())
                    .and_then(|sp| sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser))
                    .and_then(|b| b.QueryActiveShellView())
                    // (a view that is still listing may refuse: asked again)
                    .and_then(|view| view.SelectItem(ILFindLastID(item), flags))
                    .is_ok();
                if done {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        ILFree(Some(item));
        ILFree(Some(folder));
    }
}

/// Explorer opens folders through the same `Folder\shell\open` verb we take over, so an
/// argument we hand to it can come straight back (a server root like `\\nas`, a folder that
/// looked like a file). Forwards are remembered for a few seconds to break such a loop.
fn forward_log() -> std::path::PathBuf {
    std::env::temp_dir().join("leaf-forward.txt")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// True when `arg` was handed to Explorer within the last few seconds.
pub fn forwarded_recently(arg: &str) -> bool {
    forwarded_within(&forward_log(), arg, now_ms(), 5_000)
}

/// Remembers that `arg` is being handed to Explorer.
pub fn note_forward(arg: &str) {
    note_in(&forward_log(), arg, now_ms());
}

fn forwarded_within(log: &std::path::Path, arg: &str, now: u128, window_ms: u128) -> bool {
    let Ok(text) = std::fs::read_to_string(log) else { return false };
    text.lines().filter_map(|l| l.split_once('\t')).any(|(t, a)| a == arg && t.parse::<u128>().is_ok_and(|t| now.saturating_sub(t) <= window_ms))
}

fn note_in(log: &std::path::Path, arg: &str, now: u128) {
    let old = std::fs::read_to_string(log).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().filter(|l| !l.is_empty()).collect();
    let keep = lines.len().saturating_sub(7);
    lines.drain(..keep);
    let mut out: String = lines.iter().map(|l| format!("{l}\n")).collect();
    out.push_str(&format!("{now}\t{arg}\n"));
    let _ = std::fs::write(log, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch key under HKCU\Software, removed when the test ends (also when an assertion fails).
    struct Scratch(String);
    impl Scratch {
        fn new(tag: &str) -> Self {
            Scratch(format!(r"Software\leaf-{tag}-{}", std::process::id()))
        }
        fn classes(&self) -> String {
            format!(r"{}\Classes", self.0)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = CURRENT_USER.remove_tree(&self.0);
        }
    }

    #[test]
    fn ours_is_told_by_the_programs_file_name() {
        assert!(is_our_command(r#""C:\x\leaf.exe" "%1""#));
        assert!(is_our_command(r#""C:\Program Files\LEAF.EXE""#));
        assert!(is_our_command(r"C:\x\leaf.exe %1"));
        assert!(!is_our_command(r#""D:\leaf-filemanager\target\other.exe" "%1""#), "a folder named like us is not us");
        assert!(!is_our_command(r#""C:\Tools\leafpad.exe" "%1""#));
        assert!(!is_our_command(r#""C:\x\leaf-look.exe""#), "the Preview program is not the file manager");
        assert!(!is_our_command(r"C:\Windows\explorer.exe"));
        assert!(!is_our_command(""));
    }

    #[test]
    fn enable_disable_in_scratch_key() {
        let scratch = Scratch::new("test");
        let root = scratch.classes();
        enable(r"C:\x\leaf.exe", &root).unwrap();
        assert!(is_enabled(&root));
        let k = CURRENT_USER.open(format!(r"{root}\{WIN_E_CLSID}\shell\opennewwindow\command")).unwrap();
        assert_eq!(k.get_string("").unwrap(), "\"C:\\x\\leaf.exe\"");
        // Windows' own archive handlers stay Explorer's
        assert!(!ARCHIVE_VERBS.iter().any(|v| ours(&root, v)));
        assert!(app_registered(r"C:\x\leaf.exe", &root));
        let fa = CURRENT_USER.open(format!(r"{}\{APP_NAME}\Capabilities\FileAssociations", scratch.0)).unwrap();
        assert_eq!(fa.get_string(".zip").unwrap(), PROGID);
        // an older registration: its archive verb overrides go
        let base = CURRENT_USER.create(&root).unwrap();
        set_command(&base, &format!(r"{}\command", ARCHIVE_VERBS[1]), r#""C:\x\leaf.exe" "%1""#).unwrap();
        assert!(add_archives(r"C:\x\leaf.exe", &root));
        assert!(!ours(&root, ARCHIVE_VERBS[1]));
        assert!(!add_archives(r"C:\x\leaf.exe", &root)); // nothing left to do
        // the open verb lost while the explore verb is ours: written again whole (issue 97)
        let _ = base.remove_tree(r"Folder\shell\open\command");
        assert!(!is_enabled(&root));
        let fake = fake_app_exe("enable");
        assert!(heal(&fake.display().to_string(), &root));
        assert!(is_enabled(&root));
        disable(&root).unwrap();
        assert!(!is_enabled(&root));
        assert!(!ARCHIVE_VERBS.iter().any(|v| ours(&root, v)));
        assert!(!app_registered(r"C:\x\leaf.exe", &root));
        assert!(CURRENT_USER.open(format!(r"{root}\{PROGID}")).is_err());
        // another app's archive verb survives
        set_command(&base, &format!(r"{}\command", ARCHIVE_VERBS[0]), r"C:\7z\7zFM.exe %1").unwrap();
        disable(&root).unwrap();
        assert!(CURRENT_USER.open(format!(r"{root}\{}\command", ARCHIVE_VERBS[0])).is_ok());
        let _ = std::fs::remove_dir_all(fake.parent().unwrap());
        assert!(should_forward_to_explorer("::{26EE0668-A00A-44D7-9371-BEB064C98683}"));
        assert!(!should_forward_to_explorer(r"C:\Windows"));
    }

    /// A copy of the test binary named like the app (`leaf.exe`) in a folder of its own, for the "is it ours" check of `heal`.
    fn fake_app_exe(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("leaf-fm-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(crate::exe_name(crate::APP_EXE_STEM));
        std::fs::copy(std::env::current_exe().unwrap(), &fake).unwrap();
        fake
    }

    #[test]
    fn heals_a_missing_exe() {
        let scratch = Scratch::new("heal");
        let root = scratch.classes();
        enable(r"C:\yok\leaf.exe", &root).unwrap();
        assert_eq!(registered_exe(&root), Some(std::path::PathBuf::from(r"C:\yok\leaf.exe")));
        let fake = fake_app_exe("heal");
        assert!(heal(&fake.display().to_string(), &root));
        assert_eq!(registered_exe(&root), Some(fake.clone()));
        assert!(!heal(&fake.display().to_string(), &root)); // already fine
        let _ = std::fs::remove_dir_all(fake.parent().unwrap());
    }

    #[test]
    fn forward_loop_guard() {
        let log = std::env::temp_dir().join(format!("rf-fwd-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&log);
        assert!(!forwarded_within(&log, r"\\nas", 1_000, 5_000));
        note_in(&log, r"\\nas", 1_000);
        assert!(forwarded_within(&log, r"\\nas", 4_000, 5_000));
        assert!(!forwarded_within(&log, r"\\nas", 9_000, 5_000)); // long ago: forward again
        assert!(!forwarded_within(&log, r"\\other", 2_000, 5_000));
        for i in 0..20 {
            note_in(&log, &format!("x{i}"), 2_000 + i);
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 8); // bounded
        let _ = std::fs::remove_file(&log);
    }
}
