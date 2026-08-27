//! CONTRACTS.md §5 — results.
//!
//! One envelope, many payloads. Two rules run through the whole section:
//!
//! * **Bytes are never inline** (ARCHITECTURE C23). Graph images and raw classic
//!   text travel as an [`AssetRef`]; a 1.5 MB SVG in a MessagePack event blows
//!   the 16 ms / 64 KB coalescing budget for every subscribed window.
//! * **Renderers never format numbers.** Every number a card draws arrives twice:
//!   once as an `f64` for sorting and export, once as the display string
//!   `stratum_core::fmt` already produced for the classic text (A6). The card and
//!   the Classic pane therefore cannot disagree, because they are printing the
//!   same bytes.

use serde::{Deserialize, Serialize};

use crate::diagnostic::{Diagnostic, Severity};
use crate::ids::{BlockId, CodeHash, DatasetStateId, ExecutionId, ResultId};
use crate::UnixMs;

// ---------------------------------------------------------------------------
// §5.1 Envelope
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ResultEnvelope {
    pub result: ResultId,
    /// Bumped when the SAME result is re-rendered (e.g. linesize change).
    /// The CM6 widget's `eq()` compares (result, revision).
    pub revision: u32,
    pub exec: ExecutionId,
    /// None for command-bar, selection, and CLI runs.
    pub block: Option<BlockId>,
    pub dataset_state: DatasetStateId,
    pub code_hash: CodeHash,
    /// The command as submitted, after macro expansion.
    pub cmdline: String,
    pub started_at_ms: UnixMs,
    pub duration_us: u64,
    pub rc: u32,
    pub payloads: Vec<ResultPayload>,
    /// MANDATORY on every envelope. Spec §17: "Every result exposes View
    /// raw/classic output. Compatibility is never hidden behind the richer UI."
    pub raw: RawRef,
    pub layout_hint: LayoutHint,
    /// **AMENDED (A22).** The quick-action row (spec §4), computed IN RUST from
    /// `stratum_effects::CommandRegistry` — i.e. from what this build actually
    /// implements. The frontend renders exactly these and never invents one.
    ///
    /// Without this the flagship `regress` card offers "Run margins" and
    /// "Coefficient plot" (both promised by `07`), the user clicks, and gets
    /// exit-10 "unsupported" — because `margins` is out of Pass-1 scope and
    /// `twoway rcap` was not in the graph scope. A promise that fails on click is
    /// worse than an absent button.
    pub actions: Vec<CardAction>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CardAction {
    /// Always present, always last, on every payload variant including Unknown.
    RawOutput,
    CopyTable,
    /// "csv", "tex", "md"
    Export {
        formats: Vec<String>,
    },
    HideOutput,
    /// Deterministic. Emits `twoway rcap ... || scatter ...` into the log.
    PlotCoefficients,
    /// Only when `CommandRegistry::implements("margins")` AND `e(predict)` is set.
    RunMargins,
    /// Only when a prior comparable estimation exists in this session.
    CompareModel {
        with: Vec<ResultId>,
    },
    Diagnostics,
    /// AI surfaces. Rendered only when `Availability::Ready`; otherwise absent
    /// (NOT greyed out — a dead button is clutter, §21).
    AiExplain,
    AiCheckModel,
    AiSuggestNext,
}

/// Raw classic output. `head` is inline so `Raw ▸` is instant for the ~99% of
/// results under 8 KB; the full text is always at
/// `stratum-asset://localhost/result/{session}/{result}/raw`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct RawRef {
    pub bytes: u64,
    pub lines: u32,
    /// First min(8192, bytes) bytes, cut at a line boundary.
    pub head: String,
    pub truncated: bool,
    pub asset: AssetRef,
}

/// Opaque handle resolvable as `stratum-asset://localhost/{path}`. Never a
/// filesystem path.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AssetRef {
    pub path: String,
    pub mime: String,
    pub bytes: u64,
}

/// Lets the card be laid out at its final height on FIRST paint, which is what
/// keeps scroll anchoring sane (WebKit has no `overflow-anchor`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LayoutHint {
    pub rows: u32,
    pub cols: u32,
    pub est_px: u32,
}

// ---------------------------------------------------------------------------
// §5.2 Payloads
// ---------------------------------------------------------------------------

// `EstimationPayload` is ~576 bytes and the next variant is ~296, so clippy asks
// for a `Box`. Refused: the variant shapes here are frozen contract (§5.2) and
// boxing one would change every construction and match site in stats, runtime,
// exec and the CLI to save a copy on a type that is built once per command and
// then moved into an event.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultPayload {
    Log(LogPayload),
    Summarize(SummarizePayload),
    Tabulate(TabulatePayload),
    Estimation(EstimationPayload),
    Graph(GraphRef),
    /// Any matrix-shaped result we do not have a bespoke renderer for.
    Table(GenericTable),
    /// **CONTRACT DEVIATION, reported by W00.** CONTRACTS.md §5.2 spells this
    /// `Scalars(Vec<(String, ScalarValue)>)`. A newtype variant of an internally
    /// tagged enum must serialize as a map — serde has to put `"kind"` somewhere
    /// — so a variant wrapping a sequence fails at runtime with "cannot serialize
    /// tagged newtype variant ResultPayload::Scalars containing a sequence", in
    /// both JSON and MessagePack. Every other newtype variant here wraps a
    /// struct and is fine. Naming the field is the only shape that both keeps
    /// `tag = "kind"` and round-trips; the wire form is
    /// `{"kind":"scalars","values":[…]}`.
    Scalars {
        values: Vec<(String, ScalarValue)>,
    },
    /// After gen/drop/merge/reshape/… — feeds the "✓ 0.08s · +1 var" chip.
    DataChanged(DataChangeSummary),
    Error(Diagnostic),
    /// Renders through the raw renderer. No apology, no empty state.
    Unknown,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LogPayload {
    pub runs: Vec<StyledRun>,
    pub lines: u32,
}

/// Styled runs are produced IN RUST and never parsed in the frontend. Two
/// producers, one type:
///   * `stratum_runtime::smcl` parses SMCL emitted by *user* code (`display as
///     result`, `.sthlp`, log replay) into runs;
///   * `stratum_stats::*::classic_text(linesize) -> Vec<StyledRun>` emits runs
///     **directly** for every built-in statistical table.
///
/// **AMENDED (A12).** `classic_text` previously returned a flat `String` that the
/// runtime was supposed to "wrap". It cannot: given a byte-exact 78-column
/// regress table as plain text, nothing can recover which spans were `{res}` and
/// which were `{txt}`, so every statistics table would have rendered as one
/// `StyleId::Text` run and the Classic pane could not print result values in
/// Stata's distinct ink.
///
/// [`crate::styled::to_plain`] is the single flattening function; the CLI text
/// mode, the log file writer, `log_copy` and the byte-exactness goldens all go
/// through it, so styling can never change the bytes.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct StyledRun {
    pub text: String,
    pub style: StyleId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum StyleId {
    Text,
    Input,
    Result,
    Error,
    ErrorToken,
    Hilite,
    Comment,
    Heading,
    Rule,
    Link { target_index: u32 },
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ScalarValue {
    /// Raw f64. Missing values arrive in Stata's sentinel encoding — the
    /// frontend MUST NOT compare them numerically; use `display` instead.
    Num {
        value: f64,
        display: String,
    },
    Str {
        value: String,
    },
}

// ---------------------------------------------------------------------------
// `summarize` (spec §17)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SummarizePayload {
    pub detail: bool,
    pub weight: Option<String>,
    /// "if income>0"
    pub qualifier: Option<String>,
    pub rows: Vec<SummarizeRow>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SummarizeRow {
    pub var: String,
    pub label: Option<String>,
    /// The variable's Stata display format, e.g. "%8.0gc". The renderer derives
    /// decimal alignment from this; it MUST NOT reformat the numbers.
    pub format: String,
    pub obs: u64,
    pub missing: u64,
    pub mean: f64,
    pub sd: f64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    /// Pre-formatted display strings, produced by `stratum_core::fmt`, so the
    /// card and the classic text can never disagree. Same order as the fields.
    pub display: SummarizeDisplay,
    pub detail: Option<SummarizeDetail>,
    pub var_kind: VarKind,
    /// 24-bin histogram counts. Deterministic, cheap, ALWAYS sent.
    pub sparkline: Option<Vec<u32>>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SummarizeDisplay {
    pub obs: String,
    pub mean: String,
    pub sd: String,
    pub min: String,
    pub max: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SummarizeDetail {
    /// m3/m2^1.5. BIASED moment ratio, matching Stata.
    pub skewness: f64,
    /// m4/m2^2. NOT excess kurtosis (normal → 3).
    pub kurtosis: f64,
    /// N-1 denominator.
    pub variance: f64,
    /// p1 p5 p10 p25 p50 p75 p90 p95 p99, in that order.
    pub percentiles: [f64; 9],
    /// Ascending. Slots beyond `obs` hold the Stata missing sentinel.
    pub smallest4: [f64; 4],
    pub largest4: [f64; 4],
    /// **AMENDED (A6).** Pre-formatted, parallel to the fields above, in the
    /// order `[skewness, kurtosis, variance]`.
    pub display_stats: [String; 3],
    pub display_percentiles: [String; 9],
    pub display_smallest4: [String; 4],
    pub display_largest4: [String; 4],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum VarKind {
    Numeric,
    String,
    Labeled,
    Binary,
}

// ---------------------------------------------------------------------------
// `tabulate`
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TabulatePayload {
    pub row_var: String,
    pub col_var: Option<String>,
    pub row_label: Option<String>,
    pub col_label: Option<String>,
    /// (numeric level, value label if any). Ascending by level.
    pub row_keys: Vec<(f64, Option<String>)>,
    pub col_keys: Vec<(f64, Option<String>)>,
    /// Row-major, len == row_keys.len() * max(1, col_keys.len()).
    pub counts: Vec<u64>,
    pub row_totals: Vec<u64>,
    pub col_totals: Vec<u64>,
    pub total: u64,
    /// Rendered in this order: Freq → RowPct → ColPct → CellPct.
    pub requested: Vec<CellStat>,
    pub tests: Vec<AssocTest>,
    /// Set when row*col > 5000; the card renders the first 2000 cells and
    /// offers "Open in Table Viewer".
    pub truncated: Option<Truncation>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum CellStat {
    Freq,
    RowPct,
    ColPct,
    CellPct,
    Expected,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AssocTest {
    pub name: String,
    pub stat: f64,
    pub df: Option<f64>,
    pub p: f64,
    pub display: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Truncation {
    pub shown_cells: u32,
    pub total_cells: u64,
}

// ---------------------------------------------------------------------------
// Estimation (spec §§4, 17, 19)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct EstimationPayload {
    /// "regress"
    pub cmd: String,
    pub cmdline: String,
    pub depvar: String,
    pub n: u64,
    pub rank: u32,
    /// `[""]` for single-equation models.
    pub eq_names: Vec<String>,
    pub terms: Vec<Term>,
    /// INSERTION-ORDERED, matching `ereturn list`. NOT a map — `05` §8.7's
    /// ordering is part of the classic output contract.
    pub scalars: Vec<(String, f64)>,
    pub macros: Vec<(String, String)>,
    /// Present for OLS; absent under robust/cluster (Stata prints no ANOVA block).
    pub anova: Option<AnovaTable>,
    /// "ols" | "robust" | "cluster rep78"
    pub vce: String,
    /// 95.0
    pub ci_level: f64,
    /// Set by `estimates store`.
    pub estimates_name: Option<String>,
    /// Comparability key for spec §19. blake3-128 of the e(sample) bitmap.
    pub sample_hash: u64,
    /// Deterministic model notes: collinearity omissions, singleton groups,
    /// perfect prediction. NEVER AI-generated.
    pub diagnostics: Vec<ModelFlag>,
    /// Diagnostic only. Never used to compute anything reported. Warn above 1e7.
    pub cond_number: Option<f64>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Term {
    pub eq: u16,
    /// Name as it appears in e(b) colnames, including any `o.`/`b.` stripe.
    pub name: String,
    /// Name as printed in the coefficient table.
    pub display: String,
    pub b: f64,
    pub se: f64,
    pub t: f64,
    pub p: f64,
    pub ci_lo: f64,
    pub ci_hi: f64,
    /// **AMENDED (A6).** Pre-formatted, in the order `[b, se, t, p, ci_lo,
    /// ci_hi]`, produced by `stratum_core::fmt` — the SAME call that produced the
    /// classic text. W14's rule "renderers never reformat numbers" was
    /// unimplementable without this: the flagship `regress` card would have had
    /// to re-implement `fmt_g` in TypeScript, and it would have disagreed with
    /// `classic_text` on the first tie-breaking case.
    pub display_num: [String; 6],
    /// Standardized coefficient. None for _cons and under vce(cluster).
    pub beta: Option<f64>,
    /// Renders "0  (omitted)".
    pub omitted: bool,
    /// Renders "(base)".
    pub base: bool,
    pub empty: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct AnovaTable {
    pub mss: f64,
    pub df_m: f64,
    pub ms_m: f64,
    pub rss: f64,
    pub df_r: f64,
    pub ms_r: f64,
    pub tss: f64,
    pub df_t: f64,
    pub ms_t: f64,
    /// **AMENDED (A6).** Row-major in the same order as the fields above.
    pub display: [String; 9],
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ModelFlag {
    pub code: String,
    pub message: String,
    pub vars: Vec<String>,
    pub severity: Severity,
}

// ---------------------------------------------------------------------------
// Graph, generic table, data change
// ---------------------------------------------------------------------------

/// Bytes are NEVER inline (ARCHITECTURE C23) — a 1.5 MB SVG in a MessagePack
/// event blows the 16 ms event-coalescing budget for every subscribed window.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct GraphRef {
    /// Stata graph name.
    pub name: String,
    /// `stratum-asset://localhost/graph/{s}/{r}.svg`
    pub asset: AssetRef,
    pub intrinsic_pt: (f32, f32),
    pub scheme: String,
    pub source_cmd: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct GenericTable {
    pub title: Option<String>,
    pub colnames: Vec<String>,
    pub rownames: Vec<String>,
    /// Row-major. `None` renders as blank, NOT as ".".
    pub cells: Vec<Option<Cell>>,
    pub col_align: Vec<Align>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Cell {
    Num { value: f64, display: String },
    Str { value: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    Right,
    Decimal,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct DataChangeSummary {
    pub frame: String,
    pub obs_before: u64,
    pub obs_after: u64,
    pub vars_before: u32,
    pub vars_after: u32,
    pub created: Vec<String>,
    pub modified: Vec<String>,
    pub dropped: Vec<String>,
    pub renamed: Vec<(String, String)>,
    /// e.g. "(1 missing value generated)"
    pub notes: Vec<String>,
}
