//! The part of the property columns that is the same on every system: why a file has no values, the place values come from (a
//! trait each backend's `SystemSource` implements), a source made of a closure for tests, and a batch that reads from any source.
//! The backends re-export these at `propcols::`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rf_core::propcols::PropCell;

/// Why a file has no values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFail {
    /// The shell did not answer in time (a slow or broken handler).
    Timeout,
    /// The file is not there, is a folder, or its handler refused to open.
    Unavailable,
    /// The file is in the cloud and was not opened.
    OnlineOnly,
}

/// A place the values come from. The real one asks the system; tests use [`FakeSource`].
pub trait PropSource: Send + Sync {
    /// The cells of `names` for the file, in the same order. Blocks, but not longer than the source's
    /// own limit.
    fn read(&self, path: &Path, names: &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail>;
}

type FakeAnswer = dyn Fn(&Path, &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail> + Send + Sync;

/// A source made of a closure, for tests: counts its calls.
pub struct FakeSource {
    answer: Box<FakeAnswer>,
    pub calls: AtomicUsize,
}

impl FakeSource {
    pub fn new(answer: impl Fn(&Path, &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail> + Send + Sync + 'static) -> Self {
        Self { answer: Box::new(answer), calls: AtomicUsize::new(0) }
    }
}

impl PropSource for FakeSource {
    fn read(&self, path: &Path, names: &[Arc<str>]) -> Result<Vec<PropCell>, ReadFail> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.answer)(path, names)
    }
}

// ------------------------------------------------------------------ a batch

/// How a batch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchEnd {
    Finished,
    Cancelled,
    /// Several files in a row did not answer in time: the rest was not tried.
    Stalled,
}

/// This many files in a row that run out of time stop a batch (the shell is stuck on something; asking
/// on would only queue more work behind it).
pub const STRIKES: usize = 3;

/// Reads `names` for each job's file, one file after another, and hands every answer to `on_result`
/// (the job's tag first). Stops between files when `cancel` is set. A file that runs out of time is
/// reported as such and the batch goes on, unless [`STRIKES`] files in a row did.
pub fn read_batch<T>(
    src: &dyn PropSource,
    jobs: impl IntoIterator<Item = (T, PathBuf)>,
    names: &[Arc<str>],
    cancel: &AtomicBool,
    mut on_result: impl FnMut(T, Result<Vec<PropCell>, ReadFail>),
) -> BatchEnd {
    let mut strikes = 0;
    for (tag, path) in jobs {
        if cancel.load(Ordering::Relaxed) {
            return BatchEnd::Cancelled;
        }
        let r = src.read(&path, names);
        if r == Err(ReadFail::Timeout) {
            strikes += 1;
            on_result(tag, r);
            if strikes >= STRIKES {
                return BatchEnd::Stalled;
            }
        } else {
            strikes = 0;
            on_result(tag, r);
        }
    }
    BatchEnd::Finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_source_answers_with_its_closure_and_counts() {
        let src = FakeSource::new(|p, _| if p.ends_with("bad") { Err(ReadFail::Timeout) } else { Ok(Vec::new()) });
        assert_eq!(src.read(Path::new("a/good"), &[]), Ok(Vec::new()));
        assert_eq!(src.read(Path::new("a/bad"), &[]), Err(ReadFail::Timeout));
        assert_eq!(src.calls.load(Ordering::SeqCst), 2);
    }
}
