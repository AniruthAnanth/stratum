/**
 * The editor's view of the ONE segmenter — 06 §3, §4.4.
 *
 * There is no segmentation logic in this file and there must never be one. The
 * block outline comes from `stratum-wasm` through W11a's `StratumSegmenter`
 * (layer 2 of `src/wasm/types.ts`, UTF-16 offsets), and this module is the thin
 * layer between that and CodeMirror's transaction cycle. A second splitter in
 * TypeScript is the failure mode 06 §3.2 exists to prevent: it would disagree
 * with the kernel exactly on `///`, `#delimit ;` and Mata, which is where it
 * matters.
 *
 * # Why this wrapper exists at all
 *
 * Two things, both of them about cost, and both MEASURED against the real wasm
 * module rather than reasoned about.
 *
 * 1. **`regions()` is O(document); `region(i)` is not.** The layer-2 wrapper
 *    offers three ways in: decode every region, decode one by index, or find the
 *    one at an offset. On the module that ships, decoding every region costs
 *    0.35 / 1.28 / 2.26 ms at 550 / 3 300 / 6 600 regions, while thirty-two
 *    individual `region(i)` lookups cost 0.25 / 0.51 / 0.54 ms — essentially
 *    flat, because each one is a binary search and a single row decode over
 *    memory the module already holds.
 *
 *    A viewport is about thirty blocks whatever the file is. So this wrapper
 *    answers viewport questions with per-index lookups and a generation-scoped
 *    memo, and reserves the full decode for the callers that genuinely need
 *    every block — `run.file`, the Sections outline — where O(document) is
 *    inherent. Doing it the other way round costs 2.3 ms of a 6 ms keystroke
 *    budget on a 245 KB file and grows linearly; this does not grow at all.
 *
 *    (The development stub inverts the ranking, because its `regions_view()` is
 *    `Int32Array.from(plainArray)` and pays the iterator protocol per call. That
 *    is a property of a test fixture, not of the product, and it is exactly why
 *    the numbers above were taken from the real module.)
 *
 * 2. **The counters.** ADR-017 forbids proving performance with a duration, so
 *    every hot path in this unit increments a counter that expresses the
 *    property the budget was standing in for. They live here rather than in a
 *    module of their own because `docs/IMPLEMENTATION_PLAN.md` §8 enumerates
 *    this unit's files and this is the lowest one in the import graph —
 *    everything else in `editor/` can import it without a cycle.
 */

import { Facet } from "@codemirror/state";
import type { ChangeSet, EditorState } from "@codemirror/state";
import { type CodeHash, clientKey, isCodeHash } from "../../ipc/hand";
import type {
  DocChange,
  NarrativeView,
  RegionView,
  SectionView,
  StratumSegmenter,
  TokenView,
} from "../../wasm/types";

/**
 * One executable region, exactly as the segmenter reports it.
 *
 * Deliberately an alias and not a richer struct. A `Block` that carried a
 * pre-computed key or a branded hash would cost one object and one string per
 * region per keystroke — 3 000 of each on a large do-file — to save a call at
 * the handful of sites that actually need them. {@link blockKey} and
 * {@link blockHash} are those sites.
 */
export type Block = RegionView;

/** `(hash, ordinal)` — ARCHITECTURE C4's pre-`BlockMap` widget key. */
export function blockKey(block: Block): string {
  return `${block.hashKey}:${block.hashOrdinal}`;
}

/**
 * The block's `CodeHash`, validated.
 *
 * The wrapper always produces 32 lowercase hex digits, so this never throws in
 * practice — but a run request carries the hash to the kernel, which compares it
 * and answers `BlockMismatch` (06 §5.5), and a malformed hash there is a silent
 * refusal to run rather than a loud bug. Checked once, at the boundary, not per
 * region per keystroke.
 */
export function blockHash(block: Block): CodeHash {
  if (!isCodeHash(block.hashKey)) {
    throw new TypeError(`segmenter produced a malformed code hash: ${block.hashKey}`);
  }
  return block.hashKey;
}

/** `clientKey` over a block, for the stores in `state/results.ts`. */
export function blockClientKey(block: Block): string {
  return clientKey(blockHash(block), block.hashOrdinal);
}

// ---------------------------------------------------------------------------
// Counters (ADR-017)
// ---------------------------------------------------------------------------

/**
 * What the editor did, counted rather than timed.
 *
 * ADR-017 is binding and it is not bureaucracy: the same tree here benchmarked
 * 33 % apart an hour apart under nothing but machine load. Every one of these
 * is a property a stopwatch was previously standing in for.
 */
export interface EditorCounters {
  /** `resegment()` calls. Exactly one per document-changing transaction. */
  wasmResegments: number;
  /** `applyChanges()` calls — the splice batch. One per changed transaction. */
  wasmSplices: number;
  /** `setDoc()` calls. Open and resync only; never on the typing path. */
  wasmSetDocs: number;
  /**
   * Full region-vector decodes — `regions()`, which is O(document).
   *
   * Must be 0 on any typing path. It is not zero overall: `run.file` and the
   * Sections outline genuinely need every block, and both are click paths.
   */
  regionDecodePasses: number;
  /**
   * Regions actually decoded, cumulative.
   *
   * The load-bearing counter of this unit. Per keystroke it must be bounded by
   * the VIEWPORT and independent of document size; `editor.perf.test.ts` asserts
   * the same edit in a 50-block file and a 3 000-block file decodes the same
   * number of regions.
   */
  regionsDecoded: number;
  /** Individual `region(i)` / `regionAt(pos)` calls into the wasm boundary. */
  regionLookups: number;
  /** `tokens(from,to)` calls. Viewport-scoped; one per highlight rebuild. */
  tokenQueries: number;
  /** Tokens decoded, cumulative. Must track the VIEWPORT, not the document. */
  tokensDecoded: number;
  /** Highlight mark decorations built. Viewport-scoped. */
  highlightRangesBuilt: number;
  /** Gutter marker range-sets built. Skipped when nothing a marker reads moved. */
  gutterRebuilds: number;
  /** `GutterMarker` instances constructed. Cached per (status, hover-shape). */
  gutterMarkersConstructed: number;
  /** `data-hover` attribute flips. ~1 per block crossed, never per pixel. */
  hoverAttributeWrites: number;
  /** Result-decoration set rebuilds. A keystroke maps; it does not rebuild. */
  resultDecoRebuilds: number;
  /** `ResultWidget` instances constructed. Zero for a keystroke away from a card. */
  cardWidgetsConstructed: number;
  /** `toDOM()` calls — an actual card mount. */
  cardDomMounts: number;
  /** `updateDOM()` calls that reused the existing DOM. */
  cardDomPatches: number;
  /** `data-display` flips on a card. Viewport-bounded, ~1 per state change. */
  cardStateWrites: number;
  /** Streamed log appends. DOM-only: no transaction, no rebuild, no reflow. */
  cardStreamAppends: number;
  /** Scroll-anchor compensation frames. */
  scrollCompensationFrames: number;
  /** Pixels of above-viewport height change compensated for. */
  scrollCompensationPx: number;
  /** IPC calls made from the editor. MUST be 0 on any typing path. */
  ipcCalls: number;
  /** Document-writing transactions the editor dispatched (A15). */
  documentWrites: number;
}

const ZERO: EditorCounters = {
  wasmResegments: 0,
  wasmSplices: 0,
  wasmSetDocs: 0,
  regionDecodePasses: 0,
  regionsDecoded: 0,
  regionLookups: 0,
  tokenQueries: 0,
  tokensDecoded: 0,
  highlightRangesBuilt: 0,
  gutterRebuilds: 0,
  gutterMarkersConstructed: 0,
  hoverAttributeWrites: 0,
  resultDecoRebuilds: 0,
  cardWidgetsConstructed: 0,
  cardDomMounts: 0,
  cardDomPatches: 0,
  cardStateWrites: 0,
  cardStreamAppends: 0,
  scrollCompensationFrames: 0,
  scrollCompensationPx: 0,
  ipcCalls: 0,
  documentWrites: 0,
};

/** Live counters. Mutated in place so a hot path never allocates to record. */
export const counters: EditorCounters = { ...ZERO };

/** Zero every counter. Tests bracket a keystroke with this. */
export function resetCounters(): void {
  Object.assign(counters, ZERO);
}

/** A copy, for a test that wants a before/after difference. */
export function snapshotCounters(): EditorCounters {
  return { ...counters };
}

// ---------------------------------------------------------------------------
// The wrapper
// ---------------------------------------------------------------------------

/**
 * A per-document segmenter with a generation-scoped region cache.
 *
 * Not a `StratumSegmenter` subclass and not a re-export: the editor asks
 * different questions ("which blocks touch this range") than the wasm contract
 * answers ("region at this offset"), and the difference between those two is
 * where the copying cost hides.
 */
export class EditorSegmenter {
  /** The backend behind this. Dev banner only — never branch on it. */
  readonly backend: StratumSegmenter["backend"];

  private readonly seg: StratumSegmenter;
  /**
   * Decoded regions by index, valid for `cacheGen`.
   *
   * Sparse: a slot is only filled when someone asked for that index. `stamps`
   * carries the generation each slot was filled in, so invalidation is a
   * counter bump and never an O(document) clear — clearing a 6 000-entry array
   * per keystroke is the same mistake as decoding it.
   */
  private cache: (Block | undefined)[] = [];
  private stamps = new Int32Array(0);
  private cacheGen = 0;
  private complete = false;

  constructor(seg: StratumSegmenter) {
    this.seg = seg;
    this.backend = seg.backend;
  }

  /** The segmentation generation. Changes only when the document changed. */
  get generation(): number {
    return this.seg.generation;
  }

  /** Replace the whole document. Open and resync only. */
  setDoc(text: string): void {
    counters.wasmSetDocs += 1;
    this.seg.setDoc(text);
    this.seg.resegment();
    counters.wasmResegments += 1;
    this.invalidate();
  }

  /**
   * Apply one transaction and re-segment. **Exactly one `resegment()`.**
   *
   * `iterChanges` reports in pre-transaction coordinates and in ascending,
   * non-overlapping order, which is precisely what `applyChanges` documents it
   * wants, so the two compose with no translation of our own.
   */
  applyChanges(changes: ChangeSet): number {
    const list: DocChange[] = [];
    changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
      list.push({ from: fromA, to: toA, insert: inserted.toString() });
    });
    if (list.length > 0) {
      this.seg.applyChanges(list);
      counters.wasmSplices += 1;
    }
    const gen = this.seg.resegment();
    counters.wasmResegments += 1;
    this.invalidate();
    return gen;
  }

  /**
   * EVERY block, decoded in one pass. O(document).
   *
   * The honest name for what this costs. Call it from a click path — `run.file`,
   * `run.allStale`, the Sections outline — and never from a keystroke path;
   * `counters.regionDecodePasses` is asserted to be 0 across 200 keystrokes.
   */
  blocks(): readonly Block[] {
    if (this.complete && this.cacheGen === this.seg.generation) {
      return this.cache as Block[];
    }
    counters.regionDecodePasses += 1;
    const decoded = this.seg.regions();
    counters.regionsDecoded += decoded.length;
    this.reserve(decoded.length);
    for (let i = 0; i < decoded.length; i++) {
      this.cache[i] = decoded[i];
      this.stamps[i] = this.cacheGen;
    }
    this.complete = true;
    return decoded;
  }

  /** Region count. One O(1) call into the module; nothing is decoded. */
  count(): number {
    return this.seg.regionCount();
  }

  /** One block by index, memoised for this generation. */
  block(index: number): Block | null {
    if (index < 0) return null;
    const hit = this.hit(index);
    if (hit !== null) return hit;
    counters.regionLookups += 1;
    const decoded = this.seg.region(index);
    if (decoded === null) return null;
    counters.regionsDecoded += 1;
    return this.store(index, decoded);
  }

  /**
   * The block whose OUTER extent contains `pos`.
   *
   * Goes through the module's own binary search rather than one of ours: it runs
   * over the raw rows with no decoding at all and answers with exactly the one
   * region we then pay to decode.
   */
  blockAt(pos: number): Block | null {
    if (pos < 0) return null;
    counters.regionLookups += 1;
    const decoded = this.seg.regionAt(pos);
    if (decoded === null) return null;
    const hit = this.hit(decoded.index);
    if (hit !== null) return hit;
    counters.regionsDecoded += 1;
    return this.store(decoded.index, decoded);
  }

  /** Index of {@link blockAt}, or -1. */
  indexAt(pos: number): number {
    return this.blockAt(pos)?.index ?? -1;
  }

  /**
   * Blocks whose outer extent overlaps `[from, to]`, in document order.
   *
   * The viewport query, and the reason this class exists: it decodes exactly the
   * blocks it returns. A screenful is about thirty of them whether the file is
   * two kilobytes or two hundred.
   */
  blocksTouching(from: number, to: number): Block[] {
    const first = this.blockAt(Math.max(0, from));
    if (first === null) return [];
    const out: Block[] = [first];
    for (let i = first.index + 1; ; i++) {
      const block = this.block(i);
      if (block === null || block.outerFrom > to) break;
      out.push(block);
    }
    return out;
  }

  /** The next block after `block` that can be run. 06 §5.4's advance rule. */
  nextRunnable(after: Block): Block | null {
    for (let i = after.index + 1; ; i++) {
      const block = this.block(i);
      if (block === null) return null;
      if (block.executable) return block;
    }
  }

  /** Tokens overlapping `[from, to)`. Pass the VIEWPORT, never the document. */
  tokens(from: number, to: number): TokenView[] {
    counters.tokenQueries += 1;
    const out = this.seg.tokens(from, to);
    counters.tokensDecoded += out.length;
    return out;
  }

  /** `// %%` section markers. */
  sections(): SectionView[] {
    return this.seg.sections();
  }

  /** `//:` and `/*md` narrative runs. */
  narrativeRegions(): NarrativeView[] {
    return this.seg.narrativeRegions();
  }

  /** The wrapper's document mirror. Asserted against `state.doc` in tests. */
  docText(): string {
    return this.seg.docText();
  }

  /** The underlying contract, for the completion and diagnostics paths. */
  raw(): StratumSegmenter {
    return this.seg;
  }

  destroy(): void {
    this.invalidate();
    this.seg.destroy();
  }

  // --- memo ----------------------------------------------------------------

  private hit(index: number): Block | null {
    return this.stamps[index] === this.cacheGen ? (this.cache[index] ?? null) : null;
  }

  private store(index: number, block: Block): Block {
    this.reserve(index + 1);
    this.cache[index] = block;
    this.stamps[index] = this.cacheGen;
    return block;
  }

  private reserve(size: number): void {
    if (this.stamps.length >= size) return;
    // Grow geometrically. A document that gains one region per keystroke must
    // not reallocate per keystroke.
    const next = new Int32Array(Math.max(size, this.stamps.length * 2, 64));
    next.set(this.stamps);
    this.stamps = next;
  }

  /**
   * Invalidate every memo slot in O(1).
   *
   * The stamp is a generation counter, not a boolean, so a bump makes every
   * filled slot stale without touching the array. `cacheGen` deliberately does
   * not track the module's own generation number: a `setDoc` can leave that
   * unchanged in principle, and a memo that trusts an external counter is a memo
   * that hands back a block from the previous document.
   */
  private invalidate(): void {
    this.cacheGen += 1;
    this.complete = false;
  }
}

// ---------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------

/**
 * How the segmenter reaches a `StateField`.
 *
 * A facet rather than a module-level singleton because a window can hold several
 * editors — 06 §26's "multiple do-files side by side" — and each owns its own
 * wasm `Engine`. A singleton would give the second document the first one's
 * blocks, which reads as "the outline is randomly wrong" rather than as a bug.
 */
export const segmenterFacet = Facet.define<EditorSegmenter, EditorSegmenter | null>({
  combine: (values) => values[0] ?? null,
  static: true,
});

/** The segmenter for a state, or `null` before wasm has finished initialising. */
export function segmenterOf(state: EditorState): EditorSegmenter | null {
  return state.facet(segmenterFacet);
}
