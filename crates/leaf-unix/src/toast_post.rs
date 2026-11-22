//! Linux backend of rf_win::toast_post: desktop notifications through `org.freedesktop.Notifications` on the session bus (what
//! `notify-send` calls), and `notify-send` itself when the bus call fails and the tool is installed. Without either, [`show`]
//! answers `Err` (the caller then asks in a window of its own, as it does when Windows has toasts off).
//!
//! The buttons and the click on the toast come back through the `ActionInvoked` signal while this process runs. A listener thread per
//! toast ends when the server says the toast is closed.

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::zvariant::Value;

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

// TYPE-COPY(rf-win src/toast_post.rs:25)
pub struct Button {
    pub label: String,
    /// What `on_activate` gets when it is pressed.
    pub argument: String,
}

// TYPE-COPY(rf-win src/toast_post.rs:32)
pub struct Toast<'a> {
    /// Names it: a later toast with the same tag replaces it, [`remove`] takes it away.
    pub tag: &'a str,
    pub title: &'a str,
    pub lines: &'a [&'a str],
    /// What `on_activate` gets for a click on the toast itself.
    pub launch: &'a str,
    pub buttons: &'a [Button],
    /// Stays on screen until it is answered (a question), instead of going to the notification list after a few seconds.
    pub stays: bool,
}

/// The server's id of each toast shown, by tag.
static LIVE: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// Shows a toast. `on_activate` (called on a thread of ours) gets the argument of the button pressed, or `launch`.
pub fn show(_app_id: &str, toast: &Toast, on_activate: impl Fn(String) + Send + Sync + 'static) -> anyhow::Result<()> {
    match via_bus(toast, on_activate) {
        Ok(()) => Ok(()),
        Err(e) => {
            // no daemon on the bus (or no bus): the command line tool may still reach one
            if notify_send(toast) {
                return Ok(());
            }
            Err(e)
        }
    }
}

fn body_of(t: &Toast) -> String {
    t.lines.iter().take(2).copied().collect::<Vec<_>>().join("\n")
}

fn via_bus(toast: &Toast, on_activate: impl Fn(String) + Send + Sync + 'static) -> anyhow::Result<()> {
    let bus = crate::bus::session().ok_or_else(|| anyhow::anyhow!("no session bus"))?;
    // what a press gives back: the keys of the actions are ours
    let mut actions: Vec<String> = Vec::new();
    let mut answers: HashMap<String, String> = HashMap::new();
    if !toast.launch.is_empty() {
        actions.extend(["default".to_string(), String::new()]);
        answers.insert("default".into(), toast.launch.to_string());
    }
    for (i, b) in toast.buttons.iter().take(5).enumerate() {
        let key = format!("b{i}");
        actions.extend([key.clone(), b.label.clone()]);
        answers.insert(key, b.argument.clone());
    }
    // listen before asking, so that no answer is missed
    let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(DEST)?.path(PATH)?.build();
    let signals = zbus::blocking::MessageIterator::for_match_rule(rule, bus, Some(64))?;
    let replaces = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(toast.tag).copied()).unwrap_or(0);
    let mut hints: HashMap<&str, Value> = HashMap::new();
    hints.insert("desktop-entry", Value::from(crate::APP_EXE_STEM));
    if toast.stays {
        hints.insert("resident", Value::from(true));
    }
    let reply = bus.call_method(
        Some(DEST),
        PATH,
        Some(DEST),
        "Notify",
        &(crate::APP_EXE_STEM, replaces, crate::APP_EXE_STEM, toast.title, body_of(toast), actions, hints, if toast.stays { 0i32 } else { -1i32 }),
    )?;
    let id: u32 = reply.body().deserialize()?;
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(toast.tag.to_string(), id);
    if answers.is_empty() {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("leaf-toast".into())
        .spawn(move || {
            for m in signals {
                let Ok(m) = m else { break };
                let header = m.header();
                match header.member().map(|m| m.as_str()) {
                    Some("ActionInvoked") => {
                        if let Ok((nid, key)) = m.body().deserialize::<(u32, String)>()
                            && nid == id
                            && let Some(a) = answers.get(&key)
                        {
                            on_activate(a.clone());
                        }
                    }
                    Some("NotificationClosed") => {
                        if m.body().deserialize::<(u32, u32)>().is_ok_and(|(nid, _)| nid == id) {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        })?;
    Ok(())
}

/// The title and lines through `notify-send`, when it is installed (no buttons: it cannot answer).
fn notify_send(t: &Toast) -> bool {
    let Some(exe) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("notify-send")).find(|p| p.is_file())) else {
        return false;
    };
    leaf_proc::output_helper(std::process::Command::new(exe).args(["--app-name", crate::APP_EXE_STEM, "--", t.title, &body_of(t)]), Some(std::time::Duration::from_secs(10)))
        .is_ok_and(|o| o.status.success())
}

/// Takes a toast away (answered elsewhere, or no longer asked).
pub fn remove(_app_id: &str, tag: &str) {
    let id = LIVE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(tag));
    if let (Some(id), Some(bus)) = (id, crate::bus::session()) {
        let _ = bus.call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(id,));
    }
}
