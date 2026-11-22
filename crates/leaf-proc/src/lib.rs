//! Starting programs from leaf. Two kinds, kept apart on purpose:
//!
//! * **Helpers** (git, ffmpeg, yt-dlp, par2, rclone, mount and archive tools): work done for leaf. They must never outlive leaf and a helper
//!   past its time limit must not leave its own children behind. Start them with [`spawn_helper`] or [`output_helper`].
//!   Windows: every helper runs in a job object that ends all of its processes when leaf's last handle to it closes (leaf ends, however it
//!   ends) or when the limit passes. The helper starts suspended and is resumed after it joined the job, so it cannot start a child of its
//!   own outside the job. Linux: own process group (the limit kills the group) and a parent-death signal.
//! * **Programs opened for the user** ("Open", "Open with", terminal here, editors, Explorer): they must survive leaf. Start them with
//!   [`spawn_user`]; they are never put in a job and, on Linux, get a process group of their own and no parent-death signal.
//!
//! [`Gate`] is the small counting limiter used to keep the number of one kind of helper (git) at once down.

use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as sys;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

/// Starts a helper that ends with leaf (see the module doc). The window stays hidden on Windows.
pub fn spawn_helper(cmd: &mut Command) -> io::Result<Child> {
    spawn_helper_flags(cmd, 0)
}

/// [`spawn_helper`] with extra Windows process creation flags (a priority class, say); ignored on Linux.
pub fn spawn_helper_flags(cmd: &mut Command, extra_flags: u32) -> io::Result<Child> {
    sys::spawn_helper(cmd, extra_flags, None).map(|(c, _)| c)
}

/// `Command::output` for a helper: stdin closed, stdout and stderr read in full, and, with a `limit`, the whole tree the helper started is
/// killed when the limit passes (the call then fails with `TimedOut`).
pub fn output_helper(cmd: &mut Command, limit: Option<Duration>) -> io::Result<Output> {
    output_helper_flags(cmd, limit, 0)
}

/// [`output_helper`] with extra Windows process creation flags.
pub fn output_helper_flags(cmd: &mut Command, limit: Option<Duration>, extra_flags: u32) -> io::Result<Output> {
    let (mut child, tree) = sys::spawn_helper(cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()), extra_flags, Some(()))?;
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut b);
            }
            b
        })
    };
    let so = child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>);
    let se = child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>);
    let (t_out, t_err) = (drain(so), drain(se));
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if limit.is_some_and(|l| start.elapsed() > l) {
            sys::kill_tree(&mut child, tree.as_ref());
            let _ = child.wait();
            // the pipes close now that nothing holds them: the readers end
            let _ = (t_out.join(), t_err.join());
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the helper took too long"));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // the helper is done; anything it left running in its tree goes with it
    drop(tree);
    Ok(Output { status, stdout: t_out.join().unwrap_or_default(), stderr: t_err.join().unwrap_or_default() })
}

/// Starts a program the user asked for: it is not tied to leaf and keeps running after leaf ends.
pub fn spawn_user(cmd: &mut Command) -> io::Result<Child> {
    sys::spawn_user(cmd)
}

/// A counting limiter: at most `max` holders of a [`GateGuard`] at once, the rest wait their turn.
pub struct Gate {
    state: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

/// The most gits running at once in the whole app (status, log, config reads of every view); the rest wait.
pub static GIT: Gate = Gate::new(3);

pub struct GateGuard<'a>(&'a Gate);

impl Gate {
    pub const fn new(max: usize) -> Self {
        Self { state: Mutex::new(0), freed: Condvar::new(), max }
    }

    /// Waits for a free place and takes it until the guard drops.
    pub fn enter(&self) -> GateGuard<'_> {
        let mut n = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= self.max {
            n = self.freed.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n += 1;
        GateGuard(self)
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut n = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        *n -= 1;
        self.0.freed.notify_one();
    }
}

#[cfg(test)]
mod tests;
