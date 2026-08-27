/**
 * Result envelopes — 06 §4.7, §6.1.
 *
 * Three facts drive the shape of this store:
 *
 *  1. Everything goes to the scrollback, always (§6.1). A card is a *view* of a
 *     result, never the only copy of it, so nothing here may be the last
 *     reference to a payload.
 *  2. A block can have several results and several versions (§4.7). The card
 *     shows the latest; the earlier ones stay reachable.
 *  3. The envelope itself is a generated type (`ResultEnvelope`, CONTRACTS §5).
 *     This store is therefore generic over it and declares no field of it — a
 *     hand-written mirror would drift the moment §5 moved.
 */

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { type BlockId, type CodeHash, type ResultId, clientKey } from "../ipc/hand";

/** The structural minimum this store reads. The generated envelope satisfies it. */
export interface HasResultId {
  readonly id: ResultId;
}

/** How many envelopes a window keeps resident. The scrollback in Rust is the archive. */
export const RESIDENT_CAP = 2_000;

interface ResultState<E extends HasResultId> {
  byId: Record<string, E>;
  /** Insertion order, oldest first, for the resident cap. */
  order: ResultId[];
  /** `clientKey(hash, ordinal)` -> every result that block has produced, oldest first. */
  byBlock: Record<string, ResultId[]>;
}

const [state, setState] = createStore<ResultState<HasResultId>>({
  byId: {},
  order: [],
  byBlock: {},
});

const [latest, setLatest] = createSignal<ResultId | undefined>(undefined);

export const resultState = state;
/** The most recent result in this window, for `Raw ▸` and the palette's "last result". */
export const latestResult = latest;

export function recordResult(
  envelope: HasResultId,
  block?: { hash: CodeHash; ordinal: number },
): void {
  setState(
    produce((s) => {
      const key = String(envelope.id);
      if (s.byId[key] === undefined) s.order.push(envelope.id);
      s.byId[key] = envelope;

      if (block !== undefined) {
        const bkey = clientKey(block.hash, block.ordinal);
        const list = s.byBlock[bkey];
        if (list === undefined) s.byBlock[bkey] = [envelope.id];
        else if (!list.includes(envelope.id)) list.push(envelope.id);
      }

      // Evict from the head. The envelope is still in Rust's ResultStore, so an
      // evicted card re-fetches with `result_get` rather than showing a hole.
      while (s.order.length > RESIDENT_CAP) {
        const dropped = s.order.shift();
        if (dropped !== undefined) delete s.byId[String(dropped)];
      }
    }),
  );
  setLatest(() => envelope.id);
}

export function result(id: ResultId): HasResultId | undefined {
  return state.byId[String(id)];
}

/** Every version this block has produced, oldest first (§4.7). */
export function resultsForBlock(hash: CodeHash, ordinal: number): ResultId[] {
  return state.byBlock[clientKey(hash, ordinal)] ?? [];
}

/** The one the card renders. */
export function currentResultForBlock(hash: CodeHash, ordinal: number): ResultId | undefined {
  return resultsForBlock(hash, ordinal).at(-1);
}

/**
 * Re-keys a block's results after a re-segmentation moved it (ARCHITECTURE C4).
 * The `ClientBlockKey` is `(hash, ordinal)`; an edit above a block changes the
 * ordinal without changing the code, and the card must follow the code.
 */
export function rekeyBlock(
  from: { hash: CodeHash; ordinal: number },
  to: { hash: CodeHash; ordinal: number },
): void {
  const fromKey = clientKey(from.hash, from.ordinal);
  const toKey = clientKey(to.hash, to.ordinal);
  if (fromKey === toKey) return;
  setState(
    produce((s) => {
      const list = s.byBlock[fromKey];
      if (list === undefined) return;
      delete s.byBlock[fromKey];
      s.byBlock[toKey] = list;
    }),
  );
}

/** `Mod+Shift+K` clears results in this window; Rust's store is untouched. */
export function clearResults(): void {
  setState({ byId: {}, order: [], byBlock: {} });
  setLatest(undefined);
}

/** `Mod+Shift+Backspace` clears one block's output. */
export function clearBlockResults(hash: CodeHash, ordinal: number): void {
  const key = clientKey(hash, ordinal);
  setState(
    produce((s) => {
      delete s.byBlock[key];
    }),
  );
}

/** Test seam. */
export function resetResultState(): void {
  clearResults();
}

export type { BlockId };
