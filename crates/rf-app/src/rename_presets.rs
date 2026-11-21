//! Saved bulk rename rule lists: `rename-presets.toml` in the config folder
//! (`settings::app_dir`, so `LEAF_CONFIG_DIR` moves it too). A file that cannot be read is
//! logged and ignored (the built-in presets still show); before the next save it is kept aside
//! as `rename-presets.toml.corrupt-<ms>`, so what the user had is never silently lost.

use std::path::{Path, PathBuf};

use rf_core::rename_rules::{Preset, builtin_presets};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct PresetFile {
    #[serde(default, rename = "preset")]
    presets: Vec<Preset>,
}

#[derive(Debug, Clone, Default)]
pub struct Presets {
    /// The user's own, in the order they were saved.
    pub user: Vec<Preset>,
    /// The file was there but could not be read.
    broken: bool,
}

pub fn path() -> Option<PathBuf> {
    crate::settings::app_dir().map(|d| d.join("rename-presets.toml"))
}

pub fn load() -> Presets {
    path().map(|p| load_from(&p)).unwrap_or_default()
}

pub fn load_from(p: &Path) -> Presets {
    match rf_core::read_config(p) {
        Ok(text) => match toml::from_str::<PresetFile>(&text) {
            Ok(f) => Presets { user: f.presets.into_iter().filter(|p| !p.name.trim().is_empty()).collect(), broken: false },
            Err(e) => {
                log::warn!("rename presets: {} cannot be read, ignored: {e}", p.display());
                Presets { user: Vec::new(), broken: true }
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presets::default(),
        Err(e) => {
            log::warn!("rename presets: {} cannot be read, ignored: {e}", p.display());
            Presets { user: Vec::new(), broken: true }
        }
    }
}

impl Presets {
    /// Built-in presets first, then the user's (a user preset with a built-in's name hides it).
    pub fn all(&self) -> Vec<(Preset, bool)> {
        let mut out: Vec<(Preset, bool)> =
            builtin_presets().into_iter().filter(|b| !self.user.iter().any(|u| same_name(&u.name, &b.name))).map(|b| (b, true)).collect();
        out.extend(self.user.iter().cloned().map(|u| (u, false)));
        out
    }

    /// Adds `preset`, or replaces the user's preset of the same name.
    pub fn put(&mut self, preset: Preset) {
        match self.user.iter_mut().find(|u| same_name(&u.name, &preset.name)) {
            Some(u) => *u = preset,
            None => self.user.push(preset),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.user.retain(|u| !same_name(&u.name, name));
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        let p = path().ok_or_else(|| anyhow::anyhow!("no config folder"))?;
        self.save_to(&p)
    }

    pub fn save_to(&mut self, p: &Path) -> anyhow::Result<()> {
        if self.broken && p.exists() {
            rf_core::backup_corrupt(p);
        }
        self.broken = false;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = toml::to_string_pretty(&PresetFile { presets: self.user.clone() })?;
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, p)?;
        Ok(())
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core::organize::{Condition, NameOp};
    use rf_core::rename_rules::{At, Case, Op, Part, RuleStep};

    fn dir(tag: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join(format!("rename-presets-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn every_op() -> Vec<RuleStep> {
        vec![
            RuleStep::new(Op::Replace { find: "a".into(), with: "$1".into(), regex: true, match_case: true, all: false, part: Part::Whole }),
            RuleStep::new(Op::Insert { text: "x".into(), at: At::Index(-2), part: Part::Name }).when(Condition::Extension { list: vec!["jpg".into(), "png".into()] }),
            RuleStep::new(Op::RemoveRange { from: 1, count: 2, part: Part::Name }),
            RuleStep::new(Op::RemovePattern { pattern: " (copy)".into(), regex: false, match_case: false, part: Part::Name }),
            RuleStep::new(Op::Case { case: Case::Sentence, part: Part::Ext }).when(Condition::Name { op: NameOp::Wildcard, text: "*.JPG".into() }),
            RuleStep::new(Op::Number { start: 5, step: -1, pad: 3, at: At::End, sep: "_".into(), per_folder: true }),
            RuleStep::new(Op::Whitespace { trim: true, collapse: false, part: Part::Whole }),
            RuleStep::new(Op::Extension { ext: "md".into() }),
            RuleStep::new(Op::Template { template: "{modified:%Y} {name}".into(), part: Part::Name, start: 1, step: 1, per_folder: false }),
        ]
    }

    #[test]
    fn presets_round_trip_through_the_toml_file() {
        let d = dir("trip");
        let p = d.join("rename-presets.toml");
        let mut ps = load_from(&p);
        assert!(ps.user.is_empty());
        assert_eq!(ps.all().len(), builtin_presets().len());
        ps.put(Preset { name: "Everything".into(), steps: every_op() });
        ps.put(Preset { name: "Lowercase extension".into(), steps: vec![] });
        ps.save_to(&p).unwrap();
        let back = load_from(&p);
        assert_eq!(back.user, ps.user);
        // the user's preset hides the built-in one of the same name
        assert_eq!(back.all().iter().filter(|(p, _)| p.name == "Lowercase extension").count(), 1);
        assert!(back.all().iter().any(|(p, builtin)| p.name == "Lowercase extension" && !builtin));
        // put replaces by name, remove removes
        let mut ps = back;
        ps.put(Preset { name: "everything ".into(), steps: vec![] });
        assert_eq!(ps.user.len(), 2);
        ps.remove("EVERYTHING");
        assert_eq!(ps.user.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_file_is_ignored_and_kept_aside_before_the_next_save() {
        let d = dir("broken");
        let p = d.join("rename-presets.toml");
        std::fs::write(&p, "[[preset]]\nname = \"x\"\n[[preset.steps]]\nop = \"no_such_op\"\n").unwrap();
        let ps = load_from(&p);
        assert!(ps.user.is_empty());
        assert_eq!(ps.all().len(), builtin_presets().len());
        std::fs::write(&p, "this is { not toml").unwrap();
        let mut ps2 = load_from(&p);
        assert!(ps2.user.is_empty());
        ps2.put(Preset { name: "New".into(), steps: vec![] });
        ps2.save_to(&p).unwrap();
        let kept: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.iter().any(|n| n.starts_with("rename-presets.toml.corrupt-")), "{kept:?}");
        assert_eq!(load_from(&p).user.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_hand_written_file_with_defaults_loads() {
        let d = dir("hand");
        let p = d.join("rename-presets.toml");
        std::fs::write(
            &p,
            r#"
[[preset]]
name = "Photos"

[[preset.steps]]
op = "template"
template = "{taken:%Y-%m-%d} {name}"

[[preset.steps]]
op = "case"
case = "lower"
part = "ext"
when = { type = "extension", list = ["JPG"] }
"#,
        )
        .unwrap();
        let ps = load_from(&p);
        assert_eq!(ps.user.len(), 1);
        assert_eq!(ps.user[0].steps.len(), 2);
        assert_eq!(ps.user[0].steps[1].when, Some(Condition::Extension { list: vec!["JPG".into()] }));
        let _ = std::fs::remove_dir_all(&d);
    }
}
