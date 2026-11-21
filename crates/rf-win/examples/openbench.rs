//! Folder-open timing with leaf's own method, for any app.
//! `openbench <rf|other|explorer> <exe> <folder> <runs>`
//!
//! leaf runs off-screen by default (LEAF_OFFSCREEN=1; OPENBENCH_ON_SCREEN=1 for the on-screen run).
//!
//! The launched window would take the keyboard focus from whatever the owner is doing. A watcher
//! thread (off with OPENBENCH_NO_FOCUS_GUARD=1) gives the focus back to the window that had it (only when the window that took it
//! belongs to the benchmarked exe), and the tool prints whether the focus stayed.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use rf_win::bench::{App, AppKind, detect, measure_open};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow};
use windows::core::PWSTR;

fn fg() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

fn exe_name_of(h: isize) -> Option<String> {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(HWND(h as *mut _), Some(&mut pid));
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(p);
        ok.then(|| {
            let s = String::from_utf16_lossy(&buf[..n as usize]);
            std::path::Path::new(&s).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
        })
    }
}

fn give_back(prev: isize) {
    unsafe {
        let (h, cur) = (HWND(prev as *mut _), GetForegroundWindow());
        if !IsWindow(Some(h)).as_bool() {
            return;
        }
        // attach to the thread that owns the foreground, or Windows refuses the switch
        let (me, other) = (GetCurrentThreadId(), GetWindowThreadProcessId(cur, None));
        let attached = other != 0 && other != me && AttachThreadInput(me, other, true).as_bool();
        let _ = SetForegroundWindow(h);
        let _ = BringWindowToTop(h);
        if attached {
            let _ = AttachThreadInput(me, other, false);
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (kind, exe, folder, runs) = (&a[1], &a[2], PathBuf::from(&a[3]), a[4].parse::<usize>().unwrap());
    let app = match kind.as_str() {
        "rf" => {
            let cfg = std::env::temp_dir().join("leaf-openbench-cfg");
            let _ = std::fs::create_dir_all(&cfg);
            let mut env = vec![
                ("LEAF_MULTI_INSTANCE".into(), "1".into()),
                ("LEAF_CONFIG_DIR".into(), cfg.to_string_lossy().into_owned()),
                ("LEAF_MUTE".into(), "1".into()),
            ];
            // off-screen by default: the window is parked outside every monitor (OPENBENCH_ON_SCREEN=1
            // measures it on screen, as the official per-phase run does)
            if std::env::var_os("OPENBENCH_ON_SCREEN").is_none() {
                env.push(("LEAF_OFFSCREEN".into(), "1".into()));
            }
            App { kind: AppKind::Leaf, exe: PathBuf::from(exe), env }
        }
        "explorer" => detect().into_iter().find(|x| x.kind == AppKind::Explorer).unwrap(),
        _ => App { kind: AppKind::FilePilot, exe: PathBuf::from(exe), env: Vec::new() },
    };
    let cancel = AtomicBool::new(false);
    let quiet = std::env::var_os("OPENBENCH_NO_FOCUS_GUARD").is_some();
    // the window that has the focus now, and how often the watcher gave it back
    let prev_h = std::sync::atomic::AtomicIsize::new(0);
    let restores = AtomicU32::new(0);
    let stop = AtomicBool::new(false);
    let target = std::path::Path::new(exe).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    std::thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                let (p, now) = (prev_h.load(Ordering::Relaxed), fg());
                if p != 0 && now != 0 && now != p && exe_name_of(now).as_deref() == Some(target.as_str()) && !quiet {
                    give_back(p);
                    restores.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        for i in 1..=runs {
            let before = fg();
            prev_h.store(before, Ordering::Relaxed);
            let r = measure_open(&app, &folder, Duration::from_secs(60), &cancel);
            std::thread::sleep(Duration::from_millis(300));
            let after = fg();
            prev_h.store(0, Ordering::Relaxed);
            let focus = if before == after { "focus stayed".to_string() } else { format!("FOCUS MOVED {before:#x} -> {after:#x} ({:?})", exe_name_of(after)) };
            match r {
                Ok(r) => println!(
                    "{kind} run{i}: settled {:.0} ms | window {:.0} ms | cpu {:.0} ms | private {:.0} MB | shared {} | {focus} (gave back {}x)",
                    r.settled_ms,
                    r.window_ms,
                    r.cpu_ms,
                    r.private as f64 / 1048576.0,
                    r.shared,
                    restores.swap(0, Ordering::Relaxed)
                ),
                Err(e) => println!("{kind} run{i}: ERROR {e:#}"),
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        stop.store(true, Ordering::Relaxed);
    });
}
