//! 07 §0.1 and §13.1 — which surfaces exist, which of them are AI, and what
//! each one does when no provider is configured.
//!
//! The classification table below is 07 §0.1 transcribed, and it is a **test**,
//! not documentation: the plan's acceptance bullet reads "with no API key the
//! product loses 10 surfaces and keeps 17". That claim is checkable only if the
//! 27 rows exist as data, so they do.

use serde::{Deserialize, Serialize};

use crate::context::tiers::PrivacyTier;

/// The nine AI surfaces of 07 §13.1.
///
/// A surface is the unit of cancellation (07 §2.7: issuing a request for a
/// surface cancels the previous one for that surface), of budget (07 §5.2) and
/// of the privacy ceiling below.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// `[Explain]` on a failed result, after the deterministic quick-fixes had
    /// nothing confident to say.
    QuickFix,
    /// `[Explain]` on a result card.
    ResultExplain,
    /// `[Check model]` on an estimation result.
    CheckModel,
    /// `[Suggest next step]`, in the AI panel and on a run's final card only.
    NextStep,
    /// Inline ghost text. Off by default and with no default model.
    GhostCompletion,
    /// Spec §23's comment text.
    AutoComment,
    /// `[Explain]` a failing reproducibility check, and `[Draft fixes]`.
    ReproExplain,
    /// "Clean this into a reproducible block".
    HistoryCleanup,
    /// The panel.
    Chat,
}

impl Surface {
    /// Every surface, in table order.
    pub const ALL: [Self; 9] = [
        Self::QuickFix,
        Self::ResultExplain,
        Self::CheckModel,
        Self::NextStep,
        Self::GhostCompletion,
        Self::AutoComment,
        Self::ReproExplain,
        Self::HistoryCleanup,
        Self::Chat,
    ];

    /// Stable key for the audit log and the cancellation map.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QuickFix => "quick_fix",
            Self::ResultExplain => "result_explain",
            Self::CheckModel => "check_model",
            Self::NextStep => "next_step",
            Self::GhostCompletion => "ghost_completion",
            Self::AutoComment => "auto_comment",
            Self::ReproExplain => "repro_explain",
            Self::HistoryCleanup => "history_cleanup",
            Self::Chat => "chat",
        }
    }

    /// The surface's own privacy ceiling — the fourth input to
    /// [`crate::context::policy::effective_tier`].
    ///
    /// Only two surfaces cap themselves, and each for a stated reason:
    ///
    /// * **`GhostCompletion`** — 07 §4.3 names it. It fires unattended, on a
    ///   debounce, without the user having asked for anything; a surface nobody
    ///   consciously invoked must not be the one that ships summary statistics.
    /// * **`AutoComment`** — it writes prose *about code*. Nothing in that task
    ///   needs a cell value, and file-scope auto-comment is the largest single
    ///   payload the product ever sends. Capping it costs the model nothing it
    ///   was going to use.
    ///
    /// Everything else is capped only by the user, the project and the dataset.
    #[must_use]
    pub const fn ceiling(self) -> PrivacyTier {
        match self {
            Self::GhostCompletion | Self::AutoComment => PrivacyTier::SchemaOnly,
            _ => PrivacyTier::Full,
        }
    }

    /// Whether this surface is interactive enough to hold the reserved
    /// concurrency permit (07 §2.7): a file-scope auto-comment must not be able
    /// to starve a quick-fix the user is waiting on.
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        matches!(self, Self::QuickFix | Self::GhostCompletion)
    }
}

impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deterministic or model-backed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    /// Runs with no key, no network and no configuration. `stratum-intel`.
    Deterministic,
    /// Needs a provider. Always optional, always cancellable, never on the path
    /// of any core interaction.
    Ai,
}

/// One row of 07 §0.1.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct IntelligenceSurface {
    /// The spec section that asks for it.
    pub spec: &'static str,
    /// What the user would call it.
    pub name: &'static str,
    /// Which half of the architecture implements it.
    pub mechanism: Mechanism,
    /// The AI surface that runs it, for `Mechanism::Ai` rows.
    pub surface: Option<Surface>,
    /// **What the user sees with no API key.** Never empty, for any row — that
    /// is the acceptance bullet. For a deterministic row it says the feature is
    /// unaffected; for an AI row it is the sentence the disabled control's
    /// tooltip carries.
    pub unconfigured: &'static str,
}

/// The unconfigured-state sentence every disabled AI control shares.
const SETUP: &str = "Set up an AI provider in Settings › AI.";

/// The 27 rows of 07 §0.1: 17 deterministic, 10 AI.
pub const INTELLIGENCE_SURFACES: &[IntelligenceSurface] = &[
    // ---- Deterministic: 17 -------------------------------------------------
    IntelligenceSurface {
        spec: "§21",
        name: "Did you mean 'income'? on r(111)",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured:
            "Works unchanged: edit distance over the live varlist, macros and e(b) names.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Unknown command r(199) suggestion",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured:
            "Works unchanged: edit distance over the command table, ado index and user programs.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Bad option on a known command",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: edit distance over that command's option grammar.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "File-not-found r(601) suggestion",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: fuzzy path match over the project tree and cwd.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Return code explanation card",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: the authored rc table ships with the app.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Post-estimation action chips",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured:
            "Works unchanged: driven by e() introspection, each chip a fixed command template.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Merge-cardinality warning before drop _merge",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: lints L001/L002 over the AST and dataflow.",
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Editor lints L001–L010",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged, including every deterministic [Fix].",
    },
    IntelligenceSurface {
        spec: "§22",
        name: "Completion: commands, options, variables, macros, e()/r(), paths",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged, synchronously, under 2 ms.",
    },
    IntelligenceSurface {
        spec: "§23",
        name: "Auto-comment application and verification",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "The four safety gates are deterministic and run with no provider.",
    },
    IntelligenceSurface {
        spec: "§23",
        name: "Comment placement, indentation, wrapping and style",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Unchanged. Placement and wrapping are computed from the parse, so a \
                       comment typed by hand is formatted exactly as a proposed one would be.",
    },
    IntelligenceSurface {
        spec: "§24",
        name: "Narrative region detection",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: explicit markers only, zero heuristics.",
    },
    IntelligenceSurface {
        spec: "§24",
        name: "Markdown rendering and export",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged, with HTML disabled at compile time.",
    },
    IntelligenceSurface {
        spec: "§16",
        name: "All ten reproducibility checks",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Work unchanged, including clean-state verification, headless and in CI.",
    },
    IntelligenceSurface {
        spec: "§11",
        name: "History → do-file block",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: dedup, prune and order are deterministic.",
    },
    IntelligenceSurface {
        spec: "§19",
        name: "Model comparison table",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged, from e(b), e(V), e(N) and e(r2).",
    },
    IntelligenceSurface {
        spec: "§20",
        name: "Variable stats, Created by, Used by",
        mechanism: Mechanism::Deterministic,
        surface: None,
        unconfigured: "Works unchanged: Created by is an AST fact, not a guess.",
    },
    // ---- AI: 10 ------------------------------------------------------------
    IntelligenceSurface {
        spec: "§21",
        name: "Explain this error",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::QuickFix),
        unconfigured: SETUP,
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Explain this result",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::ResultExplain),
        unconfigured: SETUP,
    },
    IntelligenceSurface {
        spec: "§21",
        name: "Check model",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::CheckModel),
        unconfigured: SETUP,
    },
    IntelligenceSurface {
        spec: "§22",
        name: "Ghost-text next line",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::GhostCompletion),
        unconfigured: "Off by default. Deterministic completion is unaffected.",
    },
    IntelligenceSurface {
        spec: "§22",
        name: "Suggest next step",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::NextStep),
        unconfigured: SETUP,
    },
    IntelligenceSurface {
        spec: "§23",
        name: "Auto-comment text",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::AutoComment),
        unconfigured: SETUP,
    },
    IntelligenceSurface {
        spec: "§16",
        name: "Explain a failing check, draft fixes",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::ReproExplain),
        unconfigured:
            "The checks and their deterministic fixes still run. Only the prose needs a provider.",
    },
    IntelligenceSurface {
        spec: "§11",
        name: "Clean this into a reproducible block",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::HistoryCleanup),
        unconfigured: "History → do-file still works. Only the prose rewrite needs a provider.",
    },
    IntelligenceSurface {
        spec: "§19",
        name: "Explain the difference between these models",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::CheckModel),
        unconfigured: "The comparison table is unaffected.",
    },
    IntelligenceSurface {
        spec: "§20",
        name: "Ask about this variable",
        mechanism: Mechanism::Ai,
        surface: Some(Surface::Chat),
        unconfigured: SETUP,
    },
];

/// How many of 07 §0.1's rows survive with no provider configured.
#[must_use]
pub fn deterministic_count() -> usize {
    INTELLIGENCE_SURFACES
        .iter()
        .filter(|r| r.mechanism == Mechanism::Deterministic)
        .count()
}

/// How many of 07 §0.1's rows need a provider.
#[must_use]
pub fn ai_count() -> usize {
    INTELLIGENCE_SURFACES
        .iter()
        .filter(|r| r.mechanism == Mechanism::Ai)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_api_key_the_product_loses_ten_surfaces_and_keeps_seventeen() {
        // ADR-012 and the W21 acceptance bullet, as an assertion.
        assert_eq!(deterministic_count(), 17);
        assert_eq!(ai_count(), 10);
        assert_eq!(INTELLIGENCE_SURFACES.len(), 27);
    }

    #[test]
    fn every_row_says_what_happens_with_no_provider() {
        // "Every AI surface must have a defined behaviour when no provider is
        // configured, and that behaviour must never be a broken or empty UI."
        for row in INTELLIGENCE_SURFACES {
            assert!(
                !row.unconfigured.is_empty(),
                "{} has no unconfigured state",
                row.name
            );
            assert!(
                row.unconfigured.len() > 20,
                "{} has a placeholder",
                row.name
            );
        }
    }

    #[test]
    fn every_ai_row_names_the_surface_that_runs_it_and_no_deterministic_row_does() {
        for row in INTELLIGENCE_SURFACES {
            match row.mechanism {
                Mechanism::Ai => assert!(row.surface.is_some(), "{}", row.name),
                Mechanism::Deterministic => assert!(row.surface.is_none(), "{}", row.name),
            }
        }
    }

    #[test]
    fn the_unattended_surfaces_are_the_capped_ones() {
        assert_eq!(Surface::GhostCompletion.ceiling(), PrivacyTier::SchemaOnly);
        assert_eq!(Surface::AutoComment.ceiling(), PrivacyTier::SchemaOnly);
        assert_eq!(Surface::Chat.ceiling(), PrivacyTier::Full);
    }

    #[test]
    fn surface_keys_are_unique_and_stable() {
        let mut seen = std::collections::BTreeSet::new();
        for s in Surface::ALL {
            assert!(seen.insert(s.as_str()), "duplicate key {}", s.as_str());
        }
        assert_eq!(seen.len(), 9);
    }
}
