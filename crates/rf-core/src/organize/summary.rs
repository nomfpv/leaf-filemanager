//! A rule said in words: "uzantısı jpg, png olan dosyalar → Pictures\{year}".
//!
//! The texts are Turkish literals marked `T!`; the caller gives the translating function (the
//! app's own `t`), so this stays pure and testable with the identity.

use std::path::Path;

use super::rule::{Action, Cmp, Condition, FileKind, Join, NameOp, Rule, TimeField, clean_ext};
use super::safety::{is_inside, norm};
use crate::entry::fold_case;

/// The unit a size is entered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
    B,
    Kb,
    Mb,
    Gb,
}

impl SizeUnit {
    pub const ALL: [SizeUnit; 4] = [SizeUnit::B, SizeUnit::Kb, SizeUnit::Mb, SizeUnit::Gb];

    pub fn bytes(self) -> u64 {
        match self {
            SizeUnit::B => 1,
            SizeUnit::Kb => 1024,
            SizeUnit::Mb => 1024 * 1024,
            SizeUnit::Gb => 1024 * 1024 * 1024,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SizeUnit::B => "B",
            SizeUnit::Kb => "KB",
            SizeUnit::Mb => "MB",
            SizeUnit::Gb => "GB",
        }
    }

    /// The biggest unit that divides `bytes` evenly: 5 MB, 1536 KB, 700 B.
    pub fn split(bytes: u64) -> (u64, SizeUnit) {
        for u in [SizeUnit::Gb, SizeUnit::Mb, SizeUnit::Kb] {
            if bytes >= u.bytes() && bytes.is_multiple_of(u.bytes()) {
                return (bytes / u.bytes(), u);
            }
        }
        (bytes, SizeUnit::B)
    }
}

/// "5 MB".
pub fn size_text(bytes: u64) -> String {
    let (n, u) = SizeUnit::split(bytes);
    format!("{n} {}", u.label())
}

/// The kind as a short lower-case word ("resim").
pub fn kind_word(k: FileKind, tr: &dyn Fn(&'static str) -> String) -> String {
    match k {
        FileKind::Image => tr(T!("image")),
        FileKind::Video => tr(T!("video")),
        FileKind::Audio => tr(T!("audio")),
        FileKind::Document => tr(T!("document")),
        FileKind::Archive => tr(T!("archive")),
        FileKind::Program => tr(T!("program")),
        FileKind::Other => tr(T!("other")),
    }
}

/// A path as the user knows it: below the user's folder it is shown without that part.
pub fn show_path(p: &Path, home: Option<&Path>) -> String {
    if let Some(h) = home
        && norm(p) != norm(h)
        && is_inside(&norm(p), &norm(h))
    {
        // part by part, case-blind like the file system (Path::strip_prefix is not)
        let mut rest = p.components();
        let same = h.components().all(|hc| rest.next().is_some_and(|pc| fold_case(&pc.as_os_str().to_string_lossy()) == fold_case(&hc.as_os_str().to_string_lossy())));
        if same {
            return rest.as_path().to_string_lossy().into_owned();
        }
    }
    p.to_string_lossy().into_owned()
}

fn condition_words(c: &Condition, tr: &dyn Fn(&'static str) -> String) -> String {
    let with = |key: String, x: String| key.replace("{x}", &x);
    match c {
        Condition::Name { op, text } => {
            let key = match op {
                NameOp::Contains => tr(T!("the name contains “{x}”")),
                NameOp::StartsWith => tr(T!("the name starts with “{x}”")),
                NameOp::EndsWith => tr(T!("the name ends with “{x}”")),
                NameOp::Wildcard => tr(T!("the name matches “{x}”")),
            };
            with(key, text.trim().to_string())
        }
        Condition::Extension { list } => {
            let exts: Vec<String> = list.iter().map(|e| clean_ext(e)).filter(|e| !e.is_empty()).collect();
            with(tr(T!("the extension is {x}")), exts.join(", "))
        }
        Condition::Kind { kinds } => {
            let words: Vec<String> = kinds.iter().map(|k| kind_word(*k, tr)).collect();
            with(tr(T!("the type is {x}")), words.join(", "))
        }
        Condition::Size { cmp, bytes } => {
            let key = match cmp {
                Cmp::Greater => tr(T!("the size is over {x}")),
                Cmp::Less => tr(T!("the size is under {x}")),
            };
            with(key, size_text(*bytes))
        }
        Condition::Age { field, cmp, days } => {
            let key = match (field, cmp) {
                (TimeField::Modified, Cmp::Greater) => tr(T!("it was modified more than {x} days ago")),
                (TimeField::Modified, Cmp::Less) => tr(T!("it was modified less than {x} days ago")),
                (TimeField::Created, Cmp::Greater) => tr(T!("it was created more than {x} days ago")),
                (TimeField::Created, Cmp::Less) => tr(T!("it was created less than {x} days ago")),
            };
            with(key, days.to_string())
        }
    }
}

/// What the rule does, the part after the arrow.
pub fn action_words(a: &Action, tr: &dyn Fn(&'static str) -> String, home: Option<&Path>) -> String {
    match a {
        Action::MoveTo { folder } => show_path(folder, home),
        Action::MoveToTemplate { template } => show_path(Path::new(template), home),
        Action::Rename { template } => tr(T!("rename to {x}")).replace("{x}", template),
        Action::Recycle => tr(T!("Send to the Trash")),
    }
}

/// The whole rule in one line.
pub fn summary(rule: &Rule, tr: &dyn Fn(&'static str) -> String, home: Option<&Path>) -> String {
    let who = if rule.conditions.is_empty() {
        tr(T!("all files"))
    } else {
        let joiner = match rule.join {
            Join::All => tr(T!("and")),
            Join::Any => tr(T!("or")),
        };
        let parts: Vec<String> = rule.conditions.iter().map(|c| condition_words(c, tr)).collect();
        tr(T!("files where {c}")).replace("{c}", &parts.join(&format!(" {joiner} ")))
    };
    format!("{who} → {}", action_words(&rule.action, tr, home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn id(s: &'static str) -> String {
        s.to_string()
    }

    fn rule(conditions: Vec<Condition>, action: Action) -> Rule {
        Rule { conditions, action, ..Rule::default() }
    }

    #[test]

    #[cfg_attr(not(windows), ignore = "Windows paths and APIs; a Unix test comes when the Mac port uses this")]
    fn the_starter_rule_reads_like_the_example() {
        let r = rule(
            vec![Condition::Extension { list: vec!["jpg".into(), ".PNG".into()] }],
            Action::MoveToTemplate { template: r"C:\Users\user\Pictures\{year}\{month}".into() },
        );
        let home = PathBuf::from(r"C:\Users\user");
        assert_eq!(summary(&r, &id, Some(&home)), r"files where the extension is jpg, png → Pictures\{year}\{month}");
        assert_eq!(summary(&r, &id, None), r"files where the extension is jpg, png → C:\Users\user\Pictures\{year}\{month}");
    }

    #[test]
    fn conditions_are_joined_by_the_rules_join() {
        let mut r = rule(
            vec![
                Condition::Name { op: NameOp::StartsWith, text: " IMG ".into() },
                Condition::Size { cmp: Cmp::Greater, bytes: 5 * 1024 * 1024 },
                Condition::Age { field: TimeField::Modified, cmp: Cmp::Greater, days: 30 },
            ],
            Action::Recycle,
        );
        assert_eq!(
            summary(&r, &id, None),
            "files where the name starts with “IMG” and the size is over 5 MB and it was modified more than 30 days ago → Send to the Trash"
        );
        r.join = Join::Any;
        assert!(summary(&r, &id, None).contains(" or "));
    }

    #[test]
    fn every_condition_and_action_has_words() {
        let r = rule(
            vec![
                Condition::Name { op: NameOp::Contains, text: "rapor".into() },
                Condition::Name { op: NameOp::EndsWith, text: ".bak".into() },
                Condition::Name { op: NameOp::Wildcard, text: "*.tmp".into() },
                Condition::Kind { kinds: vec![FileKind::Image, FileKind::Video] },
                Condition::Size { cmp: Cmp::Less, bytes: 1536 },
                Condition::Age { field: TimeField::Created, cmp: Cmp::Less, days: 7 },
            ],
            Action::Rename { template: "{year}-{name}.{ext}".into() },
        );
        assert_eq!(
            summary(&r, &id, None),
            "files where the name contains “rapor” and the name ends with “.bak” and the name matches “*.tmp” and the type is image, video and the size is under 1536 B and it was created less than 7 days ago → rename to {year}-{name}.{ext}"
        );
        assert_eq!(summary(&rule(vec![], Action::MoveTo { folder: PathBuf::from(r"D:\Arşiv") }), &id, None), r"all files → D:\Arşiv");
    }

    #[test]
    fn sizes_use_the_biggest_even_unit() {
        assert_eq!(size_text(5 * 1024 * 1024), "5 MB");
        assert_eq!(size_text(1536 * 1024), "1536 KB");
        assert_eq!(size_text(3 * 1024 * 1024 * 1024), "3 GB");
        assert_eq!(size_text(700), "700 B");
        assert_eq!(size_text(0), "0 B");
        assert_eq!(SizeUnit::split(1024), (1, SizeUnit::Kb));
        assert_eq!(SizeUnit::split(1025), (1025, SizeUnit::B));
    }

    #[test]
    fn a_translator_is_used_for_every_text() {
        let marked = |s: &'static str| format!("[{s}]");
        let r = rule(vec![Condition::Kind { kinds: vec![FileKind::Audio] }], Action::Recycle);
        assert_eq!(summary(&r, &marked, None), "[files where [the type is [audio]]] → [Send to the Trash]");
    }

    #[test]

    #[cfg_attr(not(windows), ignore = "Windows paths and APIs; a Unix test comes when the Mac port uses this")]
    fn paths_outside_the_home_are_shown_whole() {
        let home = PathBuf::from(r"C:\Users\user");
        assert_eq!(show_path(Path::new(r"D:\x"), Some(&home)), r"D:\x");
        assert_eq!(show_path(Path::new(r"C:\Users\user"), Some(&home)), r"C:\Users\user");
        assert_eq!(show_path(Path::new(r"C:\Users\user.old\x"), Some(&home)), r"C:\Users\user.old\x");
        assert_eq!(show_path(Path::new(r"c:\users\user\Belgeler"), Some(&home)), "Belgeler");
    }
}
