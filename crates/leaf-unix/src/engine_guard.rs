//! Linux backend of rf_win::engine_guard. Status: STUB (Linux has no shell engine and never raises these errors)

use std::time::Duration;

pub use leaf_common::engine_guard::{NoShellEngine, Op, PartlyDone};

/// The shell engine did not start working within the limit.
// TYPE-COPY(rf-win engine_guard.rs:66)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineStalled {
    pub op: Op,
    pub waited: Duration,
}
