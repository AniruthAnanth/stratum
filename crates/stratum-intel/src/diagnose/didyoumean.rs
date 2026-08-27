//! Candidate sets for "Did you mean …?" — design 07 §6.1.
//!
//! The metric lives in [`crate::similarity`]; what lives here is the decision
//! about **which names are even candidates**, which is where the accuracy comes
//! from. Ranking a mistyped variable name against the command table would offer
//! `preserve` for `prise`; ranking it against the live varlist offers `price`.
//!
//! Every set is built without a network call, without an API key, and — apart
//! from the file's own text — without anything this crate had to be given.

use crate::lints::Doc;
use crate::similarity::{is_decisive, rank, Scored};
use crate::{Env, ParseIndex};

/// What a candidate is, which decides how the fix is worded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// A variable in the loaded dataset.
    LiveVariable,
    /// A variable an unexecuted block above would create. Design 07 §6.1: this
    /// gets the answer "not created yet — run block B4 first", which is a
    /// better reply than any model would give, and it is available only because
    /// the execution ledger knows which blocks ran.
    PendingVariable,
    /// A macro, scalar, matrix, stored result or value label.
    SessionName,
    /// A built-in command.
    Command,
    /// A user `program define` earlier in this file.
    UserProgram,
    /// A command resolvable from the ado path.
    AdoCommand,
    /// An option in the current command's grammar.
    Option,
}

/// One ranked candidate.
#[derive(Clone, PartialEq, Debug)]
pub struct Candidate {
    /// The name.
    pub name: String,
    /// Where it came from.
    pub origin: Origin,
    /// Jaro–Winkler against the typed token.
    pub score: f64,
}

/// A ranked candidate list plus whether it justifies one confident answer.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Suggestions {
    /// Best first, at most three.
    pub candidates: Vec<Candidate>,
    /// `top - second >= 0.08`, or a sole candidate.
    pub decisive: bool,
}

impl Suggestions {
    fn from(mut scored: Vec<(Scored<&str>, Origin)>) -> Suggestions {
        // A pending variable always wins its tie: "run block B4 first" is a
        // correct and complete answer, where a same-scoring live name is a
        // guess.
        scored.sort_by(|a, b| {
            let pending = |o: Origin| u8::from(o != Origin::PendingVariable);
            pending(a.1)
                .cmp(&pending(b.1))
                .then_with(|| {
                    b.0.score
                        .partial_cmp(&a.0.score)
                        .unwrap_or(core::cmp::Ordering::Equal)
                })
                .then_with(|| a.0.item.cmp(b.0.item))
        });
        scored.truncate(3);
        let plain: Vec<Scored<&str>> = scored.iter().map(|(s, _)| s.clone()).collect();
        Suggestions {
            decisive: is_decisive(&plain),
            candidates: scored
                .into_iter()
                .map(|(s, origin)| Candidate {
                    name: s.item.to_owned(),
                    origin,
                    score: s.score,
                })
                .collect(),
        }
    }
}

/// r(111) — variable or element not found.
///
/// Candidates: the live varlist, variables created by *unexecuted* blocks
/// above, macro names, scalars, matrices, `e()`/`r()`/`s()` names, stored
/// estimates, and value-label names.
#[must_use]
pub fn for_variable(typed: &str, env: &Env) -> Suggestions {
    let mut scored: Vec<(Scored<&str>, Origin)> = Vec::new();
    push(&mut scored, typed, live(env), Origin::LiveVariable);
    push(
        &mut scored,
        typed,
        env.pending_vars.iter().map(|p| p.name.as_str()),
        Origin::PendingVariable,
    );
    push(&mut scored, typed, session_names(env), Origin::SessionName);
    Suggestions::from(scored)
}

/// r(199) — unrecognized command.
///
/// Candidates: the built-in command table, commands resolvable from the ado
/// path, and user programs defined earlier in this file.
#[must_use]
pub fn for_command(typed: &str, env: &Env, doc: &Doc<'_>) -> Suggestions {
    let mut scored: Vec<(Scored<&str>, Origin)> = Vec::new();
    let builtins: Vec<&'static str> = stratum_parse::all_commands()
        .iter()
        .map(|s| s.canonical)
        .collect();
    push(
        &mut scored,
        typed,
        builtins.iter().copied(),
        Origin::Command,
    );
    push(
        &mut scored,
        typed,
        env.installed_ado.iter().map(String::as_str),
        Origin::AdoCommand,
    );
    let programs = user_programs(doc);
    push(
        &mut scored,
        typed,
        programs.iter().map(String::as_str),
        Origin::UserProgram,
    );
    Suggestions::from(scored)
}

/// r(198) — an option the command does not take.
///
/// Candidates: that command's option grammar and nothing else, which is why
/// `detial` offers `detail` rather than every option in Stata.
#[must_use]
pub fn for_option(typed: &str, command: &str) -> Suggestions {
    let Some(sig) = stratum_parse::table().canonical(command) else {
        return Suggestions::default();
    };
    let names: Vec<&'static str> = sig.options.iter().map(|o| o.canonical).collect();
    let mut scored: Vec<(Scored<&str>, Origin)> = Vec::new();
    push(&mut scored, typed, names.iter().copied(), Origin::Option);
    Suggestions::from(scored)
}

/// Every `program define <name>` at or above statement `before` in the file.
#[must_use]
pub fn user_programs(doc: &Doc<'_>) -> Vec<String> {
    use stratum_parse::ast::{BlockCommand, Command};
    doc.stmts
        .iter()
        .filter_map(|st| match &st.ast.cmd {
            Command::Block(b) => match b.as_ref() {
                BlockCommand::Program { name, .. } => Some(name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn live(env: &Env) -> impl Iterator<Item = &str> {
    env.varnames.iter().flatten().map(String::as_str)
}

fn session_names(env: &Env) -> impl Iterator<Item = &str> {
    [
        &env.locals,
        &env.globals,
        &env.scalars,
        &env.matrices,
        &env.e_names,
        &env.r_names,
        &env.s_names,
        &env.value_labels,
        &env.stored_estimates,
    ]
    .into_iter()
    .flat_map(|v| v.iter().map(String::as_str))
}

fn push<'a, I: Iterator<Item = &'a str>>(
    out: &mut Vec<(Scored<&'a str>, Origin)>,
    typed: &str,
    names: I,
    origin: Origin,
) {
    for s in rank(typed, names, 3) {
        out.push((s, origin));
    }
}

/// The command word governing the statement that covers `pos`, when it
/// resolved. Used to pick the option grammar for r(198).
#[must_use]
pub fn command_at(idx: &ParseIndex<'_>, doc: &Doc<'_>, pos: u32) -> Option<&'static str> {
    let _ = idx;
    doc.stmts
        .iter()
        .find(|st| pos >= st.span.start && pos <= st.span.end)
        .and_then(|st| st.canonical)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::PendingVar;

    fn auto_env() -> Env {
        Env {
            varnames: Some(
                [
                    "make",
                    "price",
                    "mpg",
                    "rep78",
                    "headroom",
                    "trunk",
                    "weight",
                    "length",
                    "turn",
                    "displacement",
                    "gear_ratio",
                    "foreign",
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            ),
            ..Env::default()
        }
    }

    #[test]
    fn the_golden_typo_gets_one_confident_answer() {
        // `tests/golden/stata18/errors.log`: `summarize incom` -> r(111).
        let mut env = auto_env();
        env.varnames
            .get_or_insert_with(Vec::new)
            .push("income".to_owned());
        let s = for_variable("incom", &env);
        assert!(s.decisive, "{s:?}");
        assert_eq!(
            s.candidates.first().map(|c| c.name.as_str()),
            Some("income")
        );
        assert_eq!(s.candidates[0].origin, Origin::LiveVariable);
    }

    #[test]
    fn a_name_with_no_neighbour_offers_nothing() {
        // The golden's other r(111): `summarize nosuchvar`.
        let s = for_variable("nosuchvar", &auto_env());
        assert!(s.candidates.is_empty(), "{s:?}");
        assert!(!s.decisive);
    }

    #[test]
    fn a_pending_variable_beats_a_live_lookalike() {
        let mut env = auto_env();
        env.pending_vars.push(PendingVar {
            name: "ln_price".to_owned(),
            block_label: "B4".to_owned(),
        });
        let s = for_variable("ln_pric", &env);
        assert_eq!(
            s.candidates.first().map(|c| c.origin),
            Some(Origin::PendingVariable)
        );
    }

    #[test]
    fn an_unrecognized_command_ranks_against_commands_only() {
        let idx = ParseIndex::new("");
        let doc = Doc::build(&idx);
        let s = for_command("regres", &Env::default(), &doc);
        assert_eq!(
            s.candidates.first().map(|c| c.name.as_str()),
            Some("regress")
        );
        assert!(s.decisive, "{s:?}");
    }

    #[test]
    fn a_user_program_is_a_candidate_command() {
        let src = "program define mytable\n    di 1\nend\nmytabel\n";
        let idx = ParseIndex::new(src);
        let doc = Doc::build(&idx);
        let s = for_command("mytabel", &Env::default(), &doc);
        assert_eq!(
            s.candidates.first().map(|c| (c.name.as_str(), c.origin)),
            Some(("mytable", Origin::UserProgram))
        );
    }

    #[test]
    fn a_bad_option_ranks_against_that_commands_grammar() {
        // The golden's `summarize price, detial` -> r(198).
        let s = for_option("detial", "summarize");
        assert_eq!(
            s.candidates.first().map(|c| c.name.as_str()),
            Some("detail")
        );
        // ... and its `summarize price, nosuchoption` offers nothing.
        assert!(for_option("nosuchoption", "summarize")
            .candidates
            .is_empty());
    }

    #[test]
    fn an_unknown_command_has_no_option_grammar_to_rank_against() {
        assert!(for_option("detial", "nosuchcommand").candidates.is_empty());
    }
}
