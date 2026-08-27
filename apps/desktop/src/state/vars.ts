/**
 * The variable list and its lazy statistics — 06 §11.1.
 *
 * Two tiers, and the split is the whole design. Cheap metadata (name, type,
 * format, label, value label, obs, missing count) comes from the frame header
 * and is always present. Mean/median/SD/min/max and the 24-bin sparkline are
 * computed on demand, cached per `(frame, var, dataset_state)`, and cancelled
 * when the pointer moves on — scrubbing a 200-variable list must not queue 200
 * summaries against a 10 M-row frame.
 */

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { DatasetStateId } from "../ipc/hand";
import { bridge } from "../platform/bridge";

/** The cheap tier. Shape mirrors nothing: the generated `VariableInfo` replaces it. */
export interface VariableRow {
  name: string;
  storage: string;
  format: string;
  label?: string;
  valueLabel?: string;
  missing?: number;
}

/** The expensive tier, from `variable_stats`. */
export interface QuickStats {
  obs: number;
  missing: number;
  mean?: number;
  median?: number;
  sd?: number;
  min?: number;
  max?: number;
  /** 24 bins, 06 §11.1. */
  sparkline?: number[];
}

/** 06 §11.1: past this, the drawer offers a button instead of starting work. */
export const AUTO_SUMMARY_LIMIT = 50_000_000;
/** 06 §11.1: a dwell, not a hover — scrubbing must cost nothing. */
export const DWELL_MS = 220;

interface VarState {
  frame: string;
  rows: VariableRow[];
  selected: string | undefined;
}

const [vars, setVars] = createStore<VarState>({ frame: "default", rows: [], selected: undefined });
const [loading, setLoading] = createSignal(false);

export const variables = vars;
export const variablesLoading = loading;

export async function loadVariables(frame: string): Promise<void> {
  setLoading(true);
  try {
    const rows = await bridge().invoke<VariableRow[]>("variables_list", { frame });
    setVars(
      produce((s) => {
        s.frame = frame;
        s.rows = rows;
        if (s.selected !== undefined && !rows.some((r) => r.name === s.selected)) {
          s.selected = undefined;
        }
      }),
    );
  } finally {
    setLoading(false);
  }
}

export function selectVariable(name: string | undefined): void {
  setVars("selected", name);
}

// ---------------------------------------------------------------------------
// The lazy tier
// ---------------------------------------------------------------------------

const cache = new Map<string, QuickStats>();
const inflight = new Map<string, AbortController>();

const statsKey = (frame: string, name: string, state: DatasetStateId): string =>
  `${frame} ${name} ${state}`;

export function cachedStats(
  frame: string,
  name: string,
  state: DatasetStateId,
): QuickStats | undefined {
  return cache.get(statsKey(frame, name, state));
}

/**
 * Requests the expensive tier. Supersedes any request for the same key and
 * aborts it — 06 §11.1's "the request carries an `AbortSignal` wired to a
 * `Channel` cancel so scrubbing the list does not queue work".
 */
export async function requestStats(
  frame: string,
  name: string,
  state: DatasetStateId,
  obs: number,
): Promise<QuickStats | undefined> {
  const k = statsKey(frame, name, state);
  const hit = cache.get(k);
  if (hit !== undefined) return hit;
  if (obs > AUTO_SUMMARY_LIMIT) return undefined;

  inflight.get(k)?.abort();
  const controller = new AbortController();
  inflight.set(k, controller);
  try {
    const stats = await bridge().invoke<QuickStats>("variable_stats", { frame, var: name });
    if (controller.signal.aborted) return undefined;
    cache.set(k, stats);
    return stats;
  } catch {
    return undefined;
  } finally {
    if (inflight.get(k) === controller) inflight.delete(k);
  }
}

/** Every pending summary is abandoned when the dataset advances. */
export function invalidateStats(): void {
  for (const controller of inflight.values()) controller.abort();
  inflight.clear();
  cache.clear();
}

/** Test seam. */
export function resetVarState(): void {
  invalidateStats();
  setVars({ frame: "default", rows: [], selected: undefined });
  setLoading(false);
}
