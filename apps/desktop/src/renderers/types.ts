/**
 * What a renderer reads off the wire — CONTRACTS.md §5.
 *
 * **These are not a mirror of the Rust types and must never become one.**
 * §12 is explicit: `ResultEnvelope`, `ResultPayload` and everything under them
 * are GENERATED into `src/ipc/types.ts` by `cargo test export_bindings`, and a
 * hand-written second declaration of the same shape is forbidden. That file does
 * not exist yet (it is emitted by the Tauri host, W17), so this unit follows the
 * idiom `ipc/hand.ts` and `state/results.ts` already established for exactly this
 * situation: declare the **structural minimum** the code actually reads, as
 * `readonly` fields, so the generated type substitutes into every signature the
 * day it lands and nothing here has to be deleted first.
 *
 * Two consequences that are deliberate:
 *
 *  * A field a renderer does not draw is NOT declared. `code_hash` is absent
 *    because no card shows it — which also means this file is unaffected by the
 *    §12 / `ids.rs` disagreement about whether a `CodeHash` reaches TypeScript as
 *    32 hex chars or as 16 numbers.
 *  * Every numeric field a card DRAWS arrives as a pre-formatted string produced
 *    by `stratum_core::fmt` (A6). The raw `f64` is declared beside it only where
 *    a renderer needs it for something that is not display — sorting a
 *    comparison table, scaling a CI bar, deciding whether a count is zero. If a
 *    field's only use is to be printed and it has no `display` sibling, this file
 *    does not declare it, because declaring it is how a `toFixed` gets written.
 */

import type { DatasetStateId, ExecId, ResultId } from "../ipc/hand";

// ---------------------------------------------------------------------------
// Quick actions (§5.1, A22)
// ---------------------------------------------------------------------------

/**
 * The `action` discriminant of the generated `CardAction`, in the wire's
 * snake_case spelling. Declared as a value first so the label table below cannot
 * drift from the union: adding a variant without a label is a type error.
 */
export const CARD_ACTIONS = [
  "raw_output",
  "copy_table",
  "export",
  "hide_output",
  "plot_coefficients",
  "run_margins",
  "compare_model",
  "diagnostics",
  "ai_explain",
  "ai_check_model",
  "ai_suggest_next",
] as const;

export type CardActionTag = (typeof CARD_ACTIONS)[number];

/** The structural minimum the action row reads. Payload fields stay optional. */
export interface CardActionView {
  readonly action: CardActionTag;
  /** `Export { formats }` — "csv", "tex", "md". */
  readonly formats?: readonly string[];
  /** `CompareModel { with }`. */
  readonly with?: readonly ResultId[];
}

// ---------------------------------------------------------------------------
// Envelope (§5.1)
// ---------------------------------------------------------------------------

export interface AssetRefView {
  readonly path: string;
  readonly mime: string;
  readonly bytes: number;
}

export interface RawRefView {
  readonly bytes: number;
  readonly lines: number;
  /** First min(8192, bytes) bytes, cut at a line boundary. */
  readonly head: string;
  readonly truncated: boolean;
  readonly asset: AssetRefView;
}

export interface LayoutHintView {
  readonly rows: number;
  readonly cols: number;
  readonly est_px: number;
}

export interface ResultEnvelopeView {
  readonly result: ResultId;
  readonly revision: number;
  readonly exec: ExecId;
  readonly dataset_state: DatasetStateId;
  readonly cmdline: string;
  readonly duration_us: number;
  readonly rc: number;
  readonly payloads: readonly ResultPayloadView[];
  readonly raw: RawRefView;
  readonly layout_hint: LayoutHintView;
  readonly actions: readonly CardActionView[];
}

// ---------------------------------------------------------------------------
// Payloads (§5.2)
// ---------------------------------------------------------------------------

/**
 * `#[serde(tag = "kind", rename_all = "snake_case")]` on an enum whose variants
 * are newtypes over structs means the struct's fields sit BESIDE the tag, not
 * under a second key. `ResultPayload::Summarize(SummarizePayload)` is
 * `{"kind":"summarize","detail":false,…}`. `fixture.test.ts` decodes the
 * committed MessagePack and asserts exactly that, so this comment is checked
 * rather than believed.
 */
export type ResultPayloadView =
  | LogPayloadView
  | SummarizePayloadView
  | TabulatePayloadView
  | EstimationPayloadView
  | GraphPayloadView
  | TablePayloadView
  | ScalarsPayloadView
  | DataChangedPayloadView
  | ErrorPayloadView
  | UnknownPayloadView;

/** Every `ResultPayload` variant tag. The dispatch table is exhaustive over this. */
export const PAYLOAD_KINDS = [
  "log",
  "summarize",
  "tabulate",
  "estimation",
  "graph",
  "table",
  "scalars",
  "data_changed",
  "error",
  "unknown",
] as const;

export type PayloadKind = (typeof PAYLOAD_KINDS)[number];

// -- log ---------------------------------------------------------------------

export type StyleIdView =
  | "text"
  | "input"
  | "result"
  | "error"
  | "error_token"
  | "hilite"
  | "comment"
  | "heading"
  | "rule"
  | { readonly link: { readonly target_index: number } };

export interface StyledRunView {
  readonly text: string;
  readonly style: StyleIdView;
}

export interface LogPayloadView {
  readonly kind: "log";
  readonly runs: readonly StyledRunView[];
  readonly lines: number;
}

// -- summarize ---------------------------------------------------------------

export type VarKindView = "numeric" | "string" | "labeled" | "binary";

export interface SummarizeDisplayView {
  readonly obs: string;
  readonly mean: string;
  readonly sd: string;
  readonly min: string;
  readonly max: string;
}

export interface SummarizeDetailView {
  /** `[skewness, kurtosis, variance]`. */
  readonly display_stats: readonly [string, string, string];
  /** p1 p5 p10 p25 p50 p75 p90 p95 p99, in that order. */
  readonly display_percentiles: readonly string[];
  readonly display_smallest4: readonly string[];
  readonly display_largest4: readonly string[];
}

export interface SummarizeRowView {
  readonly var: string;
  readonly label: string | null;
  /** The Stata display format, e.g. "%8.0gc". Drives decimal alignment ONLY. */
  readonly format: string;
  /** An exact integer count, not a formatted quantity. */
  readonly missing: number;
  readonly display: SummarizeDisplayView;
  readonly detail: SummarizeDetailView | null;
  readonly var_kind: VarKindView;
  readonly sparkline: readonly number[] | null;
}

export interface SummarizePayloadView {
  readonly kind: "summarize";
  readonly detail: boolean;
  readonly weight: string | null;
  readonly qualifier: string | null;
  readonly rows: readonly SummarizeRowView[];
}

// -- tabulate ----------------------------------------------------------------

export type CellStatView = "freq" | "row_pct" | "col_pct" | "cell_pct" | "expected";

export interface AssocTestView {
  readonly name: string;
  readonly display: string;
}

export interface TruncationView {
  readonly shown_cells: number;
  readonly total_cells: number;
}

export interface TabulatePayloadView {
  readonly kind: "tabulate";
  readonly row_var: string;
  readonly col_var: string | null;
  readonly row_label: string | null;
  readonly col_label: string | null;
  /** `(numeric level, value label if any)`, ascending by level. */
  readonly row_keys: readonly (readonly [number, string | null])[];
  readonly col_keys: readonly (readonly [number, string | null])[];
  /** Row-major counts. Exact integers — printed as-is, never formatted. */
  readonly counts: readonly number[];
  readonly row_totals: readonly number[];
  readonly col_totals: readonly number[];
  readonly total: number;
  readonly requested: readonly CellStatView[];
  readonly tests: readonly AssocTestView[];
  readonly truncated: TruncationView | null;
}

// -- estimation --------------------------------------------------------------

export type SeverityView = "error" | "warning" | "note" | "help";

export interface TermView {
  readonly eq: number;
  readonly name: string;
  /** Name as printed in the coefficient table. */
  readonly display: string;
  /**
   * The raw f64s. Read ONLY by the CI strip, which needs a shared numeric scale;
   * nothing prints them.
   */
  readonly b: number;
  readonly ci_lo: number;
  readonly ci_hi: number;
  /**
   * Read for ONE thing: comparing against a star threshold in the §19 view,
   * where the convention is universal. Never printed — the printed p-value is
   * `display_num[3]`. A bare `regress` card shows no stars at all (06 §6.4).
   */
  readonly p: number;
  /** `[b, se, t, p, ci_lo, ci_hi]` — A6. This is what the table prints. */
  readonly display_num: readonly string[];
  readonly omitted: boolean;
  readonly base: boolean;
  readonly empty: boolean;
}

export interface AnovaTableView {
  /** Row-major `[mss, df_m, ms_m, rss, df_r, ms_r, tss, df_t, ms_t]` — A6. */
  readonly display: readonly string[];
}

export interface ModelFlagView {
  readonly code: string;
  readonly message: string;
  readonly vars: readonly string[];
  readonly severity: SeverityView;
}

export interface EstimationPayloadView {
  readonly kind: "estimation";
  readonly cmd: string;
  readonly cmdline: string;
  readonly depvar: string;
  /** An exact observation count. */
  readonly n: number;
  readonly eq_names: readonly string[];
  readonly terms: readonly TermView[];
  /**
   * `e()` scalars in `ereturn list` order. **No display strings exist for these**
   * (see `estimation/index.tsx` — escalated). Read only as a comparison key and
   * for the §19 footer, never printed by the card.
   */
  readonly scalars: readonly (readonly [string, number])[];
  readonly macros: readonly (readonly [string, string])[];
  readonly anova: AnovaTableView | null;
  readonly vce: string;
  readonly estimates_name: string | null;
  /**
   * Comparability key for spec §19 — blake3-128 of the `e(sample)` bitmap,
   * declared `u64` in Rust.
   *
   * Deliberately not `number`. The mock's own key is `0x5354415441313835`, which
   * is above 2^53: read as a JS double it collides with its neighbours, and a
   * comparability check that silently says "same sample" is the exact
   * methodological error §19 exists to prevent. It is an OPAQUE key here — the
   * only operation is equality via `String(...)` — so it survives whichever of
   * specta's three `u64` policies (number, string, bigint) the generated
   * bindings end up using.
   */
  readonly sample_hash: number | bigint | string;
  readonly diagnostics: readonly ModelFlagView[];
}

// -- graph -------------------------------------------------------------------

export interface GraphPayloadView {
  readonly kind: "graph";
  readonly name: string;
  readonly asset: AssetRefView;
  readonly intrinsic_pt: readonly [number, number];
  readonly scheme: string;
  readonly source_cmd: string;
}

// -- generic table, scalars, data change -------------------------------------

export type AlignView = "left" | "right" | "decimal";

export type CellView =
  | { readonly t: "num"; readonly value: number; readonly display: string }
  | { readonly t: "str"; readonly value: string };

export interface TablePayloadView {
  readonly kind: "table";
  readonly title: string | null;
  readonly colnames: readonly string[];
  readonly rownames: readonly string[];
  /** Row-major. `null` renders as blank, NOT as ".". */
  readonly cells: readonly (CellView | null)[];
  readonly col_align: readonly AlignView[];
}

export type ScalarValueView = CellView;

export interface ScalarsPayloadView {
  readonly kind: "scalars";
  readonly values: readonly (readonly [string, ScalarValueView])[];
}

export interface DataChangedPayloadView {
  readonly kind: "data_changed";
  readonly frame: string;
  readonly obs_before: number;
  readonly obs_after: number;
  readonly vars_before: number;
  readonly vars_after: number;
  readonly created: readonly string[];
  readonly modified: readonly string[];
  readonly dropped: readonly string[];
  readonly renamed: readonly (readonly [string, string])[];
  readonly notes: readonly string[];
}

// -- error -------------------------------------------------------------------

export interface SuggestionView {
  readonly label: string;
  readonly kind: string;
  readonly edits: readonly unknown[];
}

export interface ErrorPayloadView {
  readonly kind: "error";
  readonly severity: SeverityView;
  readonly code: string;
  readonly stata_rc: number | null;
  readonly message: string;
  readonly offending_token: string | null;
  readonly suggestions: readonly SuggestionView[];
  readonly notes: readonly string[];
}

// -- unknown -----------------------------------------------------------------

/** Renders through the raw renderer. No apology, no empty state (§5.2). */
export interface UnknownPayloadView {
  readonly kind: "unknown";
}

// ---------------------------------------------------------------------------
// UI-side state the card needs and the wire does not carry
// ---------------------------------------------------------------------------

/**
 * Why a card is stale, and which upstream block did it (§13). The reason strip
 * names that block: "downstream of a changed block" is not actionable, and
 * `drop if missing(income)` is.
 */
export interface StaleReason {
  /** e.g. "line 12 — `drop if missing(income)`". Rendered verbatim. */
  readonly upstream: string;
  /** e.g. "code changed" | "dataset state D17 → D19". */
  readonly because: string;
}

/** The card's own UI state. Owned by the host (editor widget or Results pane). */
export interface CardUiState {
  readonly collapsed?: boolean;
  readonly stale?: StaleReason;
  /** True while the block that produced this card is running again. */
  readonly running?: boolean;
  /** 0..1. Drives the running hairline's width; there is no spinner. */
  readonly progress?: number;
}
