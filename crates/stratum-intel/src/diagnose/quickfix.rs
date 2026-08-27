//! Error quick fixes — design 07 §6.1.
//!
//! Trigger: an execution completes with `rc != 0`. [`quick_fixes`] runs
//! **synchronously** on the result and returns in well under a millisecond,
//! because it is a rank over a few hundred strings and a binary search in a
//! compiled table.
//!
//! # Where the AI is, and is not
//!
//! The AI enters only when **both** conditions hold: no deterministic fix
//! reached [`Confidence::Exact`], **and** the user clicked `[Explain]`. Never
//! automatic, never on hover, never a popup. That is why [`ExplainSource`] has
//! exactly two variants and why the static one is the default: with no provider
//! configured, every error still renders a card that says what happened and what
//! to do, because the card was authored offline and compiled in.
//!
//! # The field this whole feature stands on
//!
//! [`Diagnostic::offending_token`]. CONTRACTS §4 calls it "THE critical field
//! for spec §21. Without it 'Did you mean income?' degrades to regex-scraping
//! English prose." Everything below reads it and nothing below parses a message
//! string.

use stratum_proto::diagnostic::{Confidence, Diagnostic, Suggestion, SuggestionKind};
use stratum_proto::{Edit, Span};

use super::didyoumean::{self, Origin, Suggestions};
use super::rc_table;
use crate::lints::Doc;
use crate::similarity::{path_fuzz, PathMatch};
use crate::{Env, ParseIndex};

/// What a fix would do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FixKind {
    /// Replace the offending name with another.
    Rename,
    /// Add an option to the command.
    InsertOption,
    /// Add a statement above the failing one.
    InsertStatement,
    /// Point a file reference somewhere else.
    ChangePath,
    /// Nothing to apply — an explanation only.
    None,
}

/// Where the prose comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExplainSource {
    /// Authored by us, compiled in, available offline.
    Static(&'static str),
    /// The AI stack, on an explicit click and never otherwise.
    Ai,
}

/// One offered fix.
#[derive(Clone, PartialEq, Debug)]
pub struct QuickFix {
    /// What it would do.
    pub kind: FixKind,
    /// What the button says: `"Did you mean `income`?"`.
    pub label: String,
    /// The deterministic edit. Empty for an explanation-only card.
    pub edits: Vec<Edit>,
    /// How sure we are.
    pub confidence: Confidence,
    /// Where the longer explanation comes from.
    pub explain: ExplainSource,
}

impl QuickFix {
    /// The wire form, for `Diagnostic::suggestions`.
    #[must_use]
    pub fn to_suggestion(&self) -> Suggestion {
        Suggestion {
            label: self.label.clone(),
            kind: match self.kind {
                FixKind::Rename => SuggestionKind::Rename,
                FixKind::InsertOption => SuggestionKind::InsertOption,
                FixKind::InsertStatement => SuggestionKind::InsertLine,
                FixKind::ChangePath => SuggestionKind::ChangePath,
                FixKind::None => SuggestionKind::Explain,
            },
            edits: self.edits.clone(),
        }
    }
}

/// Deterministic fixes for one failed execution.
///
/// Returns an empty vector only when the return code is unknown to us *and* no
/// candidate cleared threshold — the one case design 07 §6.1 hands to
/// `[Explain]`.
#[must_use]
pub fn quick_fixes(diag: &Diagnostic, idx: &ParseIndex<'_>, env: &Env) -> Vec<QuickFix> {
    let doc = Doc::build(idx);
    quick_fixes_with(diag, idx, env, &doc)
}

/// [`quick_fixes`] against a [`Doc`] the caller already built.
#[must_use]
pub fn quick_fixes_with(
    diag: &Diagnostic,
    idx: &ParseIndex<'_>,
    env: &Env,
    doc: &Doc<'_>,
) -> Vec<QuickFix> {
    let mut out = Vec::new();
    let token = diag.offending_token.as_deref().unwrap_or_default();
    let span = diag.span;

    match diag.stata_rc {
        Some(111) if !token.is_empty() => {
            rename_fixes(
                &mut out,
                didyoumean::for_variable(token, env),
                token,
                span,
                env,
            );
        }
        Some(199) if !token.is_empty() => {
            rename_fixes(
                &mut out,
                didyoumean::for_command(token, env, doc),
                token,
                span,
                env,
            );
        }
        Some(198) if !token.is_empty() => {
            if let Some(cmd) = span.and_then(|s| didyoumean::command_at(idx, doc, s.start)) {
                rename_fixes(
                    &mut out,
                    didyoumean::for_option(token, cmd),
                    token,
                    span,
                    env,
                );
            }
        }
        Some(601) if !token.is_empty() => path_fixes(&mut out, token, span, env),
        _ => {}
    }

    // The authored card, always last and always present when we have one. It is
    // the answer that survives with no dataset, no project and no provider.
    if let Some(card) = diag.stata_rc.and_then(rc_table::card) {
        out.push(QuickFix {
            kind: FixKind::None,
            label: card.title.to_owned(),
            edits: Vec::new(),
            confidence: Confidence::Exact,
            explain: ExplainSource::Static(card.explain),
        });
    } else {
        // Design 07 §6.1: AI only when the rc is not in the table AND the user
        // clicks. Offering the affordance is not calling anything.
        out.push(QuickFix {
            kind: FixKind::None,
            label: "Explain this error".to_owned(),
            edits: Vec::new(),
            confidence: Confidence::Speculative,
            explain: ExplainSource::Ai,
        });
    }
    out
}

fn rename_fixes(
    out: &mut Vec<QuickFix>,
    s: Suggestions,
    token: &str,
    span: Option<Span>,
    env: &Env,
) {
    // One confident answer, or up to three tentative ones. Never both.
    let confidence = if s.decisive {
        Confidence::Exact
    } else {
        Confidence::Probable
    };
    let take = if s.decisive { 1 } else { 3 };
    for c in s.candidates.iter().take(take) {
        if c.origin == Origin::PendingVariable {
            let block = env
                .pending_vars
                .iter()
                .find(|p| p.name == c.name)
                .map_or("the block above", |p| p.block_label.as_str());
            out.push(QuickFix {
                kind: FixKind::None,
                label: format!("`{}` is not created yet — run block {block} first", c.name),
                edits: Vec::new(),
                confidence: Confidence::Exact,
                explain: ExplainSource::Static(
                    "A block above this one creates the variable, and it has not been run in this \
                     session. Running it makes the name resolve; nothing needs to be edited.",
                ),
            });
            continue;
        }
        out.push(QuickFix {
            kind: FixKind::Rename,
            label: format!("Did you mean `{}`?", c.name),
            edits: span
                .map(|sp| {
                    vec![Edit {
                        span: narrow_to_token(sp, token),
                        text: c.name.clone(),
                    }]
                })
                .unwrap_or_default(),
            confidence,
            explain: ExplainSource::Static(
                "The name in the command does not resolve. The suggested name is the closest one \
                 that does, by edit distance over the names actually available here.",
            ),
        });
    }
}

/// The diagnostic's span may cover the whole statement; the rename must replace
/// only the token. When the span is already the token's width, it is used as
/// given.
fn narrow_to_token(span: Span, token: &str) -> Span {
    let len = token.len() as u32;
    if span.end.saturating_sub(span.start) == len {
        span
    } else {
        Span {
            start: span.start,
            end: span.start + len,
        }
    }
}

fn path_fixes(out: &mut Vec<QuickFix>, written: &str, span: Option<Span>, env: &Env) {
    let mut hits: Vec<(PathMatch, &camino::Utf8Path)> = env
        .project_files
        .iter()
        .filter_map(|p| path_fuzz(written, p.as_str()).map(|m| (m, p.as_path())))
        .collect();
    // The case trap first — it is the one with an exact answer — then exact
    // basenames, then fuzz; ties by path so the list is stable.
    hits.sort_by(|a, b| {
        let rank = |m: PathMatch| match m {
            PathMatch::CaseOnly => 0u8,
            PathMatch::SameBasename => 1,
            PathMatch::FuzzyBasename => 2,
        };
        rank(a.0).cmp(&rank(b.0)).then_with(|| a.1.cmp(b.1))
    });
    for (kind, path) in hits.into_iter().take(3) {
        let label = match kind {
            PathMatch::CaseOnly => format!(
                "`{path}` exists — only the capitalisation differs. This runs on macOS and \
                 Windows and fails on Linux"
            ),
            PathMatch::SameBasename => format!("Did you mean `{path}`?"),
            PathMatch::FuzzyBasename => format!("Did you mean `{path}`?"),
        };
        out.push(QuickFix {
            kind: FixKind::ChangePath,
            label,
            edits: span
                .map(|sp| {
                    vec![Edit {
                        span: sp,
                        text: path.to_string(),
                    }]
                })
                .unwrap_or_default(),
            confidence: if kind == PathMatch::CaseOnly {
                Confidence::Exact
            } else {
                Confidence::Probable
            },
            explain: ExplainSource::Static(
                "The path does not resolve from the working directory this file runs in. The \
                 suggestion is a file the project does contain whose name is close to the one \
                 written.",
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::PendingVar;

    fn diag(rc: u32, token: &str, span: Option<Span>) -> Diagnostic {
        Diagnostic {
            severity: stratum_proto::diagnostic::Severity::Error,
            code: format!("STATA{rc:04}"),
            stata_rc: Some(rc),
            message: String::new(),
            file: None,
            span,
            offending_token: Some(token.to_owned()),
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: Vec::new(),
            confidence: Confidence::Exact,
        }
    }

    fn auto() -> Env {
        Env {
            varnames: Some(
                ["make", "price", "mpg", "foreign", "income"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            ..Env::default()
        }
    }

    #[test]
    fn the_spec_headline_produces_one_exact_rename() {
        let src = "summarize incom\n";
        let idx = ParseIndex::new(src);
        let at = Span { start: 10, end: 15 };
        let fixes = quick_fixes(&diag(111, "incom", Some(at)), &idx, &auto());
        let first = fixes.first().expect("a fix");
        assert_eq!(first.kind, FixKind::Rename);
        assert_eq!(first.label, "Did you mean `income`?");
        assert_eq!(first.confidence, Confidence::Exact);
        assert_eq!(first.edits[0].text, "income");
        assert_eq!(first.edits[0].span, at);
        // The authored card is always there too.
        assert!(fixes
            .iter()
            .any(|f| matches!(f.explain, ExplainSource::Static(_)) && f.kind == FixKind::None));
    }

    #[test]
    fn an_unmatchable_name_still_gets_the_authored_card_and_no_rename() {
        let idx = ParseIndex::new("summarize nosuchvar\n");
        let fixes = quick_fixes(&diag(111, "nosuchvar", None), &idx, &auto());
        assert!(fixes.iter().all(|f| f.kind == FixKind::None), "{fixes:?}");
        assert_eq!(fixes.len(), 1);
        assert!(matches!(fixes[0].explain, ExplainSource::Static(_)));
    }

    #[test]
    fn a_pending_variable_says_run_the_block_and_offers_no_edit() {
        let mut env = auto();
        env.pending_vars.push(PendingVar {
            name: "ln_price".to_owned(),
            block_label: "B4".to_owned(),
        });
        let idx = ParseIndex::new("summarize ln_pric\n");
        let fixes = quick_fixes(&diag(111, "ln_pric", None), &idx, &env);
        assert!(
            fixes[0].label.contains("run block B4 first"),
            "{:?}",
            fixes[0]
        );
        assert!(fixes[0].edits.is_empty(), "nothing to edit — run it");
    }

    #[test]
    fn an_unknown_command_is_ranked_against_commands() {
        let idx = ParseIndex::new("regres price mpg\n");
        let fixes = quick_fixes(
            &diag(199, "regres", Some(Span { start: 0, end: 6 })),
            &idx,
            &auto(),
        );
        assert_eq!(fixes[0].label, "Did you mean `regress`?");
    }

    #[test]
    fn a_bad_option_is_ranked_against_that_commands_grammar() {
        let src = "summarize price, detial\n";
        let idx = ParseIndex::new(src);
        let at = Span { start: 17, end: 23 };
        let fixes = quick_fixes(&diag(198, "detial", Some(at)), &idx, &auto());
        assert_eq!(fixes[0].label, "Did you mean `detail`?");
    }

    #[test]
    fn the_case_sensitivity_trap_is_called_out_by_name() {
        let mut env = auto();
        env.project_files = vec![camino::Utf8PathBuf::from("data/wave2020.dta")];
        let idx = ParseIndex::new("use data/Wave2020.dta, clear\n");
        let fixes = quick_fixes(
            &diag(601, "data/Wave2020.dta", Some(Span { start: 4, end: 21 })),
            &idx,
            &env,
        );
        assert_eq!(fixes[0].kind, FixKind::ChangePath);
        assert!(fixes[0].label.contains("capitalisation"), "{:?}", fixes[0]);
        assert_eq!(fixes[0].confidence, Confidence::Exact);
    }

    #[test]
    fn an_unknown_return_code_offers_explain_and_nothing_else() {
        let idx = ParseIndex::new("di 1\n");
        let fixes = quick_fixes(&diag(4242, "", None), &idx, &auto());
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].explain, ExplainSource::Ai);
    }

    #[test]
    fn every_authored_card_is_reachable_without_a_provider() {
        let idx = ParseIndex::new("di 1\n");
        for card in rc_table::CARDS {
            let fixes = quick_fixes(&diag(card.rc, "", None), &idx, &Env::default());
            assert!(
                fixes
                    .iter()
                    .any(|f| f.explain == ExplainSource::Static(card.explain)),
                "r({}) has no offline card",
                card.rc
            );
        }
    }
}
