//! Linux backend of rf_win::taskbar_progress: progress on the app's launcher entry, the Unity `LauncherEntry` signal that Plasma's
//! task manager, the GNOME Dash to Dock / Ubuntu dock and others show as a bar on the icon. The signal is sent from a worker thread
//! (the first send connects to the session bus) and only when the state changed. Without a session bus it stays quiet; a dock that
//! does not listen ignores it.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use zbus::zvariant::Value;

// TYPE-COPY(rf-win src/taskbar_progress.rs:8)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    None,
    Busy,
    Normal(f32),
    Paused(f32),
    Error(f32),
}

/// (visible, progress 0..1, urgent) of a state. `Busy` has no bar to fill: the bar is shown empty.
fn props_of(state: State) -> (bool, f64, bool) {
    match state {
        State::None => (false, 0.0, false),
        State::Busy => (true, 0.0, false),
        State::Normal(v) | State::Paused(v) => (true, f64::from(v.clamp(0.0, 1.0)), false),
        State::Error(v) => (true, f64::from(v.clamp(0.0, 1.0)), true),
    }
}

/// Shows `state` on the launcher entry of the app. `hwnd` (a Windows window handle) means nothing here.
pub fn set(_hwnd: isize, state: State) {
    static WORKER: OnceLock<Sender<State>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<State>();
        let _ = std::thread::Builder::new().name("leaf-progress".into()).spawn(move || {
            let mut last: Option<State> = None;
            while let Ok(mut s) = rx.recv() {
                // only the newest state matters
                while let Ok(n) = rx.try_recv() {
                    s = n;
                }
                if last == Some(s) {
                    continue;
                }
                last = Some(s);
                let Some(bus) = crate::bus::session() else { continue };
                let (visible, progress, urgent) = props_of(s);
                let mut props: HashMap<&str, Value> = HashMap::new();
                props.insert("progress-visible", Value::from(visible));
                props.insert("progress", Value::from(progress));
                props.insert("urgent", Value::from(urgent));
                let uri = format!("application://{}.desktop", crate::APP_EXE_STEM);
                let _ = bus.emit_signal(None::<&str>, "/com/canonical/Unity/LauncherEntry", "com.canonical.Unity.LauncherEntry", "Update", &(uri, props));
            }
        });
        tx
    });
    let _ = tx.send(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_becomes_the_launcher_entrys_properties() {
        assert_eq!(props_of(State::None), (false, 0.0, false));
        assert_eq!(props_of(State::Normal(0.5)), (true, 0.5, false));
        assert_eq!(props_of(State::Error(2.0)), (true, 1.0, true));
    }
}
