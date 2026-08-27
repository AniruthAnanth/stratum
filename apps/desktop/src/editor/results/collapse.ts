/**
 * Card collapse state — 06 §4.6, spec §3 ("collapse output") and §5.
 *
 * # The rule this file exists to enforce
 *
 * Collapsing output **edits the sidecar, never the document** (06 §4.8: "Renaming
 * and moving edit source text; clearing/collapsing output edits only the
 * sidecar"). There is no `view.dispatch` in this file and there must never be
 * one. `editor.doc.test.ts` drives 200 random collapses and asserts
 * `doc.toString()` is byte-identical afterwards.
 *
 * # Why the key is a code hash and not a position
 *
 * `DurableSidecar.collapsed` is `CodeHash[]` — collapse INTENT, keyed by the
 * code. That is the only key that survives the file being closed, edited in
 * another editor, and reopened: a line number does not, and a `BlockId` is
 * allocated per session by the engine. The consequence is deliberate and worth
 * knowing: two identical blocks in one file share a collapse state, and
 * re-typing a block you had collapsed brings it back collapsed. Both are what a
 * user means by "I collapsed that output".
 */

import type { CodeHash } from "../../ipc/hand";

/** Per-card presentation state. Everything here is sidecar-durable or derived. */
export interface CardUiState {
  /** Body hidden, header and action row still shown. */
  readonly collapsed: boolean;
  /** Which of the card's stacked payload sections is showing raw text. */
  readonly raw: boolean;
  /** Last measured pixel height, so reopening the file does not jump (§4.6). */
  readonly measuredHeight: number | undefined;
}

export const DEFAULT_CARD_UI: CardUiState = {
  collapsed: false,
  raw: false,
  measuredHeight: undefined,
};

const collapsed = new Set<string>();
const measured = new Map<string, number>();
const listeners = new Set<() => void>();

/** Height memo key. 06 §4.6: `(result_id, font_size, pane_width_bucket)`. */
export function heightKey(result: number, fontSizePx: number, paneWidthPx: number): string {
  // 80 px buckets: a card re-measured for every pixel of pane width would never
  // hit the memo, and the estimate only has to be close enough that the
  // scrollbar does not visibly correct itself.
  return `${result}:${fontSizePx}:${Math.round(paneWidthPx / 80)}`;
}

export function isCollapsed(hash: CodeHash): boolean {
  return collapsed.has(hash);
}

export function setCollapsed(hash: CodeHash, next: boolean): void {
  const changed = next ? !collapsed.has(hash) : collapsed.delete(hash);
  if (next) collapsed.add(hash);
  if (changed || next) notify();
}

export function toggleCollapsed(hash: CodeHash): boolean {
  const next = !collapsed.has(hash);
  setCollapsed(hash, next);
  return next;
}

export function rememberHeight(key: string, px: number): void {
  measured.set(key, px);
}

export function rememberedHeight(key: string): number | undefined {
  return measured.get(key);
}

/** The durable half, in `DurableSidecar.collapsed` order (sorted, LF, no times). */
export function collapsedHashes(): CodeHash[] {
  return [...collapsed].sort() as CodeHash[];
}

/** Rehydrate from the sidecar on file open. */
export function hydrateCollapse(hashes: readonly CodeHash[]): void {
  collapsed.clear();
  for (const hash of hashes) collapsed.add(hash);
  notify();
}

export function onCollapseChanged(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Test seam. */
export function resetCollapse(): void {
  collapsed.clear();
  measured.clear();
  notify();
}

function notify(): void {
  for (const listener of listeners) listener();
}
