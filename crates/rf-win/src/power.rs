//! Sleep and shut down this PC, for "when the downloads are done". Windows' own programs do it: `shutdown.exe` and powrprof's
//! `SetSuspendState` (through rundll32; a PC with hibernation switched on may hibernate instead of sleeping).

use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Sleep,
    Shutdown,
}

/// Whether this system has a call for it.
pub fn available(_: Action) -> bool {
    true
}

pub fn run(action: Action) -> Result<(), String> {
    let mut c = match action {
        Action::Sleep => {
            let mut c = Command::new("rundll32.exe");
            c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
            c
        }
        Action::Shutdown => {
            let mut c = Command::new("shutdown.exe");
            c.args(["/s", "/t", "0"]);
            c
        }
    };
    // (a helper: the request is made within a moment; a minute at most, and it ends with leaf)
    leaf_proc::output_helper(c.creation_flags(CREATE_NO_WINDOW), Some(std::time::Duration::from_secs(60))).map(|_| ()).map_err(|e| e.to_string())
}
