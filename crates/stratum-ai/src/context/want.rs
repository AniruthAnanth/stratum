//! ADR-012 (A5) — `AiContextWant` narrows the **fetch**, not just the prompt.
//!
//! The privacy gate ([`crate::context::gate`]) filters on the way *out*. This
//! module filters on the way *in*: it decides which fields
//! `EngineRequest::AiContext` even asks the engine for, so data above the
//! effective tier is never read into desktop memory in the first place — rather
//! than being read, held, and then merely omitted from a prompt.
//!
//! Two masks, intersected:
//!
//! * **What the surface needs.** Ghost completion has no use for stored
//!   estimates; asking for them would be a fetch nobody reads.
//! * **What the tier permits.** This is the half the acceptance bullet is about,
//!   and it is asserted by inspecting the emitted [`EngineRequest`], not the
//!   prompt it eventually produces.

use stratum_proto::engine::{AiContextWant, EngineRequest};
use stratum_proto::ids::SessionId;

use super::tiers::PrivacyTier;
use crate::service::surface::Surface;

/// What the tier permits us to *fetch*.
///
/// * `VAR_SUMMARIES` is a [`stratum_proto::data::QuickSummary`], whose `min` and
///   `max` are **literal values from single observations**. 07 §4.1 names this
///   exact hazard: "a maximum salary identifies one person". Tier 2 and above.
/// * `MACROS` carries [`stratum_proto::introspect::MacroInfo::value`], and a
///   macro's contents are tier 3 by 07 §4.1 — they routinely hold absolute
///   paths and interpolated data values. Tier 3 only.
/// * Everything else is metadata or an aggregate the renderer can further trim:
///   `AiContextSnapshot` has no shape that can carry observation-level data, by
///   construction (CONTRACTS §9.1), which is what makes the tier-1 guarantee
///   structural rather than a promise.
#[must_use]
pub fn tier_mask(tier: PrivacyTier) -> AiContextWant {
    match tier {
        // Nothing about the session leaves the machine, so nothing about the
        // session needs to be fetched either.
        PrivacyTier::Off => AiContextWant::empty(),
        PrivacyTier::SchemaOnly => {
            AiContextWant::DATASET_META
                | AiContextWant::STORED_RESULTS
                | AiContextWant::ESTIMATES
                | AiContextWant::RECENT_ERRORS
                | AiContextWant::RECENT_COMMANDS
        }
        PrivacyTier::SchemaAndStats => {
            tier_mask(PrivacyTier::SchemaOnly) | AiContextWant::VAR_SUMMARIES
        }
        PrivacyTier::Full => tier_mask(PrivacyTier::SchemaAndStats) | AiContextWant::MACROS,
    }
}

/// What a surface actually reads.
///
/// Keyed on the surface rather than on the intent because the two are 1:1 for
/// every task the product ships (07 §5.1's `Intent` selects the prompt, the
/// surface selects the budget and the fetch), and a second dimension here would
/// be a second place for the two to disagree.
#[must_use]
pub fn surface_mask(surface: Surface) -> AiContextWant {
    match surface {
        // The error and what was in scope when it fired. Not summaries: an
        // r(111) is answered by names.
        Surface::QuickFix => {
            AiContextWant::DATASET_META
                | AiContextWant::RECENT_ERRORS
                | AiContextWant::STORED_RESULTS
        }
        Surface::ResultExplain | Surface::CheckModel => {
            AiContextWant::DATASET_META
                | AiContextWant::STORED_RESULTS
                | AiContextWant::ESTIMATES
                | AiContextWant::VAR_SUMMARIES
        }
        Surface::NextStep => {
            AiContextWant::DATASET_META
                | AiContextWant::STORED_RESULTS
                | AiContextWant::RECENT_COMMANDS
                | AiContextWant::RECENT_ERRORS
        }
        // The schema and nothing else: an 800 ms deadline has no room for a
        // fetch, and the caret's syntactic role is what makes a completion good.
        Surface::GhostCompletion => AiContextWant::DATASET_META,
        // Comments describe code. The variable list is what makes a comment say
        // "log-transform income" rather than "transform the variable".
        Surface::AutoComment => AiContextWant::DATASET_META,
        Surface::ReproExplain => {
            AiContextWant::DATASET_META
                | AiContextWant::RECENT_ERRORS
                | AiContextWant::RECENT_COMMANDS
        }
        Surface::HistoryCleanup => AiContextWant::DATASET_META | AiContextWant::RECENT_COMMANDS,
        // The panel is open-ended by definition, so it asks for everything the
        // tier allows and nothing more.
        Surface::Chat => AiContextWant::all(),
    }
}

/// The fetch mask for one request: `surface ∩ tier`.
#[must_use]
pub fn want_for(surface: Surface, tier: PrivacyTier) -> AiContextWant {
    surface_mask(surface) & tier_mask(tier)
}

/// The wire request the packer emits before it can pack anything.
///
/// Returns `None` at [`PrivacyTier::Off`]: there is nothing to ask for, and a
/// request with an empty mask would still be a round-trip and still appear in
/// the engine's log as an AI fetch, which is exactly the impression tier `Off`
/// exists to avoid.
#[must_use]
pub fn context_request(
    session: SessionId,
    surface: Surface,
    tier: PrivacyTier,
) -> Option<EngineRequest> {
    let want = want_for(surface, tier);
    (!want.is_empty()).then_some(EngineRequest::AiContext { session, want })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags that can carry a value observed on one row, or a macro's
    /// contents. The acceptance bullet is about exactly these.
    const OBSERVATION_LEVEL: AiContextWant =
        AiContextWant::VAR_SUMMARIES.union(AiContextWant::MACROS);

    #[test]
    fn a_tier_one_task_never_even_requests_observation_level_data() {
        // Asserted on the EngineRequest the packer emits, not on the prompt it
        // produces: the point of A5's narrowing is that the bytes are never read
        // into desktop memory at all.
        for surface in Surface::ALL {
            let Some(EngineRequest::AiContext { want, .. }) =
                context_request(SessionId(1), surface, PrivacyTier::SchemaOnly)
            else {
                panic!("{surface} produced no request at tier 1");
            };
            assert!(
                !want.intersects(OBSERVATION_LEVEL),
                "{surface} asked for {want:?} at schema_only"
            );
        }
    }

    #[test]
    fn tier_two_may_ask_for_summaries_but_still_never_for_macro_contents() {
        let want = want_for(Surface::Chat, PrivacyTier::SchemaAndStats);
        assert!(want.contains(AiContextWant::VAR_SUMMARIES));
        assert!(
            !want.contains(AiContextWant::MACROS),
            "macro contents are tier 3"
        );
    }

    #[test]
    fn only_tier_three_reaches_macro_contents() {
        assert!(want_for(Surface::Chat, PrivacyTier::Full).contains(AiContextWant::MACROS));
    }

    #[test]
    fn tier_off_emits_no_request_at_all() {
        for surface in Surface::ALL {
            assert!(context_request(SessionId(1), surface, PrivacyTier::Off).is_none());
            assert!(want_for(surface, PrivacyTier::Off).is_empty());
        }
    }

    #[test]
    fn a_surface_never_fetches_more_than_it_reads() {
        for surface in Surface::ALL {
            for tier in PrivacyTier::ALL {
                let want = want_for(surface, tier);
                assert!(surface_mask(surface).contains(want), "{surface} at {tier}");
                assert!(tier_mask(tier).contains(want), "{surface} at {tier}");
            }
        }
    }

    #[test]
    fn ghost_completion_asks_for_the_schema_and_nothing_else_at_any_tier() {
        // Its own ceiling is schema-only (07 §4.3), and it fires unattended.
        for tier in PrivacyTier::ALL {
            let want = want_for(Surface::GhostCompletion, tier);
            assert!(
                want.difference(AiContextWant::DATASET_META).is_empty(),
                "{tier}: {want:?}"
            );
        }
    }
}
