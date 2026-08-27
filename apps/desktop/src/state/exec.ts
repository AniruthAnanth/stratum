/**
 * Execution state — spec §12, §13, §14, §15; 06 §5; ARCHITECTURE C20 and §7.3.
 *
 * This store is the window's single ingestion point for everything the engine
 * says about *what has run and whether it is still true*. `RunStarted`,
 * `BlockStarted`, `BlockFinished`, `StatusChanged` and `RunFinished` land here
 * and nowhere else, so "how many blocks are stale" has one answer in the top
 * bar, the gutter, the cards and the run queue. 06 §5.3's failure mode — the top
 * bar saying `3 stale` while the gutter shows four — is a second staleness rule,
 * not a rounding error.
 *
 * # Three properties this file is built around
 *
 * **1. Staleness is engine-authoritative (ADR-008, ARCHITECTURE C20).** The only
 * verdict this file forms on its own is `Stale{CodeChanged}`, and it can only
 * ever be *taken* by {@link worseOf}, never asserted. There is no code path here
 * that produces `Current`. That is not a style choice: INV-1 is one-directional
 * — over-marking is a UX cost, under-marking is a research-integrity hazard —
 * and a frontend that can synthesise a green tick has already broken it.
 *
 * **2. Nothing here is O(blocks).** A `StatusChanged` naming one block in a
 * 2 000-block document evaluates the display rule exactly once and touches one
 * key. The stale count is maintained incrementally rather than derived by a
 * sweep, because the top bar reads it on every keystroke (a character typed into
 * a regressed block must grey its result *this frame*, 06 §5.2) and a sweep
 * there is a long task on the interaction path. {@link execCounters} makes both
 * claims falsifiable — ADR-017: assert the counter, record the duration.
 *
 * **3. No mirror of a Rust type (CONTRACTS §12).** The `*View` interfaces below
 * declare the structural minimum this code READS, `readonly`, with the wire's
 * field names, so the generated `BlockStatus` substitutes into every signature
 * the day `src/ipc/types.ts` lands and nothing here has to be deleted first.
 * `taint` is a `number` because `stratum-proto` says so: `Taint`'s hand-written
 * `Serialize` writes the raw `u16` in both encodings, and the two fields that
 * carry it are `#[specta(type = u16)]`.
 */

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type {
  BlockId,
  BlockStatusState,
  CodeHash,
  DatasetStateId,
  DocumentId,
  ExecId,
  RunId,
} from "../ipc/hand";
import { worseOf } from "../ipc/hand";

// ---------------------------------------------------------------------------
// The structural minimum of CONTRACTS §3
// ---------------------------------------------------------------------------

/** `DepKey` — CONTRACTS §3. "Rendered verbatim in stale banners, so keep it short." */
export type DepKeyView =
  | { readonly ns: "var"; readonly frame: string; readonly name: string }
  | { readonly ns: "row_membership"; readonly frame: string }
  | { readonly ns: "row_order"; readonly frame: string }
  | { readonly ns: "var_layout"; readonly frame: string }
  | { readonly ns: "macro"; readonly name: string }
  | { readonly ns: "scalar"; readonly name: string }
  | { readonly ns: "matrix"; readonly name: string }
  | { readonly ns: "program"; readonly name: string }
  | { readonly ns: "estimates" }
  | { readonly ns: "r_class" }
  | { readonly ns: "s_class" }
  | { readonly ns: "rng" }
  | { readonly ns: "setting"; readonly name: string }
  | { readonly ns: "cwd" }
  | { readonly ns: "file"; readonly path: string };

export type StaleReasonView =
  | { readonly why: "code_changed" }
  | { readonly why: "epoch_reset" }
  | { readonly why: "input_changed"; readonly key: DepKeyView; readonly at: ExecId | null }
  | { readonly why: "file_changed"; readonly path: string }
  | { readonly why: "upstream_pending"; readonly block: BlockId; readonly via: DepKeyView }
  | { readonly why: "upstream_opaque"; readonly block: BlockId }
  | { readonly why: "rng_shifted" };

export type BrokenReasonView =
  | {
      readonly why: "unresolved_name";
      readonly name: string;
      readonly suggestion: string | null;
    }
  | {
      readonly why: "unknown_command";
      readonly name: string;
      readonly suggestion: string | null;
    }
  | { readonly why: "missing_file"; readonly path: string };

/**
 * The nine variants of CONTRACTS §3, as the frontend reads them.
 *
 * Field-for-field with `stratum_proto::status::BlockStatus` under
 * `#[serde(tag = "state", rename_all = "snake_case")]`, so a value off the wire
 * is assignable to this without a conversion step.
 */
export type BlockStatusView =
  | { readonly state: "never_run" }
  | { readonly state: "queued"; readonly position: number }
  | { readonly state: "running"; readonly exec: ExecId; readonly started_ms: number }
  | {
      readonly state: "current";
      readonly exec: ExecId;
      readonly dataset: DatasetStateId;
      readonly duration_us: number;
    }
  | {
      readonly state: "current_unverifiable";
      readonly exec: ExecId;
      readonly dataset: DatasetStateId;
      readonly duration_us: number;
      /** A raw `u16` bit set — see this file's header. */
      readonly taint: number;
    }
  | {
      readonly state: "stale";
      readonly reason: StaleReasonView;
      readonly since: ExecId | null;
    }
  | { readonly state: "failed"; readonly exec: ExecId; readonly rc: number }
  | { readonly state: "interrupted"; readonly exec: ExecId; readonly rolled_back: boolean }
  | { readonly state: "broken"; readonly reason: BrokenReasonView };

/** `Taint` — `crates/stratum-proto/src/status.rs`. The bit positions are the wire. */
export const TAINT = {
  MACRO_VARLIST: 1 << 0,
  UNKNOWN_COMMAND: 1 << 1,
  DYNAMIC_DISPATCH: 1 << 2,
  EXTERNAL: 1 << 3,
  CLOCK: 1 << 4,
  ENVIRONMENT: 1 << 5,
  UNBOUNDED_LOOP: 1 << 6,
  FILE_DYNAMIC: 1 << 7,
} as const;

export type TaintName = keyof typeof TAINT;

const TAINT_LABELS: Readonly<Record<TaintName, string>> = {
  MACRO_VARLIST: "a macro in a varlist",
  UNKNOWN_COMMAND: "an unknown command",
  DYNAMIC_DISPATCH: "a dynamically dispatched command",
  EXTERNAL: "shell, Python, Java or a plugin",
  CLOCK: "a clock read",
  ENVIRONMENT: "an environment read",
  UNBOUNDED_LOOP: "an unbounded loop",
  FILE_DYNAMIC: "a dynamically built file path",
};

/**
 * The set bits, in declaration order.
 *
 * `from_bits_retain` on the Rust side keeps a bit a newer engine set and this
 * build has no name for; the mirror of that here is that an unnamed bit is
 * simply not listed, never treated as "no taint". Dropping it would let a
 * `CurrentUnverifiable` read as `Current`, which is the one direction the
 * staleness model must never move on its own.
 */
export function taintNames(taint: number): TaintName[] {
  return (Object.keys(TAINT) as TaintName[]).filter((name) => (taint & TAINT[name]) !== 0);
}

export function taintLabel(name: TaintName): string {
  return TAINT_LABELS[name];
}

export function hasTaint(taint: number, bit: number): boolean {
  return (taint & bit) !== 0;
}

// ---------------------------------------------------------------------------
// The run plan (CONTRACTS §6)
// ---------------------------------------------------------------------------

export type PlanReasonView = "requested" | "dependency_of" | "stale" | "prefix";
export type SkipReasonView = "unaffected" | "already_current" | "not_executable";

export interface PlanItemView {
  readonly block: BlockId;
  readonly span: readonly [number, number];
  readonly code_hash: CodeHash;
  readonly reason: PlanReasonView;
}

export interface RunPlanView {
  readonly run: RunId;
  /** In execution order. */
  readonly items: readonly PlanItemView[];
  readonly epoch_reset: boolean;
  readonly clean_state: boolean;
  /** "12 blocks skipped — unaffected"; silence would feel like a bug. */
  readonly skipped: readonly (readonly [BlockId, SkipReasonView])[];
  /** "3 upstream blocks are stale — [Run them first]". Non-blocking. */
  readonly stale_upstream: readonly BlockId[];
}

// ---------------------------------------------------------------------------
// Counters (ADR-017)
// ---------------------------------------------------------------------------

/**
 * Counters, not clocks.
 *
 * `statusEvaluations` is the one that matters: it is incremented once per
 * evaluation of the display rule, so a `StatusChanged` naming k blocks must
 * raise it by exactly k regardless of how many blocks the document has. A
 * regression to "recompute the document" is then a failing assertion rather
 * than a 2 000× slower keystroke somebody notices in a demo.
 *
 * `ipcCalls` is asserted to stay at 0: nothing in the execution-state UI may
 * round-trip to decide what to draw. Staleness arrives, it is never fetched.
 */
export const execCounters = {
  statusEvaluations: 0,
  statusWrites: 0,
  staleScans: 0,
  ipcCalls: 0,
  eventsApplied: 0,
};

export function resetExecCounters(): void {
  execCounters.statusEvaluations = 0;
  execCounters.statusWrites = 0;
  execCounters.staleScans = 0;
  execCounters.ipcCalls = 0;
  execCounters.eventsApplied = 0;
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/** `${doc}:${block}` — the engine's own identity for a block, not the ordinal. */
export type BlockKey = string;

export function blockKey(doc: DocumentId, block: BlockId): BlockKey {
  return `${String(doc)}:${String(block)}`;
}

const NEVER_RUN: BlockStatusView = { state: "never_run" };

/** Whether a run is interactive (live session) or clean (fresh env) — spec §15. */
export type RunKind = "interactive" | "clean";

export interface RunState {
  readonly run: RunId | undefined;
  readonly kind: RunKind;
  /** True from `RunStarted` to `RunFinished`. Drives the CLEAN chip's duration. */
  readonly active: boolean;
  /** `plan_len` — how many blocks this run intends to execute. */
  readonly planned: number;
  readonly finished: number;
  readonly failed: number;
  readonly seed: number | undefined;
  readonly source: string | undefined;
  readonly startedAtMs: number | undefined;
  /** RECORDED, never asserted (ADR-017). */
  readonly durationUs: number | undefined;
}

const IDLE: RunState = {
  run: undefined,
  kind: "interactive",
  active: false,
  planned: 0,
  finished: 0,
  failed: 0,
  seed: undefined,
  source: undefined,
  startedAtMs: undefined,
  durationUs: undefined,
};

/** `E41 · D17 · 12 481 obs` — 06 §14.2's readout, kept current by the events. */
export interface ExecReadout {
  readonly exec: ExecId | undefined;
  readonly dataset: DatasetStateId | undefined;
  readonly obs: number | undefined;
  readonly vars: number | undefined;
}

interface ExecStoreState {
  /** The kernel's verdict per block. Never contains a locally-formed opinion. */
  statuses: Record<BlockKey, BlockStatusView>;
  /** The code hash the block's last execution was submitted with. */
  executed: Record<BlockKey, CodeHash>;
  /** Blocks whose latest execution belonged to a clean run (spec §15). */
  cleanRun: Record<BlockKey, true>;
  plan: RunPlanView | undefined;
  run: RunState;
  readout: ExecReadout;
}

const [state, setState] = createStore<ExecStoreState>({
  statuses: {},
  executed: {},
  cleanRun: {},
  plan: undefined,
  run: IDLE,
  readout: { exec: undefined, dataset: undefined, obs: undefined, vars: undefined },
});

export const execState = state;

/**
 * The current local hash per block, as the segmenter reports it *now*.
 *
 * Not in the store: it changes on every keystroke and nothing renders it
 * directly. What renders is the displayed status, and {@link staleCount} is the
 * signal that makes that reactive without putting a hash in a reactive graph.
 */
const localHash = new Map<BlockKey, CodeHash>();

/** Keys whose DISPLAYED status is `stale`. Maintained incrementally; never scanned. */
const staleKeys = new Set<BlockKey>();
const [staleN, setStaleN] = createSignal(0);

/** `⟲ 3 stale` — O(1), read on every frame the top bar paints. */
export const staleCount = staleN;

/**
 * Every stale block, materialised.
 *
 * The ONE deliberate O(stale) read in this module, and it is counted so that
 * nothing starts calling it from a render path. `run.allStale` does not use it:
 * W13 resolves that verb against the gutter's own `displayStatus`, and a second
 * answer to "which blocks are stale" is precisely the drift 06 §5.3 warns about.
 * This exists for diagnostics and for tests.
 */
export function staleBlocks(): BlockKey[] {
  execCounters.staleScans += 1;
  return [...staleKeys].sort();
}

// ---------------------------------------------------------------------------
// The display rule (ARCHITECTURE C20, CONTRACTS §3)
// ---------------------------------------------------------------------------

/**
 * The ONLY verdict this frontend forms on its own.
 *
 * `since` carries the execution the block last completed, so the banner can say
 * "code changed since E41" rather than "code changed". It is read off the kernel
 * status rather than tracked separately — the kernel status *is* the record of
 * what last ran.
 */
function localVerdict(kernel: BlockStatusView): BlockStatusView {
  return { state: "stale", reason: { why: "code_changed" }, since: execOf(kernel) ?? null };
}

/** The execution id a status carries, where it carries one. */
export function execOf(status: BlockStatusView): ExecId | undefined {
  switch (status.state) {
    case "running":
    case "current":
    case "current_unverifiable":
    case "failed":
    case "interrupted":
      return status.exec;
    case "stale":
      return status.since ?? undefined;
    default:
      return undefined;
  }
}

/**
 * `displayed = worseOf(local, kernel)`.
 *
 * `worseOf` is imported from `ipc/hand.ts`, never reimplemented: a second copy
 * of the total order is how a UI starts disagreeing with itself. It returns one
 * of its two arguments rather than a reconstruction, so a `Failed` keeps its
 * `rc` and an `InputChanged` keeps its `DepKey` — which is the whole reason the
 * banner can name the variable.
 */
export function displayedStatus(doc: DocumentId, block: BlockId): BlockStatusView {
  return displayedFor(blockKey(doc, block));
}

function displayedFor(key: BlockKey): BlockStatusView {
  execCounters.statusEvaluations += 1;
  const kernel = state.statuses[key] ?? NEVER_RUN;
  const local = localHash.get(key);
  const executed = state.executed[key];
  if (local === undefined || executed === undefined || executed === local) return kernel;
  return worseOf<BlockStatusView>(localVerdict(kernel), kernel);
}

/** The glyph state the gutter, the rail and the card draw. */
export function displayedState(doc: DocumentId, block: BlockId): BlockStatusState {
  return displayedStatus(doc, block).state;
}

/**
 * Recompute one key's membership of the stale set.
 *
 * O(1) and called from exactly the three places that can change a verdict: a
 * kernel status arriving, a local hash moving, and a block leaving the map.
 */
function reindex(key: BlockKey): void {
  const displayed = displayedFor(key).state;
  const was = staleKeys.has(key);
  const is = displayed === "stale";
  if (was === is) return;
  if (is) staleKeys.add(key);
  else staleKeys.delete(key);
  setStaleN(staleKeys.size);
}

// ---------------------------------------------------------------------------
// Ingestion
// ---------------------------------------------------------------------------

/** The kernel's verdict for one block. `StatusChanged` is the only caller. */
export function setKernelStatus(doc: DocumentId, block: BlockId, status: BlockStatusView): void {
  const key = blockKey(doc, block);
  execCounters.statusWrites += 1;
  setState("statuses", key, status);
  reindex(key);
}

/**
 * The hash a block's run was submitted with — 06 §5.2's local check, half one.
 *
 * Recorded at `BlockStarted`, which is the moment the engine confirms *which
 * text* it is running. Recording it at submit time instead would mark a block
 * current against text the engine never saw.
 */
export function setExecutedHash(doc: DocumentId, block: BlockId, hash: CodeHash): void {
  const key = blockKey(doc, block);
  setState("executed", key, hash);
  reindex(key);
}

/** The segmenter's hash for the block as it reads right now — half two. */
export function setLocalHash(doc: DocumentId, block: BlockId, hash: CodeHash | undefined): void {
  const key = blockKey(doc, block);
  if (hash === undefined) localHash.delete(key);
  else localHash.set(key, hash);
  reindex(key);
}

/** Did this block's latest execution happen inside a clean run? (spec §15) */
export function ranClean(doc: DocumentId, block: BlockId): boolean {
  return state.cleanRun[blockKey(doc, block)] === true;
}

/** A block the segmenter retired. Its ledger entries survive; its status does not. */
export function forgetBlock(doc: DocumentId, block: BlockId): void {
  const key = blockKey(doc, block);
  setState(
    produce((s) => {
      delete s.statuses[key];
      delete s.executed[key];
      delete s.cleanRun[key];
    }),
  );
  localHash.delete(key);
  if (staleKeys.delete(key)) setStaleN(staleKeys.size);
}

export function runState(): RunState {
  return state.run;
}

export function runPlan(): RunPlanView | undefined {
  return state.plan;
}

export function readout(): ExecReadout {
  return state.readout;
}

/** True while a clean run is in flight — the CLEAN chip's whole condition. */
export function cleanRunActive(): boolean {
  return state.run.active && state.run.kind === "clean";
}

/**
 * `EngineResponse::Submitted { plan }`.
 *
 * The plan arrives before the first `RunStarted`, which is what lets the queue
 * paint the whole run — including the blocks it decided NOT to run — in the same
 * frame as the keystroke.
 */
export function applyRunPlan(plan: RunPlanView): void {
  setState("plan", plan);
}

// ---------------------------------------------------------------------------
// Engine events
// ---------------------------------------------------------------------------

/** The structural minimum of the events this store consumes (CONTRACTS §11). */
export type ExecEventView =
  | {
      readonly event: "run_started";
      readonly run: RunId;
      readonly clean_state: boolean;
      readonly plan_len: number;
      readonly started_at_ms: number;
      readonly seed?: number | null;
      readonly source?: string | null;
    }
  | {
      readonly event: "block_started";
      readonly run: RunId;
      readonly exec: ExecId;
      readonly block: BlockId;
      readonly doc?: DocumentId | null;
      readonly code_hash: CodeHash;
      readonly dataset_state_in: DatasetStateId;
    }
  | {
      readonly event: "block_finished";
      readonly run: RunId;
      readonly exec: ExecId;
      readonly block: BlockId;
      readonly rc: number;
      readonly duration_us: number;
      readonly dataset_state_out: DatasetStateId;
    }
  | {
      readonly event: "status_changed";
      readonly doc: DocumentId;
      readonly changed: readonly (readonly [BlockId, BlockStatusView])[];
    }
  | {
      readonly event: "state_changed";
      readonly dataset_state: DatasetStateId;
      readonly n_obs: number;
      readonly n_vars: number;
    }
  | {
      readonly event: "run_finished";
      readonly run: RunId;
      readonly blocks_run: number;
      readonly blocks_failed: number;
      readonly duration_us: number;
    };

/**
 * The document a `BlockStarted` / `BlockFinished` belongs to.
 *
 * Those two events carry `doc: Option<DocumentId>` — a command-bar execution has
 * no document (`BlockId::EPHEMERAL`) — so the host tells the store which
 * document its editor is showing and an event without one is attributed there.
 */
let currentDoc: DocumentId | undefined;

export function setExecDocument(doc: DocumentId | undefined): void {
  currentDoc = doc;
}

/**
 * Apply one engine event. Returns false for an event this store does not model,
 * so a caller can keep an honest "events applied" count rather than claiming to
 * have understood the whole stream.
 */
export function applyExecEvent(event: ExecEventView): boolean {
  switch (event.event) {
    case "run_started": {
      setState("run", {
        run: event.run,
        kind: event.clean_state ? "clean" : "interactive",
        active: true,
        planned: event.plan_len,
        finished: 0,
        failed: 0,
        seed: event.seed ?? undefined,
        source: event.source ?? undefined,
        startedAtMs: event.started_at_ms,
        durationUs: undefined,
      });
      execCounters.eventsApplied += 1;
      return true;
    }
    case "block_started": {
      const doc = event.doc ?? currentDoc;
      if (doc !== undefined) {
        const key = blockKey(doc, event.block);
        setExecutedHash(doc, event.block, event.code_hash);
        if (state.run.kind === "clean") setState("cleanRun", key, true);
        else if (state.cleanRun[key] === true) {
          setState(
            produce((s) => {
              delete s.cleanRun[key];
            }),
          );
        }
      }
      setState("readout", { ...state.readout, exec: event.exec, dataset: event.dataset_state_in });
      execCounters.eventsApplied += 1;
      return true;
    }
    case "block_finished": {
      setState("run", {
        ...state.run,
        finished: state.run.finished + 1,
        failed: state.run.failed + (event.rc === 0 ? 0 : 1),
      });
      setState("readout", {
        ...state.readout,
        exec: event.exec,
        dataset: event.dataset_state_out,
      });
      execCounters.eventsApplied += 1;
      return true;
    }
    case "status_changed": {
      // Exactly `changed.length` evaluations, whatever the document's size.
      for (const [block, status] of event.changed) {
        setKernelStatus(event.doc, block, status);
      }
      execCounters.eventsApplied += 1;
      return true;
    }
    case "state_changed": {
      setState("readout", {
        ...state.readout,
        dataset: event.dataset_state,
        obs: event.n_obs,
        vars: event.n_vars,
      });
      execCounters.eventsApplied += 1;
      return true;
    }
    case "run_finished": {
      setState("run", {
        ...state.run,
        active: false,
        finished: event.blocks_run,
        failed: event.blocks_failed,
        durationUs: event.duration_us,
      });
      execCounters.eventsApplied += 1;
      return true;
    }
    default:
      return false;
  }
}

/** Test seam, and window teardown. */
export function resetExecState(): void {
  setState({
    statuses: {},
    executed: {},
    cleanRun: {},
    plan: undefined,
    run: IDLE,
    readout: { exec: undefined, dataset: undefined, obs: undefined, vars: undefined },
  });
  localHash.clear();
  staleKeys.clear();
  setStaleN(0);
  currentDoc = undefined;
}
