//! ADR-012 (A5) — one packer, two `SessionIntrospect` implementations, byte-identical output.
//!
//! # What this test is for
//!
//! A5 moved `SessionIntrospect` into `stratum-proto` and gave it two
//! implementations: the engine's own, in `stratum-exec`, and the desktop's
//! event-cache adapter ([`stratum_ai::context::adapter::SnapshotIntrospect`]).
//! `stratum-ai` codes against the trait and **does not know which side it is
//! on**. If that is true, packing the same session through either one produces
//! the same bytes; if it is false, the AI panel in the app and the AI surfaces
//! in the headless CLI answer differently about the same dataset, and no user
//! could tell which one was lying.
//!
//! # Why the second implementation lives here and not in `stratum-exec`
//!
//! ARCHITECTURE §5 gives this crate exactly two workspace dependencies, proto
//! and platform, precisely so the desktop that links it cannot reach the engine
//! (C24). Linking `stratum-exec` from this test to get its implementation would
//! be the coupling the amendment exists to prevent, and a dev-dependency edge is
//! still an edge — `cargo xtask layering` reads the manifest, not the profile.
//!
//! So the second implementation is [`ColumnarIntrospect`], written here and
//! deliberately **structurally unlike** `SnapshotIntrospect`: it stores the
//! session decomposed into per-field maps and rebuilds each proto value on
//! demand, the way an engine holding live state does, instead of cloning
//! sub-trees out of one cached reply. Nothing is shared between the two but the
//! trait. That is what makes the equality meaningful: two independent answers to
//! the same nine questions, and one packer that cannot tell them apart.

use std::collections::BTreeMap;

use stratum_ai::context::adapter::SnapshotIntrospect;
use stratum_ai::context::budget::{Budget, CommentScope};
use stratum_ai::context::packer::{pack, Focus, PackRequest};
use stratum_ai::context::policy::TierInputs;
use stratum_ai::context::PrivacyTier;
use stratum_ai::service::Surface;
use stratum_ai::tasks::prompt;
use stratum_proto::complete::CompletionEnv;
use stratum_proto::data::{FrameInfo, QuickSummary, StorageType, VariableInfo};
use stratum_proto::diagnostic::{Confidence, Diagnostic, Severity, Suggestion, SuggestionKind};
use stratum_proto::ids::{DatasetStateId, SessionId, VarId, VarIdx};
use stratum_proto::introspect::{
    AiContextSnapshot, DatasetMeta, EstimateHandle, MacroInfo, MacroScope, SessionIntrospect,
    StoredResultsView,
};

// ---------------------------------------------------------------------------
// The session both implementations describe
// ---------------------------------------------------------------------------

const FRAME: &str = "default";
const N_OBS: u64 = 2_246;
const STATE: DatasetStateId = DatasetStateId(0x5eed);

/// Names chosen to exercise the packer's edges: one that pseudonymises
/// (`respondent_ssn`), one long label, one with a value label, one `str`.
fn variables() -> Vec<VariableInfo> {
    let spec: [(&str, StorageType, &str, Option<&str>); 6] = [
        ("price", StorageType::Int, "Price in 1978 dollars", None),
        ("mpg", StorageType::Byte, "Mileage (mpg)", None),
        ("weight", StorageType::Int, "Weight (lbs.)", None),
        (
            "foreign",
            StorageType::Byte,
            "Car origin",
            Some("origin_lbl"),
        ),
        (
            "make",
            StorageType::Str { width: 18 },
            "Make and model",
            None,
        ),
        (
            "respondent_ssn",
            StorageType::Long,
            "Respondent social security number",
            None,
        ),
    ];
    spec.iter()
        .enumerate()
        .map(|(i, (name, ty, label, vl))| VariableInfo {
            idx: VarIdx(u32::try_from(i).expect("six variables")),
            id: VarId(u32::try_from(i + 100).expect("six variables")),
            name: (*name).to_owned(),
            ty: *ty,
            label: (*label).to_owned(),
            format: "%9.0g".to_owned(),
            value_label: vl.map(str::to_owned),
            n_missing: u64::try_from(i).expect("six variables"),
            provenance: None,
        })
        .collect()
}

fn summaries() -> Vec<QuickSummary> {
    variables()
        .iter()
        .filter(|v| !matches!(v.ty, StorageType::Str { .. }))
        .enumerate()
        .map(|(i, v)| QuickSummary {
            var: v.name.clone(),
            state: STATE,
            n: N_OBS,
            n_missing: v.n_missing,
            mean: Some(1_000.0 + i as f64),
            median: Some(900.0 + i as f64),
            sd: Some(10.5 + i as f64),
            min: Some(1.0),
            max: Some(9_999.0),
            display: vec![("Mean".to_owned(), format!("{}", 1_000 + i))],
            sparkline: None,
            deferred: false,
        })
        .collect()
}

fn macros() -> Vec<MacroInfo> {
    vec![
        MacroInfo {
            name: "controls".to_owned(),
            scope: MacroScope::Local,
            value: "mpg weight foreign".to_owned(),
            truncated: false,
            defined_at: None,
        },
        MacroInfo {
            name: "datadir".to_owned(),
            scope: MacroScope::Global,
            value: "data/raw".to_owned(),
            truncated: false,
            defined_at: None,
        },
    ]
}

fn stored() -> StoredResultsView {
    StoredResultsView {
        r_scalars: vec![("N".to_owned(), 2_246.0), ("mean".to_owned(), 6_165.257)],
        r_macros: vec![("varlist".to_owned(), "price".to_owned())],
        e_scalars: vec![("N".to_owned(), 2_246.0), ("r2".to_owned(), 0.293_4)],
        e_macros: vec![("cmd".to_owned(), "regress".to_owned())],
        e_b_colnames: vec!["mpg".to_owned(), "weight".to_owned(), "_cons".to_owned()],
        ..StoredResultsView::default()
    }
}

fn estimates() -> Vec<EstimateHandle> {
    vec![EstimateHandle {
        name: "baseline".to_owned(),
        cmd: "regress".to_owned(),
        depvar: "price".to_owned(),
        n: N_OBS,
        sample_hash: 0xdead_beef,
        result: None,
        stored_at: None,
    }]
}

fn errors() -> Vec<Diagnostic> {
    vec![Diagnostic {
        severity: Severity::Error,
        code: "STATA0111".to_owned(),
        stata_rc: Some(111),
        message: "variable incom not found".to_owned(),
        file: None,
        span: None,
        offending_token: Some("incom".to_owned()),
        block: None,
        related: Vec::new(),
        suggestions: vec![Suggestion {
            label: "Did you mean `income`?".to_owned(),
            kind: SuggestionKind::Rename,
            edits: Vec::new(),
        }],
        notes: vec!["did you mean income?".to_owned()],
        confidence: Confidence::Exact,
    }]
}

fn dataset() -> DatasetMeta {
    DatasetMeta {
        frame: FRAME.to_owned(),
        state: STATE,
        n_obs: N_OBS,
        n_vars: u32::try_from(variables().len()).expect("six variables"),
        sorted_by: vec!["make".to_owned()],
        label: "1978 automobile data".to_owned(),
        source_path: None,
        vars: variables(),
        truncated: false,
    }
}

fn snapshot() -> AiContextSnapshot {
    AiContextSnapshot {
        session: SessionId(9),
        generation: 12,
        dataset: Some(dataset()),
        macros: macros(),
        stored: Some(stored()),
        estimates: estimates(),
        recent_errors: errors(),
        recent_commands: vec![
            "use auto, clear".to_owned(),
            "summarize price".to_owned(),
            "regress price mpg weight".to_owned(),
        ],
        var_summaries: summaries(),
    }
}

// ---------------------------------------------------------------------------
// The second implementation
// ---------------------------------------------------------------------------

/// A `SessionIntrospect` that holds the session the way an engine does.
///
/// Deliberately not a snapshot: variables live in a per-frame map, summaries in
/// a name-keyed map, errors in a ring that it drains from the end, and every
/// proto value is *constructed* per call rather than cloned out of a stored
/// reply. If the packer depended on anything but the trait's nine answers — an
/// ordering that happened to come out of a `Vec`, a `frames()` shape that only
/// the adapter produces — the two would disagree here.
struct ColumnarIntrospect {
    frames: BTreeMap<String, (u64, u32, Vec<String>)>,
    /// frame → (name → column metadata), so a lookup is a map hit, not a scan.
    columns: BTreeMap<String, BTreeMap<String, VariableInfo>>,
    /// Insertion order is not the map's order, so it is kept separately —
    /// exactly the trap a `BTreeMap`-backed engine would fall into.
    column_order: BTreeMap<String, Vec<String>>,
    stats: BTreeMap<String, QuickSummary>,
    locals: Vec<(String, String)>,
    globals: Vec<(String, String)>,
    stored: StoredResultsView,
    estimates: Vec<EstimateHandle>,
    /// Newest first, which is the opposite of the trait's contract; `recent_errors`
    /// has to reverse it.
    error_ring: Vec<Diagnostic>,
    label: String,
}

impl ColumnarIntrospect {
    fn build() -> Self {
        let vars = variables();
        let mut columns = BTreeMap::new();
        let inner: BTreeMap<String, VariableInfo> =
            vars.iter().map(|v| (v.name.clone(), v.clone())).collect();
        columns.insert(FRAME.to_owned(), inner);

        let mut column_order = BTreeMap::new();
        column_order.insert(
            FRAME.to_owned(),
            vars.iter().map(|v| v.name.clone()).collect::<Vec<_>>(),
        );

        let mut frames = BTreeMap::new();
        frames.insert(
            FRAME.to_owned(),
            (
                N_OBS,
                u32::try_from(vars.len()).expect("six variables"),
                vec!["make".to_owned()],
            ),
        );

        let mut error_ring = errors();
        error_ring.reverse();

        Self {
            frames,
            columns,
            column_order,
            stats: summaries()
                .into_iter()
                .map(|s| (s.var.clone(), s))
                .collect(),
            locals: macros()
                .iter()
                .filter(|m| m.scope == MacroScope::Local)
                .map(|m| (m.name.clone(), m.value.clone()))
                .collect(),
            globals: macros()
                .iter()
                .filter(|m| m.scope == MacroScope::Global)
                .map(|m| (m.name.clone(), m.value.clone()))
                .collect(),
            stored: stored(),
            estimates: estimates(),
            error_ring,
            label: "1978 automobile data".to_owned(),
        }
    }
}

impl SessionIntrospect for ColumnarIntrospect {
    fn frames(&self) -> Vec<FrameInfo> {
        self.frames
            .iter()
            .map(|(name, (n_obs, n_vars, sorted_by))| FrameInfo {
                name: name.clone(),
                n_obs: *n_obs,
                n_vars: *n_vars,
                sorted_by: sorted_by.clone(),
                changed: false,
                state: STATE,
            })
            .collect()
    }

    fn variables(&self, frame: &str) -> Vec<VariableInfo> {
        let Some(order) = self.column_order.get(frame) else {
            return Vec::new();
        };
        let cols = &self.columns[frame];
        order.iter().map(|n| cols[n].clone()).collect()
    }

    fn var_stats(&self, frame: &str, v: &str) -> Option<QuickSummary> {
        self.columns.get(frame)?.get(v)?;
        self.stats.get(v).cloned()
    }

    fn macros(&self) -> Vec<MacroInfo> {
        // Rebuilt from two stores, in the same order the adapter's single vector
        // carries: locals then globals.
        self.locals
            .iter()
            .map(|(name, value)| (name, value, MacroScope::Local))
            .chain(
                self.globals
                    .iter()
                    .map(|(name, value)| (name, value, MacroScope::Global)),
            )
            .map(|(name, value, scope)| MacroInfo {
                name: name.clone(),
                scope,
                value: value.clone(),
                truncated: false,
                defined_at: None,
            })
            .collect()
    }

    fn stored_results(&self) -> StoredResultsView {
        self.stored.clone()
    }

    fn estimates_store(&self) -> Vec<EstimateHandle> {
        self.estimates.clone()
    }

    fn recent_errors(&self, n: usize) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = self.error_ring.iter().take(n).cloned().collect();
        out.reverse();
        out
    }

    fn dataset_meta(&self) -> DatasetMeta {
        let (n_obs, n_vars, sorted_by) = &self.frames[FRAME];
        DatasetMeta {
            frame: FRAME.to_owned(),
            state: STATE,
            n_obs: *n_obs,
            n_vars: *n_vars,
            sorted_by: sorted_by.clone(),
            label: self.label.clone(),
            source_path: None,
            vars: self.variables(FRAME),
            truncated: false,
        }
    }

    fn completion_env(&self) -> CompletionEnv {
        // The packer does not read this. Answering with a distinct shape is on
        // purpose: if a future packer started reading it, this test would begin
        // failing rather than silently coupling to one implementation.
        CompletionEnv {
            generation: 0,
            frame: FRAME.to_owned(),
            frames: vec![FRAME.to_owned()],
            varnames: self
                .variables(FRAME)
                .iter()
                .map(|v| v.name.clone())
                .collect(),
            var_total: u32::try_from(self.variables(FRAME).len()).expect("six variables"),
            truncated: false,
            locals: self.locals.iter().map(|(n, _)| n.clone()).collect(),
            globals: self.globals.iter().map(|(n, _)| n.clone()).collect(),
            scalars: Vec::new(),
            matrices: Vec::new(),
            programs: Vec::new(),
            e_names: Vec::new(),
            r_names: Vec::new(),
            value_labels: Vec::new(),
            stored_estimates: self.estimates.iter().map(|e| e.name.clone()).collect(),
            cwd: camino::Utf8PathBuf::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// The parity assertions
// ---------------------------------------------------------------------------

fn request(surface: Surface, tier: PrivacyTier) -> PackRequest {
    PackRequest {
        surface,
        session: SessionId(9),
        tier_inputs: TierInputs {
            global: tier,
            ..TierInputs::default()
        },
        focus: Some(Focus {
            header: "analysis.do:20-24".to_owned(),
            text: "regress price mpg weight\nsummarize incom\n".to_owned(),
        }),
        user_text: "why did that fail?".to_owned(),
        recent_commands: snapshot().recent_commands,
        ..PackRequest::default()
    }
}

/// Every (surface, tier) pair the product can be in. Nine surfaces by four
/// tiers: the parity claim is about the packer, not about one lucky path.
fn every_combination() -> Vec<(Surface, PrivacyTier)> {
    let mut out = Vec::new();
    for s in Surface::ALL {
        for t in PrivacyTier::ALL {
            out.push((s, t));
        }
    }
    out
}

#[test]
fn the_same_packer_over_both_implementations_produces_byte_identical_previews() {
    // **The A5 acceptance bullet.**
    let desktop = SnapshotIntrospect::new(snapshot());
    let engine = ColumnarIntrospect::build();

    let mut compared = 0u32;
    for (surface, tier) in every_combination() {
        let req = request(surface, tier);
        let budget = Budget::for_surface(surface, CommentScope::Block);
        let framing = prompt::framing(surface);

        let a = pack(&req, &desktop, &budget, &framing);
        let b = pack(&req, &engine, &budget, &framing);

        assert_eq!(
            a.preview.transcript, b.preview.transcript,
            "{surface}/{tier}: the two implementations packed different bytes"
        );
        assert_eq!(
            a.preview, b.preview,
            "{surface}/{tier}: the previews differ outside the transcript"
        );
        assert_eq!(
            a.pseudonyms, b.pseudonyms,
            "{surface}/{tier}: the pseudonym maps differ, so the reply would un-map differently"
        );
        assert_eq!(
            a.prompt, b.prompt,
            "{surface}/{tier}: the wire prompt differs"
        );
        compared += 1;
    }

    // The counter ADR-017 asks for: how many (surface, tier) pairs this actually
    // compared, not how long it took. A refactor that quietly stopped iterating
    // one dimension shows up here as a number, not as a faster test.
    assert_eq!(
        compared, 36,
        "nine surfaces times four tiers must all be compared"
    );
}

#[test]
fn the_two_implementations_are_not_secretly_the_same_object() {
    // A parity test between an implementation and itself proves nothing. These
    // two answer the trait from different storage, and `completion_env` — the
    // one method the packer never reads — is where that shows.
    let desktop = SnapshotIntrospect::new(snapshot());
    let engine = ColumnarIntrospect::build();
    assert_ne!(
        desktop.completion_env().generation,
        engine.completion_env().generation,
        "the fixtures must not be two views of one value"
    );
    assert_eq!(
        desktop.dataset_meta(),
        engine.dataset_meta(),
        "…while still describing the same session"
    );
}

#[test]
fn no_tier_two_item_ever_appears_in_a_tier_one_prompt() {
    // The gate, asserted end to end through both implementations rather than in
    // the unit that owns `filter`. Statistics are the tier-2 payload in this
    // fixture: a mean of 1000.0 is a number about the user's data.
    let desktop = SnapshotIntrospect::new(snapshot());
    let engine = ColumnarIntrospect::build();

    for sources in [
        &desktop as &dyn SessionIntrospect,
        &engine as &dyn SessionIntrospect,
    ] {
        for surface in Surface::ALL {
            let req = request(surface, PrivacyTier::SchemaOnly);
            let budget = Budget::for_surface(surface, CommentScope::Block);
            let packed = pack(&req, sources, &budget, &prompt::framing(surface));

            assert!(
                packed
                    .preview
                    .blocks
                    .iter()
                    .all(|b| b.min_tier <= PrivacyTier::SchemaOnly),
                "{surface}: a block above the effective tier survived the gate"
            );
            assert_eq!(packed.preview.effective_tier, PrivacyTier::SchemaOnly);
            for forbidden in ["6165.257", "1000", "0.2934", "mpg weight foreign"] {
                assert!(
                    !packed.preview.transcript.contains(forbidden),
                    "{surface}: tier-1 prompt contains the tier-2+ value {forbidden:?}"
                );
            }
        }
    }
}

#[test]
fn a_sensitive_variable_is_pseudonymised_identically_on_both_sides() {
    // If the two disagreed about pseudonyms, un-mapping a reply would restore
    // the wrong name in one of them — a data-corruption bug that would only
    // appear in the app or only in the CLI.
    let desktop = SnapshotIntrospect::new(snapshot());
    let engine = ColumnarIntrospect::build();
    let req = request(Surface::Chat, PrivacyTier::SchemaAndStats);
    let budget = Budget::for_surface(Surface::Chat, CommentScope::Block);
    let framing = prompt::framing(Surface::Chat);

    let a = pack(&req, &desktop, &budget, &framing);
    let b = pack(&req, &engine, &budget, &framing);

    assert!(
        !a.pseudonyms.is_empty(),
        "respondent_ssn must have been pseudonymised, or this test asserts nothing"
    );
    assert_eq!(a.pseudonyms, b.pseudonyms);
    assert!(
        !a.preview.transcript.contains("respondent_ssn"),
        "the real name reached the prompt"
    );
}
