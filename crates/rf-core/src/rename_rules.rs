//! Bulk rename with logic: an ordered list of steps, each an operation with an optional
//! condition (the folder organizer's [`Condition`], evaluated on the item's [`FileFact`]).
//! Pure: the caller collects the facts and the folder listings, [`plan`] computes every new
//! name and checks it ([`crate::rename_plan`]).
//!
//! Each step works on the name the previous steps made. A condition looks at the file as it is
//! on disk (its current name, extension, size, dates), not at the name being built, so the
//! steps "left -> right if the name has left" and "right -> left if the name has right" swap.
//! A step whose condition does not hold leaves the name as it is for that item.
//!
//! Template tokens: `{name}` (the name without extension, as built so far), `{ext}` (the
//! extension without the dot), `{parent}` (the folder's name), `{n}` / `{n:3}` (the counter,
//! zero-padded to 3), `{modified}` / `{modified:%Y-%m-%d}`, `{created:...}`, `{taken:...}`
//! (the date a photo was taken, from EXIF, when the caller supplies it), `{size}` (bytes),
//! `{date}` (the modified date as YYYY-MM-DD, as in the pattern mode). An unknown token is a
//! problem, not literal text: a typo must not end up in a few thousand file names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::organize::{Condition, FileFact};
use crate::rename::{CaseKind, Mode, RenameItem, RenameSpec};
use crate::rename_plan::{self, NameRules, Problem, Row};

/// Which part of the name an operation works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// The name without the extension.
    #[default]
    Name,
    /// The extension (without the dot). A folder has none.
    Ext,
    /// The whole name, extension included.
    Whole,
}

/// Where text goes: at the start, at the end, or before the character at an index (counted
/// from 0; a negative index counts from the end, -1 = before the last character).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum At {
    Start,
    End,
    Index(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    Lower,
    Upper,
    /// Every word starts with a capital letter.
    Title,
    /// Only the first letter is a capital.
    Sentence,
}

fn yes() -> bool {
    true
}
fn one() -> i64 {
    1
}
fn space() -> String {
    " ".into()
}
fn start() -> At {
    At::Start
}
fn end() -> At {
    At::End
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Find and replace (plain text or a regular expression with `$1` groups).
    Replace {
        find: String,
        #[serde(default)]
        with: String,
        #[serde(default)]
        regex: bool,
        #[serde(default)]
        match_case: bool,
        /// Every occurrence (else only the first).
        #[serde(default = "yes")]
        all: bool,
        #[serde(default)]
        part: Part,
    },
    Insert {
        text: String,
        #[serde(default = "end")]
        at: At,
        #[serde(default)]
        part: Part,
    },
    /// Removes `count` characters starting at `from` (a negative `from` counts from the end).
    RemoveRange {
        from: i32,
        count: u32,
        #[serde(default)]
        part: Part,
    },
    /// Removes every match of a pattern.
    RemovePattern {
        pattern: String,
        #[serde(default)]
        regex: bool,
        #[serde(default)]
        match_case: bool,
        #[serde(default)]
        part: Part,
    },
    Case {
        case: Case,
        #[serde(default)]
        part: Part,
    },
    /// A counter in the name without the extension.
    Number {
        #[serde(default = "one")]
        start: i64,
        #[serde(default = "one")]
        step: i64,
        /// Zero padding: 3 makes 001.
        #[serde(default)]
        pad: u8,
        #[serde(default = "start")]
        at: At,
        /// Between the number and the name (at the start or the end only).
        #[serde(default = "space")]
        sep: String,
        /// The counter starts again in every folder.
        #[serde(default)]
        per_folder: bool,
    },
    /// Trims spaces at both ends and/or turns every run of white space into one space.
    Whitespace {
        #[serde(default = "yes")]
        trim: bool,
        #[serde(default = "yes")]
        collapse: bool,
        #[serde(default)]
        part: Part,
    },
    /// A new extension ("" removes it). Folders keep their names.
    Extension { ext: String },
    /// A name built from tokens. With `part = Name` it makes the name without the extension and
    /// the extension stays; with `Whole` it makes the whole name.
    Template {
        template: String,
        #[serde(default)]
        part: Part,
        #[serde(default = "one")]
        start: i64,
        #[serde(default = "one")]
        step: i64,
        #[serde(default)]
        per_folder: bool,
    },
}

/// One step of a rule list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleStep {
    #[serde(flatten)]
    pub op: Op,
    /// Only items for which this holds; `None`: every item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Condition>,
}

impl RuleStep {
    pub fn new(op: Op) -> Self {
        Self { op, when: None }
    }
    pub fn when(mut self, c: Condition) -> Self {
        self.when = Some(c);
        self
    }
}

/// A named rule list (built in, or saved by the user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<RuleStep>,
}

/// The presets that come with leaf.
pub fn builtin_presets() -> Vec<Preset> {
    let p = |name: &str, steps: Vec<RuleStep>| Preset { name: name.into(), steps };
    vec![
        p(
            "Date prefix from modified time",
            vec![RuleStep::new(Op::Template { template: "{modified:%Y-%m-%d} {name}".into(), part: Part::Name, start: 1, step: 1, per_folder: false })],
        ),
        p("Lowercase extension", vec![RuleStep::new(Op::Case { case: Case::Lower, part: Part::Ext })]),
        p("Number files 001…", vec![RuleStep::new(Op::Number { start: 1, step: 1, pad: 3, at: At::Start, sep: " ".into(), per_folder: false })]),
        p(
            "Spaces to underscores",
            vec![RuleStep::new(Op::Replace { find: " ".into(), with: "_".into(), regex: false, match_case: true, all: true, part: Part::Name })],
        ),
        p("Tidy white space", vec![RuleStep::new(Op::Whitespace { trim: true, collapse: true, part: Part::Name })]),
    ]
}

/// The four modes of the classic bulk rename as rule steps. Same results as
/// [`crate::rename::compute`] except: a `Pattern` with "rename the extension too" expands
/// `{name}` to the name without the extension here (the classic mode gives the whole name), and
/// an unknown token is a problem here (the classic mode keeps it as text).
pub fn steps_for_mode(spec: &RenameSpec) -> Vec<RuleStep> {
    let part = if spec.include_ext { Part::Whole } else { Part::Name };
    match &spec.mode {
        Mode::FindReplace { find, replace, case_insensitive, regex } => {
            if find.is_empty() {
                return Vec::new();
            }
            vec![RuleStep::new(Op::Replace { find: find.clone(), with: replace.clone(), regex: *regex, match_case: !case_insensitive, all: true, part })]
        }
        Mode::Pattern { pattern, start, step } => vec![RuleStep::new(Op::Template { template: pattern.clone(), part, start: *start, step: *step, per_folder: false })],
        Mode::Case(k) => {
            let case = match k {
                CaseKind::Lower => Case::Lower,
                CaseKind::Upper => Case::Upper,
                CaseKind::Title => Case::Title,
            };
            vec![RuleStep::new(Op::Case { case, part })]
        }
        // one step, not two inserts: a prefix with a dot (or ".env" + prefix) would make an
        // extension the second insert then works around
        Mode::PrefixSuffix { prefix, suffix } => {
            let lit = |s: &str| s.replace('$', "$$");
            vec![RuleStep::new(Op::Replace { find: "(?s)^(.*)$".into(), with: format!("{}${{1}}{}", lit(prefix), lit(suffix)), regex: true, match_case: true, all: false, part })]
        }
    }
}

// ------------------------------------------------------------------ name parts

/// (name without extension, extension without the dot). `.gitignore` and `name.` have no
/// extension, a folder never has one (as in the folder listing and the organizer).
pub fn split(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn join(stem: &str, ext: &str) -> String {
    if ext.is_empty() { stem.to_string() } else { format!("{stem}.{ext}") }
}

/// Applies `f` to one part of `name` and puts the name back together.
fn edit(name: &str, is_dir: bool, part: Part, f: impl FnOnce(&str) -> String) -> String {
    let (stem, ext) = split(name, is_dir);
    match part {
        Part::Name => join(&f(stem), ext),
        Part::Ext if is_dir => name.to_string(),
        Part::Ext => join(stem, &f(ext)),
        Part::Whole => f(name),
    }
}

/// The byte offset of character `at` (negative: from the end), clamped to the text.
fn char_offset(s: &str, at: i32) -> usize {
    let n = s.chars().count() as i64;
    let i = if at < 0 { (n + at as i64).max(0) } else { (at as i64).min(n) };
    s.char_indices().nth(i as usize).map(|(b, _)| b).unwrap_or(s.len())
}

fn insert_at(s: &str, text: &str, at: At) -> String {
    match at {
        At::Start => format!("{text}{s}"),
        At::End => format!("{s}{text}"),
        At::Index(i) => {
            let b = char_offset(s, i);
            format!("{}{text}{}", &s[..b], &s[b..])
        }
    }
}

fn remove_range(s: &str, from: i32, count: u32) -> String {
    let a = char_offset(s, from);
    let rest = &s[a..];
    let b = rest.char_indices().nth(count as usize).map(|(b, _)| a + b).unwrap_or(s.len());
    format!("{}{}", &s[..a], &s[b..])
}

fn ascii_case(s: &str, case: Case) -> String {
    match case {
        Case::Upper => s.to_ascii_uppercase(),
        Case::Lower => s.to_ascii_lowercase(),
        Case::Title | Case::Sentence => {
            let lower = s.to_ascii_lowercase();
            let mut cs = lower.chars();
            match cs.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Letter case of words (the locale rules of [`crate::sort`]: Turkish words keep their i/ı).
fn word_case(s: &str, case: Case) -> String {
    match case {
        Case::Lower => crate::sort::lower(s),
        Case::Upper => crate::sort::upper(s),
        Case::Title => crate::sort::title(s),
        Case::Sentence => {
            let lower = crate::sort::lower(s);
            match lower.char_indices().find(|(_, c)| c.is_alphabetic()) {
                Some((b, c)) => format!("{}{}{}", &lower[..b], crate::sort::upper(&c.to_string()), &lower[b + c.len_utf8()..]),
                None => lower,
            }
        }
    }
}

fn change_case(name: &str, is_dir: bool, case: Case, part: Part) -> String {
    let (stem, ext) = split(name, is_dir);
    // an extension is never cased with locale rules: ".AVI" becomes ".avi", not ".avı"
    match part {
        Part::Name => join(&word_case(stem, case), ext),
        Part::Ext => join(stem, &ascii_case(ext, case)),
        Part::Whole => {
            let ext_case = if case == Case::Upper { Case::Upper } else { Case::Lower };
            join(&word_case(stem, case), &ascii_case(ext, ext_case))
        }
    }
}

fn whitespace(s: &str, trim: bool, collapse: bool) -> String {
    let mut out = if collapse {
        let mut o = String::with_capacity(s.len());
        let mut in_ws = false;
        for c in s.chars() {
            if c.is_whitespace() {
                if !in_ws {
                    o.push(' ');
                }
                in_ws = true;
            } else {
                o.push(c);
                in_ws = false;
            }
        }
        o
    } else {
        s.to_string()
    };
    if trim {
        out = out.trim().to_string();
    }
    out
}

// ------------------------------------------------------------------ templates

fn check_date_format(step: usize, fmt: &str) -> Result<(), Problem> {
    use chrono::format::{Item, StrftimeItems};
    if StrftimeItems::new(fmt).any(|i| matches!(i, Item::Error)) {
        return Err(Problem::BadDateFormat { step, format: fmt.to_string() });
    }
    Ok(())
}

fn format_ms(step: usize, ms: i64, fmt: &str) -> Result<String, Problem> {
    use chrono::{DateTime, Local};
    use std::fmt::Write as _;
    let d = DateTime::from_timestamp_millis(ms).ok_or(Problem::BadDateFormat { step, format: fmt.to_string() })?;
    let mut s = String::new();
    // (a format chrono cannot write is an error here, never a panic)
    write!(s, "{}", d.with_timezone(&Local).format(fmt)).map_err(|_| Problem::BadDateFormat { step, format: fmt.to_string() })?;
    Ok(s)
}

const DEFAULT_DATE: &str = "%Y-%m-%d";

/// Every token of a template is known and every date format valid (checked once per step).
fn check_template(step: usize, template: &str) -> Result<(), Problem> {
    let mut err: Option<Problem> = None;
    let rest = crate::rename::expand_template(template, |tok| {
        let (head, arg) = tok.split_once(':').map_or((tok, None), |(h, a)| (h, Some(a)));
        let ok = match head {
            "name" | "ext" | "parent" | "size" | "date" => arg.is_none(),
            "n" => arg.is_none_or(|a| a.parse::<usize>().is_ok()),
            "modified" | "created" | "taken" => {
                if let Some(a) = arg.filter(|a| !a.is_empty())
                    && let Err(e) = check_date_format(step, a)
                {
                    err.get_or_insert(e);
                }
                true
            }
            _ => false,
        };
        if !ok {
            err.get_or_insert(Problem::UnknownToken { step, token: tok.to_string() });
        }
        Some(String::new())
    });
    // every token expanded to nothing: a '{' left over was never closed
    if err.is_none()
        && let Some(at) = rest.find('{')
    {
        err = Some(Problem::UnknownToken { step, token: rest[at + 1..].to_string() });
    }
    err.map_or(Ok(()), Err)
}

struct TplCtx<'a> {
    step: usize,
    stem: &'a str,
    ext: &'a str,
    fact: &'a FileFact,
    n: i64,
}

fn expand(template: &str, c: &TplCtx) -> Result<String, Problem> {
    let mut err: Option<Problem> = None;
    let out = crate::rename::expand_template(template, |tok| {
        let (head, arg) = tok.split_once(':').map_or((tok, None), |(h, a)| (h, Some(a)));
        let date = |field: &'static str, ms: i64, err: &mut Option<Problem>| {
            if ms <= 0 {
                err.get_or_insert(Problem::NoDate { step: c.step, field });
                return String::new();
            }
            let fmt = arg.filter(|a| !a.is_empty()).unwrap_or(DEFAULT_DATE);
            format_ms(c.step, ms, fmt).unwrap_or_else(|e| {
                err.get_or_insert(e);
                String::new()
            })
        };
        Some(match head {
            "name" => c.stem.to_string(),
            "ext" => c.ext.to_string(),
            "parent" => c.fact.path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "size" => c.fact.size.to_string(),
            "n" => match arg.and_then(|a| a.parse::<usize>().ok()) {
                Some(w) => format!("{:0w$}", c.n, w = w.min(32)),
                None => c.n.to_string(),
            },
            "date" => date("modified", c.fact.modified_ms, &mut err),
            "modified" => date("modified", c.fact.modified_ms, &mut err),
            "created" => date("created", c.fact.created_ms, &mut err),
            "taken" => date("taken", c.fact.taken_ms, &mut err),
            _ => {
                err.get_or_insert(Problem::UnknownToken { step: c.step, token: tok.to_string() });
                String::new()
            }
        })
    });
    err.map_or(Ok(out), Err)
}

// ------------------------------------------------------------------ running the steps

/// A step made ready once for all items (regexes built, templates checked).
struct Ready<'a> {
    step: &'a RuleStep,
    re: Option<Regex>,
    /// The step cannot run at all: every item gets this problem.
    broken: Option<Problem>,
}

fn build_regex(step: usize, pattern: &str, regex: bool, match_case: bool) -> Result<Regex, Problem> {
    let pat = if regex { pattern.to_string() } else { regex::escape(pattern) };
    RegexBuilder::new(&pat)
        .case_insensitive(!match_case)
        .build()
        .map_err(|e| Problem::BadRegex { step, msg: e.to_string().lines().last().unwrap_or("").trim().to_string() })
}

fn ready(steps: &[RuleStep]) -> Vec<Ready<'_>> {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let no = i + 1;
            let (re, broken) = match &s.op {
                Op::Replace { find, regex, match_case, .. } if !find.is_empty() => match build_regex(no, find, *regex, *match_case) {
                    Ok(r) => (Some(r), None),
                    Err(e) => (None, Some(e)),
                },
                Op::RemovePattern { pattern, regex, match_case, .. } if !pattern.is_empty() => match build_regex(no, pattern, *regex, *match_case) {
                    Ok(r) => (Some(r), None),
                    Err(e) => (None, Some(e)),
                },
                Op::Template { template, .. } => (None, check_template(no, template).err()),
                _ => (None, None),
            };
            Ready { step: s, re, broken }
        })
        .collect()
}

/// Computes the new name of every item (same order as `items`; `facts[i]` belongs to
/// `items[i]`). `now_ms` is for age conditions.
pub fn apply(steps: &[RuleStep], items: &[RenameItem], facts: &[FileFact], now_ms: i64) -> Vec<Result<String, Problem>> {
    assert_eq!(items.len(), facts.len(), "one fact per item");
    let ready = ready(steps);
    if let Some(p) = ready.iter().find_map(|r| r.broken.clone()) {
        return vec![Err(p); items.len()];
    }
    // counters per step, per folder when the step restarts in every folder
    let mut counters: Vec<HashMap<PathBuf, i64>> = vec![HashMap::new(); steps.len()];
    let mut out = Vec::with_capacity(items.len());
    for (item, fact) in items.iter().zip(facts) {
        let mut name = item.name.clone();
        let mut problem = None;
        for (si, r) in ready.iter().enumerate() {
            if r.step.when.as_ref().is_some_and(|c| !c.matches(fact, now_ms)) {
                continue;
            }
            match step_name(si + 1, r, &name, item.is_dir, fact, &mut counters[si]) {
                Ok(n) => name = n,
                Err(p) => {
                    problem = Some(p);
                    break;
                }
            }
        }
        out.push(problem.map_or(Ok(name), Err));
    }
    out
}

fn next_count(counter: &mut HashMap<PathBuf, i64>, fact: &FileFact, per_folder: bool, start: i64, step: i64) -> i64 {
    let key = if per_folder { fact.path.parent().map(Path::to_path_buf).unwrap_or_default() } else { PathBuf::new() };
    let k = counter.entry(key).or_insert(0);
    let n = start.saturating_add(step.saturating_mul(*k));
    *k += 1;
    n
}

fn step_name(no: usize, r: &Ready, name: &str, is_dir: bool, fact: &FileFact, counter: &mut HashMap<PathBuf, i64>) -> Result<String, Problem> {
    Ok(match &r.step.op {
        Op::Replace { with, regex, all, part, .. } => match &r.re {
            None => name.to_string(),
            Some(re) => edit(name, is_dir, *part, |s| {
                let limit = if *all { 0 } else { 1 };
                if *regex { re.replacen(s, limit, with.as_str()).into_owned() } else { re.replacen(s, limit, regex::NoExpand(with)).into_owned() }
            }),
        },
        Op::Insert { text, at, part } => edit(name, is_dir, *part, |s| insert_at(s, text, *at)),
        Op::RemoveRange { from, count, part } => edit(name, is_dir, *part, |s| remove_range(s, *from, *count)),
        Op::RemovePattern { part, .. } => match &r.re {
            None => name.to_string(),
            Some(re) => edit(name, is_dir, *part, |s| re.replace_all(s, "").into_owned()),
        },
        Op::Case { case, part } => change_case(name, is_dir, *case, *part),
        Op::Number { start, step, pad, at, sep, per_folder } => {
            let n = next_count(counter, fact, *per_folder, *start, *step);
            let num = format!("{n:0w$}", w = *pad as usize);
            edit(name, is_dir, Part::Name, |s| match at {
                At::Start => format!("{num}{sep}{s}"),
                At::End => format!("{s}{sep}{num}"),
                At::Index(_) => insert_at(s, &num, *at),
            })
        }
        Op::Whitespace { trim, collapse, part } => edit(name, is_dir, *part, |s| whitespace(s, *trim, *collapse)),
        Op::Extension { ext } => {
            if is_dir {
                name.to_string()
            } else {
                let (stem, _) = split(name, false);
                join(stem, ext.trim().trim_start_matches('.'))
            }
        }
        Op::Template { template, part, start, step, per_folder } => {
            let n = next_count(counter, fact, *per_folder, *start, *step);
            let (stem, ext) = split(name, is_dir);
            let made = expand(template, &TplCtx { step: no, stem, ext, fact, n })?;
            match part {
                Part::Name => join(&made, ext),
                Part::Ext if is_dir => name.to_string(),
                Part::Ext => join(stem, &made),
                Part::Whole => made,
            }
        }
    })
}

/// Does any step need the date a photo was taken? (Reading EXIF costs a file read per item;
/// the caller fills `FileFact::taken_ms` only then.)
pub fn needs_taken(steps: &[RuleStep]) -> bool {
    steps.iter().any(|s| matches!(&s.op, Op::Template { template, .. } if template.contains("{taken")))
}

/// The whole plan: new names, then every check of [`rename_plan::check`]. `existing` lists every
/// name now in each folder of the batch.
pub fn plan(steps: &[RuleStep], items: &[RenameItem], facts: &[FileFact], existing: &HashMap<PathBuf, Vec<String>>, rules: NameRules, now_ms: i64) -> Vec<Row> {
    let names = apply(steps, items, facts, now_ms);
    let mut rows: Vec<Row> = items
        .iter()
        .zip(facts)
        .zip(names)
        .map(|((it, f), n)| Row::new(f.path.parent().map(Path::to_path_buf).unwrap_or_default(), it.name.clone(), n))
        .collect();
    rename_plan::check(&mut rows, existing, rules);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::organize::{Cmp, FileKind, NameOp, TimeField};
    use crate::rename::{self, RenameError};
    use crate::rename_plan::Status;

    const W: NameRules = NameRules::WINDOWS;
    const DAY: i64 = 86_400_000;

    // a folder that is absolute on the machine the tests run on: a Windows path
    // does not split into folders on Linux, so `{parent}` found no "Pictures" there
    fn root_of(tail: &str) -> PathBuf {
        let mut p = PathBuf::from(if cfg!(windows) { r"D:\" } else { "/" });
        p.push("work");
        p.push(tail);
        p
    }
    fn dir() -> PathBuf {
        root_of("Pictures")
    }
    fn batch(names: &[&str]) -> (Vec<RenameItem>, Vec<FileFact>) {
        batch_in(&dir(), names)
    }
    fn batch_in(d: &Path, names: &[&str]) -> (Vec<RenameItem>, Vec<FileFact>) {
        let items = names.iter().map(|n| RenameItem::new(*n, false, 0)).collect();
        let facts = names.iter().map(|n| FileFact::new(d.join(n), 1234, 0, 0, FileKind::Other)).collect();
        (items, facts)
    }
    fn run(steps: &[RuleStep], names: &[&str]) -> Vec<String> {
        let (i, f) = batch(names);
        apply(steps, &i, &f, 0).into_iter().map(|r| r.unwrap()).collect()
    }
    fn one(op: Op, names: &[&str]) -> Vec<String> {
        run(&[RuleStep::new(op)], names)
    }
    fn replace(find: &str, with: &str) -> Op {
        Op::Replace { find: find.into(), with: with.into(), regex: false, match_case: false, all: true, part: Part::Name }
    }
    fn tpl(t: &str) -> Op {
        Op::Template { template: t.into(), part: Part::Name, start: 1, step: 1, per_folder: false }
    }

    #[test]
    fn replace_plain_regex_case_first_all() {
        assert_eq!(one(replace("img", "Foto"), &["IMG_1 img.jpg"]), ["Foto_1 Foto.jpg"]);
        let first = Op::Replace { find: "a".into(), with: "x".into(), regex: false, match_case: true, all: false, part: Part::Name };
        assert_eq!(one(first, &["aAa.txt"]), ["xAa.txt"]);
        let cased = Op::Replace { find: "a".into(), with: "x".into(), regex: false, match_case: true, all: true, part: Part::Name };
        assert_eq!(one(cased, &["aAa.txt"]), ["xAx.txt"]);
        // '$' is literal in plain mode, a group in regex mode
        assert_eq!(one(replace("a", "$1"), &["a.txt"]), ["$1.txt"]);
        let re = Op::Replace { find: r"(\d{4})-(\d{2})-(\d{2})".into(), with: "$3.$2.$1".into(), regex: true, match_case: false, all: true, part: Part::Name };
        assert_eq!(one(re, &["report 2024-03-15.pdf"]), ["report 15.03.2024.pdf"]);
        // parts
        let ext = Op::Replace { find: "jpeg".into(), with: "jpg".into(), regex: false, match_case: false, all: true, part: Part::Ext };
        assert_eq!(one(ext, &["jpeg.JPEG"]), ["jpeg.jpg"]);
        let whole = Op::Replace { find: "txt".into(), with: "md".into(), regex: false, match_case: false, all: true, part: Part::Whole };
        assert_eq!(one(whole, &["txt.txt"]), ["md.md"]);
        // an empty find changes nothing
        assert_eq!(one(replace("", "x"), &["a.txt"]), ["a.txt"]);
    }

    #[test]
    fn a_bad_regex_is_a_problem_for_every_item() {
        let (i, f) = batch(&["a", "b"]);
        let bad = RuleStep::new(Op::Replace { find: "(".into(), with: "".into(), regex: true, match_case: false, all: true, part: Part::Name });
        let r = apply(&[RuleStep::new(replace("a", "b")), bad], &i, &f, 0);
        assert!(r.iter().all(|r| matches!(r, Err(Problem::BadRegex { step: 2, .. }))), "{r:?}");
    }

    #[test]
    fn insert_at_positions() {
        let ins = |text: &str, at| Op::Insert { text: text.into(), at, part: Part::Name };
        assert_eq!(one(ins("new_", At::Start), &["a.txt"]), ["new_a.txt"]);
        assert_eq!(one(ins("_v2", At::End), &["a.txt"]), ["a_v2.txt"]);
        assert_eq!(one(ins("-", At::Index(2)), &["abcd.txt"]), ["ab-cd.txt"]);
        assert_eq!(one(ins("-", At::Index(-1)), &["abcd.txt"]), ["abc-d.txt"]);
        assert_eq!(one(ins("-", At::Index(99)), &["ab.txt"]), ["ab-.txt"]);
        assert_eq!(one(ins("-", At::Index(-99)), &["ab.txt"]), ["-ab.txt"]);
        // characters, not bytes
        assert_eq!(one(ins("|", At::Index(1)), &["äöü.txt"]), ["ä|öü.txt"]);
        let after_ext = Op::Insert { text: ".bak".into(), at: At::End, part: Part::Whole };
        assert_eq!(one(after_ext, &["a.txt"]), ["a.txt.bak"]);
    }

    #[test]
    fn remove_range_and_pattern() {
        let rr = |from, count| Op::RemoveRange { from, count, part: Part::Name };
        assert_eq!(one(rr(0, 4), &["IMG_0001.jpg"]), ["0001.jpg"]);
        assert_eq!(one(rr(-2, 2), &["photo01.jpg"]), ["photo.jpg"]);
        assert_eq!(one(rr(3, 100), &["abcdef.jpg"]), ["abc.jpg"]);
        assert_eq!(one(rr(10, 1), &["abc.jpg"]), ["abc.jpg"]);
        assert_eq!(one(rr(1, 1), &["äöü.jpg"]), ["äü.jpg"]);
        let rp = |p: &str, regex| Op::RemovePattern { pattern: p.into(), regex, match_case: false, part: Part::Name };
        assert_eq!(one(rp(" (copy)", false), &["a (Copy).txt"]), ["a.txt"]);
        assert_eq!(one(rp(r"\d+", true), &["a1b22c.txt"]), ["abc.txt"]);
    }

    #[test]
    fn case_for_name_and_extension() {
        let c = |case, part| Op::Case { case, part };
        assert_eq!(one(c(Case::Lower, Part::Name), &["Hello World.TXT"]), ["hello world.TXT"]);
        assert_eq!(one(c(Case::Upper, Part::Name), &["Hello World.txt"]), ["HELLO WORLD.txt"]);
        assert_eq!(one(c(Case::Title, Part::Name), &["hELLO wORLD-foo_bar.txt"]), ["Hello World-Foo_Bar.txt"]);
        assert_eq!(one(c(Case::Sentence, Part::Name), &["hELLO wORLD.txt"]), ["Hello world.txt"]);
        assert_eq!(one(c(Case::Sentence, Part::Name), &["01 - my SONG.mp3"]), ["01 - My song.mp3"]);
        assert_eq!(one(c(Case::Lower, Part::Ext), &["A.JPG", "B.Jpeg", "noext"]), ["A.jpg", "B.jpeg", "noext"]);
        assert_eq!(one(c(Case::Upper, Part::Ext), &["a.jpg"]), ["a.JPG"]);
        assert_eq!(one(c(Case::Lower, Part::Whole), &["A.AVI"]), ["a.avi"]);
        assert_eq!(one(c(Case::Title, Part::Whole), &["my file.TXT"]), ["My File.txt"]);
    }

    #[test]
    fn numbering_start_step_pad_position_and_folders() {
        let num = |start, step, pad, at, sep: &str, per_folder| Op::Number { start, step, pad, at, sep: sep.into(), per_folder };
        assert_eq!(one(num(1, 1, 3, At::Start, " ", false), &["a.jpg", "b.jpg"]), ["001 a.jpg", "002 b.jpg"]);
        assert_eq!(one(num(10, 5, 0, At::End, "_", false), &["a.jpg", "b.jpg"]), ["a_10.jpg", "b_15.jpg"]);
        assert_eq!(one(num(3, -1, 2, At::Index(1), "", false), &["ab", "cd"]), ["a03b", "c02d"]);
        // per folder: the counter starts again in each folder
        let d2 = root_of("Other");
        let (mut i, mut f) = batch(&["a", "b"]);
        let (i2, f2) = batch_in(&d2, &["c", "d"]);
        i.extend(i2.clone());
        f.extend(f2.clone());
        i.push(RenameItem::new("e", false, 0));
        f.push(FileFact::new(dir().join("e"), 0, 0, 0, FileKind::Other));
        let per = [RuleStep::new(num(1, 1, 0, At::Start, "-", true))];
        let r: Vec<String> = apply(&per, &i, &f, 0).into_iter().map(Result::unwrap).collect();
        assert_eq!(r, ["1-a", "2-b", "1-c", "2-d", "3-e"]);
        let all = [RuleStep::new(num(1, 1, 0, At::Start, "-", false))];
        let r: Vec<String> = apply(&all, &i, &f, 0).into_iter().map(Result::unwrap).collect();
        assert_eq!(r, ["1-a", "2-b", "3-c", "4-d", "5-e"]);
    }

    #[test]
    fn whitespace_and_extension() {
        let ws = |trim, collapse| Op::Whitespace { trim, collapse, part: Part::Name };
        assert_eq!(one(ws(true, true), &["  a \t  b  .txt"]), ["a b.txt"]);
        assert_eq!(one(ws(true, false), &["  a   b .txt"]), ["a   b.txt"]);
        assert_eq!(one(ws(false, true), &[" a   b .txt"]), [" a b .txt"]);
        let ext = |e: &str| Op::Extension { ext: e.into() };
        assert_eq!(one(ext("jpg"), &["a.jpeg", "b", ".env"]), ["a.jpg", "b.jpg", ".env.jpg"]);
        assert_eq!(one(ext(".MD"), &["a.txt"]), ["a.MD"]);
        assert_eq!(one(ext(""), &["a.txt", "b"]), ["a", "b"]);
        // folders keep their names
        let (_, f) = batch(&["folder.v1"]);
        let it = vec![RenameItem::new("folder.v1", true, 0)];
        assert_eq!(apply(&[RuleStep::new(ext("x"))], &it, &f, 0), vec![Ok("folder.v1".to_string())]);
        // a folder has no extension: case on "ext" leaves it, "name" covers the dot
        assert_eq!(apply(&[RuleStep::new(Op::Case { case: Case::Upper, part: Part::Ext })], &it, &f, 0), vec![Ok("folder.v1".to_string())]);
        assert_eq!(apply(&[RuleStep::new(Op::Case { case: Case::Upper, part: Part::Name })], &it, &f, 0), vec![Ok("FOLDER.V1".to_string())]);
    }

    #[test]
    fn template_tokens() {
        let local = |ms: i64, fmt: &str| chrono::DateTime::from_timestamp_millis(ms).unwrap().with_timezone(&chrono::Local).format(fmt).to_string();
        let ms = 1_700_000_000_000;
        let mut f = FileFact::new(dir().join("IMG_1.JPG"), 4096, ms, ms - DAY, FileKind::Image);
        f.taken_ms = ms - 2 * DAY;
        let it = vec![RenameItem::new("IMG_1.JPG", false, ms)];
        let go = |t: &str, part| apply(&[RuleStep::new(Op::Template { template: t.into(), part, start: 7, step: 1, per_folder: false })], &it, std::slice::from_ref(&f), 0);
        assert_eq!(go("{name}-{ext}-{parent}-{size}", Part::Name), vec![Ok("IMG_1-JPG-Pictures-4096.JPG".to_string())]);
        assert_eq!(go("{n:3} {n}", Part::Name), vec![Ok("007 7.JPG".to_string())]);
        assert_eq!(go("{modified:%Y-%m-%d} {name}", Part::Name), vec![Ok(format!("{} IMG_1.JPG", local(ms, "%Y-%m-%d")))]);
        assert_eq!(go("{created:%Y%m%d}", Part::Name), vec![Ok(format!("{}.JPG", local(ms - DAY, "%Y%m%d")))]);
        assert_eq!(go("{taken}", Part::Name), vec![Ok(format!("{}.JPG", local(ms - 2 * DAY, "%Y-%m-%d")))]);
        assert_eq!(go("{date}", Part::Name), vec![Ok(format!("{}.JPG", local(ms, "%Y-%m-%d")))]);
        assert_eq!(go("{name}.{ext}.bak", Part::Whole), vec![Ok("IMG_1.JPG.bak".to_string())]);
        assert_eq!(go("jpg", Part::Ext), vec![Ok("IMG_1.jpg".to_string())]);
        // problems
        assert_eq!(go("{nmae}", Part::Name), vec![Err(Problem::UnknownToken { step: 1, token: "nmae".into() })]);
        assert_eq!(go("{n:x}", Part::Name), vec![Err(Problem::UnknownToken { step: 1, token: "n:x".into() })]);
        assert!(matches!(&go("{modified:%Q}", Part::Name)[0], Err(Problem::BadDateFormat { step: 1, .. })));
        assert_eq!(go("a {name", Part::Name), vec![Err(Problem::UnknownToken { step: 1, token: "name".into() })]);
        // a time with ':' is not a file name: the plan says so
        let e: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let rows = plan(&[RuleStep::new(tpl("{modified:%H:%M}"))], &it, std::slice::from_ref(&f), &e, W, 0);
        assert!(matches!(rows[0].status, Status::Problem(Problem::InvalidChar(_))));
        // no date known
        let mut g = f.clone();
        g.taken_ms = 0;
        let r = apply(&[RuleStep::new(tpl("{taken}"))], &it, std::slice::from_ref(&g), 0);
        assert_eq!(r, vec![Err(Problem::NoDate { step: 1, field: "taken" })]);
        assert!(needs_taken(&[RuleStep::new(tpl("{taken:%Y}"))]));
        assert!(!needs_taken(&[RuleStep::new(tpl("{modified}"))]));
    }

    #[test]
    fn conditions_choose_the_items_of_a_step() {
        let ext_is = |e: &str| Condition::Extension { list: vec![e.into()] };
        let steps = [
            RuleStep::new(Op::Insert { text: "photo_".into(), at: At::Start, part: Part::Name }).when(ext_is("jpg")),
            RuleStep::new(Op::Insert { text: "doc_".into(), at: At::Start, part: Part::Name }).when(ext_is("pdf")),
        ];
        assert_eq!(run(&steps, &["a.JPG", "b.pdf", "c.txt"]), ["photo_a.JPG", "doc_b.pdf", "c.txt"]);
        // numbering counts only the items it applies to
        let numbered = [RuleStep::new(Op::Number { start: 1, step: 1, pad: 0, at: At::End, sep: "_".into(), per_folder: false }).when(ext_is("jpg"))];
        assert_eq!(run(&numbered, &["a.jpg", "b.txt", "c.jpg"]), ["a_1.jpg", "b.txt", "c_2.jpg"]);
        // conditions see the file as it is, so two steps can swap names
        let has = |t: &str| Condition::Name { op: NameOp::Contains, text: t.into() };
        let swap = [RuleStep::new(replace("left", "right")).when(has("left")), RuleStep::new(replace("right", "left")).when(has("right"))];
        assert_eq!(run(&swap, &["left.txt", "right.txt"]), ["right.txt", "left.txt"]);
        // size and age conditions use the fact
        let (i, mut f) = batch(&["big", "small"]);
        f[0].size = 10_000;
        f[1].size = 10;
        f[0].modified_ms = 100 * DAY;
        f[1].modified_ms = 1;
        let big = [RuleStep::new(Op::Insert { text: "B_".into(), at: At::Start, part: Part::Name }).when(Condition::Size { cmp: Cmp::Greater, bytes: 1000 })];
        assert_eq!(apply(&big, &i, &f, 0), vec![Ok("B_big".to_string()), Ok("small".to_string())]);
        let old = [RuleStep::new(Op::Insert { text: "old_".into(), at: At::Start, part: Part::Name }).when(Condition::Age { field: TimeField::Modified, cmp: Cmp::Greater, days: 30 })];
        assert_eq!(apply(&old, &i, &f, 101 * DAY), vec![Ok("big".to_string()), Ok("old_small".to_string())]);
        // an empty condition matches nothing (as in the organizer)
        let empty = [RuleStep::new(replace("a", "b")).when(Condition::Name { op: NameOp::Contains, text: " ".into() })];
        assert_eq!(run(&empty, &["a"]), ["a"]);
    }

    #[test]
    fn steps_build_on_each_other() {
        let steps = [
            RuleStep::new(Op::Whitespace { trim: true, collapse: true, part: Part::Name }),
            RuleStep::new(replace(" ", "_")),
            RuleStep::new(Op::Case { case: Case::Lower, part: Part::Whole }),
            RuleStep::new(tpl("{n:2}_{name}")),
        ];
        assert_eq!(run(&steps, &["  My  Holiday .JPG", "Beach.PNG"]), ["01_my_holiday.jpg", "02_beach.png"]);
    }

    #[test]
    fn plan_reports_every_class_and_case_only() {
        let (i, f) = batch(&["a.txt", "b.txt", "c.txt", "d.TXT", "e.txt"]);
        let steps = [
            RuleStep::new(tpl("same")).when(Condition::Name { op: NameOp::Wildcard, text: "a.txt;b.txt".into() }),
            RuleStep::new(tpl("keep")).when(Condition::Name { op: NameOp::StartsWith, text: "c".into() }),
            RuleStep::new(Op::Case { case: Case::Lower, part: Part::Ext }),
            RuleStep::new(tpl("con")).when(Condition::Name { op: NameOp::StartsWith, text: "e".into() }),
        ];
        let ex = HashMap::from([(dir(), vec!["a.txt", "b.txt", "c.txt", "d.TXT", "e.txt", "KEEP.TXT"].into_iter().map(String::from).collect())]);
        let rows = plan(&steps, &i, &f, &ex, W, 0);
        assert!(matches!(rows[0].status, Status::Problem(Problem::Duplicate(_))));
        assert!(matches!(rows[1].status, Status::Problem(Problem::Duplicate(_))));
        assert_eq!(rows[2].status, Status::Problem(Problem::Exists("KEEP.TXT".into())));
        assert_eq!(rows[3].status, Status::CaseOnly);
        assert_eq!(rows[3].new, "d.txt");
        assert!(matches!(rows[4].status, Status::Problem(Problem::Reserved(_))));
    }

    #[test]
    fn the_classic_modes_as_steps_give_the_same_names() {
        let names = [
            "IMG_1.jpg", "img_2.JPG", "rapor 2024-03-15.pdf", "Hello World.TXT", "hELLO wORLD-foo_bar.txt", ".env", "noext", "a.b.c", "txt.txt", "İstanbul ılık.txt",
            "Ärger Über.md", "  spaced  .txt",
        ];
        let items: Vec<RenameItem> = names.iter().map(|n| RenameItem::new(*n, false, 1_700_000_000_000)).collect();
        let facts: Vec<FileFact> = names.iter().map(|n| FileFact::new(dir().join(n), 1, 1_700_000_000_000, 0, FileKind::Other)).collect();
        let mut specs = Vec::new();
        for include_ext in [false, true] {
            let mut modes = vec![
                Mode::FindReplace { find: "img".into(), replace: "Foto".into(), case_insensitive: true, regex: false },
                Mode::FindReplace { find: "img".into(), replace: "Foto".into(), case_insensitive: false, regex: false },
                Mode::FindReplace { find: "t".into(), replace: "$1".into(), case_insensitive: false, regex: false },
                Mode::FindReplace { find: r"(\d{4})-(\d{2})-(\d{2})".into(), replace: "$3.$2.$1".into(), case_insensitive: false, regex: true },
                Mode::FindReplace { find: "".into(), replace: "x".into(), case_insensitive: false, regex: false },
                Mode::Case(CaseKind::Lower),
                Mode::Case(CaseKind::Upper),
                Mode::Case(CaseKind::Title),
                Mode::PrefixSuffix { prefix: "new_".into(), suffix: "_v2".into() },
            ];
            if !include_ext {
                // (with the extension included, the classic `{name}` is the whole name)
                modes.push(Mode::Pattern { pattern: "Holiday_{n:03}".into(), start: 1, step: 1 });
                modes.push(Mode::Pattern { pattern: "{n:02}-{name}-{ext}-{date}".into(), start: 10, step: 5 });
                modes.push(Mode::Pattern { pattern: "f{n}".into(), start: 99, step: -1 });
            }
            for mode in modes {
                specs.push(RenameSpec { mode, include_ext, existing: Vec::new() });
            }
        }
        for spec in specs {
            let classic = rename::compute(&items, &spec);
            let steps = steps_for_mode(&spec);
            let new = apply(&steps, &items, &facts, 0);
            for ((c, n), name) in classic.iter().zip(&new).zip(names) {
                match (c, n) {
                    (Ok(c), Ok(n)) => assert_eq!(c, n, "{spec:?} on {name}"),
                    // the classic mode validates in `compute`; the steps leave that to the plan
                    (Err(RenameError::Duplicate | RenameError::Exists), Ok(_)) => {}
                    (Err(_), Ok(n)) => assert!(rename_plan::name_problem(n).is_some(), "{spec:?} on {name}: {n}"),
                    (c, n) => panic!("{spec:?} on {name}: {c:?} vs {n:?}"),
                }
            }
        }
    }

    #[test]
    fn presets_round_trip_through_json_and_are_all_usable() {
        for p in builtin_presets() {
            let s = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Preset>(&s).unwrap(), p);
            let (i, f) = batch(&["My  File .JPG"]);
            let r = apply(&p.steps, &i, &f, 0);
            // the date preset needs a date; the rest change the name
            if p.name.starts_with("Date") {
                assert!(matches!(r[0], Err(Problem::NoDate { .. })));
            } else {
                assert!(r[0].as_ref().is_ok_and(|n| n != "My  File .JPG"), "{}: {r:?}", p.name);
            }
        }
        // defaults fill in what a hand-written file leaves out
        let s: RuleStep = serde_json::from_str(r#"{"op":"replace","find":"a"}"#).unwrap();
        assert_eq!(s.op, Op::Replace { find: "a".into(), with: "".into(), regex: false, match_case: false, all: true, part: Part::Name });
        let s: RuleStep = serde_json::from_str(r#"{"op":"insert","text":"x","at":{"index":-2},"when":{"type":"extension","list":["jpg"]}}"#).unwrap();
        assert_eq!(s.op, Op::Insert { text: "x".into(), at: At::Index(-2), part: Part::Name });
        assert_eq!(s.when, Some(Condition::Extension { list: vec!["jpg".into()] }));
    }

    #[test]
    fn ten_thousand_items_plan_quickly() {
        let names: Vec<String> = (0..10_000).map(|i| format!("IMG_{i:05}.JPG")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (i, f) = batch(&refs);
        let steps = [RuleStep::new(replace("IMG_", "photo ")), RuleStep::new(Op::Case { case: Case::Lower, part: Part::Ext }), RuleStep::new(tpl("{name} {n:5}"))];
        let ex = HashMap::from([(dir(), names.clone())]);
        let t = std::time::Instant::now();
        let rows = plan(&steps, &i, &f, &ex, W, 0);
        let took = t.elapsed();
        assert!(rows.iter().all(|r| r.status == Status::Rename));
        assert!(took.as_millis() < 2000, "{took:?}");
    }
}
