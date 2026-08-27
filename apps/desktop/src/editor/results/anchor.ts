/**
 * Where a card lives — 06 §4.6. The heart of this unit.
 *
 * # The one invariant
 *
 * **The document text is never touched.** A result is a
 * `Decoration.widget({ block: true, side: 1 })` anchored at a document OFFSET,
 * and nothing in this file, in `widget.ts`, in `collapse.ts` or in `orphans.ts`
 * ever dispatches a change. `editor.doc.test.ts` drives 200 random
 * runs/collapses/expands and asserts the document is byte-identical.
 *
 * # Why there are two range sets and not one
 *
 * `anchors` is the execution record — one point per block that has been run,
 * carrying the hash it ran at, the kernel's verdict and the result id. `deco` is
 * the presentation — block widgets, and only when the inline-results mode calls
 * for them (`off` is the Classic preset default and produces `Decoration.none`).
 *
 * Keeping them apart is what makes the gutter correct with inline results
 * switched off: the status glyph reads `anchors`, which exists in every mode.
 * Folding them into one set would mean either fabricating an invisible widget
 * per run in `off` mode, or losing execution state the moment a user switches
 * to Classic.
 *
 * # What a keystroke costs
 *
 * Two `RangeSet.map` calls and nothing else. CodeMirror's own position mapping
 * moves every anchor and every widget correctly for inserts, deletes,
 * multi-cursor edits, undo and external replacements, so an edit 40 lines above
 * a card is zero bookkeeping — that is the entire reason cards anchor by offset
 * and never by line number. `counters.resultDecoRebuilds` and
 * `counters.cardWidgetsConstructed` are asserted to stay at 0 across 200
 * keystrokes in `editor.perf.test.ts`.
 *
 * The three cases that DO need policy are handled here, and only over the
 * changed ranges, never over the document:
 *
 * 1. identity survived → the mapping already put the card in the right place;
 *    if the block's last line moved, `reanchor` moves it in the SAME
 *    transaction, so there is no intermediate frame;
 * 2. `code_hash` changed → nothing happens here at all. Staleness is computed at
 *    read time by {@link displayStatus} from the block's current hash, which is
 *    how it is instant and costs zero IPC (06 §5.2);
 * 3. the anchor was inside deleted text → CodeMirror drops the range, and the
 *    record is handed to `orphans.ts`. **Output is never destroyed by an edit.**
 */

import { MapMode, RangeSet, RangeValue, StateEffect, StateField } from "@codemirror/state";
import type { EditorState, Transaction } from "@codemirror/state";
import { Decoration, EditorView } from "@codemirror/view";
import type { DecorationSet } from "@codemirror/view";
import type {
  CodeHash,
  DatasetStateId,
  ExecId,
  HasBlockState,
  InlineResultsMode,
  ResultId,
} from "../../ipc/hand";
import { blockAt, cardAnchor, stateSegmenter } from "../blocks/blockField";
import type { Block } from "../blocks/segmenter";
import { counters } from "../blocks/segmenter";
import { type CardUiState, DEFAULT_CARD_UI } from "./collapse";
import type { OrphanResult } from "./orphans";
import { ResultWidget } from "./widget";

/** Monotonic per-window anchor id. Not a `BlockId`; the engine allocates those. */
let nextAnchorId = 1;

/** What one executed block knows about itself. Immutable; replaced, never edited. */
export interface ExecRecord {
  /** Window-local identity. Survives every edit; the anchor position does not. */
  readonly id: number;
  /** The code hash at the moment the run was submitted (06 §5.2's local check). */
  readonly executedHash: CodeHash;
  /** Occurrence index of that hash when it ran. */
  readonly executedOrdinal: number;
  /** First line of the code as it was when it ran — the orphan menu's label. */
  readonly label: string;
  /** The kernel's verdict. `queued` until the engine says otherwise. */
  readonly kernel: HasBlockState;
  /** Execution id, once the engine has allocated one. */
  readonly exec: ExecId | undefined;
  /** Dataset state the run observed. */
  readonly dataset: DatasetStateId | undefined;
  /** Duration in ms, RECORDED not asserted (ADR-017). */
  readonly durationMs: number | undefined;
  /** The envelope this card renders, once one has arrived. */
  readonly result: ResultId | undefined;
  /** Presentation state. Collapse intent itself is durable in `collapse.ts`. */
  readonly ui: CardUiState;
  /** True while output is streaming — the card keeps a FIXED height (§4.6). */
  readonly streaming: boolean;
}

/**
 * The side a block widget with `side: 1` ends up on.
 *
 * Not an arbitrary large number, and specifically NOT 1e9: CodeMirror uses
 * ±1e9 as the sentinel in `RangeSet.findIndex`, so a value carrying that exact
 * side compares equal to the sentinel and `between()` silently skips it — the
 * anchor is in the set, `size` is 1, and every query returns nothing. Matching
 * `Decoration.widget({ block: true, side: 1 })`'s own arithmetic
 * (`side + 300000000`) is both correct and the honest expression of the intent:
 * the anchor sorts exactly where its card sorts.
 */
const BLOCK_WIDGET_SIDE = 300_000_001;

/**
 * One execution record, positioned.
 *
 * `MapMode.TrackDel` is what makes case 3 above CodeMirror's job rather than
 * ours: the range is dropped exactly when the text around it was deleted.
 */
export class ResultAnchor extends RangeValue {
  override point = true;
  override startSide = BLOCK_WIDGET_SIDE;
  override endSide = BLOCK_WIDGET_SIDE;
  override mapMode = MapMode.TrackDel;

  constructor(readonly rec: ExecRecord) {
    super();
  }

  override eq(other: RangeValue): boolean {
    return other instanceof ResultAnchor && other.rec === this.rec;
  }
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

/** A run was submitted: create (or replace) the anchor for this block. */
export const beginRun = StateEffect.define<{
  readonly at: number;
  readonly executedHash: CodeHash;
  readonly executedOrdinal: number;
  readonly label: string;
}>();

/** The engine spoke about a run. Keyed by anchor id, never by position. */
export const updateRun = StateEffect.define<{
  readonly id: number;
  readonly patch: Partial<Omit<ExecRecord, "id">>;
}>();

/** Drop one anchor and its card. The result survives in the scrollback. */
export const detachAnchor = StateEffect.define<number>();

/** Per-card presentation change — collapse, raw toggle, measured height. */
export const setCardUi = StateEffect.define<{ readonly id: number; readonly ui: CardUiState }>();

/** `Mod+Alt+I`. Reconfigures presentation only; no anchor is touched. */
export const setInlineMode = StateEffect.define<InlineResultsMode>();

// ---------------------------------------------------------------------------
// The field
// ---------------------------------------------------------------------------

export interface ResultsState {
  /** Execution records, positioned. Present in every inline-results mode. */
  readonly anchors: RangeSet<ResultAnchor>;
  /** Block widgets. `Decoration.none` when the mode is `off`. */
  readonly deco: DecorationSet;
  /** Current inline-results mode. */
  readonly mode: InlineResultsMode;
  /**
   * Anchors this transaction destroyed. Drained by the update listener in
   * `setup.ts` into `orphans.ts`, so this field stays a pure function of its
   * inputs and the store is written exactly once per transaction.
   */
  readonly orphaned: readonly OrphanResult[];
}

const NOTHING: readonly OrphanResult[] = [];

export const resultsField = StateField.define<ResultsState>({
  create() {
    return {
      anchors: RangeSet.empty,
      deco: Decoration.none,
      mode: "always",
      orphaned: NOTHING,
    };
  },

  update(value, tr) {
    let anchors = value.anchors;
    let deco = value.deco;
    let mode = value.mode;
    let orphaned = NOTHING;
    let rebuild = false;

    if (tr.docChanged) {
      orphaned = collectOrphans(value.anchors, tr);
      anchors = anchors.map(tr.changes);
      deco = deco.map(tr.changes);
      const moved = reanchor(tr.state, anchors, tr);
      if (moved !== null) {
        anchors = moved;
        rebuild = true;
      }
    }

    for (const effect of tr.effects) {
      if (effect.is(beginRun)) {
        anchors = applyBeginRun(tr.state, anchors, effect.value);
        rebuild = true;
      } else if (effect.is(updateRun)) {
        anchors = patchRecord(anchors, effect.value.id, effect.value.patch);
        rebuild = true;
      } else if (effect.is(setCardUi)) {
        anchors = patchRecord(anchors, effect.value.id, { ui: effect.value.ui });
        rebuild = true;
      } else if (effect.is(detachAnchor)) {
        anchors = dropAnchor(anchors, effect.value);
        rebuild = true;
      } else if (effect.is(setInlineMode)) {
        mode = effect.value;
        rebuild = true;
      }
    }

    if (rebuild) deco = buildDeco(anchors, mode);

    return anchors === value.anchors &&
      deco === value.deco &&
      mode === value.mode &&
      orphaned === value.orphaned
      ? value
      : { anchors, deco, mode, orphaned };
  },

  provide: (field) => EditorView.decorations.from(field, (state) => state.deco),
});

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/**
 * Records whose anchor this transaction deleted.
 *
 * Skipped entirely when the transaction only inserts — which is what typing is —
 * because a pure insertion cannot delete a point range, and scanning the anchor
 * set per keystroke is exactly the O(cards) work this design exists to avoid.
 * When there IS a deletion, only the deleted spans are scanned.
 */
function collectOrphans(anchors: RangeSet<ResultAnchor>, tr: Transaction): readonly OrphanResult[] {
  if (anchors.size === 0) return NOTHING;
  let deletes = false;
  tr.changes.iterChanges((fromA, toA) => {
    if (toA > fromA) deletes = true;
  });
  if (!deletes) return NOTHING;

  const out: OrphanResult[] = [];
  const detachedAt = Date.now();
  tr.changes.iterChanges((fromA, toA) => {
    if (toA <= fromA) return;
    anchors.between(fromA, toA, (from, _to, anchor) => {
      if (tr.changes.mapPos(from, 1, MapMode.TrackDel) !== null) return;
      const rec = anchor.rec;
      if (rec.result === undefined) return; // nothing produced yet: nothing to orphan
      out.push({
        result: rec.result,
        executedHash: rec.executedHash,
        executedOrdinal: rec.executedOrdinal,
        label: rec.label,
        detachedAt,
      });
    });
  });
  return out.length === 0 ? NOTHING : out;
}

/**
 * Move any anchor that no longer sits at its block's last line.
 *
 * **Scoped to the changed ranges, never to the anchor set.** An anchor far from
 * the edit was already moved correctly by `RangeSet.map` — that is the whole
 * point of anchoring by offset — so examining it would be one wasm lookup per
 * card per keystroke, which on a 500-card document is the entire frame budget
 * spent confirming that nothing happened.
 *
 * Each changed range is widened to the blocks that now contain its ends, which
 * is what catches the case this function exists for: deleting the newline
 * between two blocks merges them and leaves the first card floating in the
 * middle of a statement. 06 §4.6 is explicit that we never leave one there.
 */
function reanchor(
  state: EditorState,
  anchors: RangeSet<ResultAnchor>,
  tr: Transaction,
): RangeSet<ResultAnchor> | null {
  if (anchors.size === 0) return null;
  const seg = stateSegmenter(state);
  if (seg === null) return null;

  const seen = new Set<ResultAnchor>();
  let moves: { to: number; anchor: ResultAnchor }[] | null = null;

  tr.changes.iterChangedRanges((_fromA, _toA, fromB, toB) => {
    const head = seg.blockAt(Math.max(0, fromB - 1));
    const tail = seg.blockAt(Math.min(state.doc.length, toB));
    const start = head?.outerFrom ?? fromB;
    const end = tail?.outerTo ?? toB;
    anchors.between(start, end, (at, _to, anchor) => {
      if (seen.has(anchor)) return;
      seen.add(anchor);
      const block = seg.blockAt(at);
      if (block === null) return;
      const want = cardAnchor(state, block);
      if (want === at) return;
      if (moves === null) moves = [];
      moves.push({ to: want, anchor });
    });
  });

  if (moves === null) return null;
  const moved: { to: number; anchor: ResultAnchor }[] = moves;
  const filter = new Set(moved.map((m) => m.anchor));
  return anchors.update({
    filter: (_from, _to, value) => !filter.has(value),
    add: moved.map((m) => m.anchor.range(m.to)),
    sort: true,
  });
}

function applyBeginRun(
  state: EditorState,
  anchors: RangeSet<ResultAnchor>,
  spec: {
    readonly at: number;
    readonly executedHash: CodeHash;
    readonly executedOrdinal: number;
    readonly label: string;
  },
): RangeSet<ResultAnchor> {
  const block = blockAt(state, spec.at);
  const at = block === null ? state.doc.lineAt(spec.at).to : cardAnchor(state, block);

  // One block, one card (06 §4.7). Re-running replaces the record in place; the
  // previous `ResultView` stays reachable through `state/results.ts`, which keeps
  // every version a block has produced.
  const replaced = new Set<ResultAnchor>();
  anchors.between(at, at, (_from, _to, anchor) => {
    replaced.add(anchor);
  });

  const rec: ExecRecord = {
    id: nextAnchorId++,
    executedHash: spec.executedHash,
    executedOrdinal: spec.executedOrdinal,
    label: spec.label,
    kernel: { state: "queued" },
    exec: undefined,
    dataset: undefined,
    durationMs: undefined,
    result: undefined,
    ui: DEFAULT_CARD_UI,
    streaming: false,
  };

  return anchors.update({
    filter: (_from, _to, value) => !replaced.has(value),
    add: [new ResultAnchor(rec).range(at)],
    sort: true,
  });
}

function patchRecord(
  anchors: RangeSet<ResultAnchor>,
  id: number,
  patch: Partial<Omit<ExecRecord, "id">>,
): RangeSet<ResultAnchor> {
  let found: { at: number; anchor: ResultAnchor } | null = null;
  const cursor = anchors.iter();
  for (; cursor.value !== null; cursor.next()) {
    if (cursor.value.rec.id === id) {
      found = { at: cursor.from, anchor: cursor.value };
      break;
    }
  }
  if (found === null) return anchors;
  const next = new ResultAnchor({ ...found.anchor.rec, ...patch, id });
  return anchors.update({
    filter: (_from, _to, value) => value !== found?.anchor,
    add: [next.range(found.at)],
    sort: true,
  });
}

function dropAnchor(anchors: RangeSet<ResultAnchor>, id: number): RangeSet<ResultAnchor> {
  return anchors.update({ filter: (_from, _to, value) => value.rec.id !== id });
}

/**
 * Derive the widget decorations.
 *
 * Called only when an effect changed something a card renders — never on a
 * keystroke. `off` produces an empty set, which is 06 §4.6's "no widgets at all"
 * and the Classic preset's default.
 */
function buildDeco(anchors: RangeSet<ResultAnchor>, mode: InlineResultsMode): DecorationSet {
  counters.resultDecoRebuilds += 1;
  if (mode === "off" || anchors.size === 0) return Decoration.none;

  const ranges: { from: number; to: number; value: Decoration }[] = [];
  const cursor = anchors.iter();
  for (; cursor.value !== null; cursor.next()) {
    const widget = new ResultWidget(cursor.value.rec, mode);
    ranges.push({
      from: cursor.from,
      to: cursor.from,
      value: Decoration.widget({ widget, block: true, side: 1, anchorId: cursor.value.rec.id }),
    });
  }
  return Decoration.set(
    ranges.map((r) => r.value.range(r.from, r.to)),
    true,
  );
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/** Every execution record overlapping a range, in document order. */
export function anchorsIn(
  state: EditorState,
  from: number,
  to: number,
): { at: number; rec: ExecRecord }[] {
  const field = state.field(resultsField, false);
  if (field === undefined) return [];
  const out: { at: number; rec: ExecRecord }[] = [];
  field.anchors.between(from, to, (at, _to, anchor) => {
    out.push({ at, rec: anchor.rec });
  });
  return out;
}

/** The execution record attached inside this block, if any. */
export function anchorForBlock(state: EditorState, block: Block): ExecRecord | null {
  const field = state.field(resultsField, false);
  if (field === undefined) return null;
  let found: ExecRecord | null = null;
  field.anchors.between(block.outerFrom, block.outerTo, (_at, _to, anchor) => {
    found = anchor.rec;
  });
  return found;
}

/** Look one up by id — what a card's own event handlers hold. */
export function anchorById(state: EditorState, id: number): { at: number; rec: ExecRecord } | null {
  const field = state.field(resultsField, false);
  if (field === undefined) return null;
  const cursor = field.anchors.iter();
  for (; cursor.value !== null; cursor.next()) {
    if (cursor.value.rec.id === id) return { at: cursor.from, rec: cursor.value.rec };
  }
  return null;
}

/**
 * `displayed = worseOf(local, kernel)`.
 *
 * Defined in `widget.ts` and re-exported here, where callers look for it: this
 * module constructs widgets, so the import may only run in that direction.
 */
export { displayStatus } from "./widget";

/** Test seam: the anchor id counter is window-global and must be resettable. */
export function resetAnchorIds(): void {
  nextAnchorId = 1;
}
