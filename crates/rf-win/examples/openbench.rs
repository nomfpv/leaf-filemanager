//! Folder-open timing with leaf's own method, for any app.
//! `openbench <rf|other|explorer> <exe> <folder> <runs>`
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use rf_win::bench::{App, AppKind, detect, measure_open};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (kind, exe, folder, runs) = (&a[1], &a[2], PathBuf::from(&a[3]), a[4].parse::<usize>().unwrap());
    let app = match kind.as_str() {
        "rf" => {
            let cfg = std::env::temp_dir().join("rf-openbench-cfg");
            let _ = std::fs::create_dir_all(&cfg);
            App {
                kind: AppKind::Leaf,
                exe: PathBuf::from(exe),
                env: vec![
                    ("LEAF_MULTI_INSTANCE".into(), "1".into()),
                    ("LEAF_CONFIG_DIR".into(), cfg.to_string_lossy().into_owned()),
                    ("LEAF_MUTE".into(), "1".into()),
                    // baseline builds from before the rename (bench\bin) read the old names: the same isolation for them
                    ("RUSTYFILES_MULTI_INSTANCE".into(), "1".into()),
                    ("RUSTYFILES_CONFIG_DIR".into(), cfg.to_string_lossy().into_owned()),
                    ("RUSTYFILES_MUTE".into(), "1".into()),
                ],
            }
        }
        "explorer" => detect().into_iter().find(|x| x.kind == AppKind::Explorer).unwrap(),
        _ => App { kind: AppKind::FilePilot, exe: PathBuf::from(exe), env: Vec::new() },
    };
    let cancel = AtomicBool::new(false);
    for i in 1..=runs {
        match measure_open(&app, &folder, Duration::from_secs(60), &cancel) {
            Ok(r) => println!(
                "{kind} run{i}: settled {:.0} ms | window {:.0} ms | cpu {:.0} ms | private {:.0} MB | shared {}",
                r.settled_ms,
                r.window_ms,
                r.cpu_ms,
                r.private as f64 / 1048576.0,
                r.shared
            ),
            Err(e) => println!("{kind} run{i}: ERROR {e:#}"),
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
