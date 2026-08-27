/**
 * The segmenter seam — 06 §3, ARCHITECTURE §6.1.
 *
 * Block segmentation is wasm on the main thread inside the CodeMirror
 * transaction cycle, and the wasm module belongs to W11a (`src/wasm/**`), not to
 * the shell. So the shell does not import it; it publishes a slot that W11a
 * fills and W13 reads.
 *
 * Doing it this way rather than with a dynamic `import("../wasm/…")` is
 * deliberate: a dynamic import of a module that does not exist yet is a build
 * error in Vite, and a shell that cannot build until an unrelated unit lands is
 * exactly the sequencing failure IMPLEMENTATION_PLAN §7 is arranged to avoid.
 */

/** The shape 06 §3.4 specifies, reduced to what the shell has to know. */
export interface Segmenter {
  /** Full segmentation. The 8 ms budget of §15.1 applies to this call. */
  segment(text: string): unknown;
  /** Incremental re-segmentation over a change set. */
  resegment(text: string, from: number, to: number): unknown;
}

let segmenter: Segmenter | undefined;
const waiters = new Set<(s: Segmenter) => void>();

/** W11a calls this once its wasm module has initialised. */
export function provideSegmenter(next: Segmenter): void {
  segmenter = next;
  for (const waiter of waiters) waiter(next);
  waiters.clear();
}

export function currentSegmenter(): Segmenter | undefined {
  return segmenter;
}

/**
 * Resolves when a segmenter exists. The editor mounts before wasm finishes
 * instantiating, so the first document is unsegmented for a few milliseconds —
 * which is correct: showing the text immediately and the block outline a frame
 * later beats an empty editor.
 */
export function whenSegmenter(): Promise<Segmenter> {
  if (segmenter !== undefined) return Promise.resolve(segmenter);
  return new Promise((resolve) => waiters.add(resolve));
}

/** Test seam. */
export function resetSegmenter(): void {
  segmenter = undefined;
  waiters.clear();
}
