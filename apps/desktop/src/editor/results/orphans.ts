/**
 * Detached results — 06 §4.6, anchor policy 3.
 *
 * **Output is never destroyed by an edit.** When the text a card was anchored to
 * is deleted, CodeMirror drops the mapped range and the widget goes with it. The
 * result does not: it was already in the Results scrollback (§6.1, and the
 * scrollback is authoritative in Rust), and it lands here so the block menu and
 * the command palette can offer **Show detached results**.
 *
 * This store holds ids and provenance, never payloads. A payload is fetched from
 * the result store by id, so an orphan costs a few dozen bytes and a document
 * whose entire text was selected and deleted does not pin 500 envelopes in the
 * webview.
 */

import type { CodeHash, ResultId } from "../../ipc/hand";

/** One result whose anchor was deleted. */
export interface OrphanResult {
  /** The envelope, still resident or re-fetchable by id. */
  readonly result: ResultId;
  /** The code hash the block had when it ran — the "what produced this" label. */
  readonly executedHash: CodeHash;
  /** Occurrence index of that hash at run time. */
  readonly executedOrdinal: number;
  /** First line of the code as it was when it ran, for the menu label. */
  readonly label: string;
  /** Wall-clock at detachment. Display only; never in the durable sidecar. */
  readonly detachedAt: number;
}

/** Newest first, so "Show detached results" leads with what just vanished. */
const orphans: OrphanResult[] = [];
const listeners = new Set<() => void>();

/**
 * How many detached results a window keeps.
 *
 * Bounded because a select-all-and-delete on a 500-card document produces 500 of
 * these in one transaction and none of them is worth pinning memory for
 * indefinitely. The results themselves survive in Rust regardless; this cap only
 * limits how far back the *menu* reaches.
 */
export const ORPHAN_CAP = 200;

export function recordOrphan(orphan: OrphanResult): void {
  orphans.unshift(orphan);
  if (orphans.length > ORPHAN_CAP) orphans.length = ORPHAN_CAP;
  notify();
}

export function recordOrphans(batch: readonly OrphanResult[]): void {
  if (batch.length === 0) return;
  orphans.unshift(...batch);
  if (orphans.length > ORPHAN_CAP) orphans.length = ORPHAN_CAP;
  notify();
}

export function orphanResults(): readonly OrphanResult[] {
  return orphans;
}

/** Drops one orphan — the user re-attached it or dismissed the notice. */
export function forgetOrphan(result: ResultId): void {
  const at = orphans.findIndex((o) => o.result === result);
  if (at < 0) return;
  orphans.splice(at, 1);
  notify();
}

export function onOrphansChanged(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Test seam, and window teardown. */
export function resetOrphans(): void {
  orphans.length = 0;
  notify();
}

function notify(): void {
  for (const listener of listeners) listener();
}
