//! UI text. The interface is English only for now: every UI string in the source IS its English
//! text, and `t!("New folder")` returns it unchanged.
//!
//! The calls stay as a hook: a second language can be added later by keying a table on the
//! English text here, without touching the call sites. Strings produced on worker threads
//! (error messages from the lower crates, group labels) go through [`td`] where they are drawn.

/// The text for the UI language (now: the English text itself).
pub fn t(en: &'static str) -> &'static str {
    en
}

/// The same for a text whose English alone is ambiguous for a later translation (e.g. "Light"
/// for a theme). Kept apart so a translation can key it differently.
pub fn tx(en: &'static str) -> &'static str {
    en
}

/// A dynamic string that may be a known UI text (error messages from the lower crates, group
/// labels, built-in tag names...). Returned unchanged for now.
pub fn td(s: &str) -> String {
    s.to_string()
}

/// Percentage: "53%".
pub fn pct(v: f32) -> String {
    format!("{v:.0}%")
}

/// "1 item" / "5 items".
pub fn items(n: usize) -> String {
    if n == 1 { "1 item".to_string() } else { format!("{n} items") }
}

#[macro_export]
macro_rules! t {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_is_the_text_itself() {
        assert_eq!(t("New folder"), "New folder");
        assert_eq!(tx("Light"), "Light");
        assert_eq!(td("Folder not found: C:/x"), "Folder not found: C:/x");
        assert_eq!(items(1), "1 item");
        assert_eq!(items(3), "3 items");
        assert_eq!(pct(53.), "53%");
        assert_eq!(rf_core::format::size(1536), "1.5 KB");
    }
}
