//! Linux: own process group, parent-death signal.
//!
//! The parent-death signal is tied to the *thread* that started the child: when that thread ends the child gets the signal. Helpers are
//! therefore started from one thread that lives as long as leaf.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

/// Only the group id is needed to end a helper's tree; the type exists to mirror the Windows side.
pub struct Job;

struct Req {
    cmd: *mut Command,
    reply: Sender<io::Result<Child>>,
}
// the caller blocks until the reply, so the command outlives its use on the other thread
unsafe impl Send for Req {}

fn spawner() -> &'static Mutex<Sender<Req>> {
    static S: OnceLock<Mutex<Sender<Req>>> = OnceLock::new();
    S.get_or_init(|| {
        let (tx, rx) = channel::<Req>();
        std::thread::Builder::new()
            .name("leaf-proc-spawner".into())
            .spawn(move || {
                for r in rx {
                    // SAFETY: see `Req`
                    let cmd = unsafe { &mut *r.cmd };
                    let _ = r.reply.send(cmd.spawn());
                }
            })
            .expect("leaf-proc spawner thread");
        Mutex::new(tx)
    })
}

pub fn spawn_helper(cmd: &mut Command, _extra_flags: u32, _own_job: Option<()>) -> io::Result<(Child, Option<Job>)> {
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            // SIGTERM when the starting thread (the spawner, alive as long as leaf) ends; if leaf is already gone, end now
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0, 0, 0);
            if libc::getppid() == 1 {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    let (reply, got) = std::sync::mpsc::channel();
    spawner().lock().unwrap_or_else(|e| e.into_inner()).send(Req { cmd: cmd as *mut Command, reply }).map_err(|_| io::Error::other("spawner gone"))?;
    let child = got.recv().map_err(|_| io::Error::other("spawner gone"))??;
    Ok((child, Some(Job)))
}

pub fn kill_tree(child: &mut Child, _job: Option<&Job>) {
    // the helper leads its own group: the signal reaches everything it started that stayed in it
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
}

pub fn spawn_user(cmd: &mut Command) -> io::Result<Child> {
    cmd.process_group(0).spawn()
}
