//! The kinds of file operation and the two errors the app tells apart when a system's file engine cannot do one (the app downcasts
//! them). The Windows rules behind them are in rf-win's `engine_guard`; other systems never raise them.

/// The kinds of operation the shell engine does for the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Rename,
    PermanentDelete,
    RecycleDelete,
    Copy,
    Move,
    /// Restoring items from the Recycle Bin (also what undoing a delete does).
    RestoreFromBin,
    /// Deleting items inside the Recycle Bin for good.
    DeleteFromBin,
}

impl Op {
    /// For the English message.
    pub fn what(self) -> &'static str {
        match self {
            Op::Rename => "Renaming",
            Op::PermanentDelete => "Deleting some of these items",
            Op::RecycleDelete => "Moving to the Recycle Bin",
            Op::Copy => "This copy",
            Op::Move => "This move",
            Op::RestoreFromBin => "Restoring from the Recycle Bin",
            Op::DeleteFromBin => "Deleting in the Recycle Bin",
        }
    }
}

/// The operation needs the Windows shell and no shell is running. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoShellEngine {
    pub op: Op,
}

impl std::fmt::Display for NoShellEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} needs the Windows shell (Explorer), and no shell is running. Nothing was changed.", self.op.what())
    }
}

impl std::error::Error for NoShellEngine {}

/// The app's own engine did part of an operation and the rest needs the shell (which is not running), or could not be done. Unlike
/// [`NoShellEngine`] this is raised AFTER something was changed, so it never says "nothing was changed": the copies our engine made stay,
/// a move's verified sources are gone, and the user is told how much is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartlyDone {
    pub op: Op,
    /// How many top-level items are left (`None`: not counted).
    pub left: Option<usize>,
}

impl std::fmt::Display for PartlyDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let items = match self.left {
            Some(1) => "1 item".to_string(),
            Some(n) => format!("{n} items"),
            None => "Some items".to_string(),
        };
        match self.op {
            Op::Copy | Op::Move => {
                let verb = if self.op == Op::Copy { "copied" } else { "moved" };
                write!(f, "{items} could not be {verb} because that needs the Windows shell (Explorer), and no shell is running. Everything else was done.")
            }
            _ => write!(f, "{items} could not be deleted (locked, read-only or access denied); the rest was deleted. No shell is running to show details."),
        }
    }
}

impl std::error::Error for PartlyDone {}
