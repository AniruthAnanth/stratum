//! `07` §5 — the context packer.
//!
//! # The shape of the guarantee
//!
//! The packer never decides what may be sent. For every source it renders one
//! [`ContextItem`] **per tier variant it is capable of producing** — a
//! variables block without statistics at [`PrivacyTier::SchemaOnly`] and one
//! with them at [`PrivacyTier::SchemaAndStats`], macro names at tier 1 and macro
//! contents at tier 3 — hands the whole set to [`super::gate`], and keeps the
//! richest variant that survived. The gate is one `filter`, it is the only place
//! a tier is compared, and a source that gains a new richer rendering has to
//! declare its tier at the type level to be renderable at all.
//!
//! That is why `tests/privacy_gate.rs` can seed a fixture with canaries at every
//! tier and assert their absence from a tier-1 prompt: the tier-2 item genuinely
//! exists, is genuinely built, and is genuinely dropped by the comparison.
//!
//! # Two narrowings, in two directions
//!
//! [`super::want`] narrows the **fetch** — a tier-1 task does not even ask the
//! engine for `QuickSummary` data, so it is never read into desktop memory.
//! [`super::gate`] narrows the **send**. Neither subsumes the other: the fetch
//! mask is per (surface, tier) and travels on the wire; the gate is per item and
//! runs on bytes that already exist.
//!
//! # Work is bounded by the *schema*, never by the data
//!
//! Nothing here is O(observations) — [`stratum_proto::introspect::DatasetMeta`]
//! has no shape that can carry one. The only unbounded input is the variable
//! count, and [`MAX_VARIABLE_LINES`] caps the rendered lines at 400 regardless:
//! a 10 000-variable administrative extract costs the same 400 lines as a
//! 500-variable one, and the header says so rather than pretending the list is
//! complete.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::Serialize;
use stratum_proto::engine::{AiContextWant, EngineRequest};
use stratum_proto::ids::SessionId;
use stratum_proto::introspect::SessionIntrospect;

use super::budget::{cap_tokens, Budget, CategoryCap, CATEGORIES};
use super::policy::{effective_tier, TierBound, TierInputs};
use super::redact::Pseudonymiser;
use super::render::{self, RankSignals};
use super::tiers::PrivacyTier;
use super::{gate, ContextItem, ContextSource, PackedPrompt, PromptBlock};
use crate::provider::backends::anthropic::estimate_tokens;
use crate::service::surface::Surface;

/// The hard ceiling on rendered variable lines, whatever the budget allows.
///
/// A 10 000-variable extract cannot be listed and nobody has ever been helped by
/// the 900th line. Capping here rather than at the budget is what keeps the
/// packer's cost a function of the *cap* instead of a function of the dataset's
/// width, which is the difference between a bounded and an unbounded pack.
pub const MAX_VARIABLE_LINES: usize = 400;

/// The document a request is about.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct DocumentRef {
    /// Project-relative at every tier below [`PrivacyTier::Full`]; absolute
    /// paths are tier 3 (`07` §4.1) and the caller is responsible for having
    /// relativised it.
    pub path: Utf8PathBuf,
    /// The editor's document version, so a reply that arrives after an edit can
    /// be discarded rather than applied to a buffer it does not describe.
    pub version: u64,
}

/// The block, selection or error site the user acted on.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Focus {
    /// `analysis.do:20-24, block B7, status: failed` — the human-readable
    /// locator the rendered `## FOCUS` header carries.
    pub header: String,
    /// The code itself.
    pub text: String,
}

/// A project file excerpt the caller chose to offer (`07` §5.3 category 7).
///
/// Supplied by the caller rather than pulled through [`SessionIntrospect`],
/// because the trait deliberately has no filesystem shape: an `include`d file's
/// bytes are the workspace's to read, not the engine's to volunteer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct FileExcerpt {
    /// Project-relative path.
    pub path: Utf8PathBuf,
    /// The excerpt.
    pub text: String,
}

/// `07` §5.1's `ContextRequest`.
///
/// [`Default`] is hand-written rather than derived: `SessionId` deliberately has
/// no `Default` (CONTRACTS §1 — a silently-defaulted id is a real bug, not a
/// typo), and neither does [`Surface`], because a defaulted surface would carry
/// the wrong budget *and* the wrong privacy ceiling. Both are named explicitly
/// here so that the value a test gets is one somebody chose.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct PackRequest {
    /// Which surface is asking, which fixes the budget, the fetch mask and the
    /// privacy ceiling.
    pub surface: Surface,
    /// The engine session the snapshot came from.
    pub session: SessionId,
    /// The four inputs to `effective = min(global, project, dataset, surface)`.
    /// The caller sets three; [`pack`] overwrites `surface` from
    /// [`Surface::ceiling`] so a caller cannot raise a surface's own cap.
    pub tier_inputs: TierInputs,
    /// The block or selection the user acted on.
    pub focus: Option<Focus>,
    /// The document it came from.
    pub document: Option<DocumentRef>,
    /// What the user typed. Their own words are never gated — a user cannot
    /// leak data to themselves, and gating the question would produce answers
    /// to a question nobody asked.
    pub user_text: String,
    /// `07` §5.4's ranking signals, computed deterministically by the caller
    /// (`stratum-intel` owns the analysis that produces them).
    pub signals: RankSignals,
    /// Project file excerpts.
    pub files: Vec<FileExcerpt>,
    /// Per-project, per-variable pseudonym allowlist (`07` §4.4), from the
    /// sidecar.
    pub allow_names: Vec<String>,
    /// Commands executed before the focus, oldest first. Category 6.
    pub recent_commands: Vec<String>,
}

impl Default for PackRequest {
    fn default() -> Self {
        Self {
            // The panel: the one surface whose ceiling is `Full`, so a defaulted
            // request exercises the gate rather than being capped before it.
            surface: Surface::Chat,
            session: SessionId(0),
            tier_inputs: TierInputs::default(),
            focus: None,
            document: None,
            user_text: String::new(),
            signals: RankSignals::default(),
            files: Vec::new(),
            allow_names: Vec::new(),
            recent_commands: Vec::new(),
        }
    }
}

impl PackRequest {
    /// The effective tier for this request: `min` of the four inputs, with the
    /// surface's own ceiling forced rather than trusted.
    #[must_use]
    pub fn effective_tier(&self) -> PrivacyTier {
        effective_tier(self.resolved_inputs())
    }

    /// The tier inputs with the surface ceiling forced to
    /// [`Surface::ceiling`].
    #[must_use]
    pub fn resolved_inputs(&self) -> TierInputs {
        TierInputs {
            surface: self.surface.ceiling(),
            ..self.tier_inputs
        }
    }

    /// The wire request this pack needs, or `None` at [`PrivacyTier::Off`].
    ///
    /// The acceptance bullet is asserted against *this*, not against the prompt:
    /// A5's narrowing means tier-3 data is never read into desktop memory, which
    /// a test of the rendered bytes could not tell apart from a filter.
    #[must_use]
    pub fn context_request(&self) -> Option<EngineRequest> {
        super::want::context_request(self.session, self.surface, self.effective_tier())
    }
}

/// Why a category is not in the prompt.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    /// Every variant of it sat above the effective tier.
    AboveTier,
    /// The session has none of it.
    Empty,
    /// It could not fit its `min_useful` in what was left of the budget, and a
    /// category that cannot fit its minimum is dropped whole rather than
    /// truncated: three of 3 127 variables is not context, it is misleading
    /// context.
    BelowMinUseful,
    /// The budget was exhausted before this category's turn.
    NoBudget,
}

impl OmissionReason {
    /// The phrasing the prompt's own `## OMITTED` block uses. The model must
    /// know its context is partial, or it will confidently assert that a
    /// variable does not exist.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::AboveTier => "withheld by the privacy tier",
            Self::Empty => "none in this session",
            Self::BelowMinUseful => "too little would have fitted to be useful",
            Self::NoBudget => "no budget left",
        }
    }
}

/// One category that did not make it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct Omission {
    /// Which category.
    pub source: ContextSource,
    /// Why.
    pub reason: OmissionReason,
}

/// Everything "Preview what will be sent" shows, and everything the audit record
/// stores about the request side.
///
/// **Byte-comparable on purpose.** `tests/packer_parity.rs` asserts that the
/// same packer over the engine-shaped `SessionIntrospect` and over the desktop's
/// [`super::adapter::SnapshotIntrospect`] produces an identical value of this
/// type — which is the whole point of A5 declaring one trait with two
/// implementations.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct PromptPreview {
    /// Which surface.
    pub surface: Surface,
    /// The tier the gate actually ran at.
    pub effective_tier: PrivacyTier,
    /// Which of the four constraints bound it. "Why is this so restricted?" has
    /// exactly four answers and a user who cannot see which one applies will
    /// assume the product is broken.
    pub bound_by: TierBound,
    /// All four inputs, so the preview can show the arithmetic.
    pub tier_inputs: TierInputs,
    /// The fetch mask this request used.
    pub want: AiContextWant,
    /// Every block, in send order, with provenance and tier.
    pub blocks: Vec<PromptBlock>,
    /// Estimated input tokens including the cached prefix.
    pub est_input_tokens: u32,
    /// Estimated input tokens excluding the cached prefix — the number the
    /// budget in `07` §5.2 is denominated in.
    pub est_context_tokens: u32,
    /// The budget those context tokens were fitted to.
    pub budget_tokens: u32,
    /// What was left out, and why.
    pub omitted: Vec<Omission>,
    /// How many variables were pseudonymised (`07` §4.4).
    pub pseudonymised: usize,
    /// The byte-exact transcript, block by block.
    pub transcript: String,
}

/// A packed prompt plus everything needed to explain it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packed {
    /// The bytes.
    pub prompt: PackedPrompt,
    /// The explanation.
    pub preview: PromptPreview,
    /// Pseudonym → real name, for un-mapping the reply (`07` §4.4).
    pub pseudonyms: BTreeMap<String, String>,
    /// How many times a category body was rendered. An upper bound on the
    /// packer's own work that is a **counter**, not a duration (ADR-017): the
    /// fit is a binary search, so this is `O(log n)` per fitted category and a
    /// regression to a linear scan shows up here immediately.
    pub renders: u32,
}

/// Pack one request.
///
/// Synchronous and network-free by construction — it takes no provider, no
/// client and no runtime. That is what makes `AiService::preview` honest: the
/// "preview what will be sent" control cannot itself send anything.
#[must_use]
pub fn pack(
    req: &PackRequest,
    sources: &dyn SessionIntrospect,
    budget: &Budget,
    framing: &str,
) -> Packed {
    let inputs = req.resolved_inputs();
    let tier = effective_tier(inputs);
    let want = super::want::want_for(req.surface, tier);
    let mut pseudo = Pseudonymiser::with_allowlist(req.allow_names.iter().cloned());
    let mut renders = 0u32;

    let meta = sources.dataset_meta();
    let mut omitted: Vec<Omission> = Vec::new();
    let mut kept: Vec<ContextItem> = Vec::new();
    let total = budget.context_tokens;

    // `07` §5.3: "Category 1 is reserved before anything else is placed."
    let focus_cap = cap_for(ContextSource::Focus);
    let reserve = cap_tokens(focus_cap, total);
    let focus_item = req.focus.as_ref().map(|f| {
        let (body, n) = fit_focus(&f.header, &f.text, reserve);
        renders += n;
        item(
            ContextSource::Focus,
            PrivacyTier::SchemaOnly,
            focus_cap.priority,
            body,
        )
    });
    let mut remaining = total.saturating_sub(focus_item.as_ref().map_or(0, |i| i.est_tokens));

    for cap in ordered_categories() {
        if cap.source == ContextSource::Focus {
            if let Some(f) = focus_item.clone() {
                kept.push(f);
            } else {
                omitted.push(Omission {
                    source: cap.source,
                    reason: OmissionReason::Empty,
                });
            }
            continue;
        }

        let allowance = cap_tokens(cap, total).min(remaining);
        let (variants, n, fitted_out) = build_variants(
            cap.source,
            req,
            sources,
            &meta,
            want,
            &mut pseudo,
            allowance,
        );
        renders += n;

        if variants.is_empty() {
            // `Empty` means the session genuinely has none of this, and
            // `omission_note` deliberately does not report those rows — telling
            // a model "no macros were omitted" is noise. A category that *had*
            // data and could not seat its minimum is the opposite case and has
            // to be reported, or the model asserts the variable it was never
            // shown does not exist.
            let reason = if fitted_out {
                if allowance == 0 {
                    OmissionReason::NoBudget
                } else {
                    OmissionReason::BelowMinUseful
                }
            } else {
                OmissionReason::Empty
            };
            omitted.push(Omission {
                source: cap.source,
                reason,
            });
            continue;
        }
        // THE GATE. One comparison, over typed items, and the only place in this
        // crate where a tier is compared against a tier.
        let survivors = gate(variants, tier);
        let Some(chosen) = richest(survivors) else {
            omitted.push(Omission {
                source: cap.source,
                reason: OmissionReason::AboveTier,
            });
            continue;
        };
        if allowance == 0 || chosen.est_tokens > allowance {
            let reason = if remaining == 0 {
                OmissionReason::NoBudget
            } else {
                OmissionReason::BelowMinUseful
            };
            omitted.push(Omission {
                source: cap.source,
                reason,
            });
            continue;
        }
        remaining = remaining.saturating_sub(chosen.est_tokens);
        kept.push(chosen);
    }

    if let Some(note) = omission_note(&omitted) {
        // Counts and category names, never data. It exists because a model that
        // is not told its context was trimmed will assert that the missing
        // thing does not exist.
        kept.push(item(
            ContextSource::Omissions,
            PrivacyTier::SchemaOnly,
            u8::MAX,
            note,
        ));
    }
    kept.sort_by_key(|i| i.priority);

    let prompt = PackedPrompt::from_gated(framing.to_owned(), kept, req.user_text.clone(), tier);
    let est_context_tokens = prompt
        .blocks()
        .iter()
        .filter(|b| !matches!(b.source, ContextSource::Task))
        .map(|b| b.est_tokens)
        .sum();

    let preview = PromptPreview {
        surface: req.surface,
        effective_tier: tier,
        bound_by: inputs.binding(),
        tier_inputs: inputs,
        want,
        blocks: prompt.blocks().to_vec(),
        est_input_tokens: prompt.est_tokens(),
        est_context_tokens,
        budget_tokens: total,
        omitted,
        pseudonymised: pseudo.len(),
        transcript: prompt.transcript(),
    };

    Packed {
        prompt,
        preview,
        pseudonyms: pseudo.mapping().clone(),
        renders,
    }
}

// ---------------------------------------------------------------------------
// Category construction
// ---------------------------------------------------------------------------

fn cap_for(source: ContextSource) -> CategoryCap {
    super::budget::category(source).unwrap_or(CategoryCap {
        source,
        priority: u8::MAX,
        cap_pct: 0,
        min_useful: 0,
        hard_reserve: false,
    })
}

/// The budget table in fill order. `sort_by_key` rather than a second hand-kept
/// list, so a priority edited in `budget.rs` cannot silently disagree with the
/// order things are actually placed in.
fn ordered_categories() -> Vec<CategoryCap> {
    let mut v: Vec<CategoryCap> = CATEGORIES.to_vec();
    v.sort_by_key(|c| c.priority);
    v
}

fn item(source: ContextSource, min_tier: PrivacyTier, priority: u8, body: String) -> ContextItem {
    ContextItem {
        source,
        min_tier,
        priority,
        est_tokens: estimate_tokens(&body),
        body,
    }
}

/// Keep the richest variant that survived the gate.
///
/// "Richest" is the greatest `min_tier`: at tier 2 both the schema-only and the
/// with-statistics variables block survive the filter, and the one the user
/// consented to is the fuller one.
fn richest(survivors: Vec<ContextItem>) -> Option<ContextItem> {
    survivors.into_iter().max_by_key(|i| i.min_tier)
}

/// Every tier variant of one category, how many bodies were rendered, and
/// whether the category observed data it then failed to fit.
fn build_variants(
    source: ContextSource,
    req: &PackRequest,
    sources: &dyn SessionIntrospect,
    meta: &stratum_proto::introspect::DatasetMeta,
    want: AiContextWant,
    pseudo: &mut Pseudonymiser,
    allowance: u32,
) -> (Vec<ContextItem>, u32, bool) {
    let cap = cap_for(source);
    let mut out = Vec::new();
    let mut renders = 0u32;
    // Set only by the categories whose body is *fitted* to an allowance: those
    // are the ones that can look at real data and still return nothing. The
    // caller needs to tell that apart from "the session has none of this".
    let mut fitted_out = false;

    match source {
        ContextSource::Session => {
            if want.contains(AiContextWant::DATASET_META) && meta.n_vars > 0 {
                renders += 1;
                out.push(item(
                    source,
                    PrivacyTier::SchemaOnly,
                    cap.priority,
                    render::session(meta),
                ));
            }
        }
        ContextSource::Errors => {
            let errors = sources.recent_errors(1);
            if want.contains(AiContextWant::RECENT_ERRORS) && !errors.is_empty() {
                renders += 2;
                out.push(item(
                    source,
                    PrivacyTier::SchemaOnly,
                    cap.priority,
                    render::errors(&errors, PrivacyTier::SchemaOnly),
                ));
                // The message text is tier 1; the `notes` a runtime attaches can
                // echo the offending *value*, so that rendering declares tier 3.
                out.push(item(
                    source,
                    PrivacyTier::Full,
                    cap.priority,
                    render::errors(&errors, PrivacyTier::Full),
                ));
            }
        }
        ContextSource::Variables => {
            let vars = sources.variables(&meta.frame);
            if want.contains(AiContextWant::DATASET_META) && !vars.is_empty() {
                let summaries: Vec<_> = if want.contains(AiContextWant::VAR_SUMMARIES) {
                    vars.iter()
                        .filter_map(|v| sources.var_stats(&meta.frame, &v.name))
                        .collect()
                } else {
                    Vec::new()
                };
                let (body, n) = fit_variables(
                    &vars,
                    &[],
                    &req.signals,
                    PrivacyTier::SchemaOnly,
                    pseudo,
                    allowance,
                    cap.min_useful as usize,
                );
                renders += n;
                if let Some(body) = body {
                    out.push(item(source, PrivacyTier::SchemaOnly, cap.priority, body));
                }
                if !summaries.is_empty() {
                    let (body, n) = fit_variables(
                        &vars,
                        &summaries,
                        &req.signals,
                        PrivacyTier::SchemaAndStats,
                        pseudo,
                        allowance,
                        cap.min_useful as usize,
                    );
                    renders += n;
                    if let Some(body) = body {
                        out.push(item(
                            source,
                            PrivacyTier::SchemaAndStats,
                            cap.priority,
                            body,
                        ));
                    }
                }
                fitted_out = out.is_empty();
            }
        }
        ContextSource::Estimates => {
            if want.contains(AiContextWant::STORED_RESULTS) {
                let stored = sources.stored_results();
                let handles = if want.contains(AiContextWant::ESTIMATES) {
                    sources.estimates_store()
                } else {
                    Vec::new()
                };
                for tier in [PrivacyTier::SchemaOnly, PrivacyTier::SchemaAndStats] {
                    renders += 1;
                    let body = render::estimates(&stored, &handles, tier);
                    if !body.is_empty() {
                        out.push(item(source, tier, cap.priority, body));
                    }
                }
            }
        }
        ContextSource::Macros => {
            if want.contains(AiContextWant::MACROS) {
                let list = sources.macros();
                for tier in [PrivacyTier::SchemaOnly, PrivacyTier::Full] {
                    renders += 1;
                    let body = render::macros(&list, tier);
                    if !body.is_empty() {
                        out.push(item(source, tier, cap.priority, body));
                    }
                }
            }
        }
        ContextSource::Block => {
            if want.contains(AiContextWant::RECENT_COMMANDS) && !req.recent_commands.is_empty() {
                let (body, n) = fit_commands(&req.recent_commands, allowance);
                renders += n;
                if let Some(body) = body {
                    out.push(item(source, PrivacyTier::SchemaOnly, cap.priority, body));
                }
                fitted_out = out.is_empty();
            }
        }
        ContextSource::Files => {
            if !req.files.is_empty() {
                renders += 1;
                let mut body = String::from("## PROJECT FILES\n");
                for f in &req.files {
                    body.push_str(&format!("### {}\n", f.path));
                    body.push_str(&super::redact::fence(f.text.trim_end()));
                    body.push('\n');
                }
                out.push(item(source, PrivacyTier::SchemaOnly, cap.priority, body));
            }
        }
        // Never built here. Focus is category 1 and `pack` reserves its budget
        // and renders it *before* the fill loop starts, so the loop `continue`s
        // past it; the framing and the user's own words are not gated at all;
        // and the omission note is appended after the fill, from the omissions
        // the fill itself recorded. Listing them explicitly rather than with a
        // wildcard is what makes a newly added `ContextSource` a compile error
        // in this function instead of a category that silently packs nothing.
        ContextSource::Focus
        | ContextSource::Task
        | ContextSource::UserText
        | ContextSource::Omissions => {}
    }
    (out, renders, fitted_out)
}

// ---------------------------------------------------------------------------
// Fitting
// ---------------------------------------------------------------------------

/// Fit the focus into its reserve.
///
/// `07` §5.3: elide in the **middle**, centred on the cursor, never at the tail
/// — the end of a block is usually the interesting part. [`render::focus`] does
/// the eliding; this finds the largest line count that fits, by binary search
/// rather than by a token-per-line guess, because a `#delimit ;` block and a
/// `foreach` body differ by a factor of five per line.
fn fit_focus(header: &str, text: &str, reserve: u32) -> (String, u32) {
    let lines = text.lines().count();
    let full = render::focus(header, text, lines.max(1));
    if estimate_tokens(&full) <= reserve || lines <= 4 {
        return (full, 1);
    }
    let mut renders = 1u32;
    let (mut lo, mut hi) = (4usize, lines);
    let mut best = render::focus(header, text, 4);
    renders += 1;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let candidate = render::focus(header, text, mid);
        renders += 1;
        if estimate_tokens(&candidate) <= reserve {
            best = candidate;
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }
    (best, renders)
}

/// Fit the variable list into its allowance.
///
/// Returns `None` when fewer than `min_useful` variables fit, which drops the
/// whole category rather than truncating it.
#[allow(clippy::too_many_arguments)]
fn fit_variables(
    vars: &[stratum_proto::data::VariableInfo],
    summaries: &[stratum_proto::data::QuickSummary],
    signals: &RankSignals,
    tier: PrivacyTier,
    pseudo: &mut Pseudonymiser,
    allowance: u32,
    min_useful: usize,
) -> (Option<String>, u32) {
    let ceiling = vars.len().min(MAX_VARIABLE_LINES);
    let mut renders = 0u32;
    let mut best: Option<String> = None;
    let mut best_n = 0usize;
    let (mut lo, mut hi) = (0usize, ceiling);
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let candidate = render::variables(vars, summaries, signals, tier, pseudo, mid);
        renders += 1;
        if estimate_tokens(&candidate) <= allowance {
            best = Some(candidate);
            best_n = mid;
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }
    if best_n < min_useful.min(ceiling) {
        return (None, renders);
    }
    (best, renders)
}

/// Fit the preceding executed blocks into their allowance, newest kept.
fn fit_commands(commands: &[String], allowance: u32) -> (Option<String>, u32) {
    let mut renders = 0u32;
    let mut best: Option<String> = None;
    let (mut lo, mut hi) = (1usize, commands.len());
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let candidate = render::recent_commands(commands, mid);
        renders += 1;
        if estimate_tokens(&candidate) <= allowance {
            best = Some(candidate);
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }
    (best, renders)
}

/// The `## OMITTED` block, or `None` when nothing was left out.
///
/// `Empty` omissions are not reported: "there are no stored estimates" is a fact
/// about the session, not a fact about the prompt, and listing it would train
/// the reader to skip the block that matters.
fn omission_note(omitted: &[Omission]) -> Option<String> {
    let reportable: Vec<&Omission> = omitted
        .iter()
        .filter(|o| o.reason != OmissionReason::Empty)
        .collect();
    if reportable.is_empty() {
        return None;
    }
    let mut body = String::from("## OMITTED FROM THIS PROMPT\n");
    for o in reportable {
        body.push_str(&format!("{}: {}\n", o.source.label(), o.reason.phrase()));
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use stratum_proto::data::{StorageType, VariableInfo};
    use stratum_proto::ids::{VarId, VarIdx};
    use stratum_proto::introspect::DatasetMeta;

    use super::*;
    use crate::context::adapter::SnapshotIntrospect;
    use crate::context::budget::CommentScope;

    fn wide(n: u32) -> SnapshotIntrospect {
        let vars: Vec<VariableInfo> = (0..n)
            .map(|i| VariableInfo {
                idx: VarIdx(i),
                id: VarId(i),
                name: format!("v{i}"),
                ty: StorageType::Float,
                label: format!("variable number {i}"),
                format: "%9.0g".to_owned(),
                value_label: None,
                n_missing: 0,
                provenance: None,
            })
            .collect();
        SnapshotIntrospect::new(stratum_proto::introspect::AiContextSnapshot {
            dataset: Some(DatasetMeta {
                frame: "default".to_owned(),
                n_obs: 1_000_000,
                n_vars: n,
                vars,
                ..DatasetMeta::default()
            }),
            ..stratum_proto::introspect::AiContextSnapshot::default()
        })
    }

    fn request(surface: Surface, tier: PrivacyTier) -> PackRequest {
        PackRequest {
            surface,
            tier_inputs: TierInputs {
                global: tier,
                ..TierInputs::default()
            },
            user_text: "why?".to_owned(),
            ..PackRequest::default()
        }
    }

    #[test]
    fn a_ten_thousand_variable_dataset_renders_at_most_four_hundred_lines() {
        // The counter ADR-017 asks for: bounded by the cap, not by the schema.
        let sources = wide(10_000);
        let req = request(Surface::Chat, PrivacyTier::SchemaAndStats);
        let budget = Budget::for_surface(Surface::Chat, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        let vars = packed
            .preview
            .blocks
            .iter()
            .find(|b| b.source == ContextSource::Variables)
            .expect("a variables block");
        let lines = vars.body.lines().filter(|l| l.starts_with('v')).count();
        assert!(lines <= MAX_VARIABLE_LINES, "{lines} lines rendered");
        assert!(
            vars.body.contains("showing"),
            "the header must state the true total"
        );
        assert!(
            vars.body.contains("and 9,"),
            "the trailer must state what was left out"
        );
    }

    #[test]
    fn the_fit_is_a_binary_search_not_a_scan() {
        // 400 candidate line counts must cost O(log n) renders, not O(n). This
        // is the counter that catches a regression to "render, drop one, repeat".
        let sources = wide(10_000);
        let req = request(Surface::Chat, PrivacyTier::SchemaOnly);
        let budget = Budget::for_surface(Surface::Chat, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        assert!(packed.renders <= 32, "{} renders", packed.renders);
    }

    #[test]
    fn a_surface_ceiling_cannot_be_raised_by_a_caller() {
        // `AutoComment` caps itself at schema-only. A caller passing `Full` in
        // every field still gets schema-only, because `pack` overwrites the
        // surface input from `Surface::ceiling` rather than trusting it.
        let mut req = request(Surface::AutoComment, PrivacyTier::Full);
        req.tier_inputs.surface = PrivacyTier::Full;
        assert_eq!(req.effective_tier(), PrivacyTier::SchemaOnly);
    }

    #[test]
    fn tier_off_packs_the_framing_and_the_users_own_words_and_nothing_else() {
        let sources = wide(50);
        let req = request(Surface::Chat, PrivacyTier::Off);
        let budget = Budget::for_surface(Surface::Chat, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        let sources_seen: Vec<ContextSource> =
            packed.preview.blocks.iter().map(|b| b.source).collect();
        assert_eq!(
            sources_seen,
            vec![ContextSource::Task, ContextSource::UserText]
        );
        assert!(
            req.context_request().is_none(),
            "tier off is not a round-trip"
        );
    }

    #[test]
    fn the_preview_records_which_constraint_bound_the_tier() {
        let sources = wide(10);
        let mut req = request(Surface::Chat, PrivacyTier::Full);
        req.tier_inputs.project = Some(PrivacyTier::SchemaOnly);
        let budget = Budget::for_surface(Surface::Chat, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        assert_eq!(packed.preview.effective_tier, PrivacyTier::SchemaOnly);
        assert_eq!(packed.preview.bound_by, TierBound::Project);
    }

    #[test]
    fn a_trimmed_category_is_announced_in_the_prompt() {
        // A model that is not told its context was trimmed will assert that the
        // missing thing does not exist.
        let sources = wide(10_000);
        let mut req = request(Surface::QuickFix, PrivacyTier::SchemaOnly);
        req.focus = Some(Focus {
            header: "analysis.do:1-400".to_owned(),
            text: (0..400)
                .map(|i| format!("gen x{i} = {i}\n"))
                .collect::<String>(),
        });
        let budget = Budget::for_surface(Surface::QuickFix, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        assert!(packed.preview.transcript.contains("lines elided"));
        assert!(!packed.preview.omitted.is_empty());
        assert!(packed
            .preview
            .transcript
            .contains("## OMITTED FROM THIS PROMPT"));
    }

    #[test]
    fn the_focus_reserve_holds_even_when_everything_else_would_have_filled_the_budget() {
        let sources = wide(10_000);
        let mut req = request(Surface::QuickFix, PrivacyTier::SchemaOnly);
        req.focus = Some(Focus {
            header: "analysis.do:20-24".to_owned(),
            text: "regress price mpg weight\nsummarize incom\n".to_owned(),
        });
        let budget = Budget::for_surface(Surface::QuickFix, CommentScope::Block);
        let packed = pack(&req, &sources, &budget, "FRAMING");
        assert!(
            packed
                .preview
                .blocks
                .iter()
                .any(|b| b.source == ContextSource::Focus),
            "the focus is reserved before anything else is placed"
        );
        assert!(packed.preview.est_context_tokens <= budget.context_tokens);
    }
}
