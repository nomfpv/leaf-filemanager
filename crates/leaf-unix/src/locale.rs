//! Linux backend of the leaf-sys module `locale` (its Windows side is written in rf-win by step A3). Status: REAL (the names only)
//!
//! Which locale the user's dates are in: `LC_ALL`, then `LC_TIME`, then `LANG`, as the C library picks it. "C" and "POSIX" mean none.

/// The locale name for times ("de_DE.UTF-8"), `None` for the C locale or when nothing is set.
pub fn time_locale() -> Option<String> {
    pick(|k| std::env::var(k).ok())
}

fn pick(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    for k in ["LC_ALL", "LC_TIME", "LANG"] {
        if let Some(v) = get(k).filter(|v| !v.is_empty()) {
            return (v != "C" && v != "POSIX" && !v.starts_with("C.")).then_some(v);
        }
    }
    None
}

/// Language and region of a locale name, lower and upper case: "de_AT.UTF-8@euro" is ("de", "AT"); "fr" is ("fr", "").
pub fn split(name: &str) -> (String, String) {
    let base = name.split(['.', '@']).next().unwrap_or("");
    let mut it = base.splitn(2, ['_', '-']);
    let lang = it.next().unwrap_or("").to_ascii_lowercase();
    let region = it.next().unwrap_or("").to_ascii_uppercase();
    (lang, region)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_set_variable_wins_and_c_means_none() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(pick(env(&[("LANG", "de_DE.UTF-8"), ("LC_TIME", "en_GB.UTF-8")])).as_deref(), Some("en_GB.UTF-8"));
        assert_eq!(pick(env(&[("LC_ALL", "C"), ("LANG", "de_DE.UTF-8")])), None);
        assert_eq!(pick(env(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")])).as_deref(), Some("fr_FR.UTF-8"));
        assert_eq!(pick(env(&[])), None);
        assert_eq!(split("de_AT.UTF-8@euro"), ("de".into(), "AT".into()));
        assert_eq!(split("fr"), ("fr".into(), String::new()));
    }
}
