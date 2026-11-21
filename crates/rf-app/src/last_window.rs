//! What happens when the last main window is gone: the app ends, or stays in the notification area. Also looked at again when a
//! second launch's request (see `leaf_sys::handover`) held back a quit and then left no window open (GitHub issue 15).

use gpui_kit::App;

/// What to do once no main window is open any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// A main window is open: nothing to do.
    Nothing,
    /// The app stays in the notification area: the settings are saved, the process lives on.
    StayInBackground,
    /// The app ends (once running copies and moves are done).
    Quit,
}

/// The decision, from the facts only.
pub(crate) fn next(main_window_open: bool, run_in_background: bool, quitting: bool) -> Next {
    if main_window_open {
        Next::Nothing
    } else if run_in_background && !quitting {
        Next::StayInBackground
    } else {
        Next::Quit
    }
}

/// Run when a window closes, and again when a request from a second launch has been acted on while a quit was held back. Windows
/// left behind (menus, flyouts) go with the last main window, so the process cannot linger with nothing on the screen.
pub(crate) fn settle(cx: &mut App) {
    let main = crate::workspace::any_main_window(cx);
    let background = cx.global::<crate::settings::Settings>().run_in_background;
    let what = next(main, background, crate::background::quitting());
    if what == Next::Nothing {
        return;
    }
    // menus / flyouts left behind go too
    for w in cx.windows() {
        let _ = w.update(cx, |_, window, _| window.remove_window());
    }
    match what {
        Next::StayInBackground => {
            let s = cx.global::<crate::settings::Settings>().clone();
            std::thread::spawn(move || s.save());
        }
        _ => crate::quit_when_idle(cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_window_keeps_the_app_whatever_else_is_set() {
        for background in [false, true] {
            for quitting in [false, true] {
                assert_eq!(next(true, background, quitting), Next::Nothing);
            }
        }
    }

    #[test]
    fn no_window_ends_the_app_unless_it_is_to_stay_in_the_notification_area() {
        assert_eq!(next(false, false, false), Next::Quit);
        assert_eq!(next(false, true, false), Next::StayInBackground);
        // the tray's own "quit" is on its way: the app ends
        assert_eq!(next(false, true, true), Next::Quit);
    }
}
