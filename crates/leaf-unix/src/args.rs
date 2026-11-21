//! Linux backend of the leaf-sys module `args` (Windows side: rf-win args.rs, `CommandLineToArgvW`). Status: REAL
//!
//! A command line's arguments split as a POSIX shell splits words, without any expansion: blanks separate; `'...'` is literal;
//! `"..."` is literal except that a backslash keeps a following `"`, `\`, `$` or `` ` `` (and drops a newline); outside quotes a
//! backslash keeps the next character. `$`, globs, `~` and `;` are plain characters here (nothing is run through a shell).

use std::ffi::OsString;

/// `text` (the arguments only, without a program) as the list of arguments a program gets.
pub fn split(text: &str) -> anyhow::Result<Vec<OsString>> {
    anyhow::ensure!(!text.contains('\0'), "Invalid command arguments");
    let mut out = Vec::new();
    let mut word = String::new();
    // a word was begun (so that "" and '' give an empty argument)
    let mut begun = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if begun {
                    out.push(OsString::from(std::mem::take(&mut word)));
                    begun = false;
                }
            }
            '\'' => {
                begun = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => anyhow::bail!("Invalid command arguments: a quote is not closed"),
                    }
                }
            }
            '"' => {
                begun = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some('"' | '\\' | '$' | '`') => word.push(chars.next().unwrap_or_default()),
                            Some('\n') => {
                                chars.next();
                            }
                            _ => word.push('\\'),
                        },
                        Some(c) => word.push(c),
                        None => anyhow::bail!("Invalid command arguments: a quote is not closed"),
                    }
                }
            }
            '\\' => {
                begun = true;
                match chars.next() {
                    // (a line continuation)
                    Some('\n') => {}
                    Some(c) => word.push(c),
                    None => word.push('\\'),
                }
            }
            c => {
                begun = true;
                word.push(c);
            }
        }
    }
    if begun {
        out.push(OsString::from(word));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::split;

    fn s(text: &str) -> Vec<String> {
        split(text).unwrap().into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn words_quotes_and_backslashes_split_as_a_shell_splits_them() {
        assert_eq!(s(""), Vec::<String>::new());
        assert_eq!(s("  a   b\tc  "), ["a", "b", "c"]);
        assert_eq!(s(r#"--name "two words" 'it''s' x"#), ["--name", "two words", "its", "x"]);
        assert_eq!(s(r#""" ''"#), ["", ""]);
        assert_eq!(s(r#"a\ b "q\"q" "back\\slash" "keep\n" 'raw\"'"#), ["a b", "q\"q", "back\\slash", "keep\\n", "raw\\\""]);
        assert_eq!(s("$HOME ~ *.txt ;"), ["$HOME", "~", "*.txt", ";"]);
        assert_eq!(s("pre\"mid dle\"post"), ["premid dlepost"]);
    }

    #[test]
    fn an_open_quote_or_a_nul_is_an_error() {
        assert!(split("\"open").is_err());
        assert!(split("'open").is_err());
        assert!(split("a\0b").is_err());
    }
}
