//! Linux backend of rf_win::propcols. Status: PARTIAL
//!
//! The source, the batch and the test source are leaf-common's (the same on every system). There is no property system here yet:
//! the list of properties is empty, and [`SystemSource`] answers every file with [`ReadFail::Unavailable`] (an empty answer, so a
//! row is not asked again). The queue answers each job at once, on the caller's thread, with that same answer: no thread, and no
//! row waits for a reply that never comes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use leaf_common::stub;
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

pub fn list_properties(_which: Listing) -> Vec<PropInfo> {
    stub!("propcols::list_properties")
}

pub fn describe(names: &[Arc<str>]) -> Vec<Option<PropInfo>> {
    // (one answer per name, as rf-win gives: none is known)
    stub::hit("propcols::describe");
    vec![None; names.len()]
}

/// The system's properties: none yet on Linux.
// TYPE-COPY(rf-win propcols.rs:209): the Windows fields (store flags, key map) dropped
pub struct SystemSource {
    _p: (),
}

impl SystemSource {
    pub fn new(_limit: Duration) -> Self {
        stub::hit("propcols::SystemSource::new");
        Self { _p: () }
    }

    pub fn with_flags(_limit: Duration, _flags: i32) -> Self {
        stub::hit("propcols::SystemSource::with_flags");
        Self { _p: () }
    }
}

impl PropSource for SystemSource {
    fn read(&self, _path: &Path, _names: &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail> {
        stub::hit("propcols::SystemSource::read");
        Err(ReadFail::Unavailable)
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

/// The files the visible rows of a list want. Here each job is answered at once in [`replace`](Self::replace), with the source's
/// own answer when it is the system's (always `Unavailable`), so `sink` gets exactly one `Done` per job.
// TYPE-COPY(rf-win propcols.rs:460): no thread, no shared state
pub struct ReadQueue {
    src: Arc<dyn PropSource>,
    sink: Arc<dyn Fn(Done) + Send + Sync>,
}

impl ReadQueue {
    pub fn new(src: Arc<dyn PropSource>, sink: impl Fn(Done) + Send + Sync + 'static) -> Self {
        stub::hit("propcols::ReadQueue::new");
        Self { src, sink: Arc::new(sink) }
    }

    /// Answers every job now, on the caller's thread (request/response: once per job).
    pub fn replace(&self, names: Arc<[Arc<str>]>, jobs: Vec<QueueJob>) {
        stub::hit("propcols::ReadQueue::replace");
        for job in jobs {
            let result = self.src.read(&job.path, &names);
            (self.sink)(Done { job, names: names.clone(), result });
        }
    }

    pub fn clear(&self) {
        stub!("propcols::ReadQueue::clear")
    }

    pub fn waiting(&self) -> usize {
        stub!("propcols::ReadQueue::waiting")
    }

    pub fn paused(&self) -> bool {
        stub!("propcols::ReadQueue::paused")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn the_queue_answers_each_job_once_and_the_system_has_no_values() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let q = ReadQueue::new(Arc::new(SystemSource::new(Duration::from_secs(1))), move |d| g.lock().unwrap().push((d.job.path, d.result)));
        let job = |p: &str| QueueJob { path: PathBuf::from(p), size: 1, modified_ms: 2 };
        q.replace(Arc::from(vec![Arc::<str>::from("System.Title")]), vec![job("a"), job("b")]);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|(_, r)| *r == Err(ReadFail::Unavailable)));
        assert_eq!(q.waiting(), 0);
        assert_eq!(describe(&[Arc::from("x"), Arc::from("y")]), vec![None, None]);
    }
}
