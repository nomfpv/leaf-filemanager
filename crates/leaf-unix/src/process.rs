//! Linux backend of the leaf-sys module `process` (Windows side: rf-win process.rs). Status: PARTIAL (`front_is_ours` is a stub)

use std::time::{Duration, Instant};

use leaf_common::stub;

/// Windows' `CommandExt::creation_flags`, so the same call compiles here: there are no console windows to keep away, so it does
/// nothing.
pub trait CommandExt {
    fn creation_flags(&mut self, flags: u32) -> &mut std::process::Command;
}

impl CommandExt for std::process::Command {
    fn creation_flags(&mut self, _flags: u32) -> &mut std::process::Command {
        self
    }
}

/// Waits until the process `pid` has ended, at most `timeout_ms`; returns at once when there is no such process. (It is not our
/// child, so it cannot be waited for: `/proc/<pid>` is looked at every 20 ms.)
pub fn wait_pid(pid: u32, timeout_ms: u32) {
    let dir = format!("/proc/{pid}");
    let until = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    while is_running(&dir) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The process of `/proc/<pid>` exists and is not a zombie (an ended child its parent has not reaped yet).
fn is_running(dir: &str) -> bool {
    match std::fs::read_to_string(format!("{dir}/stat")) {
        Ok(stat) => stat.rfind(')').and_then(|i| stat[i + 1..].split_whitespace().next()).is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
    }
}

/// Whether the window in front belongs to this process. Wayland does not tell a client; until GPUI's activation events are used
/// for it, the answer is "yes", so that nothing is closed for a window it cannot see.
pub fn front_is_ours() -> bool {
    // STUB(linux): `false` (the default) would put every open menu away at once
    stub::hit("process::front_is_ours");
    true
}

/// (idle, kernel + user) CPU time of the whole machine, in 100 ns units (the first line of `/proc/stat`; idle counts I/O wait,
/// and the total counts idle, as Windows' kernel time does).
pub fn cpu_times() -> Option<(u64, u64)> {
    cpu_times_from(&std::fs::read_to_string("/proc/stat").ok()?, crate::clock_ticks())
}

fn cpu_times_from(stat: &str, ticks: u64) -> Option<(u64, u64)> {
    let line = stat.lines().next()?.strip_prefix("cpu ")?;
    let f: Vec<u64> = line.split_whitespace().map(|v| v.parse().unwrap_or(0)).collect();
    // user nice system idle iowait irq softirq steal (guest and guest_nice are already in user and nice)
    let field = |i: usize| f.get(i).copied().unwrap_or(0);
    let idle = field(3) + field(4);
    let total: u64 = (0..8).map(field).sum();
    let to_100ns = |t: u64| t * 10_000_000 / ticks.max(1);
    Some((to_100ns(idle), to_100ns(total)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_machines_cpu_times_from_proc_stat() {
        let stat = "cpu  100 5 50 800 20 1 2 3 7 0\ncpu0 1 2 3 4\n";
        assert_eq!(super::cpu_times_from(stat, 100), Some((820 * 100_000, 981 * 100_000)));
        assert_eq!(super::cpu_times_from("garbage", 100), None);
        let (idle, total) = super::cpu_times().expect("/proc/stat");
        assert!(idle <= total);
    }

    #[test]
    fn waiting_for_an_ended_or_missing_process_returns_at_once_and_a_running_one_is_waited_for() {
        let t = std::time::Instant::now();
        // (pid 0 never has a /proc entry)
        super::wait_pid(0, 5_000);
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        let mut child = std::process::Command::new("sleep").arg("0.3").spawn().expect("sleep");
        let t = std::time::Instant::now();
        let pid = child.id();
        let reaper = std::thread::spawn(move || child.wait());
        super::wait_pid(pid, 5_000);
        assert!(t.elapsed() >= std::time::Duration::from_millis(250), "{:?}", t.elapsed());
        let _ = reaper.join();
        let t = std::time::Instant::now();
        super::wait_pid(pid, 100);
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }
}
