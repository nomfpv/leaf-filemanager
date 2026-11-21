//! Linux backend of rf_win::encoding: text of unknown encoding, and whether the user's language is Turkish.

/// Text from bytes of unknown encoding: UTF-8 if valid (a truncated trailing sequence is tolerated, since previews read only the
/// head of a file), otherwise the bytes as Latin-1 (there is no "legacy ANSI page" here; this shows old 8-bit text readably).
pub fn decode_text(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&raw[..e.valid_up_to()]).into_owned(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    }
}

/// The first of LC_ALL, LC_MESSAGES, LANG that is set names the language ("tr_TR.UTF-8").
pub fn user_locale_is_turkish() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("tr"))
}

#[cfg(test)]
mod tests {
    use super::decode_text;

    #[test]
    fn utf8_with_a_cut_end_and_old_8_bit_text() {
        assert_eq!(decode_text("Şirket".as_bytes()), "Şirket");
        let cut = [b'a', b'b', b'c', 0xC5];
        assert_eq!(decode_text(&cut), "abc");
        assert_eq!(decode_text(&[b'a', 0xE9, b'b']), "aéb");
    }
}
