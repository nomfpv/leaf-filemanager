//! Linux backend of rf_win::hotkey: one global shortcut through the desktop's `org.freedesktop.portal.GlobalShortcuts` portal (KDE Plasma
//! has it, GNOME 48 and later too; it works under Wayland and X11 alike). leaf asks for the shortcut with `BindShortcuts`; the desktop may
//! show its own confirmation or let the user pick another key, and sends `Activated` when it is pressed. Without the portal the answer is
//! `Unavailable`. An X11 key grab as a fallback is not written (no X11 connection of our own is kept here).
//!
//! All of it happens on one thread (blocking zbus), so a slow desktop dialog never holds the app: `set` waits a few seconds for the
//! answer and then says "accepted"; the shortcut works from the moment the desktop binds it.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

// TYPE-COPY(rf-win src/hotkey.rs:25)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    Invalid,
    Taken,
    Unavailable,
}

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const SHORTCUT_ID: &str = "quick-search";
/// How long `set` waits for the portal before it says "accepted" (a desktop dialog may be open).
const WAIT: Duration = Duration::from_secs(4);

type OnPress = Box<dyn Fn() + Send>;

fn handler() -> &'static Mutex<Option<OnPress>> {
    static H: OnceLock<Mutex<Option<OnPress>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Where a press goes (called on the portal thread; the newest caller wins).
pub fn set_handler(f: impl Fn() + Send + 'static) {
    *handler().lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

/// "alt-space" (GPUI's syntax, as the shortcut editor records it) as the portal's trigger text, `ALT+space`: modifiers `CTRL`, `ALT`,
/// `SHIFT`, `LOGO`, then the key's XKB name. A modifier is required (a bare key would be taken from every app).
fn trigger(keys: &str) -> Option<String> {
    let keys = keys.trim().to_ascii_lowercase();
    let (mods_part, key) = match keys.strip_suffix('-') {
        // (the key is the dash itself: "ctrl--")
        Some(rest) if !rest.is_empty() => (rest.trim_end_matches('-'), "minus"),
        _ => match keys.rsplit_once('-') {
            Some((m, k)) => (m, k),
            None => ("", keys.as_str()),
        },
    };
    let mut out: Vec<&str> = Vec::new();
    for m in mods_part.split('-').filter(|m| !m.is_empty()) {
        let name = match m {
            "ctrl" | "control" => "CTRL",
            "alt" => "ALT",
            "shift" => "SHIFT",
            "win" | "super" | "cmd" | "meta" | "logo" => "LOGO",
            _ => return None,
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        return None;
    }
    let key = match key {
        "space" => "space".to_string(),
        "enter" | "return" => "Return".into(),
        "tab" => "Tab".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "Page_Up".into(),
        "pagedown" => "Page_Down".into(),
        "insert" => "Insert".into(),
        "delete" => "Delete".into(),
        "minus" => "minus".into(),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k.to_string(),
        k if k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n)) => k.to_ascii_uppercase(),
        _ => return None,
    };
    Some(format!("{}+{key}", out.join("+")))
}

enum Cmd {
    /// The trigger to hold (None: none) and where to answer.
    Set(Option<String>, Sender<Result<(), HotkeyError>>),
}

fn worker() -> &'static Sender<Cmd> {
    static W: OnceLock<Sender<Cmd>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = channel::<Cmd>();
        let _ = std::thread::Builder::new().name("leaf-hotkey".into()).spawn(move || {
            let mut state = State { conn: None, session: None, listening: false };
            while let Ok(Cmd::Set(trigger, reply)) = rx.recv() {
                let _ = reply.send(state.set(trigger));
            }
        });
        tx
    })
}

struct State {
    conn: Option<Connection>,
    session: Option<OwnedObjectPath>,
    listening: bool,
}

fn token(what: &str) -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("leaf_{what}_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

impl State {
    fn connection(&mut self) -> Result<&Connection, HotkeyError> {
        if self.conn.is_none() {
            self.conn = Some(Connection::session().map_err(|e| {
                log::info!("hotkey: no session bus: {e}");
                HotkeyError::Unavailable
            })?);
        }
        Ok(self.conn.as_ref().expect("connected"))
    }

    fn set(&mut self, trigger: Option<String>) -> Result<(), HotkeyError> {
        if let Some(old) = self.session.take()
            && let Ok(c) = self.connection()
        {
            // (the old session is closed: its shortcut goes with it)
            if let Ok(p) = Proxy::new(c, PORTAL, old.as_ref(), "org.freedesktop.portal.Session") {
                let _ = p.call_method("Close", &());
            }
        }
        let Some(trigger) = trigger else { return Ok(()) };
        let conn = self.connection()?.clone();
        let session = create_session(&conn)?;
        bind(&conn, &session, &trigger)?;
        self.session = Some(session);
        if !self.listening {
            self.listening = true;
            listen(conn);
        }
        Ok(())
    }
}

/// Calls a portal method that answers through a `Request` object: the `Response` signal is subscribed to before the call (the
/// request's path is known from our token), then its results come back. `Err(code)` when the portal answers with another code than 0.
fn request(
    conn: &Connection,
    method: &str,
    handle_token: &str,
    call: impl FnOnce() -> zbus::Result<OwnedObjectPath>,
) -> Result<HashMap<String, OwnedValue>, Result<u32, HotkeyError>> {
    let sender = conn.unique_name().map(|n| n.as_str().trim_start_matches(':').replace('.', "_")).ok_or(Err(HotkeyError::Unavailable))?;
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}");
    let req = Proxy::new(conn, PORTAL, path, "org.freedesktop.portal.Request").map_err(|_| Err(HotkeyError::Unavailable))?;
    let mut responses = req.receive_signal("Response").map_err(|_| Err(HotkeyError::Unavailable))?;
    call().map_err(|e| {
        log::info!("hotkey: {method}: {e}");
        Err(HotkeyError::Unavailable)
    })?;
    let msg = responses.next().ok_or(Err(HotkeyError::Unavailable))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|_| Err(HotkeyError::Unavailable))?;
    if code == 0 { Ok(results) } else { Err(Ok(code)) }
}

fn portal(conn: &Connection) -> Result<Proxy<'static>, HotkeyError> {
    let p = Proxy::new(conn, PORTAL, PATH, IFACE).map_err(|_| HotkeyError::Unavailable)?;
    // (no such interface on this desktop: the version property is not there)
    p.get_property::<u32>("version").map_err(|e| {
        log::info!("hotkey: no GlobalShortcuts portal: {e}");
        HotkeyError::Unavailable
    })?;
    Ok(p)
}

fn create_session(conn: &Connection) -> Result<OwnedObjectPath, HotkeyError> {
    let p = portal(conn)?;
    let (ht, st) = (token("req"), token("session"));
    let results = request(conn, "CreateSession", &ht, || {
        let mut o: HashMap<&str, Value> = HashMap::new();
        o.insert("handle_token", Value::from(ht.as_str()));
        o.insert("session_handle_token", Value::from(st.as_str()));
        p.call("CreateSession", &(o,))
    })
    .map_err(|_| HotkeyError::Unavailable)?;
    let handle: String = results.get("session_handle").and_then(|v| v.try_clone().ok()).and_then(|v| String::try_from(v).ok()).ok_or(HotkeyError::Unavailable)?;
    ObjectPath::try_from(handle).map(Into::into).map_err(|_| HotkeyError::Unavailable)
}

fn bind(conn: &Connection, session: &OwnedObjectPath, trigger: &str) -> Result<(), HotkeyError> {
    let p = portal(conn)?;
    let ht = token("bind");
    let r = request(conn, "BindShortcuts", &ht, || {
        let mut props: HashMap<&str, Value> = HashMap::new();
        props.insert("description", Value::from("Open leaf's quick search"));
        props.insert("preferred_trigger", Value::from(trigger));
        let shortcuts = vec![(SHORTCUT_ID, props)];
        let mut o: HashMap<&str, Value> = HashMap::new();
        o.insert("handle_token", Value::from(ht.as_str()));
        p.call("BindShortcuts", &(session, shortcuts, "", o))
    });
    match r {
        Ok(_) => Ok(()),
        // the user closed the desktop's question, or the desktop refused the key: the shortcut is not held
        Err(Ok(code)) => {
            log::info!("hotkey: the portal answered {code} to BindShortcuts");
            Err(HotkeyError::Taken)
        }
        Err(Err(e)) => Err(e),
    }
}

/// Sends every `Activated` of our shortcut to the handler, on a thread of its own.
fn listen(conn: Connection) {
    let _ = std::thread::Builder::new().name("leaf-hotkey-press".into()).spawn(move || {
        let Ok(p) = Proxy::new(&conn, PORTAL, PATH, IFACE) else { return };
        let Ok(signals) = p.receive_signal("Activated") else { return };
        for msg in signals {
            let Ok((_session, id, _time, _opts)) = msg.body().deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>() else { continue };
            if id == SHORTCUT_ID
                && let Some(f) = handler().lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            {
                f();
            }
        }
    });
}

/// Makes `keys` (GPUI's syntax: "alt-space", "ctrl-shift-k") the global shortcut, or drops it (`None`). `Unavailable` where the desktop
/// has no GlobalShortcuts portal; `Taken` when the desktop did not bind the key.
pub fn set(keys: Option<&str>) -> Result<(), HotkeyError> {
    let want = match keys {
        None => None,
        Some(k) => Some(trigger(k).ok_or(HotkeyError::Invalid)?),
    };
    // nothing was ever asked and nothing is asked now: the bus is not touched
    if want.is_none() && !STARTED.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = channel();
    if worker().send(Cmd::Set(want, tx)).is_err() {
        return Err(HotkeyError::Unavailable);
    }
    // (a desktop's own question may be open: the shortcut then works from the moment it is answered)
    rx.recv_timeout(WAIT).unwrap_or(Ok(()))
}

static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::trigger;

    #[test]
    fn shortcuts_become_the_portals_trigger_text() {
        assert_eq!(trigger("alt-space").as_deref(), Some("ALT+space"));
        assert_eq!(trigger("ctrl-shift-k").as_deref(), Some("CTRL+SHIFT+k"));
        assert_eq!(trigger("super-f5").as_deref(), Some("LOGO+F5"));
        assert_eq!(trigger("ctrl--").as_deref(), Some("CTRL+minus"));
        assert_eq!(trigger("k"), None);
        assert_eq!(trigger("ctrl-nothing"), None);
    }
}
