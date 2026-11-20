//! What the app says when something fails because the Windows shell (Explorer) is not running, or does not answer.
//!
//! Before this, the Start panel said "Başlatılamadı. Kaldırılmış ya da taşınmış olabilir." ("may have been uninstalled or moved") for
//! EVERY failed start, including "Sınıf kaydedilmemiş (0x80040154)" from a shell that is not there: the user's trial without Explorer
//! was told the apps had been removed. And a file operation that needs the shell's engine showed the engine's raw English text. The
//! texts here say what happened. The choice of text is pure ([`classify_start`], tested); the words are Turkish with an English pair.

use rf_win::engine_guard::{EngineStalled, NoShellEngine, Op, PartlyDone};

/// What the Status Center shows for a failed file operation: a true sentence for the failures the shell's absence causes, the error's
/// own text for everything else.
pub fn op_failure_text(err: &anyhow::Error) -> String {
    if let Some(n) = err.downcast_ref::<NoShellEngine>() {
        return match n.op {
            Op::RecycleDelete => crate::t!("Geri Dönüşüm Kutusu'na taşıma Windows kabuğunu (Explorer) gerektiriyor ve kabuk çalışmıyor. Hiçbir şey silinmedi; kalıcı silme kullanılabilir.").to_string(),
            Op::Copy | Op::Move => crate::t!("Bu kopyalama ya da taşıma Windows kabuğunu (Explorer) gerektiriyor ve kabuk çalışmıyor. Hiçbir şey değiştirilmedi.").to_string(),
            Op::RestoreFromBin | Op::DeleteFromBin => crate::t!("Geri Dönüşüm Kutusu işlemi Windows kabuğunu (Explorer) gerektiriyor ve kabuk çalışmıyor. Hiçbir şey değiştirilmedi.").to_string(),
            Op::PermanentDelete | Op::Rename => crate::t!("Bazı öğeler silinemedi (kilitli, salt okunur ya da izin yok) ve ayrıntıyı gösterecek Windows kabuğu (Explorer) çalışmıyor.").to_string(),
        };
    }
    // our own engine did its part first: the sentence says how much is left and that the rest was done (never "nothing was changed")
    if let Some(p) = err.downcast_ref::<PartlyDone>() {
        let n = p.left.unwrap_or(0).to_string();
        return match p.op {
            Op::Copy => crate::t!("{n} öğe kopyalanamadı: bunun için Windows kabuğu (Explorer) gerekiyor ve kabuk çalışmıyor. Geri kalan her şey yapıldı.").replace("{n}", &n),
            Op::Move => crate::t!("{n} öğe taşınamadı: bunun için Windows kabuğu (Explorer) gerekiyor ve kabuk çalışmıyor. Geri kalan her şey yapıldı.").replace("{n}", &n),
            _ => crate::t!("Bazı öğeler silinemedi (kilitli, salt okunur ya da izin yok); geri kalanı silindi. Ayrıntıyı gösterecek Windows kabuğu (Explorer) çalışmıyor.").to_string(),
        };
    }
    if let Some(s) = err.downcast_ref::<EngineStalled>() {
        return crate::t!("Windows'un dosya motoru {n} saniyede başlamadı; Explorer kapanmış ya da takılmış olabilir. İşlem yine de bitebilir: Windows'un kendi penceresine bakın.").replace("{n}", &s.waited.as_secs().to_string());
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
        assert!(text.contains('3') && !text.contains("{n}") && text.contains("Geri kalan"), "{text}");
        assert!(!text.contains("Hiçbir şey değiştirilmedi"), "{text}");
        let d: anyhow::Error = PartlyDone { op: Op::PermanentDelete, left: None }.into();
        assert!(op_failure_text(&d).contains("geri kalanı silindi"));
        // anything else is its own text
        assert_eq!(op_failure_text(&anyhow::anyhow!("disk full")), "disk full");
    }
}
