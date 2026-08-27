//! Diagnosis — return-code cards and deterministic quick fixes (design 07 §6.1,
//! spec §21).
//!
//! This is the module that makes spec §21's headline feature not an AI feature.
//! `Did you mean 'income'?` is [`similarity::jaro_winkler`] over the live
//! varlist: microseconds, offline, free, no failure mode, and *correct* rather
//! than *plausible*.
//!
//! [`similarity`]: crate::similarity

pub mod didyoumean;
pub mod quickfix;
pub mod rc_table;

pub use didyoumean::{for_command, for_option, for_variable, Candidate, Origin, Suggestions};
pub use quickfix::{quick_fixes, quick_fixes_with, ExplainSource, FixKind, QuickFix};
pub use rc_table::{card, RcCard, CARDS};
