//! Folder organizer (rule based): the pure part. No file system access in here except the rules
//! file in [`store`]; the planning ([`plan`]) works on lists of file facts the caller collected.
//!
//! * [`rule`]: what a rule is, how its conditions match, the template variables.
//! * [`plan`]: rules + facts -> a list of steps (move / rename / recycle) that never overwrite.
//! * [`safety`]: folders that may not be watched, files that must be left alone.
//! * [`store`]: the versioned rules file.
//! * [`summary`]: a rule said in words.

/// Marks a user-facing text in this module (English). It is the identity; a test in rf-app finds
/// every `T!("...")` in these files and checks that it is English (the texts go through the app's
/// own i18n hook where they are drawn).
macro_rules! T {
    ($s:literal) => {
        $s
    };
}

pub mod plan;
pub mod rule;
pub mod safety;
pub mod store;
pub mod summary;

pub use plan::{Dest, Plan, PlanContext, SkipReason, Skipped, Step, Unusable, numbered_name, plan, preview_dest};
pub use rule::{Action, Cmp, Condition, FileFact, FileKind, Join, NameOp, Rule, RuleProblem, TEMPLATE_VARS, TimeField};
pub use safety::{ProtectedPaths, Refusal, is_partial_download};
pub use store::{LoadOutcome, RulesFile};
