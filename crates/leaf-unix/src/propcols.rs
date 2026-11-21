//! Linux backend of rf_win::propcols. Status: PARTIAL (the cheap properties, from the file's metadata)
//!
//! The source, the batch and the test source are leaf-common's (the same on every system). There is no property system here: the
//! list holds the few properties that come from `stat` (size, the three dates, extension, type by extension, owner), under the
//! names Windows uses (`System.Size`, `System.DateModified`, ...). Any other name is known to nobody (`describe` answers `None`,
//! a read gives an empty cell). The queue reads on one worker thread of its own (a `stat` on a dead network mount must not stop
//! the window), newest `replace` first.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use rf_core::propcols::PropValue;
pub use leaf_common::propcols::{BatchEnd, FakeSource, PropSource, ReadFail, STRIKES, read_batch};
use rf_core::propcols::{PropCell, PropKind};

/// One property of the system's schema.
// TYPE-COPY(rf-win propcols.rs:84)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropInfo {
    pub canonical: String,
    pub display: String,
    pub kind: PropKind,
    pub default_width: u16,
    pub multiple: bool,
}

/// Which properties to list.
// TYPE-COPY(rf-win propcols.rs:97)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    Viewable,
    Column,
}

const KNOWN: &[(&str, &str, PropKind, u16)] = &[
    ("System.DateAccessed", "Date accessed", PropKind::DateTime, 140),
    ("System.DateCreated", "Date created", PropKind::DateTime, 140),
    ("System.DateModified", "Date modified", PropKind::DateTime, 140),
    ("System.FileExtension", "File extension", PropKind::Text, 90),
    ("System.FileOwner", "Owner", PropKind::Text, 110),
    ("System.ItemTypeText", "Item type", PropKind::Text, 150),
    ("System.Size", "Size", PropKind::Number, 90),
];

fn known(name: &str) -> Option<PropInfo> {
    KNOWN.iter().find(|k| k.0.eq_ignore_ascii_case(name)).map(|&(c, d, kind, w)| PropInfo { canonical: c.to_string(), display: d.to_string(), kind, default_width: w, multiple: false })
}

/// The properties this backend can read, by display name.
pub fn list_properties(_which: Listing) -> Vec<PropInfo> {
    let mut v: Vec<PropInfo> = KNOWN.iter().filter_map(|k| known(k.0)).collect();
    v.sort_by(|a, b| a.display.cmp(&b.display));
    v
}

pub fn describe(names: &[Arc<str>]) -> Vec<Option<PropInfo>> {
    names.iter().map(|n| known(n)).collect()
}

/// "1.4 MB" (powers of 1024, the unit names Explorer uses).
fn size_text(n: u64) -> String {
    const U: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} bytes");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

fn date_cell(t: std::io::Result<std::time::SystemTime>) -> PropCell {
    let Ok(t) = t else { return PropCell::empty() };
    let local: chrono::DateTime<chrono::Local> = t.into();
    PropCell::new(&local.format("%Y-%m-%d %H:%M").to_string(), PropValue::Int(local.timestamp_millis()))
}

fn cell_of(path: &Path, m: &std::fs::Metadata, name: &str) -> PropCell {
    let is_dir = m.is_dir();
    match name.to_ascii_lowercase().as_str() {
        "system.size" if !is_dir => PropCell::new(&size_text(m.len()), PropValue::Int(m.len().min(i64::MAX as u64) as i64)),
        "system.datemodified" => date_cell(m.modified()),
        "system.datecreated" => date_cell(m.created()),
        "system.dateaccessed" => date_cell(m.accessed()),
        "system.fileextension" if !is_dir => match path.extension().and_then(|e| e.to_str()) {
            Some(e) => {
                let e = format!(".{e}");
                PropCell::new(&e, PropValue::text(&e.to_lowercase()))
            }
            None => PropCell::empty(),
        },
        "system.itemtypetext" => {
            if is_dir {
                PropCell::new("File folder", PropValue::text("File folder"))
            } else {
                match crate::fileinfo::mime_guess(path) {
                    Some(t) => PropCell::new(t, PropValue::text(t)),
                    None => PropCell::empty(),
                }
            }
        }
        "system.fileowner" => match crate::fileinfo::owner_name(path) {
            Some(o) => PropCell::new(&o, PropValue::text(&o)),
            None => PropCell::empty(),
        },
        _ => PropCell::empty(),
    }
}

/// The properties read from the file's metadata.
// TYPE-COPY(rf-win propcols.rs:209): the Windows fields (store flags, key map) dropped
pub struct SystemSource {
    _p: (),
}

impl SystemSource {
    pub fn new(_limit: Duration) -> Self {
        Self { _p: () }
    }

    pub fn with_flags(_limit: Duration, _flags: i32) -> Self {
        Self { _p: () }
    }
}

impl PropSource for SystemSource {
    fn read(&self, path: &Path, names: &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail> {
        let m = std::fs::metadata(path).map_err(|_| ReadFail::Unavailable)?;
        Ok(names.iter().map(|n| cell_of(path, &m, n)).collect())
    }
}

/// A file to read: where it is and the size and time it has now (the cache key).
// TYPE-COPY(rf-win propcols.rs:428)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueJob {
    pub path: PathBuf,
    pub size: u64,
    pub modified_ms: i64,
}

/// What came of a queued job.
// TYPE-COPY(rf-win propcols.rs:435)
pub struct Done {
    pub job: QueueJob,
    pub names: Arc<[Arc<str>]>,
    pub result: Result<Vec<PropCell>, ReadFail>,
}

struct Work {
    names: Arc<[Arc<str>]>,
    jobs: VecDeque<QueueJob>,
    gone: bool,
}

struct Shared {
    work: Mutex<Work>,
    cv: Condvar,
}

/// The files the visible rows of a list want, read on a worker thread of its own, in the order given. `replace` swaps the waiting
/// work for a new list; `sink` gets one `Done` per job that was read.
// TYPE-COPY(rf-win propcols.rs:460)
pub struct ReadQueue {
    shared: Arc<Shared>,
}

impl ReadQueue {
    pub fn new(src: Arc<dyn PropSource>, sink: impl Fn(Done) + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared { work: Mutex::new(Work { names: Arc::from(Vec::new()), jobs: VecDeque::new(), gone: false }), cv: Condvar::new() });
        let s = shared.clone();
        let spawned = std::thread::Builder::new().name("leaf-propcols".into()).spawn(move || {
            loop {
                let (job, names) = {
                    let mut w = s.work.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if w.gone {
                            return;
                        }
                        if let Some(j) = w.jobs.pop_front() {
                            break (j, w.names.clone());
                        }
                        w = s.cv.wait(w).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let result = src.read(&job.path, &names);
                sink(Done { job, names, result });
            }
        });
        if spawned.is_err() {
            log::debug!("propcols: no thread for the read queue");
        }
        Self { shared }
    }

    pub fn replace(&self, names: Arc<[Arc<str>]>, jobs: Vec<QueueJob>) {
        let mut w = self.shared.work.lock().unwrap_or_else(|e| e.into_inner());
        w.names = names;
        w.jobs = jobs.into();
        drop(w);
        self.shared.cv.notify_one();
    }

    pub fn clear(&self) {
        self.shared.work.lock().unwrap_or_else(|e| e.into_inner()).jobs.clear();
    }

    pub fn waiting(&self) -> usize {
        self.shared.work.lock().unwrap_or_else(|e| e.into_inner()).jobs.len()
    }

    pub fn paused(&self) -> bool {
        false
    }
}

impl Drop for ReadQueue {
    fn drop(&mut self) {
        self.shared.work.lock().unwrap_or_else(|e| e.into_inner()).gone = true;
        self.shared.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_reads_size_and_type_from_metadata() {
        let dir = std::env::temp_dir().join(format!("leaf-propcols-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.png");
        std::fs::write(&f, vec![0u8; 2048]).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let q = ReadQueue::new(Arc::new(SystemSource::new(Duration::from_secs(1))), move |d| {
            let _ = tx.lock().unwrap().send(d.result);
        });
        let names: Arc<[Arc<str>]> = Arc::from(vec![Arc::<str>::from("System.Size"), Arc::from("System.ItemTypeText"), Arc::from("System.Title")]);
        q.replace(names, vec![QueueJob { path: f, size: 2048, modified_ms: 0 }]);
        let cells = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        assert_eq!(&*cells[0].text, "2.0 KB");
        assert_eq!(&*cells[1].text, "image/png");
        assert!(cells[2].value.is_empty());
        assert_eq!(describe(&[Arc::from("System.Size"), Arc::from("x")]).iter().map(|d| d.is_some()).collect::<Vec<_>>(), vec![true, false]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
