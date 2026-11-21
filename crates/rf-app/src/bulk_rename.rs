//! Bulk rename dialog: a rule list (ordered steps, each with an optional condition from the
//! folder organizer), presets (built in and saved), and a live preview of every item with its
//! status. The plan (`rf_core::rename_rules::plan`) is computed off the UI thread a moment after
//! the last change, so a selection of 10,000 items stays responsive; the preview list only draws
//! the rows on screen. Rename is disabled while any item has a problem, unless "Skip items with
//! problems" is ticked. It runs through `rename_exec` (safe order, never replaces a file),
//! records one undo entry for the whole batch and lets the tags follow.
//!
//! Embed: `cx.new(|cx| BulkRename::new(paths, window, cx))`, subscribe to [`BulkRenameEvent::Close`].
//! The view renders only the card; the caller positions it as an overlay.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_core::organize::rule::parse_ext_list;
use rf_core::organize::summary::SizeUnit;
use rf_core::organize::{Cmp, Condition, FileFact, FileKind, NameOp, TimeField};
use rf_core::rename::RenameItem;
use rf_core::rename_plan::{NameRules, Row, Status, Summary};
use rf_core::rename_rules::{self, At, Case, Op, Part, Preset, RuleStep};

use crate::organizer::editor::{Field, cmp_label, kind_label, name_op_label, parse_number, value_hint};
use crate::select_box::{Choice, select_box_fill, select_box_h};
use crate::t;
use crate::theme::{self, Palette};

pub enum BulkRenameEvent {
    Close,
}

/// The operation of a step, as the select box lists them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Replace,
    Insert,
    RemoveRange,
    RemovePattern,
    Case,
    Number,
    Whitespace,
    Extension,
    Template,
}

impl Kind {
    const ALL: [Kind; 9] = [Kind::Replace, Kind::Insert, Kind::RemovePattern, Kind::RemoveRange, Kind::Case, Kind::Number, Kind::Template, Kind::Whitespace, Kind::Extension];

    fn label(self) -> &'static str {
        match self {
            Kind::Replace => t!("Replace text"),
            Kind::Insert => t!("Insert text"),
            Kind::RemoveRange => t!("Remove characters"),
            Kind::RemovePattern => t!("Remove text"),
            Kind::Case => t!("Change letter case"),
            Kind::Number => t!("Add a number"),
            Kind::Whitespace => t!("Tidy white space"),
            Kind::Extension => t!("Change extension"),
            Kind::Template => t!("New name from a template"),
        }
    }
}

fn part_label(p: Part) -> &'static str {
    match p {
        Part::Name => t!("Name"),
        Part::Ext => t!("Extension"),
        Part::Whole => t!("Name and extension"),
    }
}

fn case_label(c: Case) -> &'static str {
    match c {
        Case::Lower => t!("lowercase"),
        Case::Upper => t!("UPPERCASE"),
        Case::Title => t!("Title Case"),
        Case::Sentence => t!("Sentence case"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtKind {
    Start,
    End,
    Index,
}

fn at_label(a: AtKind) -> &'static str {
    match a {
        AtKind::Start => t!("At the start"),
        AtKind::End => t!("At the end"),
        AtKind::Index => t!("At position"),
    }
}

/// The condition of a step, as edited (the organizer's fields and words).
struct CondEd {
    field: Field,
    name_op: NameOp,
    cmp: Cmp,
    text: Entity<InputState>,
    kinds: Vec<FileKind>,
    unit: SizeUnit,
}

impl CondEd {
    /// The condition, or why it cannot be read. A condition that cannot be read matches no
    /// item (never every item): see `StepEd::to_step`.
    fn condition(&self, cx: &App) -> Result<Condition, &'static str> {
        let text = self.text.read(cx).value().to_string();
        Ok(match self.field {
            Field::Name => Condition::Name { op: self.name_op, text },
            Field::Extension => Condition::Extension { list: parse_ext_list(&text) },
            Field::Kind => Condition::Kind { kinds: self.kinds.clone() },
            Field::Size => {
                let n = parse_number(&text).ok_or(t!("Enter a number for the size"))?;
                Condition::Size { cmp: self.cmp, bytes: (n * self.unit.bytes() as f64).round() as u64 }
            }
            Field::Modified | Field::Created => {
                let days: u32 = text.trim().parse().map_err(|_| t!("Enter a whole number of days"))?;
                Condition::Age { field: if self.field == Field::Modified { TimeField::Modified } else { TimeField::Created }, cmp: self.cmp, days }
            }
        })
    }
}

/// One step as edited. The five boxes mean different things for each kind (see `set_kind`).
struct StepEd {
    kind: Kind,
    a: Entity<InputState>,
    b: Entity<InputState>,
    c: Entity<InputState>,
    d: Entity<InputState>,
    e: Entity<InputState>,
    regex: bool,
    match_case: bool,
    all: bool,
    part: Part,
    at: AtKind,
    case: Case,
    per_folder: bool,
    trim: bool,
    collapse: bool,
    cond: Option<CondEd>,
}

fn int(s: &str, default: i64) -> i64 {
    s.trim().parse().unwrap_or(default)
}

/// A condition no item matches (the organizer's empty name condition).
fn never() -> Condition {
    Condition::Name { op: NameOp::Contains, text: String::new() }
}

impl StepEd {
    fn to_step(&self, cx: &App) -> RuleStep {
        let v = |e: &Entity<InputState>| e.read(cx).value().to_string();
        let at = match self.at {
            AtKind::Start => At::Start,
            AtKind::End => At::End,
            AtKind::Index => At::Index(int(&v(&self.e), 0).clamp(i32::MIN as i64, i32::MAX as i64) as i32),
        };
        let op = match self.kind {
            Kind::Replace => Op::Replace { find: v(&self.a), with: v(&self.b), regex: self.regex, match_case: self.match_case, all: self.all, part: self.part },
            Kind::Insert => Op::Insert { text: v(&self.a), at, part: self.part },
            Kind::RemoveRange => Op::RemoveRange { from: int(&v(&self.a), 0).clamp(-100_000, 100_000) as i32, count: int(&v(&self.b), 1).clamp(0, 100_000) as u32, part: self.part },
            Kind::RemovePattern => Op::RemovePattern { pattern: v(&self.a), regex: self.regex, match_case: self.match_case, part: self.part },
            Kind::Case => Op::Case { case: self.case, part: self.part },
            Kind::Number => Op::Number { start: int(&v(&self.a), 1), step: int(&v(&self.b), 1), pad: int(&v(&self.c), 0).clamp(0, 32) as u8, at, sep: v(&self.d), per_folder: self.per_folder },
            Kind::Whitespace => Op::Whitespace { trim: self.trim, collapse: self.collapse, part: self.part },
            Kind::Extension => Op::Extension { ext: v(&self.a) },
            Kind::Template => Op::Template { template: v(&self.a), part: self.part, start: int(&v(&self.b), 1), step: int(&v(&self.c), 1), per_folder: self.per_folder },
        };
        // (a condition that cannot be read applies the step to nothing, never to everything)
        RuleStep { op, when: self.cond.as_ref().map(|c| c.condition(cx).unwrap_or_else(|_| never())) }
    }

    fn cond_error(&self, cx: &App) -> Option<&'static str> {
        self.cond.as_ref().and_then(|c| c.condition(cx).err())
    }
}

pub struct BulkRename {
    items: Arc<Vec<RenameItem>>,
    facts: Arc<Vec<FileFact>>,
    existing: Arc<HashMap<PathBuf, Vec<String>>>,
    /// Dates, sizes and the folder listings are read.
    loaded: bool,
    /// EXIF dates were read (only when a step uses `{taken}`).
    taken_read: bool,
    steps: Vec<StepEd>,
    presets: crate::rename_presets::Presets,
    preset_name: Entity<InputState>,
    /// The preset the steps came from (for "Delete preset").
    current: Option<String>,
    rows: Arc<Vec<Row>>,
    summary: Summary,
    /// The preview is being computed.
    computing: bool,
    skip_problems: bool,
    applying: bool,
    errors: Vec<String>,
    note: Option<String>,
    _subs: Vec<Subscription>,
    _plan: Option<Task<()>>,
}

impl EventEmitter<BulkRenameEvent> for BulkRename {}

fn input(window: &mut Window, cx: &mut Context<BulkRename>, ph: &str, val: &str) -> Entity<InputState> {
    let (ph, val) = (ph.to_string(), val.to_string());
    cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(val))
}

fn ms_of(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl BulkRename {
    pub fn new(paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Names are known at once; sizes, dates and the folder listings (collision checks) are
        // read in the background so opening the dialog never touches the disk on the UI thread.
        let name_of = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let items: Vec<RenameItem> = paths.iter().map(|p| RenameItem::new(name_of(p), false, 0)).collect();
        let facts: Vec<FileFact> = paths.iter().map(|p| FileFact::new(p.clone(), 0, 0, 0, FileKind::Other)).collect();
        let rows: Vec<Row> = paths.iter().map(|p| Row::new(p.parent().map(|d| d.to_path_buf()).unwrap_or_default(), name_of(p), Ok(name_of(p)))).collect();
        let summary = Summary { unchanged: rows.len(), ..Summary::default() };
        let meta = cx.background_spawn(async move {
            let mut items = Vec::with_capacity(paths.len());
            let mut facts = Vec::with_capacity(paths.len());
            for p in &paths {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let m = std::fs::metadata(p).ok();
                let is_dir = m.as_ref().is_some_and(|m| m.is_dir());
                let (size, modified, created) = m.as_ref().map(|m| (if is_dir { 0 } else { m.len() }, ms_of(m.modified()), ms_of(m.created()))).unwrap_or((0, 0, 0));
                let kind = if is_dir { FileKind::Other } else { crate::organizer::classify(rf_core::organize::rule::ext_of(&name)) };
                items.push(RenameItem::new(name, is_dir, modified));
                facts.push(FileFact::new(p.clone(), size, modified, created, kind));
            }
            let existing = crate::rename_exec::listings(paths.iter().filter_map(|p| p.parent()));
            (items, facts, existing)
        });
        cx.spawn(async move |this, cx| {
            let (items, facts, existing) = meta.await;
            let _ = this.update(cx, |b, cx| {
                b.items = Arc::new(items);
                b.facts = Arc::new(facts);
                b.existing = Arc::new(existing);
                b.loaded = true;
                b.recompute(cx);
            });
        })
        .detach();

        let preset_name = input(window, cx, t!("Preset name"), "");
        let mut this = Self {
            items: Arc::new(items),
            facts: Arc::new(facts),
            existing: Arc::new(HashMap::new()),
            loaded: false,
            taken_read: false,
            steps: Vec::new(),
            presets: crate::rename_presets::load(),
            preset_name,
            current: None,
            rows: Arc::new(rows),
            summary,
            computing: false,
            skip_problems: false,
            applying: false,
            errors: Vec::new(),
            note: None,
            _subs: Vec::new(),
            _plan: None,
        };
        this.watch_name(window, cx);
        // a new dialog starts with one "Replace text" step to fill in
        this.add_step(Kind::Replace, window, cx);
        if let Some(s) = this.steps.first() {
            s.a.update(cx, |s, cx| s.focus(window, cx));
        }
        this
    }

    fn watch(&mut self, e: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        self._subs.push(cx.subscribe_in(e, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                this.recompute(cx);
            }
        }));
    }

    fn new_step(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) -> StepEd {
        let mut s = StepEd {
            kind,
            a: input(window, cx, "", ""),
            b: input(window, cx, "", ""),
            c: input(window, cx, "", ""),
            d: input(window, cx, "", ""),
            e: input(window, cx, "", ""),
            regex: false,
            match_case: false,
            all: true,
            part: Part::Name,
            at: AtKind::End,
            case: Case::Lower,
            per_folder: false,
            trim: true,
            collapse: true,
            cond: None,
        };
        for e in [s.a.clone(), s.b.clone(), s.c.clone(), s.d.clone(), s.e.clone()] {
            self.watch(&e, window, cx);
        }
        Self::set_kind(&mut s, kind, window, cx);
        s
    }

    /// Placeholders and starting values of the five boxes for `kind`.
    fn set_kind(s: &mut StepEd, kind: Kind, window: &mut Window, cx: &mut App) {
        s.kind = kind;
        let fill = |e: &Entity<InputState>, ph: &str, val: &str, window: &mut Window, cx: &mut App| {
            let (ph, val) = (ph.to_string(), val.to_string());
            e.update(cx, |st, cx| {
                st.set_placeholder(ph, window, cx);
                st.set_value(val, window, cx);
            });
        };
        let (a, b, c, d, e): ((&str, &str), (&str, &str), (&str, &str), (&str, &str), (&str, &str)) = match kind {
            Kind::Replace => ((t!("Find"), ""), (t!("Replace with"), ""), ("", ""), ("", ""), ("", "")),
            Kind::Insert => ((t!("Text to insert"), ""), ("", ""), ("", ""), ("", ""), (t!("Position"), "0")),
            Kind::RemoveRange => ((t!("From (0 = first, -1 = last)"), "0"), (t!("How many"), "1"), ("", ""), ("", ""), ("", "")),
            Kind::RemovePattern => ((t!("Text to remove"), ""), ("", ""), ("", ""), ("", ""), ("", "")),
            Kind::Number => ((t!("Start"), "1"), (t!("Step"), "1"), (t!("Digits"), "3"), (t!("Separator"), " "), (t!("Position"), "0")),
            Kind::Extension => ((t!("New extension (empty removes it)"), ""), ("", ""), ("", ""), ("", ""), ("", "")),
            Kind::Template => ((t!("{modified:%Y-%m-%d} {name}"), "{name}"), (t!("Start"), "1"), (t!("Step"), "1"), ("", ""), ("", "")),
            Kind::Case | Kind::Whitespace => (("", ""), ("", ""), ("", ""), ("", ""), ("", "")),
        };
        fill(&s.a, a.0, a.1, window, cx);
        fill(&s.b, b.0, b.1, window, cx);
        fill(&s.c, c.0, c.1, window, cx);
        fill(&s.d, d.0, d.1, window, cx);
        fill(&s.e, e.0, e.1, window, cx);
        s.at = if kind == Kind::Number { AtKind::Start } else { AtKind::End };
    }

    fn add_step(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.new_step(kind, window, cx);
        self.steps.push(s);
        self.recompute(cx);
    }

    fn new_cond(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) -> CondEd {
        let text = input(window, cx, value_hint(field), "");
        self.watch(&text, window, cx);
        CondEd { field, name_op: NameOp::Contains, cmp: Cmp::Greater, text, kinds: Vec::new(), unit: SizeUnit::Mb }
    }

    /// The editor rows for a saved step.
    fn step_of(&mut self, st: &RuleStep, window: &mut Window, cx: &mut Context<Self>) -> StepEd {
        let set = |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| e.update(cx, |s, cx| s.set_value(v, window, cx));
        let at_of = |at: At| match at {
            At::Start => (AtKind::Start, 0),
            At::End => (AtKind::End, 0),
            At::Index(i) => (AtKind::Index, i),
        };
        let kind = match &st.op {
            Op::Replace { .. } => Kind::Replace,
            Op::Insert { .. } => Kind::Insert,
            Op::RemoveRange { .. } => Kind::RemoveRange,
            Op::RemovePattern { .. } => Kind::RemovePattern,
            Op::Case { .. } => Kind::Case,
            Op::Number { .. } => Kind::Number,
            Op::Whitespace { .. } => Kind::Whitespace,
            Op::Extension { .. } => Kind::Extension,
            Op::Template { .. } => Kind::Template,
        };
        let mut s = self.new_step(kind, window, cx);
        match &st.op {
            Op::Replace { find, with, regex, match_case, all, part } => {
                set(&s.a, find.clone(), window, cx);
                set(&s.b, with.clone(), window, cx);
                (s.regex, s.match_case, s.all, s.part) = (*regex, *match_case, *all, *part);
            }
            Op::Insert { text, at, part } => {
                set(&s.a, text.clone(), window, cx);
                let (k, i) = at_of(*at);
                s.at = k;
                set(&s.e, i.to_string(), window, cx);
                s.part = *part;
            }
            Op::RemoveRange { from, count, part } => {
                set(&s.a, from.to_string(), window, cx);
                set(&s.b, count.to_string(), window, cx);
                s.part = *part;
            }
            Op::RemovePattern { pattern, regex, match_case, part } => {
                set(&s.a, pattern.clone(), window, cx);
                (s.regex, s.match_case, s.part) = (*regex, *match_case, *part);
            }
            Op::Case { case, part } => (s.case, s.part) = (*case, *part),
            Op::Number { start, step, pad, at, sep, per_folder } => {
                set(&s.a, start.to_string(), window, cx);
                set(&s.b, step.to_string(), window, cx);
                set(&s.c, pad.to_string(), window, cx);
                set(&s.d, sep.clone(), window, cx);
                let (k, i) = at_of(*at);
                s.at = k;
                set(&s.e, i.to_string(), window, cx);
                s.per_folder = *per_folder;
            }
            Op::Whitespace { trim, collapse, part } => (s.trim, s.collapse, s.part) = (*trim, *collapse, *part),
            Op::Extension { ext } => set(&s.a, ext.clone(), window, cx),
            Op::Template { template, part, start, step, per_folder } => {
                set(&s.a, template.clone(), window, cx);
                set(&s.b, start.to_string(), window, cx);
                set(&s.c, step.to_string(), window, cx);
                (s.part, s.per_folder) = (*part, *per_folder);
            }
        }
        if let Some(c) = &st.when {
            let (field, name_op, cmp, text, kinds, unit) = match c {
                Condition::Name { op, text } => (Field::Name, *op, Cmp::Greater, text.clone(), Vec::new(), SizeUnit::Mb),
                Condition::Extension { list } => (Field::Extension, NameOp::Contains, Cmp::Greater, list.join(", "), Vec::new(), SizeUnit::Mb),
                Condition::Kind { kinds } => (Field::Kind, NameOp::Contains, Cmp::Greater, String::new(), kinds.clone(), SizeUnit::Mb),
                Condition::Size { cmp, bytes } => {
                    let (n, unit) = SizeUnit::split(*bytes);
                    (Field::Size, NameOp::Contains, *cmp, n.to_string(), Vec::new(), unit)
                }
                Condition::Age { field, cmp, days } => {
                    (if *field == TimeField::Modified { Field::Modified } else { Field::Created }, NameOp::Contains, *cmp, days.to_string(), Vec::new(), SizeUnit::Mb)
                }
            };
            let mut ce = self.new_cond(field, window, cx);
            (ce.name_op, ce.cmp, ce.kinds, ce.unit) = (name_op, cmp, kinds, unit);
            set(&ce.text, text, window, cx);
            s.cond = Some(ce);
        }
        s
    }

    fn load_preset(&mut self, p: &Preset, window: &mut Window, cx: &mut Context<Self>) {
        self._subs.clear();
        let mut steps = Vec::new();
        for st in &p.steps {
            steps.push(self.step_of(st, window, cx));
        }
        self.steps = steps;
        self.current = Some(p.name.clone());
        let name = p.name.clone();
        self.preset_name.update(cx, |s, cx| s.set_value(name, window, cx));
        self.watch_name(window, cx);
        self.recompute(cx);
    }

    fn watch_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let e = self.preset_name.clone();
        self._subs.push(cx.subscribe_in(&e, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        }));
    }

    fn rule_list(&self, cx: &App) -> Vec<RuleStep> {
        self.steps.iter().map(|s| s.to_step(cx)).collect()
    }

    fn save_preset(&mut self, cx: &mut Context<Self>) {
        let name = self.preset_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        let steps = self.rule_list(cx);
        self.presets.put(Preset { name: name.clone(), steps });
        self.note = Some(match self.presets.save() {
            Ok(()) => format!("{} \"{name}\"", t!("Saved preset")),
            Err(e) => format!("{}: {e}", t!("The preset could not be saved")),
        });
        self.current = Some(name);
        cx.notify();
    }

    fn delete_preset(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.current.take() else { return };
        self.presets.remove(&name);
        self.note = Some(match self.presets.save() {
            Ok(()) => format!("{} \"{name}\"", t!("Deleted preset")),
            Err(e) => format!("{}: {e}", t!("The preset could not be saved")),
        });
        cx.notify();
    }

    /// Plans again a moment after the last change, off the UI thread (a newer change drops the
    /// older computation).
    fn recompute(&mut self, cx: &mut Context<Self>) {
        self.errors.clear();
        if !self.loaded {
            cx.notify();
            return;
        }
        let steps = self.rule_list(cx);
        if rename_rules::needs_taken(&steps) && !self.taken_read {
            self.read_taken(cx);
        }
        let (items, facts, existing) = (self.items.clone(), self.facts.clone(), self.existing.clone());
        self.computing = true;
        let now = crate::organizer::now_ms();
        self._plan = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(60)).await;
            let rows = cx.background_spawn(async move { rename_rules::plan(&steps, &items, &facts, &existing, NameRules::native(), now) }).await;
            let _ = this.update(cx, |b, cx| {
                b.summary = Summary::of(&rows);
                b.rows = Arc::new(rows);
                b.computing = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The EXIF dates, once, when a step first asks for `{taken}`.
    fn read_taken(&mut self, cx: &mut Context<Self>) {
        self.taken_read = true;
        let facts = self.facts.clone();
        cx.spawn(async move |this, cx| {
            let taken: Vec<i64> = cx.background_spawn(async move { facts.iter().map(|f| leaf_sys::look::decode::exif_taken_ms(&f.path).unwrap_or(0)).collect() }).await;
            let _ = this.update(cx, |b, cx| {
                let mut facts = (*b.facts).clone();
                for (f, t) in facts.iter_mut().zip(taken) {
                    f.taken_ms = t;
                }
                b.facts = Arc::new(facts);
                b.recompute(cx);
            });
        })
        .detach();
    }

    fn can_apply(&self) -> bool {
        self.loaded && !self.computing && !self.applying && self.summary.can_run(self.skip_problems)
    }

    /// Runs the batch in the Status Center, like undo / redo. What has to happen afterwards
    /// (the undo entry, tags following their files, failures being reported) runs in its
    /// `on_done`, so it does not depend on this dialog still being there. Closing the dialog is
    /// not offered while the batch runs.
    pub(crate) fn apply(&mut self, cx: &mut Context<Self>) {
        if !self.can_apply() {
            return;
        }
        let rows: Vec<Row> = (*self.rows).clone();
        let skip = self.skip_problems;
        self.applying = true;
        type Res = Result<crate::rename_exec::Outcome, String>;
        let result: Arc<std::sync::Mutex<Option<Res>>> = Arc::default();
        let slot = result.clone();
        let me = cx.entity().downgrade();
        let title = format!("{} {}", t!("Bulk rename:"), crate::i18n::items(self.summary.rename));
        let id = crate::status_center::start_with_id(
            cx,
            title,
            move |_| {
                let r = crate::rename_exec::apply_batch(&rows, skip, crate::rename_exec::journal_dir().as_deref(), &mut crate::rename_exec::native_rename);
                // the Status Center keeps the failures too (the dialog may be gone by the time it ends)
                let failure = match &r {
                    Ok(out) if out.failed.is_empty() => None,
                    Ok(out) => {
                        let lines: Vec<String> = out.failed.iter().take(8).map(|f| f.line()).collect();
                        Some(format!("{} {}: {}", crate::i18n::items(out.failed.len()), t!("could not be renamed"), lines.join("; ")))
                    }
                    Err(e) => Some(e.clone()),
                };
                *slot.lock().unwrap() = Some(r);
                match failure {
                    None => Ok(true),
                    Some(f) => Err(anyhow::anyhow!(f)),
                }
            },
            Some(Box::new(move |_, cx| {
                let result = result.lock().unwrap().take();
                if let Some(Ok(out)) = &result
                    && !out.done.is_empty()
                {
                    // (the tags of the renamed files follow them, all at once: names may have swapped places)
                    crate::tags::follow(cx, &out.done);
                    crate::undo::push(crate::undo::UndoOp::BulkRenamed { pairs: out.done.clone() });
                }
                let _ = me.update(cx, |v, cx| v.finished(result, cx));
            })),
        );
        if id == 0 {
            self.applying = false;
            self.errors = vec![t!("The rename could not be started").to_string()];
        }
        cx.notify();
    }

    /// The batch ended (`None`: its worker ended without a result).
    fn finished(&mut self, result: Option<Result<crate::rename_exec::Outcome, String>>, cx: &mut Context<Self>) {
        self.applying = false;
        match result {
            Some(Ok(out)) => {
                if out.failed.is_empty() {
                    cx.emit(BulkRenameEvent::Close);
                } else {
                    self.errors = out.failed.iter().map(|f| f.line()).collect();
                }
            }
            Some(Err(e)) => {
                self.errors = vec![e];
                self.loaded = false;
                self.reload(cx);
            }
            None => self.errors = vec![t!("The rename ended unexpectedly: check the folder").to_string()],
        }
        cx.notify();
    }

    /// Reads the folder listings again (after the folder changed under the preview).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = self.rows.iter().map(|r| r.dir.clone()).collect();
        cx.spawn(async move |this, cx| {
            let existing = cx.background_spawn(async move { crate::rename_exec::listings(dirs.iter().map(|d| d.as_path())) }).await;
            let _ = this.update(cx, |b, cx| {
                b.existing = Arc::new(existing);
                b.loaded = true;
                let errors = std::mem::take(&mut b.errors);
                b.recompute(cx);
                b.errors = errors;
            });
        })
        .detach();
    }
}

// ---------------------------------------------------------------- drawing

fn checkbox(id: impl Into<ElementId>, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id(id)
        .gap_2()
        .items_center()
        .cursor_pointer()
        .text_sm()
        .child(
            div()
                .size(px(16.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .border_1()
                .border_color(if on { p.accent } else { p.text_tertiary })
                .when(on, |d| d.bg(p.accent))
                .when(on, |d| d.child(Icon::new(L::Check).size(px(12.)).text_color(p.accent_text))),
        )
        .child(label)
}

fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .cursor_pointer()
        .when(on, |d| d.bg(p.card_bg).text_color(p.text).shadow_sm())
        .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.text_color(p.text)))
        .child(label.into())
}

fn seg() -> Div {
    h_flex().p_0p5().gap_0p5().rounded(theme::RADIUS).flex_shrink_0()
}

fn button(id: impl Into<ElementId>, label: &'static str, primary: bool, enabled: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .px_3()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .text_sm()
        .when(primary, |d| d.bg(p.accent).text_color(p.accent_text))
        .when(!primary, |d| d.bg(p.hover))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
        .when(!enabled, |d| d.opacity(0.45))
        .child(label)
}

fn icon_button(id: impl Into<ElementId>, icon: L, tip: &'static str, enabled: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(theme::RADIUS_SM)
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(p.hover)))
        .when(!enabled, |d| d.opacity(0.35))
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(w, cx))
        .child(Icon::new(icon).size(px(14.)))
}

fn input_box(input: &Entity<InputState>, p: &Palette) -> Div {
    div().h(px(30.)).px_2().flex().items_center().rounded(theme::RADIUS_SM).bg(p.content_bg).border_1().border_color(p.border).child(Input::new(input).appearance(false))
}

fn boxed(w: f32, input: &Entity<InputState>, p: &Palette) -> Div {
    div().w(px(w)).flex_shrink_0().child(input_box(input, p))
}

fn grow(input: &Entity<InputState>, p: &Palette) -> Div {
    div().flex_1().min_w(px(80.)).child(input_box(input, p))
}

fn word(p: &Palette, text: &'static str) -> Div {
    div().flex_shrink_0().text_xs().text_color(p.text_secondary).child(text)
}

fn plural(n: usize, one: &'static str, many: &'static str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "38 will be renamed, 2 unchanged, 1 collision".
fn summary_text(s: &Summary) -> String {
    let mut parts = vec![format!("{} {}", s.rename, t!("will be renamed")), format!("{} {}", s.unchanged, t!("unchanged"))];
    if s.collisions > 0 {
        parts.push(plural(s.collisions, t!("collision"), t!("collisions")));
    }
    if s.other_problems > 0 {
        parts.push(plural(s.other_problems, t!("other problem"), t!("other problems")));
    }
    parts.join(", ")
}

impl BulkRename {
    fn step_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity();
        let s = &self.steps[i];
        let n = self.steps.len();
        let kind = s.kind;
        let me1 = me.clone();
        let kind_box = select_box_h(SharedString::from(format!("br-kind-{i}")), p, kind.label(), 30., move |_| {
            Kind::ALL
                .iter()
                .map(|&k| {
                    let me2 = me1.clone();
                    Choice::new(k.label(), k == kind, move |window, cx| {
                        me2.update(cx, |v, cx| {
                            if let Some(s) = v.steps.get_mut(i)
                                && s.kind != k
                            {
                                Self::set_kind(s, k, window, cx);
                            }
                            v.recompute(cx);
                        })
                    })
                })
                .collect()
        });

        // the boxes of the operation
        let mut fields = h_flex().flex_1().min_w_0().gap_2().items_center();
        fields = match kind {
            Kind::Replace => fields.child(grow(&s.a, p)).child(Icon::new(L::ArrowRight).size(px(13.)).text_color(p.text_tertiary)).child(grow(&s.b, p)),
            Kind::Insert => fields.child(grow(&s.a, p)),
            Kind::RemoveRange => fields.child(word(p, t!("From"))).child(boxed(70., &s.a, p)).child(word(p, t!("count"))).child(boxed(70., &s.b, p)),
            Kind::RemovePattern | Kind::Extension => fields.child(grow(&s.a, p)),
            Kind::Template => fields.child(grow(&s.a, p)),
            Kind::Number => fields
                .child(word(p, t!("Start")))
                .child(boxed(56., &s.a, p))
                .child(word(p, t!("Step")))
                .child(boxed(48., &s.b, p))
                .child(word(p, t!("Digits")))
                .child(boxed(44., &s.c, p))
                .child(word(p, t!("Separator")))
                .child(boxed(44., &s.d, p)),
            Kind::Case => {
                let mut row = seg().bg(p.hover);
                for (ci, c) in [Case::Lower, Case::Upper, Case::Title, Case::Sentence].into_iter().enumerate() {
                    row = row.child(chip(SharedString::from(format!("br-case-{i}-{ci}")), case_label(c), s.case == c, p).on_click(cx.listener(move |v, _, _, cx| {
                        if let Some(s) = v.steps.get_mut(i) {
                            s.case = c;
                        }
                        v.recompute(cx);
                    })));
                }
                fields.child(row)
            }
            Kind::Whitespace => fields
                .child(checkbox(SharedString::from(format!("br-trim-{i}")), t!("Trim the ends"), s.trim, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.trim = !s.trim;
                    }
                    v.recompute(cx);
                })))
                .child(checkbox(SharedString::from(format!("br-collapse-{i}")), t!("One space between words"), s.collapse, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.collapse = !s.collapse;
                    }
                    v.recompute(cx);
                }))),
        };

        let mover = |id: &str, icon: L, tip: &'static str, enabled: bool, delta: isize, cx: &mut Context<Self>| {
            icon_button(SharedString::from(format!("{id}-{i}")), icon, tip, enabled, p).when(enabled, |d| {
                d.on_click(cx.listener(move |v, _, _, cx| {
                    let j = i as isize + delta;
                    if j >= 0 && (j as usize) < v.steps.len() {
                        v.steps.swap(i, j as usize);
                    }
                    v.recompute(cx);
                }))
            })
        };
        let line1 = h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(18.)).flex_shrink_0().text_xs().text_color(p.text_tertiary).child(format!("{}.", i + 1)))
            .child(div().w(px(190.)).flex_shrink_0().child(kind_box))
            .child(fields)
            .child(mover("br-up", L::ChevronUp, t!("Move up"), i > 0, -1, cx))
            .child(mover("br-down", L::ChevronDown, t!("Move down"), i + 1 < n, 1, cx))
            .child(icon_button(SharedString::from(format!("br-del-{i}")), L::X, t!("Remove step"), true, p).on_click(cx.listener(move |v, _, _, cx| {
                if i < v.steps.len() {
                    v.steps.remove(i);
                }
                v.recompute(cx);
            })));

        // options: the part of the name, where, regex / case, per folder
        let mut opts = h_flex().gap_3().items_center().flex_wrap().pl(px(26.));
        let has_part = matches!(kind, Kind::Replace | Kind::Insert | Kind::RemoveRange | Kind::RemovePattern | Kind::Case | Kind::Whitespace | Kind::Template);
        if has_part {
            let mut row = seg().bg(p.hover);
            for (pi, part) in [Part::Name, Part::Ext, Part::Whole].into_iter().enumerate() {
                row = row.child(chip(SharedString::from(format!("br-part-{i}-{pi}")), part_label(part), s.part == part, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.part = part;
                    }
                    v.recompute(cx);
                })));
            }
            opts = opts.child(row);
        }
        if matches!(kind, Kind::Insert | Kind::Number) {
            let mut row = seg().bg(p.hover);
            for (ai, at) in [AtKind::Start, AtKind::End, AtKind::Index].into_iter().enumerate() {
                row = row.child(chip(SharedString::from(format!("br-at-{i}-{ai}")), at_label(at), s.at == at, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.at = at;
                    }
                    v.recompute(cx);
                })));
            }
            opts = opts.child(row);
            if s.at == AtKind::Index {
                opts = opts.child(boxed(56., &s.e, p));
            }
        }
        if matches!(kind, Kind::Replace | Kind::RemovePattern) {
            opts = opts
                .child(checkbox(SharedString::from(format!("br-mc-{i}")), t!("Match case"), s.match_case, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.match_case = !s.match_case;
                    }
                    v.recompute(cx);
                })))
                .child(checkbox(SharedString::from(format!("br-re-{i}")), t!("Regular expression"), s.regex, p).on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(s) = v.steps.get_mut(i) {
                        s.regex = !s.regex;
                    }
                    v.recompute(cx);
                })));
        }
        if kind == Kind::Replace {
            opts = opts.child(checkbox(SharedString::from(format!("br-all-{i}")), t!("Every occurrence"), s.all, p).on_click(cx.listener(move |v, _, _, cx| {
                if let Some(s) = v.steps.get_mut(i) {
                    s.all = !s.all;
                }
                v.recompute(cx);
            })));
        }
        if matches!(kind, Kind::Number | Kind::Template) {
            if kind == Kind::Template {
                opts = opts.child(word(p, t!("Counter start"))).child(boxed(56., &s.b, p)).child(word(p, t!("step"))).child(boxed(48., &s.c, p));
            }
            opts = opts.child(checkbox(SharedString::from(format!("br-pf-{i}")), t!("Restart in every folder"), s.per_folder, p).on_click(cx.listener(move |v, _, _, cx| {
                if let Some(s) = v.steps.get_mut(i) {
                    s.per_folder = !s.per_folder;
                }
                v.recompute(cx);
            })));
        }
        // the condition: "Only if ..." or a button to add one
        let cond: AnyElement = match &s.cond {
            None => h_flex()
                .id(SharedString::from(format!("br-addcond-{i}")))
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(p.text_secondary)
                .cursor_pointer()
                .hover(|d| d.text_color(p.text))
                .child(Icon::new(L::Plus).size(px(12.)))
                .child(t!("Only for some items…"))
                .on_click(cx.listener(move |v, _, window, cx| {
                    let c = v.new_cond(Field::Extension, window, cx);
                    c.text.update(cx, |s, cx| s.focus(window, cx));
                    if let Some(s) = v.steps.get_mut(i) {
                        s.cond = Some(c);
                    }
                    v.recompute(cx);
                }))
                .into_any_element(),
            Some(c) => self.cond_row(i, c, s.cond_error(cx), p, &me, cx),
        };
        opts = opts.child(cond);

        v_flex()
            .id(SharedString::from(format!("br-step-{i}")))
            .gap_1p5()
            .p_2()
            .rounded(theme::RADIUS_SM)
            .border_1()
            .border_color(p.divider)
            .child(line1)
            .child(opts)
            .into_any_element()
    }

    fn cond_row(&self, i: usize, c: &CondEd, error: Option<&'static str>, p: &Palette, me: &Entity<Self>, cx: &mut Context<Self>) -> AnyElement {
        let field = c.field;
        let me1 = me.clone();
        let field_box = select_box_fill(SharedString::from(format!("br-cf-{i}")), p, field.label(), 28., move |_| {
            Field::ALL
                .iter()
                .map(|&f| {
                    let me2 = me1.clone();
                    Choice::new(f.label(), f == field, move |window, cx| {
                        me2.update(cx, |v, cx| {
                            if let Some(c) = v.steps.get_mut(i).and_then(|s| s.cond.as_mut())
                                && c.field != f
                            {
                                c.field = f;
                                c.cmp = Cmp::Greater;
                                c.kinds.clear();
                                c.text.update(cx, |s, cx| {
                                    s.set_value("", window, cx);
                                    s.set_placeholder(value_hint(f), window, cx);
                                });
                            }
                            v.recompute(cx);
                        })
                    })
                })
                .collect()
        });
        let me1 = me.clone();
        let op_box: AnyElement = match field {
            Field::Name => {
                let cur = c.name_op;
                select_box_h(SharedString::from(format!("br-cop-{i}")), p, name_op_label(cur), 28., move |_| {
                    [NameOp::Contains, NameOp::StartsWith, NameOp::EndsWith, NameOp::Wildcard]
                        .iter()
                        .map(|&op| {
                            let me2 = me1.clone();
                            Choice::new(name_op_label(op), op == cur, move |_, cx| {
                                me2.update(cx, |v, cx| {
                                    if let Some(c) = v.steps.get_mut(i).and_then(|s| s.cond.as_mut()) {
                                        c.name_op = op;
                                    }
                                    v.recompute(cx);
                                })
                            })
                        })
                        .collect()
                })
            }
            Field::Size | Field::Modified | Field::Created => {
                let cur = c.cmp;
                select_box_h(SharedString::from(format!("br-cop-{i}")), p, cmp_label(field, cur), 28., move |_| {
                    [Cmp::Greater, Cmp::Less]
                        .iter()
                        .map(|&cm| {
                            let me2 = me1.clone();
                            Choice::new(cmp_label(field, cm), cm == cur, move |_, cx| {
                                me2.update(cx, |v, cx| {
                                    if let Some(c) = v.steps.get_mut(i).and_then(|s| s.cond.as_mut()) {
                                        c.cmp = cm;
                                    }
                                    v.recompute(cx);
                                })
                            })
                        })
                        .collect()
                })
            }
            Field::Extension | Field::Kind => word(p, t!("is one of")).into_any_element(),
        };
        let value: AnyElement = match field {
            Field::Kind => {
                let mut chips = h_flex().flex_wrap().gap_1().min_w_0();
                for (ki, k) in FileKind::ALL.iter().copied().enumerate() {
                    let on = c.kinds.contains(&k);
                    chips = chips.child(chip(SharedString::from(format!("br-ck-{i}-{ki}")), kind_label(k), on, p).when(on, |d| d.border_1().border_color(p.accent)).on_click(cx.listener(
                        move |v, _, _, cx| {
                            if let Some(c) = v.steps.get_mut(i).and_then(|s| s.cond.as_mut()) {
                                match c.kinds.iter().position(|x| *x == k) {
                                    Some(at) => {
                                        c.kinds.remove(at);
                                    }
                                    None => c.kinds.push(k),
                                }
                            }
                            v.recompute(cx);
                        },
                    )));
                }
                chips.into_any_element()
            }
            Field::Size => {
                let unit = c.unit;
                let me1 = me.clone();
                h_flex()
                    .gap_2()
                    .child(boxed(70., &c.text, p))
                    .child(div().w(px(80.)).flex_shrink_0().child(select_box_fill(SharedString::from(format!("br-cu-{i}")), p, unit.label(), 28., move |_| {
                        SizeUnit::ALL
                            .iter()
                            .map(|&u| {
                                let me2 = me1.clone();
                                Choice::new(u.label(), u == unit, move |_, cx| {
                                    me2.update(cx, |v, cx| {
                                        if let Some(c) = v.steps.get_mut(i).and_then(|s| s.cond.as_mut()) {
                                            c.unit = u;
                                        }
                                        v.recompute(cx);
                                    })
                                })
                            })
                            .collect()
                    })))
                    .into_any_element()
            }
            Field::Modified | Field::Created => h_flex().gap_2().items_center().child(boxed(60., &c.text, p)).child(word(p, t!("days ago:"))).into_any_element(),
            Field::Name | Field::Extension => boxed(150., &c.text, p).into_any_element(),
        };
        let is_age = matches!(field, Field::Modified | Field::Created);
        let mut line = h_flex().gap_2().items_center().child(word(p, t!("Only if"))).child(div().w(px(150.)).flex_shrink_0().child(field_box));
        line = if is_age { line.child(value).child(div().flex_shrink_0().child(op_box)) } else { line.child(div().flex_shrink_0().child(op_box)).child(value) };
        line.child(icon_button(SharedString::from(format!("br-cdel-{i}")), L::X, t!("Remove condition"), true, p).on_click(cx.listener(move |v, _, _, cx| {
            if let Some(s) = v.steps.get_mut(i) {
                s.cond = None;
            }
            v.recompute(cx);
        })))
        .when_some(error, |d, e| d.child(div().text_xs().text_color(p.danger).child(e)))
        .into_any_element()
    }
}

impl Render for BulkRename {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        let me = cx.entity();
        let can_apply = self.can_apply();
        let has_errors = !self.errors.is_empty();
        let total = self.rows.len();

        let header = h_flex()
            .px_4()
            .py_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::List).size(px(18.)).text_color(p.text_secondary))
            .child(
                v_flex()
                    .flex_1()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(t!("Bulk rename")))
                    .child(div().text_xs().text_color(p.text_secondary).child(crate::i18n::items(total))),
            )
            .child(
                div()
                    .id("br-close")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme::RADIUS_SM)
                    .cursor_pointer()
                    .hover(|s| s.bg(p.hover))
                    .child(Icon::new(L::X).size(px(14.)))
                    .when(self.applying, |d| d.opacity(0.35))
                    .on_click(cx.listener(|v, _, _, cx| {
                        // (not while the batch runs: see `apply`)
                        if !v.applying {
                            cx.emit(BulkRenameEvent::Close)
                        }
                    })),
            );

        // presets: pick one, save the steps under a name, delete the user's own
        let me1 = me.clone();
        let all = self.presets.all();
        let current = self.current.clone();
        let picker = select_box_h("br-preset", &p, current.clone().unwrap_or_else(|| t!("Presets…").to_string()), 30., move |_| {
            all.iter()
                .map(|(pr, builtin)| {
                    let me2 = me1.clone();
                    let pr2 = pr.clone();
                    let label = if *builtin { pr.name.clone() } else { format!("{} ({})", pr.name, t!("saved")) };
                    Choice::new(label, current.as_deref() == Some(pr.name.as_str()), move |window, cx| {
                        me2.update(cx, |v, cx| v.load_preset(&pr2, window, cx));
                    })
                })
                .collect()
        });
        let name_now = self.preset_name.read(cx).value().trim().to_string();
        let is_user = self.current.as_ref().is_some_and(|c| self.presets.user.iter().any(|u| u.name.eq_ignore_ascii_case(c)));
        let presets = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(div().w(px(260.)).flex_shrink_0().child(picker))
            .child(div().flex_1())
            .child(div().w(px(200.)).flex_shrink_0().child(input_box(&self.preset_name, &p)))
            .child(button("br-save", t!("Save as preset"), false, !name_now.is_empty(), &p).on_click(cx.listener(|v, _, _, cx| v.save_preset(cx))))
            .when(is_user, |d| d.child(button("br-delpreset", t!("Delete preset"), false, true, &p).on_click(cx.listener(|v, _, _, cx| v.delete_preset(cx)))));

        // the steps
        let mut steps = v_flex().gap_2();
        for i in 0..self.steps.len() {
            steps = steps.child(self.step_row(i, &p, cx));
        }
        let me1 = me.clone();
        let add = h_flex().gap_2().items_center().child(div().w(px(220.)).child(select_box_h("br-add", &p, t!("Add a step…"), 30., move |_| {
            Kind::ALL
                .iter()
                .map(|&k| {
                    let me2 = me1.clone();
                    Choice::new(k.label(), false, move |window, cx| me2.update(cx, |v, cx| v.add_step(k, window, cx)))
                })
                .collect()
        })));
        let add = add.when_some(self.note.clone(), |d, n| d.child(div().text_xs().text_color(p.text_secondary).child(n)));
        let hint = div()
            .text_xs()
            .text_color(p.text_tertiary)
            .child(t!("Tokens: {name} {ext} {parent} {n} {n:3} {size} {modified:%Y-%m-%d} {created:…} {taken:…}. Conditions look at the file as it is now."));
        let rules = div()
            .id("br-steps")
            .max_h(px(220.))
            .overflow_y_scroll()
            .px_4()
            .py_2()
            .child(v_flex().gap_2().child(steps).child(add).child(hint));

        let list_head = h_flex()
            .px_4()
            .py_1()
            .gap_2()
            .text_xs()
            .text_color(p.text_secondary)
            .border_t_1()
            .border_b_1()
            .border_color(p.divider)
            .child(div().flex_1().child(t!("Old name")))
            .child(div().w(px(16.)))
            .child(div().flex_1().child(t!("New name")))
            .child(div().w(px(220.)).child(t!("Status")));

        let list = uniform_list(
            "br-rows",
            total,
            cx.processor(move |this, range: std::ops::Range<usize>, window, _cx| {
                let p = Palette::for_window(window);
                range
                    .map(|i| {
                        let r = &this.rows[i];
                        let (status, color, bad) = match &r.status {
                            Status::Rename => (String::new(), p.text_secondary, false),
                            Status::CaseOnly => (t!("letter case only").to_string(), p.text_secondary, false),
                            Status::Unchanged => (t!("unchanged").to_string(), p.text_tertiary, false),
                            Status::Problem(e) => (crate::i18n::td(&e.to_string()), p.danger, true),
                        };
                        let changed = r.status.runs();
                        let new_text = if r.new.is_empty() { "—".to_string() } else { r.new.clone() };
                        h_flex()
                            .id(("br-row", i))
                            .w_full()
                            .h(px(26.))
                            .px_3()
                            .gap_2()
                            .items_center()
                            .rounded(theme::RADIUS_SM)
                            .text_sm()
                            .when(bad, |d| d.bg(p.danger.opacity(0.10)))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(p.text_secondary).child(r.old.clone()))
                            .child(Icon::new(L::ArrowRight).size(px(14.)).text_color(p.text_tertiary))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .when(changed, |d| d.text_color(p.text).font_weight(FontWeight::MEDIUM))
                                    .when(!changed, |d| d.text_color(p.text_tertiary))
                                    .child(new_text),
                            )
                            .child(
                                h_flex()
                                    .w(px(220.))
                                    .min_w_0()
                                    .gap_1()
                                    .items_center()
                                    .text_xs()
                                    .text_color(color)
                                    .when(bad, |d| d.child(Icon::new(L::TriangleAlert).size(px(12.))))
                                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(status)),
                            )
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .p_1();

        let s = self.summary;
        let status = if has_errors {
            plural(self.errors.len(), t!("item could not be renamed"), t!("items could not be renamed"))
        } else if self.applying {
            t!("Renaming…").to_string()
        } else if !self.loaded {
            t!("Reading the items…").to_string()
        } else {
            summary_text(&s)
        };
        let warn = has_errors || s.problems() > 0;

        let footer = h_flex()
            .px_4()
            .py_3()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(p.divider)
            .child(div().flex_1().min_w_0().text_sm().text_color(if warn { p.danger } else { p.text_secondary }).child(status))
            .when(s.problems() > 0, |d| {
                d.child(checkbox("br-skip", t!("Skip items with problems"), self.skip_problems, &p).on_click(cx.listener(|v, _, _, cx| {
                    v.skip_problems = !v.skip_problems;
                    cx.notify();
                })))
            })
            .child(button("br-cancel", if has_errors { t!("Close") } else { t!("Cancel") }, false, !self.applying, &p).on_click(cx.listener(|v, _, _, cx| {
                if !v.applying {
                    cx.emit(BulkRenameEvent::Close)
                }
            })))
            .child(button("br-apply", t!("Rename"), true, can_apply, &p).on_click(cx.listener(|this, _, _, cx| this.apply(cx))));

        let errors_box = has_errors.then(|| {
            div()
                .id("br-errors")
                .max_h(px(110.))
                .overflow_y_scroll()
                .px_4()
                .py_2()
                .bg(p.danger.opacity(0.10))
                .text_xs()
                .text_color(p.danger)
                .child(v_flex().gap_0p5().children(self.errors.iter().map(|e| div().child(crate::i18n::td(e)))))
        });

        v_flex()
            .w(px(940.))
            .h(px(700.))
            .rounded(px(12.))
            .overflow_hidden()
            .bg(p.card_bg)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .text_color(p.text)
            .child(header)
            .child(presets)
            .child(rules)
            .child(list_head)
            .child(list)
            .children(errors_box)
            .child(footer)
    }
}
