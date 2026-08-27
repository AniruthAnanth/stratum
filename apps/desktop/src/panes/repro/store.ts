/**
 * The reproducibility report — spec §16; 03 §10; CONTRACTS §9.
 *
 * The panel this store feeds makes a claim on the researcher's behalf, and the
 * only interesting engineering question about it is *when it is allowed to*.
 *
 * > "`runs_clean` is `Tri::Unknown` until verification, and the UI renders 'not
 * > verified' rather than a green tick. We never infer a green tick from static
 * > analysis alone; the claim 'this file runs from a clean state' is only made
 * > after we have actually run it from a clean state." — 03 §10.2
 *
 * {@link canTickRunsClean} is that sentence as code, and it is deliberately
 * stricter than `runs_clean === "yes"`: the tick also requires `verified_by`,
 * the `ExecutionId` of the run that proved it. An engine that set the `Tri`
 * without recording the execution would be making the claim without the
 * evidence, and this frontend refuses to draw it. `repro.test.tsx` asserts
 * exactly that case.
 *
 * Everything else here is derivation with no opinions: the four `Tri`s and the
 * R001 count become five rows, in spec §16's order and wording.
 */

import { createSignal } from "solid-js";
import type { BlockId, DocumentId, ExecId } from "../../ipc/hand";

// ---------------------------------------------------------------------------
// The structural minimum of CONTRACTS §9
// ---------------------------------------------------------------------------

export type TriView = "yes" | "no" | "unknown";
export type SeverityView = "error" | "warning" | "note" | "help";
export type ConfidenceView = "certain" | "likely" | "possible" | "unknown";

export interface SuggestionView {
  readonly label?: string;
  readonly title?: string;
}

export interface FindingView {
  /** "R001" — the ONE registry of ARCHITECTURE C14. */
  readonly lint: string;
  readonly severity: SeverityView;
  readonly title: string;
  readonly message: string;
  readonly detail?: string | null;
  readonly block?: BlockId | null;
  readonly span?: readonly [number, number] | null;
  /** Deterministic text edit. NEVER AI-generated (CONTRACTS §9). */
  readonly fix?: SuggestionView | null;
  readonly confidence?: ConfidenceView;
}

export interface ReproReportView {
  readonly doc: DocumentId;
  readonly generated_at_ms: number;
  /** `unknown` until an ACTUAL `Isolation::Subprocess` clean run verifies it. */
  readonly runs_clean: TriView;
  /** The clean run that proved it. No id, no tick. */
  readonly verified_by: ExecId | null;
  readonly verified_duration_us?: number | null;
  readonly seed_defined: TriView;
  readonly inputs_resolved: TriView;
  readonly no_hidden_deps: TriView;
  readonly findings: readonly FindingView[];
  /** Suppressions are listed so they cannot hide problems silently. */
  readonly suppressed: readonly (readonly [string, readonly [number, number]])[];
}

// ---------------------------------------------------------------------------
// The tick that has to be earned
// ---------------------------------------------------------------------------

/**
 * Whether "✓ File runs from clean state" may be drawn.
 *
 * Two conditions, and the second is the one that matters: `verified_by` is
 * `Some(ExecutionId)` only after an `Isolation::Subprocess` run (ARCHITECTURE
 * §7.7, "the ONLY thing that may set the §16 tick"). Static analysis can reach
 * `runs_clean: Yes` — nothing stops a future lint pass being confident — and it
 * can never produce an execution id, so requiring the id is what makes the
 * frontend's refusal structural rather than a matter of the engine behaving.
 */
export function canTickRunsClean(report: ReproReportView): boolean {
  return report.runs_clean === "yes" && report.verified_by !== null;
}

// ---------------------------------------------------------------------------
// The five rows of spec §16
// ---------------------------------------------------------------------------

export type RowMark = "ok" | "warn" | "bad" | "unverified";

export interface ReproRow {
  /** Stable id for tests and for the row's key. */
  readonly id: "runs_clean" | "seed" | "inputs" | "hidden_deps" | "paths";
  readonly mark: RowMark;
  /** The sentence spec §16 prints, with its count where it has one. */
  readonly label: string;
  /** The lint ids this row rolls up (03 §10.2). */
  readonly lints: readonly string[];
  readonly detail?: string;
}

const markOf = (tri: TriView): RowMark => (tri === "yes" ? "ok" : tri === "no" ? "bad" : "warn");

export function countFindings(report: ReproReportView, lints: readonly string[]): number {
  return report.findings.filter((f) => lints.includes(f.lint)).length;
}

/**
 * Spec §16's panel, rolled up per 03 §10.2. Exactly five rows, always five —
 * a row that vanishes when it is clean would make "nothing to report" and
 * "nobody checked" look the same.
 */
export function reproRows(report: ReproReportView): ReproRow[] {
  const dynamicPaths = countFindings(report, ["R005"]);
  const absolutePaths = countFindings(report, ["R001"]);

  const runsClean: ReproRow = canTickRunsClean(report)
    ? {
        id: "runs_clean",
        mark: "ok",
        label: "File runs from clean state",
        lints: [],
        detail: `Verified by E${String(report.verified_by)}.`,
      }
    : {
        id: "runs_clean",
        mark: report.runs_clean === "no" ? "bad" : "unverified",
        label:
          report.runs_clean === "no"
            ? "File does not run from clean state"
            : "File runs from clean state — not verified",
        lints: [],
        detail:
          report.runs_clean === "no"
            ? "The last clean run did not complete."
            : "Only an actual clean run in a separate process can tick this. Static analysis never does.",
      };

  const inputs: ReproRow =
    report.inputs_resolved === "yes" && dynamicPaths > 0
      ? {
          id: "inputs",
          mark: "warn",
          label: `${String(dynamicPaths)} dynamic path${dynamicPaths === 1 ? "" : "s"}`,
          lints: ["R004", "R005"],
          detail: "The path is built at run time, so it cannot be checked ahead of the run.",
        }
      : {
          id: "inputs",
          mark: markOf(report.inputs_resolved),
          label: "Inputs resolved",
          lints: ["R004", "R005"],
        };

  return [
    runsClean,
    {
      id: "seed",
      mark: markOf(report.seed_defined),
      label: "Random seed defined",
      lints: ["R002", "R003"],
    },
    inputs,
    {
      id: "hidden_deps",
      mark: markOf(report.no_hidden_deps),
      label: "No hidden interactive dependencies",
      lints: ["R006", "R009", "R011"],
    },
    absolutePaths === 0
      ? { id: "paths", mark: "ok", label: "No absolute file paths", lints: ["R001"] }
      : {
          id: "paths",
          mark: "warn",
          label: `${String(absolutePaths)} absolute file path${absolutePaths === 1 ? "" : "s"}`,
          lints: ["R001"],
        },
  ];
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

const [report, setReport] = createSignal<ReproReportView | undefined>(undefined);

/** The report for the active document, or `undefined` before the first audit. */
export const reproReport = report;

/** `EngineResponse::ReproReport` / the `reproChanged` push (06 §13.2). */
export function applyReproReport(next: ReproReportView): void {
  setReport(() => next);
}

/** Test seam, and document close. */
export function resetReproState(): void {
  setReport(undefined);
}
