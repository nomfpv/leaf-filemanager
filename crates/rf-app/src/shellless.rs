//! What the app says when something fails because the Windows shell (Explorer) is not running, or does not answer.
//!
//! Before this, the Start panel said "Başlatılamadı. Kaldırılmış ya da taşınmış olabilir." ("may have been uninstalled or moved") for
//! EVERY failed start, including "Sınıf kaydedilmemiş (0x80040154)" from a shell that is not there: the user's trial without Explorer
//! was told the apps had been removed. And a file operation that needs the shell's engine showed the engine's raw English text. The
//! texts here say what happened. The choice of text is pure ([`classify_start`], tested); the words are Turkish with an English pair.

use leaf_sys::engine_guard::{EngineStalled, NoShellEngine, Op, PartlyDone};

/// What the Status Center shows for a failed file operation: a true sentence for the failures the shell's absence causes, the error's
/// own text for everything else.
pub fn op_failure_text(err: &anyhow::Error) -> String {
    if let Some(n) = err.downcast_ref::<NoShellEngine>() {
        return match n.op {
            Op::RecycleDelete => crate::t!("Moving to the Trash needs the Windows shell (Explorer), which is not running. Nothing was deleted; permanent delete is still available.").to_string(),
            Op::Copy | Op::Move => crate::t!("This copy or move needs the Windows shell (Explorer), which is not running. Nothing was changed.").to_string(),
            Op::RestoreFromBin | Op::DeleteFromBin => crate::t!("This Trash operation needs the Windows shell (Explorer), which is not running. Nothing was changed.").to_string(),
            Op::PermanentDelete | Op::Rename => crate::t!("Some items could not be deleted (locked, read-only or no permission), and the Windows shell (Explorer) that would show the details is not running.").to_string(),
        };
    }
    // our own engine did its part first: the sentence says how much is left and that the rest was done (never "nothing was changed")
    if let Some(p) = err.downcast_ref::<PartlyDone>() {
        let n = p.left.unwrap_or(0).to_string();
        return match p.op {
            Op::Copy => crate::t!("{n} items could not be copied: that needs the Windows shell (Explorer) and no shell is running. Everything else was done.").replace("{n}", &n),
            Op::Move => crate::t!("{n} items could not be moved: that needs the Windows shell (Explorer) and no shell is running. Everything else was done.").replace("{n}", &n),
            _ => crate::t!("Some items could not be deleted (locked, read-only or access denied); the rest was deleted. No Windows shell (Explorer) is running to show details.").to_string(),
        };
    }
    if let Some(s) = err.downcast_ref::<EngineStalled>() {
        return crate::t!("Windows' file engine did not start within {n} seconds; Explorer may have closed or hung. The operation may still finish: look at Windows' own window.").replace("{n}", &s.waited.as_secs().to_string());
    }
    format!("{err:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_engine_errors_have_their_own_sentences() {
        let e: anyhow::Error = NoShellEngine { op: Op::RecycleDelete }.into();
        assert_ne!(op_failure_text(&e), format!("{e:#}"), "the raw English text is not shown as it is");
        let s: anyhow::Error = EngineStalled { op: Op::Copy, waited: std::time::Duration::from_secs(60) }.into();
        assert!(op_failure_text(&s).contains("60"));
        // an operation our engine did part of is told as partly done, with the count
        let p: anyhow::Error = PartlyDone { op: Op::Move, left: Some(3) }.into();
        let text = op_failure_text(&p);
        assert!(text.contains('3') && !text.contains("{n}") && text.contains("Everything else was done"), "{text}");
        assert!(!text.contains("Nothing was changed"), "{text}");
        let d: anyhow::Error = PartlyDone { op: Op::PermanentDelete, left: None }.into();
        assert!(op_failure_text(&d).contains("the rest was deleted"));
        // anything else is its own text
        assert_eq!(op_failure_text(&anyhow::anyhow!("disk full")), "disk full");
    }
}
