//! Windows: job objects with kill-on-close.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::sync::OnceLock;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::core::PCWSTR;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// A job object whose processes all end when the last handle to it closes (leaf ending closes it too).
pub struct Job(HANDLE);

// a job handle may be used from any thread
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    fn new() -> io::Result<Job> {
        unsafe {
            let h = CreateJobObjectW(None, PCWSTR::null()).map_err(|e| io::Error::other(e.to_string()))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if let Err(e) = set {
                let _ = CloseHandle(h);
                return Err(io::Error::other(e.to_string()));
            }
            Ok(Job(h))
        }
    }

    fn kill(&self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The one job all unbounded helpers share.
fn global() -> Option<&'static Job> {
    static G: OnceLock<Option<Job>> = OnceLock::new();
    G.get_or_init(|| Job::new().ok()).as_ref()
}

/// Starts `cmd` suspended, puts it in a job and lets it run. `own_job`: a job of its own for this one helper (so its tree can be ended
/// alone), returned to the caller; otherwise the shared job.
pub fn spawn_helper(cmd: &mut Command, extra_flags: u32, own_job: Option<()>) -> io::Result<(Child, Option<Job>)> {
    let job = if own_job.is_some() { Job::new().ok() } else { None };
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED | extra_flags);
    let child = cmd.spawn()?;
    let h = HANDLE(child.as_raw_handle());
    let target = job.as_ref().or_else(|| global());
    if let Some(j) = target {
        if unsafe { AssignProcessToJobObject(j.0, h) }.is_err() {
            // (a launcher's job that forbids it: the helper still runs, just not tied to leaf)
            eprintln!("leaf-proc: a helper could not be put in a job");
        }
    }
    unsafe {
        NtResumeProcess(h);
    }
    Ok((child, job))
}

pub fn kill_tree(child: &mut Child, job: Option<&Job>) {
    if let Some(j) = job {
        j.kill();
    }
    let _ = child.kill();
}

pub fn spawn_user(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn()
}
