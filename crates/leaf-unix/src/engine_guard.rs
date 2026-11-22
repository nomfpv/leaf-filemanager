//! Linux backend of rf_win::engine_guard. Status: N/A (Linux has no shell engine and never raises these errors; the types exist for the app)

use std::time::Duration;

pub use leaf_common::engine_guard::{NoShellEngine, Op, PartlyDone};

/// The shell engine did not start working within the limit.
// TYPE-COPY(rf-win engine_guard.rs:66)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStalled {
    pub op: Op,
    pub waited: Duration,
}

impl std::fmt::Display for EngineStalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The file engine did not start within {} s ({}).", self.waited.as_secs(), self.op.what())
    }
}

impl std::error::Error for EngineStalled {}
