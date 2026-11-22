//! The tests start this test program again in a role (set through the environment) and watch what happens to the processes it starts:
//! a plain sleeper, nothing that opens a window.

use super::*;
use std::path::{Path, PathBuf};

fn role() -> String {
    std::env::var("LEAF_PROC_ROLE").unwrap_or_default()
}

fn me(test: &str, role: &str, pidfile: &Path) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", test, "--nocapture", "--test-threads=1"]).env("LEAF_PROC_ROLE", role).env("LEAF_PROC_PIDFILE", pidfile);
    c
}

fn pidfile(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("leaf-proc-test-{}-{name}.pid", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn write_pid(pid: u32) {
    let f = std::env::var("LEAF_PROC_PIDFILE").unwrap();
    std::fs::write(format!("{f}.tmp"), pid.to_string()).unwrap();
    std::fs::rename(format!("{f}.tmp"), f).unwrap();
}

fn wait_pid(f: &Path) -> u32 {
    for _ in 0..600 {
        if let Ok(s) = std::fs::read_to_string(f) {
            return s.trim().parse().unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no pid file");
}

#[cfg(windows)]
mod os {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess};
    pub fn alive(pid: u32) -> bool {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code).is_ok();
            let _ = CloseHandle(h);
            ok && code == 259
        }
    }
    pub fn kill(pid: u32) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 1);
                let _ = CloseHandle(h);
            }
        }
    }
}
#[cfg(unix)]
mod os {
    pub fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    pub fn kill(pid: u32) {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

fn gone_within(pid: u32, secs: u64) -> bool {
    for _ in 0..secs * 20 {
        if !os::alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Role target: sleeps for a minute.
#[test]
fn role_sleeper() {
    if role() == "sleeper" {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Role target: starts a sleeper the way `how` says, reports its id, then waits.
#[test]
fn role_parent() {
    let r = role();
    let Some(how) = r.strip_prefix("parent-") else { return };
    let f = PathBuf::from(std::env::var("LEAF_PROC_PIDFILE").unwrap());
    let sleeper_file = f.with_extension("sleeper");
    let mut c = me("tests::role_sleeper", "sleeper", &sleeper_file);
    c.stdout(Stdio::null()).stderr(Stdio::null());
    let child = match how {
        "helper" => spawn_helper(&mut c).unwrap(),
        "plain" => c.spawn().unwrap(),
        _ => spawn_user(&mut c).unwrap(),
    };
    write_pid(child.id());
    if how == "user" {
        return; // leaf ends, the user's program stays
    }
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn helper_started_by_a_dead_parent_ends() {
    let f = pidfile("orphan");
    let mut parent = me("tests::role_parent", "parent-helper", &f).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let sleeper = wait_pid(&f);
    assert!(os::alive(sleeper));
    parent.kill().unwrap();
    let _ = parent.wait();
    let ended = gone_within(sleeper, 5);
    if !ended {
        os::kill(sleeper);
    }
    assert!(ended, "the helper outlived its parent");
}

#[test]
fn helper_past_its_limit_leaves_no_child() {
    let f = pidfile("limit");
    // the helper itself starts a plain child of its own (the way git starts its programs) and then waits
    let mut c = me("tests::role_parent", "parent-plain", &f);
    let t = Instant::now();
    let r = output_helper(&mut c, Some(Duration::from_secs(2)));
    assert!(r.is_err_and(|e| e.kind() == io::ErrorKind::TimedOut));
    assert!(t.elapsed() < Duration::from_secs(10));
    let grandchild = wait_pid(&f);
    let ended = gone_within(grandchild, 5);
    if !ended {
        os::kill(grandchild);
    }
    assert!(ended, "the helper's own child was left behind");
}

#[test]
fn program_opened_for_the_user_survives_the_parent() {
    let f = pidfile("user");
    let mut parent = me("tests::role_parent", "parent-user", &f).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let opened = wait_pid(&f);
    let _ = parent.wait();
    std::thread::sleep(Duration::from_secs(2));
    let survived = os::alive(opened);
    os::kill(opened);
    assert!(survived, "the user's program ended with its parent");
}
