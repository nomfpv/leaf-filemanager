//! Bulk rename plan: every item's old name, new name and status, then a safe order of moves.
//! Pure: the caller lists the folders (`existing`), this module never touches the disk.
//!
//! * [`check`] gives every row a [`Status`]: unchanged, rename, case-only rename, or a
//!   [`Problem`] (bad name for the platform, collision inside the batch, collision with a file
//!   that is not part of the batch, a step that cannot run).
//! * [`order`] turns the rows that may run into [`Move`]s: chains run back to front without a
//!   temporary name, cycles (a<->b, a->b->c->a) go through exactly one temporary name each.
//!   No move ever targets a name that is still taken at that point of the sequence.
//!
//! An item that is not renamed (problem, unchanged, or excluded) keeps its old name, and that
//! name stays taken: [`check`] repeats its collision pass until no renamed item lands on a name
//! that an unrenamed item keeps, so "skip items with problems" can never lead to an overwrite.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Why an item cannot get its new name. The text is shown in the preview row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("The new name is empty")]
    Empty,
    #[error("The name contains {0}, which is not allowed in a file name")]
    InvalidChar(String),
    #[error("The name cannot end with a dot or a space")]
    TrailingDotSpace,
    #[error("\"{0}\" is a reserved name on Windows")]
    Reserved(String),
    #[error("The name is too long ({0} characters, at most 255)")]
    TooLong(usize),
    /// Another item of the batch would end up with the same name (or keeps that name).
    #[error("Collides with \"{0}\" in this batch")]
    Duplicate(String),
    /// A file or folder that is not part of the batch already has the name.
    #[error("\"{0}\" already exists in the folder")]
    Exists(String),
    /// The same item was given twice.
    #[error("This item is listed twice")]
    Twice,
    /// The item is not in its folder any more (undo / redo after something else changed it).
    #[error("The item is not there any more")]
    Missing,
    #[error("Step {step}: invalid regular expression: {msg}")]
    BadRegex { step: usize, msg: String },
    #[error("Step {step}: unknown token {{{token}}}")]
    UnknownToken { step: usize, token: String },
    #[error("Step {step}: invalid date format \"{format}\"")]
    BadDateFormat { step: usize, format: String },
    #[error("Step {step}: the file has no {field} date")]
    NoDate { step: usize, field: &'static str },
}

impl Problem {
    /// Collisions (inside the batch or with the folder), for the summary line.
    pub fn is_collision(&self) -> bool {
        matches!(self, Problem::Duplicate(_) | Problem::Exists(_) | Problem::Twice)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The new name is the old one.
    Unchanged,
    /// Will be renamed.
    Rename,
    /// Only the letter case changes ("a.txt" -> "A.txt"): not a collision with itself; the
    /// executor renames it in one direct step (the file system treats it as the same name).
    CaseOnly,
    Problem(Problem),
}

impl Status {
    pub fn runs(&self) -> bool {
        matches!(self, Status::Rename | Status::CaseOnly)
    }
}

/// One item of the batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The folder the item is in (a bulk rename never moves an item to another folder).
    pub dir: PathBuf,
    pub old: String,
    /// The computed name (empty when a step could not compute one).
    pub new: String,
    pub status: Status,
}

impl Row {
    /// A row before [`check`]: `Err` is a problem found while computing the name.
    pub fn new(dir: impl Into<PathBuf>, old: impl Into<String>, new: Result<String, Problem>) -> Self {
        let old = old.into();
        match new {
            Ok(n) => Row { dir: dir.into(), old, new: n, status: Status::Unchanged },
            Err(p) => Row { dir: dir.into(), old, new: String::new(), status: Status::Problem(p) },
        }
    }
}

/// How names are compared on the target file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    /// "a.txt" and "A.TXT" are the same name (Windows, macOS default).
    pub case_insensitive: bool,
}

impl NameRules {
    pub const WINDOWS: NameRules = NameRules { case_insensitive: true };

    /// The rules of the platform this build runs on.
    // TODO(linux): case-sensitive by default; a case-insensitive mount (FAT, SMB, ext4 casefold)
    // would need the folder's own answer.
    pub fn native() -> NameRules {
        if cfg!(windows) { NameRules::WINDOWS } else { NameRules { case_insensitive: false } }
    }

    pub fn key(&self, name: &str) -> String {
        if self.case_insensitive { name.to_lowercase() } else { name.to_string() }
    }
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9",
];

/// Maximum length of one name, in UTF-16 units (NTFS).
pub const MAX_NAME_UNITS: usize = 255;

/// Is `name` a valid file name on the target platform? The one place the platform rule lives.
// TODO(linux): on Linux only '/' and NUL are invalid and the limit is 255 bytes of UTF-8;
// trailing dots/spaces and the device names are fine there. Windows rules for now (the files
// may well end up on a Windows share).
pub fn name_problem(name: &str) -> Option<Problem> {
    if name.trim().is_empty() {
        return Some(Problem::Empty);
    }
    if let Some(c) = name.chars().find(|&c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 32) {
        let shown = if (c as u32) < 32 { "a control character".to_string() } else { format!("'{c}'") };
        return Some(Problem::InvalidChar(shown));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(Problem::TrailingDotSpace);
    }
    // "CON", "con.txt", "NUL .tar.gz": the part before the first dot, trailing spaces ignored
    let base = name.split('.').next().unwrap_or("").trim_end().to_uppercase();
    if RESERVED.contains(&base.as_str()) {
        return Some(Problem::Reserved(name.split('.').next().unwrap_or("").trim_end().to_string()));
    }
    let units = name.encode_utf16().count();
    if units > MAX_NAME_UNITS {
        return Some(Problem::TooLong(units));
    }
    None
}

/// Gives every row its status. `existing` lists, per folder, every name in it now (batch members
/// included; they are recognised and not counted as "other files").
pub fn check(rows: &mut [Row], existing: &HashMap<PathBuf, Vec<String>>, rules: NameRules) {
    // the same item twice: the second one is a problem (two moves of one file)
    let mut seen: HashSet<(PathBuf, String)> = HashSet::new();
    for r in rows.iter_mut() {
        if !seen.insert((r.dir.clone(), rules.key(&r.old))) {
            r.status = Status::Problem(Problem::Twice);
        }
    }
    // names and the platform rule
    for r in rows.iter_mut() {
        if matches!(r.status, Status::Problem(_)) {
            continue;
        }
        r.status = if r.new == r.old {
            Status::Unchanged
        } else if let Some(p) = name_problem(&r.new) {
            Status::Problem(p)
        } else if rules.key(&r.new) == rules.key(&r.old) {
            Status::CaseOnly
        } else {
            Status::Rename
        };
    }
    // files of the folder that are not part of the batch
    let mut batch_old: HashMap<&Path, HashSet<String>> = HashMap::new();
    for r in rows.iter() {
        batch_old.entry(r.dir.as_path()).or_default().insert(rules.key(&r.old));
    }
    let mut others: HashMap<&Path, HashMap<String, &str>> = HashMap::new();
    for (dir, names) in existing {
        let mine = batch_old.get(dir.as_path());
        let set = others.entry(dir.as_path()).or_default();
        for n in names {
            let k = rules.key(n);
            if !mine.is_some_and(|m| m.contains(&k)) {
                set.insert(k, n.as_str());
            }
        }
    }
    for r in rows.iter_mut() {
        if r.status == Status::Rename
            && let Some(name) = others.get(r.dir.as_path()).and_then(|s| s.get(&rules.key(&r.new)))
        {
            r.status = Status::Problem(Problem::Exists(name.to_string()));
        }
    }
    // collisions inside the batch, until stable: an item that is not renamed keeps its old name
    loop {
        let mut at: HashMap<(&Path, String), Vec<usize>> = HashMap::new();
        for (i, r) in rows.iter().enumerate() {
            let fin = if r.status.runs() { &r.new } else { &r.old };
            at.entry((r.dir.as_path(), rules.key(fin))).or_default().push(i);
        }
        let mut hits: Vec<(usize, String)> = Vec::new();
        for group in at.values().filter(|g| g.len() > 1) {
            for &i in group {
                if rows[i].status.runs() {
                    let other = group.iter().find(|&&j| j != i).map(|&j| rows[j].old.clone()).unwrap_or_default();
                    hits.push((i, other));
                }
            }
        }
        if hits.is_empty() {
            break;
        }
        for (i, other) in hits {
            rows[i].status = Status::Problem(Problem::Duplicate(other));
        }
    }
}

/// Counts for the summary line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Will be renamed (case-only renames included).
    pub rename: usize,
    pub unchanged: usize,
    pub collisions: usize,
    /// Problems that are not collisions.
    pub other_problems: usize,
}

impl Summary {
    pub fn of(rows: &[Row]) -> Summary {
        let mut s = Summary::default();
        for r in rows {
            match &r.status {
                Status::Rename | Status::CaseOnly => s.rename += 1,
                Status::Unchanged => s.unchanged += 1,
                Status::Problem(p) if p.is_collision() => s.collisions += 1,
                Status::Problem(_) => s.other_problems += 1,
            }
        }
        s
    }

    pub fn problems(&self) -> usize {
        self.collisions + self.other_problems
    }

    /// May the batch run? Not with problems unless they are skipped; not with nothing to do.
    pub fn can_run(&self, skip_problems: bool) -> bool {
        self.rename > 0 && (self.problems() == 0 || skip_problems)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Old name -> new name.
    Direct,
    /// Old name -> temporary name (the first move of a cycle).
    ToTemp,
    /// Temporary name -> new name (the last move of a cycle).
    FromTemp,
}

/// One rename inside one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    /// The row this move belongs to.
    pub row: usize,
    pub dir: PathBuf,
    pub from: String,
    pub to: String,
    pub kind: MoveKind,
    /// The cycle this move is part of (moves of one cycle are consecutive).
    pub cycle: Option<usize>,
}

/// The moves for the rows that run (`Status::Rename` / `CaseOnly`), in a safe order. Rows with
/// any other status are left out: they keep their names, and [`check`] made sure no running row
/// targets such a name. A temporary name is `~leaf-rename-{salt}-{n}.tmp`, with `n` chosen so it
/// is neither in `existing` nor any old or new name of the batch.
///
/// Call it only on rows that went through [`check`]: it relies on the new names being unique.
pub fn order(rows: &[Row], existing: &HashMap<PathBuf, Vec<String>>, rules: NameRules, salt: &str) -> Vec<Move> {
    let mut out = Vec::new();
    let mut cycle_no = 0usize;
    // per folder, in the order the folders first appear
    let mut dirs: Vec<&Path> = Vec::new();
    let mut by_dir: HashMap<&Path, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if r.status.runs() {
            by_dir.entry(r.dir.as_path()).or_insert_with(|| {
                dirs.push(r.dir.as_path());
                Vec::new()
            });
            by_dir.get_mut(r.dir.as_path()).unwrap().push(i);
        }
    }
    for dir in dirs {
        let members = &by_dir[dir];
        // which running row holds the name a row wants (case-only rows want their own name)
        let holder: HashMap<String, usize> = members.iter().map(|&i| (rules.key(&rows[i].old), i)).collect();
        let mut blocker: HashMap<usize, usize> = HashMap::new();
        let mut dependent: HashMap<usize, usize> = HashMap::new();
        for &i in members {
            if rows[i].status == Status::CaseOnly {
                continue;
            }
            if let Some(&j) = holder.get(&rules.key(&rows[i].new))
                && j != i
            {
                blocker.insert(i, j);
                dependent.insert(j, i);
            }
        }
        let mut done: HashSet<usize> = HashSet::new();
        let direct = |i: usize, out: &mut Vec<Move>, cycle: Option<usize>| {
            out.push(Move { row: i, dir: dir.to_path_buf(), from: rows[i].old.clone(), to: rows[i].new.clone(), kind: MoveKind::Direct, cycle });
        };
        // chains: a row whose target is free goes first, then the row that wanted its old name
        for &start in members {
            if done.contains(&start) || blocker.contains_key(&start) {
                continue;
            }
            let mut k = Some(start);
            while let Some(i) = k {
                if !done.insert(i) {
                    break;
                }
                direct(i, &mut out, None);
                k = dependent.get(&i).copied();
            }
        }
        // what is left are cycles: one temporary name each
        let mut taken: HashSet<String> = existing.get(dir).map(|v| v.iter().map(|n| rules.key(n)).collect()).unwrap_or_default();
        for &i in members {
            taken.insert(rules.key(&rows[i].old));
            taken.insert(rules.key(&rows[i].new));
        }
        let mut n = 0usize;
        for &c in members {
            if done.contains(&c) {
                continue;
            }
            let tmp = loop {
                let t = format!("~leaf-rename-{salt}-{n}.tmp");
                n += 1;
                if taken.insert(rules.key(&t)) {
                    break t;
                }
            };
            let cy = Some(cycle_no);
            cycle_no += 1;
            done.insert(c);
            out.push(Move { row: c, dir: dir.to_path_buf(), from: rows[c].old.clone(), to: tmp.clone(), kind: MoveKind::ToTemp, cycle: cy });
            // (on checked rows every row left here is on a cycle; the guards only stop a misuse
            // from looping)
            let mut k = dependent.get(&c).copied();
            while let Some(i) = k {
                if i == c || !done.insert(i) {
                    break;
                }
                direct(i, &mut out, cy);
                k = dependent.get(&i).copied();
            }
            out.push(Move { row: c, dir: dir.to_path_buf(), from: tmp, to: rows[c].new.clone(), kind: MoveKind::FromTemp, cycle: cy });
        }
    }
    out
}

/// Rows for renaming `pairs` of full paths (old -> new, same folder each), checked. For undo and
/// redo of a bulk rename: the same checks and the same safe order as the batch itself.
pub fn rows_for_pairs(pairs: &[(PathBuf, PathBuf)], existing: &HashMap<PathBuf, Vec<String>>, rules: NameRules) -> Vec<Row> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut rows: Vec<Row> = pairs
        .iter()
        .map(|(a, b)| {
            let dir = a.parent().map(Path::to_path_buf).unwrap_or_default();
            if b.parent() != a.parent() {
                // never happens for a bulk rename; refuse rather than move
                return Row::new(dir, name(a), Err(Problem::InvalidChar("a folder separator".into())));
            }
            Row::new(dir, name(a), Ok(name(b)))
        })
        .collect();
    check(&mut rows, existing, rules);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: NameRules = NameRules::WINDOWS;
    const CS: NameRules = NameRules { case_insensitive: false };

    fn dir() -> PathBuf {
        PathBuf::from(r"C:\Users\user\Pictures")
    }
    fn rows(pairs: &[(&str, &str)]) -> Vec<Row> {
        pairs.iter().map(|(a, b)| Row::new(dir(), *a, Ok(b.to_string()))).collect()
    }
    fn listing(names: &[&str]) -> HashMap<PathBuf, Vec<String>> {
        HashMap::from([(dir(), names.iter().map(|s| s.to_string()).collect())])
    }
    fn checked(pairs: &[(&str, &str)], others: &[&str], rules: NameRules) -> Vec<Row> {
        let mut r = rows(pairs);
        let mut all: Vec<&str> = pairs.iter().map(|(a, _)| *a).collect();
        all.extend_from_slice(others);
        check(&mut r, &listing(&all), rules);
        r
    }
    fn statuses(r: &[Row]) -> Vec<Status> {
        r.iter().map(|r| r.status.clone()).collect()
    }

    /// Plays the moves on a set of names: no move may start from a missing name or land on a
    /// taken one; returns the final names.
    fn simulate(start: &[&str], moves: &[Move], rules: NameRules) -> Vec<String> {
        let mut fs: Vec<String> = start.iter().map(|s| s.to_string()).collect();
        for m in moves {
            let at = fs.iter().position(|n| *n == m.from).unwrap_or_else(|| panic!("{} is not there", m.from));
            let case_only = rules.key(&m.from) == rules.key(&m.to);
            assert!(case_only || !fs.iter().any(|n| rules.key(n) == rules.key(&m.to)), "{} would be overwritten", m.to);
            fs[at] = m.to.clone();
        }
        fs.sort();
        fs
    }
    fn sorted(v: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn statuses_of_plain_rows() {
        let r = checked(&[("a.txt", "b.txt"), ("c.txt", "c.txt"), ("d.txt", "D.txt")], &[], W);
        assert_eq!(statuses(&r), vec![Status::Rename, Status::Unchanged, Status::CaseOnly]);
        // case-sensitive: a case change is a plain rename
        let r = checked(&[("d.txt", "D.txt")], &[], CS);
        assert_eq!(statuses(&r), vec![Status::Rename]);
    }

    #[test]
    fn every_name_problem() {
        let p = |n: &str| name_problem(n);
        assert_eq!(p(""), Some(Problem::Empty));
        assert_eq!(p("   "), Some(Problem::Empty));
        for c in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
            assert_eq!(p(&format!("a{c}b")), Some(Problem::InvalidChar(format!("'{c}'"))), "{c}");
        }
        assert_eq!(p("a\u{7}b"), Some(Problem::InvalidChar("a control character".into())));
        assert_eq!(p("a."), Some(Problem::TrailingDotSpace));
        assert_eq!(p("a "), Some(Problem::TrailingDotSpace));
        for r in ["CON", "con", "Nul.txt", "COM1", "lpt9.tar.gz", "aux .txt"] {
            assert!(matches!(p(r), Some(Problem::Reserved(_))), "{r}");
        }
        for ok in ["console", "COM10", "nul_", "a.con", ".gitignore", "ünïcödé 名前.txt", "x"] {
            assert_eq!(p(ok), None, "{ok}");
        }
        assert_eq!(p(&"a".repeat(255)), None);
        assert_eq!(p(&"a".repeat(256)), Some(Problem::TooLong(256)));
        // UTF-16 units count, not bytes: 255 CJK letters are fine, an emoji is two units
        assert_eq!(p(&"名".repeat(255)), None);
        assert_eq!(p(&"😀".repeat(128)), Some(Problem::TooLong(256)));
        let r = checked(&[("a", "a:b"), ("b", "")], &[], W);
        assert!(matches!(r[0].status, Status::Problem(Problem::InvalidChar(_))));
        assert_eq!(r[1].status, Status::Problem(Problem::Empty));
    }

    #[test]
    fn collisions_inside_the_batch() {
        let r = checked(&[("a", "x"), ("b", "x"), ("c", "y")], &[], W);
        assert_eq!(r[0].status, Status::Problem(Problem::Duplicate("b".into())));
        assert_eq!(r[1].status, Status::Problem(Problem::Duplicate("a".into())));
        assert_eq!(r[2].status, Status::Rename);
        // only by letter case: the same name on Windows, two names on a case-sensitive system
        let r = checked(&[("a", "X"), ("b", "x")], &[], W);
        assert!(r.iter().all(|r| matches!(r.status, Status::Problem(Problem::Duplicate(_)))));
        let r = checked(&[("a", "X"), ("b", "x")], &[], CS);
        assert!(r.iter().all(|r| r.status == Status::Rename));
        // onto an item of the batch that keeps its name
        let r = checked(&[("a", "b"), ("b", "b")], &[], W);
        assert_eq!(r[0].status, Status::Problem(Problem::Duplicate("b".into())));
        assert_eq!(r[1].status, Status::Unchanged);
        // the same item twice
        let r = checked(&[("a", "x"), ("A", "y")], &[], W);
        assert_eq!(r[1].status, Status::Problem(Problem::Twice));
    }

    #[test]
    fn a_problem_item_keeps_its_name_and_that_name_stays_taken() {
        // b cannot be renamed (bad name), so b stays; a -> b must not run, and c -> a then not either
        let r = checked(&[("c", "a"), ("a", "b"), ("b", "b?")], &[], W);
        assert!(matches!(r[2].status, Status::Problem(Problem::InvalidChar(_))));
        assert_eq!(r[1].status, Status::Problem(Problem::Duplicate("b".into())));
        assert_eq!(r[0].status, Status::Problem(Problem::Duplicate("a".into())));
        assert_eq!(Summary::of(&r), Summary { rename: 0, unchanged: 0, collisions: 2, other_problems: 1 });
        assert!(!Summary::of(&r).can_run(true));
    }

    #[test]
    fn collision_with_files_outside_the_batch() {
        let r = checked(&[("a", "keep.txt"), ("b", "B2")], &["KEEP.TXT", "other"], W);
        assert_eq!(r[0].status, Status::Problem(Problem::Exists("KEEP.TXT".into())));
        assert_eq!(r[1].status, Status::Rename);
        // case-sensitive: KEEP.TXT is another name
        let r = checked(&[("a", "keep.txt")], &["KEEP.TXT"], CS);
        assert_eq!(r[0].status, Status::Rename);
        // a name another batch member frees is not an "existing" file
        let r = checked(&[("a", "b"), ("b", "c")], &["z"], W);
        assert_eq!(statuses(&r), vec![Status::Rename, Status::Rename]);
        // another folder's files do not count
        let mut r = rows(&[("a", "x")]);
        check(&mut r, &HashMap::from([(PathBuf::from(r"C:\Users\user\Other"), vec!["x".to_string()]), (dir(), vec!["a".to_string()])]), W);
        assert_eq!(r[0].status, Status::Rename);
    }

    #[test]
    fn summary_and_can_run() {
        let r = checked(&[("a", "x"), ("b", "b"), ("c", "x"), ("d", "e?"), ("f", "g")], &[], W);
        let s = Summary::of(&r);
        assert_eq!(s, Summary { rename: 1, unchanged: 1, collisions: 2, other_problems: 1 });
        assert!(!s.can_run(false));
        assert!(s.can_run(true));
        assert!(!Summary::of(&checked(&[("a", "a")], &[], W)).can_run(true), "nothing to do");
    }

    #[test]
    fn chains_run_back_to_front_without_temporary_names() {
        let pairs = [("a", "b"), ("b", "c"), ("c", "d")];
        let r = checked(&pairs, &[], W);
        let m = order(&r, &listing(&["a", "b", "c"]), W, "t");
        assert!(m.iter().all(|m| m.kind == MoveKind::Direct));
        assert_eq!(m.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(simulate(&["a", "b", "c"], &m, W), sorted(&["b", "c", "d"]));
    }

    #[test]
    fn two_cycle_swap() {
        let r = checked(&[("a", "b"), ("b", "a")], &[], W);
        let m = order(&r, &listing(&["a", "b"]), W, "t");
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].kind, m[1].kind, m[2].kind), (MoveKind::ToTemp, MoveKind::Direct, MoveKind::FromTemp));
        assert_eq!(m[0].to, "~leaf-rename-t-0.tmp");
        assert!(m.iter().all(|m| m.cycle == Some(0)));
        assert_eq!(simulate(&["a", "b"], &m, W), sorted(&["a", "b"]));
        // and the contents really swapped places: track them
        let mut fs: Vec<(String, &str)> = vec![("a".into(), "A-content"), ("b".into(), "B-content")];
        for mv in &m {
            let at = fs.iter().position(|(n, _)| *n == mv.from).unwrap();
            fs[at].0 = mv.to.clone();
        }
        assert!(fs.contains(&("b".into(), "A-content")) && fs.contains(&("a".into(), "B-content")));
    }

    #[test]
    fn three_cycle_and_chain_into_a_freed_name() {
        // a->b->c->a is a cycle; d->e and x->d is a chain (x takes the name d frees)
        let pairs = [("a", "b"), ("b", "c"), ("c", "a"), ("x", "d"), ("d", "e")];
        let r = checked(&pairs, &[], W);
        let start = ["a", "b", "c", "d", "x"];
        let m = order(&r, &listing(&start), W, "s");
        assert_eq!(m.iter().filter(|m| m.kind == MoveKind::ToTemp).count(), 1, "one temporary name per cycle");
        assert_eq!(m.len(), 6);
        assert_eq!(simulate(&start, &m, W), sorted(&["a", "b", "c", "d", "e"]));
        let d_at = m.iter().position(|m| m.from == "d").unwrap();
        let x_at = m.iter().position(|m| m.from == "x").unwrap();
        assert!(d_at < x_at);
        // several cycles in one batch get separate temporary names
        let r = checked(&[("1", "2"), ("2", "1"), ("3", "4"), ("4", "3")], &[], W);
        let m = order(&r, &listing(&["1", "2", "3", "4"]), W, "s");
        let temps: HashSet<&str> = m.iter().filter(|m| m.kind == MoveKind::ToTemp).map(|m| m.to.as_str()).collect();
        assert_eq!(temps.len(), 2);
        assert_eq!(simulate(&["1", "2", "3", "4"], &m, W), sorted(&["1", "2", "3", "4"]));
    }

    #[test]
    fn temporary_names_avoid_every_name_in_the_folder_and_batch() {
        let r = checked(&[("a", "b"), ("b", "a")], &["~leaf-rename-t-0.tmp", "~LEAF-RENAME-T-1.TMP"], W);
        let m = order(&r, &listing(&["a", "b", "~leaf-rename-t-0.tmp", "~LEAF-RENAME-T-1.TMP"]), W, "t");
        assert_eq!(m[0].to, "~leaf-rename-t-2.tmp");
    }

    #[test]
    fn case_only_renames_are_direct_and_mix_with_swaps() {
        // a.txt -> A.txt (case only) while b <-> c swap and B.TXT does not exist
        let r = checked(&[("a.txt", "A.txt"), ("b", "c"), ("c", "b")], &[], W);
        assert_eq!(r[0].status, Status::CaseOnly);
        let m = order(&r, &listing(&["a.txt", "b", "c"]), W, "t");
        let case = m.iter().find(|m| m.row == 0).unwrap();
        assert_eq!((case.kind, case.to.as_str()), (MoveKind::Direct, "A.txt"));
        assert_eq!(simulate(&["a.txt", "b", "c"], &m, W), sorted(&["A.txt", "b", "c"]));
        // a case swap: "a"->"B" and "b"->"A" is a cycle on Windows
        let r = checked(&[("a", "B"), ("b", "A")], &[], W);
        assert_eq!(statuses(&r), vec![Status::Rename, Status::Rename]);
        let m = order(&r, &listing(&["a", "b"]), W, "t");
        assert_eq!(m.iter().filter(|m| m.kind == MoveKind::ToTemp).count(), 1);
        assert_eq!(simulate(&["a", "b"], &m, W), sorted(&["A", "B"]));
    }

    #[test]
    fn rows_with_problems_are_left_out_of_the_moves() {
        let r = checked(&[("a", "x"), ("b", "x"), ("c", "d"), ("e", "f?")], &[], W);
        let m = order(&r, &listing(&["a", "b", "c", "e"]), W, "t");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].from, "c");
    }

    #[test]
    fn folders_are_planned_apart() {
        let d2 = PathBuf::from(r"C:\Users\user\Other");
        let mut r = vec![Row::new(dir(), "a", Ok("b".into())), Row::new(d2.clone(), "b", Ok("a".into())), Row::new(d2.clone(), "a", Ok("b".into()))];
        let ex = HashMap::from([(dir(), vec!["a".to_string()]), (d2.clone(), vec!["a".to_string(), "b".to_string()])]);
        check(&mut r, &ex, W);
        assert!(r.iter().all(|r| r.status == Status::Rename), "{r:?}");
        let m = order(&r, &ex, W, "t");
        assert_eq!(m.iter().filter(|m| m.dir == dir()).count(), 1);
        assert_eq!(m.iter().filter(|m| m.dir == d2 && m.kind == MoveKind::ToTemp).count(), 1);
    }

    #[test]
    fn unicode_names_and_long_names() {
        let long = "x".repeat(250) + ".txt"; // 254
        let r = checked(&[("İstanbul.txt", "istanbul.txt"), ("ß.txt", "SS.txt"), ("a.txt", &long), ("b.txt", &("y".repeat(253) + ".txt"))], &[], W);
        // Rust's lowercase of İ is "i̇", so this is a rename, not case-only (NTFS agrees: İ and i differ)
        assert_eq!(r[0].status, Status::Rename);
        assert_eq!(r[1].status, Status::Rename);
        assert_eq!(r[2].status, Status::Rename);
        assert_eq!(r[3].status, Status::Problem(Problem::TooLong(257)));
        let r = checked(&[("ÄRGER.txt", "ärger.txt")], &[], W);
        assert_eq!(r[0].status, Status::CaseOnly);
    }

    #[test]
    fn pairs_for_undo_are_checked_like_a_batch() {
        let d = dir();
        let pairs = vec![(d.join("b"), d.join("a")), (d.join("a"), d.join("b"))];
        let ex = listing(&["a", "b"]);
        let r = rows_for_pairs(&pairs, &ex, W);
        assert!(r.iter().all(|r| r.status == Status::Rename));
        let m = order(&r, &ex, W, "u");
        assert_eq!(simulate(&["a", "b"], &m, W), sorted(&["a", "b"]));
        // a name that came back meanwhile is a problem, not an overwrite
        let r = rows_for_pairs(&[(d.join("x"), d.join("y"))], &listing(&["x", "y"]), W);
        assert_eq!(r[0].status, Status::Problem(Problem::Exists("y".into())));
        let r = rows_for_pairs(&[(d.join("x"), PathBuf::from(r"C:\Users\user\Other\x"))], &listing(&["x"]), W);
        assert!(matches!(r[0].status, Status::Problem(_)));
    }
}
