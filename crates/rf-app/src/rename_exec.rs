//! Runs a bulk rename on disk: the moves of [`rf_core::rename_plan::order`], one by one, each
//! with a rename that fails instead of replacing. No window needed (the dialog, undo / redo and
//! the tests all come through here).
//!
//! What happens on a failure:
//! * a direct move fails: the item keeps its name; a later move that wanted that name is not
//!   tried (reported as "still taken"), and so on down the chain;
//! * the move to a temporary name fails: the item keeps its name, its cycle stops there;
//! * the move from the temporary name fails: the cycle is rolled back (its finished moves in
//!   reverse, then the temporary name back to the original name). If even that fails, the
//!   item's temporary name is reported, never hidden.
//!
//! [`Outcome::done`] lists exactly the items that have their new names (for undo and tags),
//! [`Outcome::failed`] every item that does not, with the reason.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rf_core::rename_plan::{self, Move, MoveKind, NameRules, Row, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The item, under its original name.
    pub path: PathBuf,
    /// The name it was meant to get.
    pub to: String,
    pub reason: String,
    /// The item could not be put back and is now under this temporary name.
    pub left_at: Option<PathBuf>,
}

impl Failure {
    pub fn line(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match &self.left_at {
            Some(t) => format!("{name} → {}: {} (the file is now named {})", self.to, self.reason, t.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
            None => format!("{name} → {}: {}", self.to, self.reason),
        }
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// (original path, new path) of every item that has its new name.
    pub done: Vec<(PathBuf, PathBuf)>,
    pub failed: Vec<Failure>,
}

/// The rename every move uses: fails when the target exists (the existing safe rename of leaf).
pub fn native_rename(from: &Path, to: &Path) -> std::io::Result<()> {
    rf_win::fileops::rename_no_replace(from, to)
}

/// Every name in each folder now (what the plan checks collisions against). A folder that
/// cannot be listed gives no names; the moves still never replace anything.
pub fn listings<'a>(dirs: impl IntoIterator<Item = &'a Path>) -> HashMap<PathBuf, Vec<String>> {
    let mut out = HashMap::new();
    for d in dirs {
        if out.contains_key(d) {
            continue;
        }
        let names = std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        out.insert(d.to_path_buf(), names);
    }
    out
}

/// Part of the temporary names: this process and this moment.
pub fn salt() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{}-{}", std::process::id(), ms % 100_000_000)
}

/// Runs `moves` (made by `order` from `rows`).
pub fn run(rows: &[Row], moves: &[Move], rules: NameRules, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Outcome {
    let key = |dir: &Path, n: &str| (dir.to_path_buf(), rules.key(n));
    // names that an item still holds because its own move failed or was not tried
    let mut blocked: HashSet<(PathBuf, String)> = HashSet::new();
    let mut at_temp: HashMap<usize, PathBuf> = HashMap::new();
    let mut finished = vec![false; rows.len()];
    let mut failed: HashMap<usize, Failure> = HashMap::new();
    let mut cycle_done: HashMap<usize, Vec<usize>> = HashMap::new();
    let fail = |failed: &mut HashMap<usize, Failure>, row: usize, reason: String, left_at: Option<PathBuf>| {
        let r = &rows[row];
        failed.insert(row, Failure { path: r.dir.join(&r.old), to: r.new.clone(), reason, left_at });
    };
    for (mi, m) in moves.iter().enumerate() {
        let (from, to) = (m.dir.join(&m.from), m.dir.join(&m.to));
        match m.kind {
            MoveKind::Direct => {
                if blocked.contains(&key(&m.dir, &m.to)) {
                    fail(&mut failed, m.row, format!("not renamed: \"{}\" is still taken because an earlier rename failed", m.to), None);
                    blocked.insert(key(&m.dir, &m.from));
                    continue;
                }
                match rename(&from, &to) {
                    Ok(()) => {
                        finished[m.row] = true;
                        if let Some(c) = m.cycle {
                            cycle_done.entry(c).or_default().push(mi);
                        }
                    }
                    Err(e) => {
                        fail(&mut failed, m.row, e.to_string(), None);
                        blocked.insert(key(&m.dir, &m.from));
                    }
                }
            }
            MoveKind::ToTemp => match rename(&from, &to) {
                Ok(()) => {
                    at_temp.insert(m.row, to);
                }
                Err(e) => {
                    fail(&mut failed, m.row, e.to_string(), None);
                    blocked.insert(key(&m.dir, &m.from));
                }
            },
            MoveKind::FromTemp => {
                // (no temporary file: the move there failed and was reported)
                let Some(tmp) = at_temp.remove(&m.row) else { continue };
                let r = if blocked.contains(&key(&m.dir, &m.to)) {
                    Err(std::io::Error::other(format!("\"{}\" is still taken because an earlier rename failed", m.to)))
                } else {
                    rename(&tmp, &to)
                };
                if let Err(e) = r {
                    // roll the cycle back: its finished moves in reverse, then the original name
                    let mut back_ok = true;
                    for &dm in m.cycle.and_then(|c| cycle_done.get(&c)).map(Vec::as_slice).unwrap_or(&[]).iter().rev() {
                        let d = &moves[dm];
                        if rename(&d.dir.join(&d.to), &d.dir.join(&d.from)).is_ok() {
                            finished[d.row] = false;
                            fail(&mut failed, d.row, "put back: a swap it is part of could not be completed".into(), None);
                        } else {
                            back_ok = false;
                            break;
                        }
                    }
                    let original = m.dir.join(&rows[m.row].old);
                    if back_ok && rename(&tmp, &original).is_ok() {
                        fail(&mut failed, m.row, e.to_string(), None);
                    } else {
                        fail(&mut failed, m.row, e.to_string(), Some(tmp));
                    }
                } else {
                    finished[m.row] = true;
                }
            }
        }
    }
    let mut out = Outcome::default();
    for (i, r) in rows.iter().enumerate() {
        if finished[i] {
            out.done.push((r.dir.join(&r.old), r.dir.join(&r.new)));
        } else if let Some(f) = failed.remove(&i) {
            out.failed.push(f);
        }
    }
    out
}

/// Runs a batch the dialog previewed (`preview`, rows after [`rename_plan::check`]): lists the
/// folders again, checks again, and runs what may still run. `Err` (nothing was renamed) when
/// the batch can no longer run as a whole, e.g. a new collision while "skip items with
/// problems" is off.
///
/// A row that the preview showed as renamed but that is a problem now (another program took
/// its new name meanwhile, say) is not renamed and is listed in [`Outcome::failed`] with the
/// reason: it is never left out silently.
pub fn apply_batch(preview: &[Row], skip_problems: bool, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Result<Outcome, String> {
    let rules = NameRules::native();
    // the folders as they are now: something may have changed since the preview
    let existing = listings(preview.iter().map(|r| r.dir.as_path()));
    let mut rows = preview.to_vec();
    rename_plan::check(&mut rows, &existing, rules);
    if !rename_plan::Summary::of(&rows).can_run(skip_problems) {
        return Err(crate::t!("The folder changed since the preview: check the list and try again").to_string());
    }
    let moves = rename_plan::order(&rows, &existing, rules, &salt());
    let mut out = run(&rows, &moves, rules, rename);
    for (before, now) in preview.iter().zip(&rows) {
        if before.status.runs()
            && let Status::Problem(p) = &now.status
        {
            out.failed.push(Failure { path: now.dir.join(&now.old), to: now.new.clone(), reason: format!("{} {p}", crate::t!("Not renamed, the folder changed since the preview:")), left_at: None });
        }
    }
    Ok(out)
}

/// Renames (old path -> new path) pairs within their folders with the same checks and the same
/// safe order as a batch: undo (the pairs reversed) and redo of a bulk rename. A pair that can
/// no longer be done (the file is gone, the name is taken) is reported, the rest still run.
pub fn run_pairs(pairs: &[(PathBuf, PathBuf)], rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Outcome {
    let rules = NameRules::native();
    let existing = listings(pairs.iter().filter_map(|(a, _)| a.parent()));
    let mut rows = rename_plan::rows_for_pairs(pairs, &existing, rules);
    // a file that is not there any more (its name is then free, so nothing collides with it)
    for r in rows.iter_mut() {
        if r.status.runs() && !existing.get(&r.dir).is_some_and(|names| names.iter().any(|n| rules.key(n) == rules.key(&r.old))) {
            r.status = Status::Problem(rename_plan::Problem::Missing);
        }
    }
    let moves = rename_plan::order(&rows, &existing, rules, &salt());
    let mut out = run(&rows, &moves, rules, rename);
    for r in &rows {
        if let Status::Problem(p) = &r.status {
            out.failed.push(Failure { path: r.dir.join(&r.old), to: r.new.clone(), reason: p.to_string(), left_at: None });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! On throwaway files under the workspace's `target` folder, through the real rename.
    use super::*;
    use rf_core::organize::{Condition, FileFact, FileKind, NameOp};
    use rf_core::rename::RenameItem;
    use rf_core::rename_plan::{Summary, check, order};
    use rf_core::rename_rules::{self, Op, Part, RuleStep};

    const RULES: NameRules = NameRules::WINDOWS;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new(tag: &str) -> Sandbox {
            let d = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().join("target").join(format!("rename-exec-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Sandbox(d)
        }
        fn write(&self, name: &str, content: &[u8]) {
            std::fs::write(self.0.join(name), content).unwrap();
        }
        /// name -> content hash, every file of the folder.
        fn state(&self) -> std::collections::BTreeMap<String, u64> {
            std::fs::read_dir(&self.0).unwrap().flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), fnv(&std::fs::read(e.path()).unwrap()))).collect()
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fnv(b: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for x in b {
            h ^= *x as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// `n` files `f000.txt`… with distinct contents, plus `keep_0.txt`… that are not in the batch.
    fn fill(s: &Sandbox, n: usize, keep: usize) {
        for i in 0..n {
            s.write(&format!("f{i:03}.txt"), format!("content of file {i} / {}", "x".repeat(i % 37)).as_bytes());
        }
        for i in 0..keep {
            s.write(&format!("keep_{i}.txt"), format!("not part of the batch {i}").as_bytes());
        }
    }

    fn rows_for(s: &Sandbox, pairs: &[(String, String)]) -> (Vec<Row>, HashMap<PathBuf, Vec<String>>) {
        let ex = listings([s.0.as_path()]);
        let mut rows: Vec<Row> = pairs.iter().map(|(a, b)| Row::new(s.0.clone(), a.clone(), Ok(b.clone()))).collect();
        check(&mut rows, &ex, RULES);
        (rows, ex)
    }

    fn rename_all(rows: &[Row], ex: &HashMap<PathBuf, Vec<String>>, rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>) -> Outcome {
        let moves = order(rows, ex, RULES, &salt());
        run(rows, &moves, RULES, rename)
    }

    fn inverse(done: &[(PathBuf, PathBuf)]) -> Vec<(PathBuf, PathBuf)> {
        done.iter().map(|(a, b)| (b.clone(), a.clone())).collect()
    }

    /// After a run: every content still there exactly once, and where the plan said.
    fn assert_contents(before: &std::collections::BTreeMap<String, u64>, after: &std::collections::BTreeMap<String, u64>, moved: &HashMap<String, String>) {
        let mut b: Vec<u64> = before.values().copied().collect();
        let mut a: Vec<u64> = after.values().copied().collect();
        b.sort();
        a.sort();
        assert_eq!(a, b, "a file's content was lost or duplicated");
        for (old, h) in before {
            let now = moved.get(old).unwrap_or(old);
            assert_eq!(after.get(now), Some(h), "{old} should be {now}");
        }
    }

    fn moved_map(out: &Outcome) -> HashMap<String, String> {
        let n = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        out.done.iter().map(|(a, b)| (n(a), n(b))).collect()
    }

    #[test]
    fn swaps_chains_and_collisions_with_existing_files_on_disk_then_undo() {
        let s = Sandbox::new("swap");
        fill(&s, 300, 5);
        let before = s.state();
        assert_eq!(before.len(), 305);
        // the rule list: reverse the numbers (150 two-way swaps), through the core's rules
        let names: Vec<String> = (0..300).map(|i| format!("f{i:03}.txt")).collect();
        let items: Vec<RenameItem> = names.iter().map(|n| RenameItem::new(n.clone(), false, 0)).collect();
        let facts: Vec<FileFact> = names.iter().map(|n| FileFact::new(s.0.join(n), 1, 1, 1, FileKind::Document)).collect();
        let mut steps = vec![RuleStep::new(Op::Template { template: "f{n:3}".into(), part: Part::Name, start: 299, step: -1, per_folder: false })];
        // ... and f007 / f008 aim at files outside the batch: collisions with existing files
        steps.push(RuleStep::new(Op::Template { template: "keep_1".into(), part: Part::Name, start: 1, step: 1, per_folder: false }).when(Condition::Name { op: NameOp::Wildcard, text: "f007.txt;f008.txt".into() }));
        let ex = listings([s.0.as_path()]);
        let rows = rename_rules::plan(&steps, &items, &facts, &ex, RULES, 0);
        let sum = Summary::of(&rows);
        // f007, f008 collide with keep_1; their partners f292 / f291 would take a name f007 / f008 keep
        assert_eq!(sum.collisions, 4, "{sum:?}");
        assert_eq!(sum.rename, 296);
        assert!(!sum.can_run(false) && sum.can_run(true));
        let out = rename_all(&rows, &ex, &mut native_rename);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.done.len(), 296);
        let after = s.state();
        assert_contents(&before, &after, &moved_map(&out));
        assert_eq!(after.len(), 305, "no temporary file left");
        assert_eq!(after.get("keep_1.txt"), before.get("keep_1.txt"));
        assert_eq!(after.get("f000.txt"), before.get("f299.txt"));
        // undo: the exact original names
        let back = run_pairs(&inverse(&out.done), &mut native_rename);
        assert!(back.failed.is_empty(), "{:?}", back.failed);
        assert_eq!(s.state(), before);
    }

    #[test]
    fn three_cycles_chains_and_case_only_on_disk() {
        let s = Sandbox::new("cycle");
        for n in ["a", "b", "c", "d", "x", "Mixed.TXT", "p", "q"] {
            s.write(n, n.as_bytes());
        }
        let before = s.state();
        let pairs: Vec<(String, String)> = [("a", "b"), ("b", "c"), ("c", "a"), ("x", "d"), ("d", "e"), ("Mixed.TXT", "mixed.txt"), ("p", "Q"), ("q", "P")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let (rows, ex) = rows_for(&s, &pairs);
        assert!(rows.iter().all(|r| r.status.runs()), "{rows:?}");
        let out = rename_all(&rows, &ex, &mut native_rename);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let after = s.state();
        assert_contents(&before, &after, &moved_map(&out));
        // the letter case is what was asked for (not only the same name ignoring case)
        assert!(after.contains_key("mixed.txt") && after.contains_key("P") && after.contains_key("Q"), "{after:?}");
        let back = run_pairs(&inverse(&out.done), &mut native_rename);
        assert!(back.failed.is_empty(), "{:?}", back.failed);
        assert_eq!(s.state(), before);
    }

    #[test]
    fn a_failure_mid_batch_is_reported_exactly_and_undo_restores_everything() {
        let s = Sandbox::new("fail");
        fill(&s, 200, 0);
        let before = s.state();
        // 90 swaps (f000<->f001, ...) and a chain of 20 (f180 -> f181 -> ... -> f199 -> g.txt)
        let mut pairs: Vec<(String, String)> = (0..180).map(|i| (format!("f{i:03}.txt"), format!("f{:03}.txt", i ^ 1))).collect();
        pairs.extend((180..200).map(|i| (format!("f{i:03}.txt"), if i == 199 { "g.txt".to_string() } else { format!("f{:03}.txt", i + 1) })));
        let (rows, ex) = rows_for(&s, &pairs);
        // the first rename from some names fails: every 13th file, and the move out of every 3rd
        // temporary name to its new name (a swap starts at its even file, so that target is odd);
        // the way back (to the even original) works
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut temps = 0usize;
        let mut failing = |a: &Path, b: &Path| {
            let first = seen.insert(a.to_path_buf());
            let name = a.file_name().unwrap().to_string_lossy().into_owned();
            let to = b.file_name().unwrap().to_string_lossy().into_owned();
            let is_temp = name.starts_with("~leaf-rename");
            let forward = is_temp && to[1..4].parse::<usize>().is_ok_and(|i| i % 2 == 1);
            if first && forward {
                temps += 1;
            }
            let fail = first && ((!is_temp && name[1..4].parse::<usize>().is_ok_and(|i| i % 13 == 5)) || (forward && temps % 3 == 0));
            if fail { Err(std::io::Error::other("simulated failure")) } else { native_rename(a, b) }
        };
        let out = rename_all(&rows, &ex, &mut failing);
        eprintln!("mid-batch failure: {} renamed, {} reported as not renamed", out.done.len(), out.failed.len());
        assert!(out.failed.len() > 20, "{}", out.failed.len());
        assert!(out.done.len() > 100, "{}", out.done.len());
        assert_eq!(out.done.len() + out.failed.len(), 200, "every item is either done or reported");
        // the chain runs from its end: f187 fails, so f186 ... f180 are not even tried
        assert!(out.failed.iter().any(|f| f.reason.contains("still taken")), "{:?}", out.failed);
        let after = s.state();
        assert_eq!(after.len(), 200, "no stray temporary file");
        assert!(out.failed.iter().all(|f| f.left_at.is_none()), "every failed swap was put back: {:?}", out.failed);
        // what is reported done really is, what is reported failed really is not
        assert_contents(&before, &after, &moved_map(&out));
        for f in &out.failed {
            let old = f.path.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(after.get(&old), before.get(&old), "{old} was reported as not renamed");
        }
        let back = run_pairs(&inverse(&out.done), &mut native_rename);
        assert!(back.failed.is_empty(), "{:?}", back.failed);
        assert_eq!(s.state(), before);
    }

    #[test]
    fn a_failed_chain_stops_and_nothing_is_overwritten() {
        let s = Sandbox::new("chain");
        for n in ["a", "b", "c"] {
            s.write(n, n.as_bytes());
        }
        let before = s.state();
        let pairs = vec![("a".to_string(), "b".to_string()), ("b".to_string(), "c".to_string()), ("c".to_string(), "d".to_string())];
        let (rows, ex) = rows_for(&s, &pairs);
        // the first move (c -> d) fails: b and a must not even be tried
        let mut tried = Vec::new();
        let mut first_fails = |a: &Path, b: &Path| {
            tried.push(a.file_name().unwrap().to_string_lossy().into_owned());
            if tried.len() == 1 { Err(std::io::Error::other("simulated failure")) } else { native_rename(a, b) }
        };
        let out = rename_all(&rows, &ex, &mut first_fails);
        assert_eq!(tried, ["c"]);
        assert!(out.done.is_empty());
        assert_eq!(out.failed.len(), 3);
        assert_eq!(s.state(), before);
    }

    #[test]
    fn a_failure_at_the_end_of_a_swap_rolls_the_swap_back() {
        let s = Sandbox::new("rollback");
        for n in ["a", "b", "c"] {
            s.write(n, n.as_bytes());
        }
        let before = s.state();
        let pairs = vec![("a".to_string(), "b".to_string()), ("b".to_string(), "c".to_string()), ("c".to_string(), "a".to_string())];
        let (rows, ex) = rows_for(&s, &pairs);
        let moves = order(&rows, &ex, RULES, "t");
        assert_eq!(moves.last().unwrap().kind, MoveKind::FromTemp);
        let last = moves.len();
        let mut calls = 0;
        let mut last_fails = |a: &Path, b: &Path| {
            calls += 1;
            if calls == last { Err(std::io::Error::other("simulated failure")) } else { native_rename(a, b) }
        };
        let out = run(&rows, &moves, RULES, &mut last_fails);
        assert!(out.done.is_empty(), "{:?}", out.done);
        assert_eq!(out.failed.len(), 3);
        assert!(out.failed.iter().all(|f| f.left_at.is_none()));
        assert_eq!(s.state(), before);
        // and when even the way back fails, the temporary name is reported, not hidden
        let mut calls = 0;
        let mut all_fail_after = |a: &Path, b: &Path| {
            calls += 1;
            if calls >= last { Err(std::io::Error::other("simulated failure")) } else { native_rename(a, b) }
        };
        let out = run(&rows, &moves, RULES, &mut all_fail_after);
        let stranded: Vec<&Failure> = out.failed.iter().filter(|f| f.left_at.is_some()).collect();
        assert_eq!(stranded.len(), 1);
        let st = s.state();
        assert!(st.contains_key(stranded[0].left_at.as_ref().unwrap().file_name().unwrap().to_str().unwrap()));
        let mut a: Vec<u64> = st.values().copied().collect();
        let mut b: Vec<u64> = before.values().copied().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "no content lost");
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_file_is_reported_and_the_rest_is_renamed() {
        use std::os::windows::fs::OpenOptionsExt;
        let s = Sandbox::new("locked");
        fill(&s, 50, 0);
        let before = s.state();
        let pairs: Vec<(String, String)> = (0..50).map(|i| (format!("f{i:03}.txt"), format!("g{i:03}.txt"))).collect();
        let (rows, ex) = rows_for(&s, &pairs);
        // no sharing at all: the file cannot be renamed while this handle is open
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(s.0.join("f010.txt")).unwrap();
        let out = rename_all(&rows, &ex, &mut native_rename);
        drop(lock);
        assert_eq!(out.done.len(), 49);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].path.file_name().unwrap(), "f010.txt");
        let after = s.state();
        assert_contents(&before, &after, &moved_map(&out));
        let back = run_pairs(&inverse(&out.done), &mut native_rename);
        assert!(back.failed.is_empty(), "{:?}", back.failed);
        assert_eq!(s.state(), before);
    }

    #[test]
    fn a_row_that_becomes_a_problem_before_apply_is_reported_not_skipped_silently() {
        let s = Sandbox::new("recheck");
        for n in ["a", "b", "c"] {
            s.write(n, n.as_bytes());
        }
        // preview: a -> x and b -> y would run, c has a problem ("skip items with problems" is on)
        let pairs: Vec<(String, String)> = [("a", "x"), ("b", "y"), ("c", "c?")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let (preview, _) = rows_for(&s, &pairs);
        assert_eq!(Summary::of(&preview).rename, 2);
        assert!(Summary::of(&preview).can_run(true));
        // then another program creates "x"
        s.write("x", b"someone else's x");
        let before = s.state();
        let out = apply_batch(&preview, true, &mut native_rename).unwrap();
        assert_eq!(moved_map(&out), HashMap::from([("b".to_string(), "y".to_string())]));
        assert_eq!(out.failed.len(), 1, "{:?}", out.failed);
        assert_eq!(out.failed[0].path, s.0.join("a"));
        assert!(out.failed[0].reason.contains("already exists"), "{:?}", out.failed);
        let after = s.state();
        assert_contents(&before, &after, &moved_map(&out));
        assert_eq!(after.get("x"), before.get("x"), "the other program's file is untouched");
        // without "skip items with problems" the whole batch is refused and nothing moves
        let s = Sandbox::new("recheck2");
        for n in ["a", "b"] {
            s.write(n, n.as_bytes());
        }
        let (preview, _) = rows_for(&s, &[("a".to_string(), "x".to_string()), ("b".to_string(), "y".to_string())]);
        assert!(Summary::of(&preview).can_run(false));
        s.write("x", b"new");
        let before = s.state();
        assert!(apply_batch(&preview, false, &mut native_rename).is_err());
        assert_eq!(s.state(), before);
    }

    #[test]
    fn undo_reports_what_cannot_be_put_back() {
        let s = Sandbox::new("undo");
        for n in ["a", "b", "z"] {
            s.write(n, n.as_bytes());
        }
        // a -> x was done; meanwhile someone made a new "a" and deleted b's result
        let d = &s.0;
        let out = run_pairs(&[(d.join("z"), d.join("a")), (d.join("gone"), d.join("b2"))], &mut native_rename);
        assert!(out.done.is_empty());
        assert_eq!(out.failed.len(), 2);
        assert!(out.failed.iter().any(|f| f.reason.contains("already exists")), "{:?}", out.failed);
        assert!(out.failed.iter().any(|f| f.reason.contains("not there")), "{:?}", out.failed);
        assert_eq!(std::fs::read(d.join("a")).unwrap(), b"a");
        assert_eq!(std::fs::read(d.join("z")).unwrap(), b"z");
    }
}
